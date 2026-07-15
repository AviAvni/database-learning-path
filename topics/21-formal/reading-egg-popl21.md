# egg: equality saturation with deferred rebuilding

egg is the e-graph library behind a wave of optimizer research —
and behind our `eqsat.rs` stub. Its POPL 2021 paper makes two
contributions worth reading the source for: **deferred rebuilding**
(batch congruence repair instead of fixing invariants after every
union) and **e-class analyses** (attach lattice facts like constant
values to classes). This chapter builds the data structure from its
parts — union-find, hashcons, congruence — then the saturation loop
and egg's two contributions, before pointing you at the code: the
`src/` tree is ~10K lines and half of that is `explain.rs`/tests —
you can read the core tonight.

## The problem in one sentence

A rewrite optimizer that applies rules in a fixed order,
destructively, can rewrite itself into a corner — `(a*2)/2` gets
stuck at cost 5 when strength-reduction fires first — and the fix
(keep *every* equivalent form, pick the best at the end) needs a
data structure that stores exponentially many terms in linear
space and repairs its invariants fast (egg's batching alone is
worth up to 88×).

## The concepts, step by step

### Step 1 — union-find: merging sets in near-constant time

A **union-find** (disjoint-set) structure maintains a partition of
ids into groups, under two operations: `find(x)` returns the
group's canonical representative id, and `union(x, y)` merges two
groups. With path compression (each `find` re-points ids directly
at the root) both run in effectively O(1) — amortized inverse
Ackermann, written O(α). It's the standard answer to "these two
things just became equal; remember that, cheaply, forever." egg's
entire union-find is 60 lines (`unionfind.rs`).

### Step 2 — the e-graph: a set of terms closed under equivalence

An **e-graph** stores terms (expression trees like `(a*2)/2`)
compactly under an equivalence relation. Its parts: an **e-node**
is an operator whose children are *ids of equivalence classes*
rather than subterms (`*` with children [class 3, class 7]); an
**e-class** is a set of e-nodes that are all equal (identified by
a union-find id); a **hashcons** (a hash map from e-node to its
e-class id — topic 8's hash table, again) guarantees each distinct
e-node is stored once.

```
  e-class {a*2, a<<1}          union-find: id → canonical id
       /        \              hashcons:  e-node → e-class id
  e-class {a}   e-class {2}
```

The compression is the point: because children are *classes*, one
e-node represents every combination of its children's forms —
`(a*2)/2` and `(a<<1)/2` share one `/` node. n e-nodes can
represent exponentially many distinct terms.

### Step 3 — congruence: equal children make equal parents

The invariant that makes an e-graph more than a union-find is
**congruence**: if x ≡ y, then f(x) ≡ f(y) — merging two classes
must also merge every pair of parent e-nodes that now have
identical (canonicalized) children. Mechanically: after
union(a, b), re-canonicalize every parent e-node of the merged
class; if two parents collide in the hashcons — they became the
same node — their classes are equal too, so union *them*, and
repeat to fixpoint. This upward cascade (**congruence closure**)
is the expensive part of every e-graph operation, and it's exactly
the invariant egg chooses to let go stale (step 5).

### Step 4 — equality saturation: rewrite all ways, then pick

**Equality saturation** replaces ordered, destructive rewriting
with: seed an e-graph with the input term; match every rewrite
rule against the whole e-graph; *apply* each match by `add`-ing
the right-hand side and `union`-ing it with the left — never
deleting anything; repeat until **saturated** (no rule adds
anything new) or a budget trips; then run an **extractor** with a
cost function to pick the cheapest term the graph now represents.
The trap it fixes:

```
        (a*2)/2
  hand (ordered):  strength-reduce FIRST → (a<<1)/2 … stuck, cost 5
  egg (saturate):  keep BOTH forms; (x*y)/z→x*(y/z) still matches
                   → a*(2/2) → a*1 → a, cost 1
```

The catch: the e-graph can blow up (associativity+commutativity
rules alone are exponential), so egg's `Runner` carries
node/iteration/time limits and reports a `StopReason` — saturation
is best-effort, a *search budget* like topic 10's join-order DP
cutoff.

### Step 5 — deferred rebuilding: the headline contribution

Classic congruence closure (and old eqsat engines) restores the
congruence invariant after EVERY union — the full upward cascade
of step 3, every time. egg lets the e-graph go stale during a
batch of rule applications, then `rebuild()` repairs once:

```
  per-union repair:   union → fix parents → fix grandparents → …
  egg:                union, union, union, …  → rebuild (dedup work:
                      a class touched 10× is repaired once)
```

```rust
fn union(&mut self, a: Id, b: Id) {
    let root = self.unionfind.union(a, b);          // O(α) — and STOP:
    self.pending.extend(self.classes[&root].parents()); // repair deferred
}

fn rebuild(&mut self) {
    while let Some((node, class)) = self.pending.pop() {
        let node = node.canonicalize(&self.unionfind); // re-canon children
        if let Some(old) = self.memo.insert(node, class) {
            // hashcons collision = two nodes became equal children-wise:
            // a DISCOVERED congruence — union them, which refills pending
            self.union(old, class);                    // hence: loop to fixpoint
        }
    }
}
```

Paper reports up to 88× from this alone. It is exactly the
delta-matrix `wait` (topic 20) / LSM memtable flush (topic 4) move:
make mutation O(1) by batching the expensive invariant restoration.
Z3's new e-graph adopted it (`euf_egraph.h:23` cites egg). The
subtlety worth holding: between rebuilds the hashcons is *stale*
(non-canonical keys), which is fine during rule application because
matching tolerates it — the invariant is needed at iteration
boundaries, not continuously.

### Step 6 — e-class analyses: facts that ride along with classes

An **e-class analysis** attaches a lattice value (a fact with a
defined way to merge two facts, like `Option<i64>` for "known
constant") to every e-class, maintained through merges
(`analysis_pending`, `egraph.rs:70`; `N::remake`/`merge` in
`process_unions`). The canonical one: constant folding — a class
carries `Option<i64>`; when it becomes `Some`, `modify` adds the
literal node, and extraction gets it for free. Our stub sidesteps
this (`(/ 2 2)` folds via the `div-same` rule), but M21's planner
stage would carry *cardinality estimates* as the analysis — topic
10's `estimate()` as a lattice.

### Step 7 — extraction is the weak spot

`find_best` is greedy per e-class — fixpoint of per-class
best-cost, optimal for tree cost like AstSize (count the nodes),
NOT optimal with sharing (DAG cost: a subterm used twice should be
priced once). `lp_extract.rs` does ILP extraction for that.
Planner analogy: greedy extraction ≈ picking the cheapest subplan
per group in a memo — which is exactly what a Cascades optimizer
does, and e-graph ≈ Cascades **memo** discovered independently
(question 5 pushes on what each side has that the other lacks).

## How to read the paper (with the concepts in hand)

- **§2** is the best e-graph intro in print — steps 1-4 with
  pictures; skim if the steps above landed, read closely if not.
- **§3** is deferred rebuilding (step 5) — the invariant-staleness
  argument and the 88× measurement; check their figure against the
  `rebuild` pseudocode above.
- **§4** is e-class analyses (step 6) — read with the M21
  cardinality-lattice idea in mind.

## Where each step lives in the code

| file:line | step | what |
|---|---|---|
| `unionfind.rs:30/:37/:47` | 1 | `find` (path-compressing in `find_mut`), `union` — 60 lines, the whole thing |
| `egraph.rs:66` | 2 | `memo: HashMap<L, Id>` — the hashcons; canonical only *after* rebuild |
| `egraph.rs:970` | 2 | `EGraph::add` — canonicalize children, memo lookup-or-insert |
| `egraph.rs:1147` | 3, 5 | `EGraph::union` — merge classes, push parents onto `pending` (:69) |
| `egraph.rs:1346` | 3, 5 | `process_unions` — drain `pending`: re-canonicalize node, re-insert into memo; a collision *is* a discovered congruence → recursive union |
| `egraph.rs:1416` | 5 | `rebuild` — the public batched-repair entry point |
| `machine.rs:8/:24` | 4 | pattern matching compiled to a tiny VM: `Bind`/`Scan`/`Compare` instructions over the e-graph |
| `run.rs:138/:161/:237` | 4 | `Runner`, `RunnerLimits` (iter/node/time), `StopReason` |
| `extract.rs:41/:116/:157/:225` | 7 | `Extractor`, `CostFunction`, `AstSize`, `find_best` (fixpoint of per-class best-cost) |

Navigation advice: read `unionfind.rs` fully (it's 60 lines), then
`egraph.rs` by the anchors above, then `run.rs`'s loop, then
`extract.rs`. Skip `explain.rs` on the first pass — it's half the
tree and orthogonal.

## Questions (answer in notes.md)

1. Trace `(a*2)/2` by hand: which unions happen in iteration 1, and
   in which e-class do `(/ 2 2)` and `1` meet?
2. Why must `memo` re-canonicalization happen in a loop (a repair
   can create a new collision)? Find the fixpoint in
   `process_unions` (:1346).
3. `machine.rs`: what does `Scan` cost when a pattern's root op has
   thousands of e-nodes? Relate to `classes_by_op` (:81).
4. Assoc+comm on `+` alone: estimate e-graph growth per iteration on
   a depth-8 sum. Which `RunnerLimit` trips first (predict, then
   measure in the stub)?
5. Cascades memo vs e-graph: what does Cascades have that egg lacks
   (physical properties, promises), and vice versa (congruence)?

## References

**Papers**
- Willsey, Nandi, Wang, Flatt, Tatlock, Panchekha — "egg: Fast and
  Extensible Equality Saturation" (POPL 2021,
  [arXiv:2004.03082](https://arxiv.org/abs/2004.03082)) — §2 is the
  best e-graph intro in print; §3 deferred rebuilding, §4 analyses

**Code**
- [egg](https://github.com/egraphs-good/egg) `src/unionfind.rs`,
  `src/egraph.rs` (add :970, union :1147, process_unions :1346,
  rebuild :1416), `src/machine.rs`, `src/run.rs`, `src/extract.rs`
  — read fully; skip `explain.rs` on the first pass
