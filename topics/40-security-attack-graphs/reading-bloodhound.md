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

That one sentence is the entire data model. A *node* is a principal or a resource: a user, a
group, a computer, a GPO, a certificate template. An *edge* is a right, oriented so that
traversal means privilege escalation. `Alice -MemberOf-> Engineering` means controlling Alice
gets you Engineering's rights. `Engineering -AdminTo-> WKSTN-14` means Engineering members are
local administrators there. Once every right is expressed this way, "can Alice become a Domain
Admin?" is `MATCH p = shortestPath((alice)-[*1..]->(da))` and nothing else. The reason this
felt like a revelation in 2016 is that the directory's own tooling has no way to ask it: the
console shows you one object's ACL at a time, and composition is invisible one object at a time.

### Step 2 — The ontology: 104 kinds, 63 of them traversable

`graphschema/ad/ad.go:28` onward is a wall of constants — `graph.StringKind("User")`,
`StringKind("GenericAll")`, `StringKind("ADCSESC1")` — 104 of them, node kinds and edge kinds in
one namespace. What matters is that the file then partitions them into *purposeful* sets:

```go
func Relationships() []graph.Kind          // ad.go:1151 — everything
func ACLRelationships() []graph.Kind       // ad.go:1154 — rights that come from a DACL
func PathfindingRelationships() []graph.Kind  // ad.go:1160 — the 63 an attacker may walk
func PostProcessedRelationships() []graph.Kind // ad.go:1172 — the 31 that are DERIVED
```

`PathfindingRelationships` is the attacker's alphabet, and it is smaller than the full set: some
edges (`Contains`, structural containment) exist for display or for post-processing but do not
by themselves grant control. A traversal that ignores this distinction reports paths that are not
attacks. This is a *query-time edge-kind filter*, and it is exactly the mask your Cypher engine
has to push down into the CSR scan — see the capstone.

### Step 3 — 31 edge kinds are materialized views

`AdminTo` is not collected. Nor is `CanRDP`, nor `DCSync`, nor the ADCS certificate-abuse family
`ADCSESC1..ADCSESC13`. They are *derived* by a post-processing pass and written back into the
graph as real edges. `analysis/ad/post.go:84`:

```go
// PostDCSync: an attacker who holds both GetChanges and GetChangesAll on the domain
// can replicate secrets — so synthesize one DCSync edge instead of making every
// query re-derive the conjunction.
func PostDCSync(ctx context.Context, db graph.Database, localGroupData *LocalGroupData)
    (*post.AtomicPostProcessingStats, error)
```

The trade is the one topic 1 calls RUM and topic 27 calls incremental view maintenance: pay once
at analysis time, in bulk, so that every subsequent path query is a plain traversal instead of a
predicate evaluation per hop. The ADCS edges are the extreme case — `esc1.go`, `esc3.go`,
`esc4.go`, `esc6.go`, `esc9.go`, `esc10.go`, `esc13.go` each encode a multi-condition certificate
misconfiguration into a single edge. The whole pipeline is declared in one place,
`analysis/analysis.go:346`:

```go
func newPipeline() analysisPipeline {
    return analysisPipeline{
        {analysisStep: model.AnalysisStepADPostProcessing(),    operation: adPostProcessingOperation},
        {analysisStep: model.AnalysisStepAzurePostProcessing(), operation: azurePostProcessingOperation},
        {analysisStep: model.AnalysisStepTagging(),             operation: taggingOperation},
        {name: DataQuality,                                     operation: dataQualityOperation},
    }
}
```

Four ordered stages, run after every ingest. That is a batch view-maintenance schedule, and the
question it dodges — why not maintain the derived edges incrementally on write? — is exactly the
question topic 27 spends a whole topic on.

### Step 4 — Principal sets are roaring bitmaps

Every interesting operation here is set algebra over node ids: "principals with `GetChanges`"
intersected with "principals with `GetChangesAll`", "everything reachable from these seeds" minus
"everything already tagged". BloodHound stores those sets as roaring bitmaps —
`cardinality.Duplex[uint64]` — and `analysis/ad/post.go:244` is the workhorse:

```go
// FetchNodeIDsByKind fetches a bitmap of node IDs where each node has at least one
// kind assignment that matches the given kind.
func FetchNodeIDsByKind(tx graph.Transaction, targetKind graph.Kind) (cardinality.Duplex[uint64], error)
```

This is topic 23's postings-list structure doing identity management. The intersection that
derives `DCSync` is a roaring AND; the "have I visited this node" check in a traversal is a
roaring membership test. You have already read this data structure; here it is holding
principals instead of document ids.

### Step 5 — Traversal: parallel BFS with a shared bitmap as the visited set

`analysis/ad/membership.go:81`:

```go
func FetchPathMembers(ctx context.Context, db graph.Database, root graph.ID,
                      direction graph.Direction, queryCriteria ...graph.Criteria)
                      (cardinality.Duplex[uint64], error) {
    traversalMap := cardinality.ThreadSafeDuplex(cardinality.NewBitmap64())
    return traversalMap, traversal.New(db, post.MaximumDatabaseParallelWorkers).BreadthFirst(ctx, traversal.Plan{
        Root: graph.NewNode(root, graph.NewProperties()),
        Driver: func(...) ([]*graph.PathSegment, error) {
            // ... for each neighbour:
            if traversalMap.CheckedAdd(next.Node.ID.Uint64()) {
                nextSegments = append(nextSegments, nextSegment)
            }
        },
    })
}
```

Three things to notice. The traversal is *parallel* over a worker pool. The visited set is a
thread-safe roaring bitmap, and `CheckedAdd` is the atomic test-and-set that makes the frontier
expansion race-free without a lock per node — the same trick as a CAS-marked visited array in a
GPU BFS (topic 18). And `direction` is a parameter: the same code walks forward ("what can this
principal reach?") and backward ("who can reach this?"). The backward direction is the one that
matters for defense, and it is the one lane 2's dominator analysis builds on.

### Step 6 — Tier Zero: the label the product is organised around

`analysis/tiering/tiering.go:37`:

```go
const (
    StrTagTierZero = "Tag_Tier_Zero"
    StrTagOwned    = "Tag_Owned"
)

func IsTierZero(node *graph.Node) bool {
    if node.Kinds.ContainsOneOf(KindTagTierZero) { return true }
    startSystemTags, _ := node.Properties.Get(common.SystemTags.String()).String()
    return strings.Contains(startSystemTags, ad.AdminTierZero)
}
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

1. `PathfindingRelationships` (63 kinds) is a strict subset of `Relationships` (104). Pick two
   kinds that are excluded and explain, in attacker terms, why walking them would produce a path
   that is not an attack.
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

- [ ] You can state the edge semantics in one sentence and derive the shortest-path formulation
      from it.
- [ ] You can name the four kind partitions and explain what each is for.
- [ ] You can explain why 31 edge kinds are derived, and connect that to topic 27.
- [ ] You can point at the roaring bitmap in the traversal and say what it replaces.
- [ ] Your `chokepoint.rs` reproduces the tiered/flat contrast: 1992-user blast radius vs none.
- [ ] You wrote answers to all five questions in notes.md.

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
