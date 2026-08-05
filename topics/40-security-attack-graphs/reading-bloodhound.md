# BloodHound: the directory is a graph, and the attacker already knows

Active Directory is administered as a list of objects with a list of permissions on each. It is
*used* as a directed graph, because permissions compose: if you control a group you control what
the group controls. BloodHound's contribution was not an algorithm — it was noticing that the
whole discipline of "review each object's ACL" is answering the wrong query, and that the right
one is shortest path. The engineering underneath is a graph database with a 104-concept ontology,
31 edge kinds that are *computed* rather than collected, roaring bitmaps for principal sets, and
a parallel BFS. All of that is this book's material, wearing a security badge.

## The problem in one sentence

**"Who is a Domain Admin?" is a membership lookup that returns five names; "who can *become*
Domain Admin?" is a reachability query over composed permissions that returns most of the
company — and no per-object permission review can see the difference.**

## The concepts, step by step

### Step 1 — An edge means "control of the source yields control of the target"

> **In:** the directory as a list of objects, each carrying an ACL.
> **Out:** the graph reframe — nodes are principals or resources, edges are rights oriented so that traversal *is* privilege escalation — and why "can Alice become Domain Admin?" is a shortest-path query the AD console cannot ask.

That one sentence is the entire data model. A *node* is a principal or a resource: a user, a
group, a computer, a GPO, a certificate template. An *edge* is a right, oriented so that
traversal means privilege escalation. `Alice -MemberOf-> Engineering` means controlling Alice
gets you Engineering's rights. `Engineering -AdminTo-> WKSTN-14` means Engineering members are
local administrators there. Once every right is expressed this way, "can Alice become a Domain
Admin?" is `MATCH p = shortestPath((alice)-[*1..]->(da))` and nothing else. The reason this
felt like a revelation in 2016 is that the directory's own tooling has no way to ask it: the
console shows you one object's ACL at a time, and composition is invisible one object at a time.

### Step 2 — The ontology: 104 kinds, 64 of them traversable

> **In:** the edge-means-control model from Step 1.
> **Out:** the kind namespace (104 `StringKind` constants) and its four purposeful partitions — `Relationships` (88 edge kinds), `ACLRelationships` (30), `PathfindingRelationships` (64), `PostProcessedRelationships` (31) — and why the pathfinding subset is a query-time edge-kind filter.

`graphschema/ad/ad.go:28` onward is a wall of constants — `graph.StringKind("User")`,
`StringKind("GenericAll")`, `StringKind("ADCSESC1")` — 104 of them, node kinds and edge kinds in
one namespace. What matters is that the file then partitions them into *purposeful* sets:

```go
// graphschema/ad/ad.go — the four partitions (non-adjacent in the file; each line keeps its real number)
1151  func Relationships() []graph.Kind             // everything: 88 edge kinds
1154  func ACLRelationships() []graph.Kind          // 30 rights that come from a DACL
1160  func PathfindingRelationships() []graph.Kind  // the 64 an attacker may walk
1172  func PostProcessedRelationships() []graph.Kind // the 31 that are DERIVED
```

`PathfindingRelationships` is the attacker's alphabet, and it is smaller than the full 88-kind edge
set: some collected rights are deliberately *excluded* because they do not, on their own, grant
control. `GetChanges` and `GetChangesAll` — the two directory-replication rights — are a clean
example: both appear in `Relationships` and `ACLRelationships` (they are real, collected ACEs), but
neither is in `PathfindingRelationships`, because only their *conjunction* is dangerous, and
post-processing synthesizes that conjunction into a single `DCSync` edge (Step 3). A traversal that
walked `GetChanges` alone would report a path that is not an attack. This is a *query-time edge-kind
filter*, and it is exactly the mask your Cypher engine has to push down into the CSR scan — see the
capstone.

### Step 3 — 31 edge kinds are materialized views

> **In:** the four partitions, and the 31-kind `PostProcessedRelationships` set.
> **Out:** why edges like `AdminTo` / `DCSync` / `ADCSESC*` are *derived* by a post-ingest pass and written back as real edges — a batch materialized view, which is topic 27's question in disguise.

`AdminTo` is not collected. Nor is `CanRDP`, nor `DCSync`, nor the ADCS certificate-abuse family
`ADCSESC1..ADCSESC13`. They are *derived* by a post-processing pass and written back into the
graph as real edges. `analysis/ad/post.go:84`:

```go
// analysis/ad/post.go — an attacker holding both GetChanges and GetChangesAll on the domain
// can replicate secrets, so post-processing synthesizes one DCSync edge instead of making
// every query re-derive the conjunction.
84  func PostDCSync(ctx context.Context, db graph.Database, localGroupData *LocalGroupData) (*post.AtomicPostProcessingStats, error) {
```

The trade is the one topic 1 calls RUM and topic 27 calls incremental view maintenance: pay once
at analysis time, in bulk, so that every subsequent path query is a plain traversal instead of a
predicate evaluation per hop. The ADCS edges are the extreme case — `esc1.go`, `esc3.go`,
`esc4.go`, `esc6.go`, `esc9.go`, `esc10.go`, `esc13.go` each encode a multi-condition certificate
misconfiguration into a single edge. The whole pipeline is declared in one place,
`analysis/analysis.go:346`:

```go
// analysis/analysis.go — the post-ingest pipeline, four ordered stages
345  // The definition of our analysis pipeline
346  func newPipeline() analysisPipeline {
347  	return analysisPipeline{
348  		{
349  			analysisStep: model.AnalysisStepADPostProcessing(),
350  			operation:    adPostProcessingOperation,
351  		},
352  		{
353  			analysisStep: model.AnalysisStepAzurePostProcessing(),
354  			operation:    azurePostProcessingOperation,
355  		},
356  		{
357  			analysisStep: model.AnalysisStepTagging(),
358  			operation:    taggingOperation,
359  		},
360  		{
361  			name:      DataQuality,
362  			operation: dataQualityOperation,
363  		},
364  	}
365  }
```

Four ordered stages, run after every ingest. That is a batch view-maintenance schedule, and the
question it dodges — why not maintain the derived edges incrementally on write? — is exactly the
question topic 27 spends a whole topic on.

### Step 4 — Principal sets are roaring bitmaps

> **In:** the derived-edge pass, which is set algebra over node ids.
> **Out:** roaring bitmaps (`cardinality.Duplex[uint64]`) as the principal-set representation, and the recognition that this is topic 23's postings lists holding principals instead of document ids.

Every interesting operation here is set algebra over node ids: "principals with `GetChanges`"
intersected with "principals with `GetChangesAll`", "everything reachable from these seeds" minus
"everything already tagged". BloodHound stores those sets as roaring bitmaps —
`cardinality.Duplex[uint64]` — and `analysis/ad/post.go:244` is the workhorse:

```go
// analysis/ad/post.go — roaring bitmaps as principal sets
242  // FetchNodeIDsByKind fetches a bitmap of node IDs where each node has at least one kind assignment
243  // that matches the given kind.
244  func FetchNodeIDsByKind(tx graph.Transaction, targetKind graph.Kind) (cardinality.Duplex[uint64], error) {
```

This is topic 23's postings-list structure doing identity management. The intersection that
derives `DCSync` is a roaring AND; the "have I visited this node" check in a traversal is a
roaring membership test. You have already read this data structure; here it is holding
principals instead of document ids.

### Step 5 — Traversal: parallel BFS with a shared bitmap as the visited set

> **In:** roaring-bitmap principal sets and the derived graph.
> **Out:** the parallel BFS whose visited set is a thread-safe roaring bitmap, with `CheckedAdd` as the atomic test-and-set, and `direction` as the forward/backward switch that defense needs.

`analysis/ad/membership.go:81`:

```go
// analysis/ad/membership.go — parallel BFS; the thread-safe roaring bitmap is the visited set
 81  func FetchPathMembers(ctx context.Context, db graph.Database, root graph.ID, direction graph.Direction, queryCriteria ...graph.Criteria) (cardinality.Duplex[uint64], error) {
 82  	traversalMap := cardinality.ThreadSafeDuplex(cardinality.NewBitmap64())
 84  	return traversalMap, traversal.New(db, post.MaximumDatabaseParallelWorkers).BreadthFirst(ctx, traversal.Plan{
     	// … Driver visits each neighbour of the current segment:
 95  		for next := range cursor.Chan() {
 96  			nextSegment := segment.Descend(next.Node, next.Relationship)
 98  			if traversalMap.CheckedAdd(next.Node.ID.Uint64()) {
 99  				nextSegments = append(nextSegments, nextSegment)
100  			}
101  		}
```

Three things to notice. The traversal is *parallel* over a worker pool. The visited set is a
thread-safe roaring bitmap, and `CheckedAdd` is the atomic test-and-set that makes the frontier
expansion race-free without a lock per node — the same trick as a CAS-marked visited array in a
GPU BFS (topic 18). And `direction` is a parameter: the same code walks forward ("what can this
principal reach?") and backward ("who can reach this?"). The backward direction is the one that
matters for defense, and it is the one lane 2's dominator analysis builds on.

### Step 6 — Tier Zero: the label the product is organised around

> **In:** cheap forward/backward reachability from Step 5.
> **Out:** the two labeled node sets — Tier Zero and Owned — that every product question reduces to, and lane 2's measured tiered-vs-flat contrast.

`analysis/tiering/tiering.go:37`:

```go
// analysis/tiering/tiering.go — the Tier Zero predicate (string tags at :28, kind tags at :33)
28  	StrTagTierZero = "Tag_Tier_Zero"
29  	StrTagOwned    = "Tag_Owned"
37  func IsTierZero(node *graph.Node) bool {
38  	if node.Kinds.ContainsOneOf(KindTagTierZero) {
39  		return true
40  	} else {
42  		startSystemTags, _ := node.Properties.Get(common.SystemTags.String()).String()
43  		return strings.Contains(startSystemTags, ad.AdminTierZero)
44  	}
45  }
```

Tier Zero is "assets whose compromise is game over"; `Owned` is "assets the attacker already
holds". Every question the product asks is one of those two sets against the other: paths from
`Owned` to Tier Zero, principals with a path *into* Tier Zero, edges whose removal shrinks that
set. Two labeled node sets and a reachability relation between them — that is the whole product,
and it is why the interesting engineering is in making reachability cheap rather than in the
security domain knowledge.

Tiering also turns out to be what makes the graph *analyzable*, not just safer. Lane 2 measures
it: in a tiered directory one group has a 99.6% blast radius and one cut collapses exposure from
2000 users to 8; in the same directory with three unmanaged service-account groups and two
misplaced Domain Admin tokens, **no single node cut frees a single user**. Same exposure number,
and the second one has no remediation you can rank.

### Step 7 — Asset-group selectors: user-defined node sets, diffed

> **In:** labeled node sets, and a way to declare more of them.
> **Out:** analyst-declared selectors (by object id or arbitrary Cypher), expanded along known parent/child paths and kept current as a *diff* rather than a rewrite — with two operational details worth stealing.

`analysis/agt.go:137` (`FetchNodesFromSeeds`) and `:562` (`SelectNodes`) implement "an analyst
declares a set of nodes — by object id, or by an arbitrary Cypher selector — and the system
expands it along known parent/child paths and keeps it current". Two details worth stealing:
selection failures from a user-supplied Cypher query get their own error type
(`CypherSelectorError`, `agt.go:77`) so a bad selector degrades to a *partial* completion instead
of failing the pipeline; and `fetchOldSelectedNodes` (`:549`) loads the previous selection so the
write is a diff, not a rewrite. Both are the kind of thing you only build after operating
something.

## Where each step lives in the code

Repo: [`~/repos/bloodhound`](https://github.com/SpecterOps/BloodHound) @ `1968388`, all paths under `packages/go/`.

| step | anchor | what to read for |
|---|---|---|
| 1, 2 | `graphschema/ad/ad.go:28` | the 104 kinds; note how many are ACL rights vs structure |
| 2 | `graphschema/ad/ad.go:1151/:1154/:1160/:1172` | the four partitions — and which kinds appear in `Pathfinding` but not `ACL` |
| 3 | `graphschema/ad/ad.go:1172` | `PostProcessedRelationships` — count them, then find where each is written |
| 3 | `analysis/ad/post.go:84`, `:125`, `:172` | `PostDCSync`, `PostProtectAdminGroups`, `PostHasTrustKeys` |
| 3 | `analysis/ad/esc1.go`, `esc9.go`, `esc13.go` | one derived edge per certificate misconfiguration |
| 3 | `analysis/analysis.go:346` | `newPipeline` — the four ordered stages |
| 4 | `analysis/ad/post.go:244`, `:271` | `FetchNodeIDsByKind` — roaring bitmaps as principal sets |
| 5 | `analysis/ad/membership.go:81` | `FetchPathMembers` — parallel BFS, `CheckedAdd` as the visited test |
| 5 | `analysis/analysis.go:104` | `ExpandGroupMembershipPaths` — nesting expansion as a path query |
| 6 | `analysis/tiering/tiering.go:37`, `:47` | `IsTierZero`, `IsOwned` |
| 7 | `analysis/agt.go:77`, `:137`, `:549`, `:562` | selector errors, seed expansion, previous-state diffing |

## Questions to answer in notes.md

1. `PathfindingRelationships` (64 kinds) is a strict subset of the 104-kind ontology (and of the
   88-kind `Relationships` edge set). Pick two kinds that are excluded and explain, in attacker
   terms, why walking them would produce a path that is not an attack.
2. The 31 post-processed edges are a materialized view refreshed after every ingest. Sketch what
   incremental maintenance would cost instead, for `DCSync` specifically: which writes invalidate
   it, and how would you index for that?
3. `FetchPathMembers` uses `CheckedAdd` on a thread-safe roaring bitmap as its visited set. What
   goes wrong with a plain `Contains` followed by `Add`, and what does that tell you about the
   ordering guarantee a parallel BFS actually needs?
4. Lane 1 measures exposure rising from 39 to 1969 users as session data accumulates. BloodHound
   collects sessions by sampling live logons. What does that make "% of users with a path to Tier
   Zero" a measurement *of*, and how would you report it honestly?
5. Lane 2 finds no single-node choke point in the flat directory. Using the edge kinds in
   `PathfindingRelationships`, name the three concrete misconfigurations most likely to create
   that situation in a real domain, and say which one you would fix first and why.

## Done when

Answer each before unfolding it.

- [ ] You can state the edge semantics in one sentence and derive the shortest-path formulation
      from it.

  <details><summary>Answer</summary>

  Edge semantics: `A → B` means *control of A yields control of B*, oriented so that traversal is
  privilege escalation. Because control composes transitively, "can A become Domain Admin?" is
  exactly "is there a directed path `A →* DA`?" — `MATCH p = shortestPath((a)-[*1..]->(da))`. A
  per-object ACL review sees one hop at a time; the query sees the composition, which is why the
  reachable set is most of the company while the membership answer is five names.

  </details>

- [ ] You can name the four kind partitions and explain what each is for.

  <details><summary>Answer</summary>

  From `graphschema/ad/ad.go`: `Relationships()` (:1151, **88** edge kinds) is the full edge
  alphabet; `ACLRelationships()` (:1154, **30**) are the rights that come from a DACL;
  `PathfindingRelationships()` (:1160, **64**) is the subset an attacker may actually walk — the
  traversal mask; `PostProcessedRelationships()` (:1172, **31**) are the derived edges written back
  after ingest. The whole namespace is **104** `StringKind` constants (16 node kinds + 88 edge
  kinds).

  </details>

- [ ] You can explain why 31 edge kinds are derived, and connect that to topic 27.

  <details><summary>Answer</summary>

  `AdminTo`, `DCSync`, `CanRDP`, the `ADCSESC*` family, etc. are conjunctions or closures over
  collected rights — e.g. `DCSync` = `GetChanges` ∧ `GetChangesAll` on the domain. Materializing
  them once per ingest (`newPipeline`, `analysis.go:346`) turns every later path query into a plain
  traversal instead of a per-hop predicate evaluation: RUM's read/update trade (topic 1), i.e. a
  batch materialized view. Topic 27's question is why refresh in bulk rather than incrementally on
  write.

  </details>

- [ ] You can point at the roaring bitmap in the traversal and say what it replaces.

  <details><summary>Answer</summary>

  `FetchPathMembers` (`membership.go:81`) uses `traversalMap`, a `cardinality.ThreadSafeDuplex`
  roaring bitmap, as its visited set; `CheckedAdd` — an atomic test-and-set — replaces a per-node
  lock, so the parallel frontier expansion is race-free without one. It is topic 23's postings-list
  membership test doing identity management: a roaring AND derives `DCSync`, a roaring membership
  check dedupes the BFS frontier.

  </details>

- [ ] Your `chokepoint.rs` reproduces the tiered/flat contrast: 1992-user blast radius vs none.

  <details><summary>Answer</summary>

  Tiered directory: one group has a **1992 / 2000 = 99.6%** blast radius and a single cut collapses
  exposure `2000 → 8`. Flat directory (three unmanaged service-account groups plus two misplaced
  Domain Admin tokens): the *same* exposure number, but **no single-node cut frees a single user** —
  there is no remediation you can rank. Same headline, only one is actionable.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five: (1) two pathfinding-excluded kinds explained in attacker terms (e.g. `GetChanges` /
  `GetChangesAll`); (2) the incremental-maintenance cost of `DCSync` and how to index for it; (3)
  why `CheckedAdd` beats `Contains`-then-`Add` in a parallel BFS; (4) what "% of users with a path
  to Tier Zero" measures given that sessions are *sampled* live; (5) three misconfigurations that
  create a no-choke-point flat directory, and which to fix first.

  </details>

## References

- Code: [SpecterOps/BloodHound](https://github.com/SpecterOps/BloodHound) — `packages/go/graphschema/ad/`,
  `packages/go/analysis/`. Read `ad.go` first, then `analysis.go`, then `post.go`.
- Robbins, Vazarkar, Schroeder. *Six Degrees of Domain Admin.* DEF CON 24 (2016) — the original
  framing of the list-vs-graph gap.
- Microsoft, *Securing privileged access: the administrative tier model* — the tiering that lane 2
  shows is what creates choke points in the first place.
- Local experiment: `topics/40-security-attack-graphs/experiments/ad_graph.rs` — the five edge
  kinds above, with a planted over-privileged group and planted policy violations.
- Topic 26 (probabilistic & indexing) — roaring bitmaps; topic 27 (streaming & IVM) — the
  derived-edge refresh question; topic 18 (GPU) — frontier BFS with an atomic visited set.
