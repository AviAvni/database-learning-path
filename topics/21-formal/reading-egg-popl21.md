# egg: equality saturation with deferred rebuilding

egg is the e-graph library behind a wave of optimizer research —
and behind our `eqsat.rs` stub. Its POPL 2021 paper makes two
contributions worth reading the source for: **deferred rebuilding**
(batch congruence repair instead of fixing invariants after every
union) and **e-class analyses** (attach lattice facts like constant
values to classes). The `src/` tree is ~10K lines and half of that
is `explain.rs`/tests — you can read the core tonight.

## The data structure, bottom-up

| file:line | what |
|---|---|
| `unionfind.rs:30/:37/:47` | `find` (path-compressing in `find_mut`), `union` — 60 lines, the whole thing |
| `egraph.rs:66` | `memo: HashMap<L, Id>` — the hashcons; canonical only *after* rebuild |
| `egraph.rs:970` | `EGraph::add` — canonicalize children, memo lookup-or-insert |
| `egraph.rs:1147` | `EGraph::union` — merge classes, push parents onto `pending` (:69) |
| `egraph.rs:1346` | `process_unions` — drain `pending`: re-canonicalize node, re-insert into memo; a collision *is* a discovered congruence → recursive union |
| `egraph.rs:1416` | `rebuild` — the public batched-repair entry point |
| `machine.rs:8/:24` | pattern matching compiled to a tiny VM: `Bind`/`Scan`/`Compare` instructions over the e-graph |
| `run.rs:138/:161/:237` | `Runner`, `RunnerLimits` (iter/node/time), `StopReason` |
| `extract.rs:41/:116/:157/:225` | `Extractor`, `CostFunction`, `AstSize`, `find_best` (fixpoint of per-class best-cost) |

## Deferred rebuilding — the headline

Classic congruence closure (and old eqsat engines) restores the
invariant after EVERY union. egg lets the e-graph go stale during a
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
Z3's new e-graph adopted it (`euf_egraph.h:23` cites egg).

## E-class analyses

A lattice value per e-class, maintained through merges
(`analysis_pending`, `egraph.rs:70`; `N::remake`/`merge` in
`process_unions`). The canonical one: constant folding — class
carries `Option<i64>`; when it becomes `Some`, `modify` adds the
literal node, and extraction gets it for free. Our stub sidesteps
this (`(/ 2 2)` folds via the `div-same` rule), but M21's planner
stage would carry *cardinality estimates* as the analysis — topic
10's `estimate()` as a lattice.

## Extraction is the weak spot

`find_best` is greedy per e-class — optimal for tree cost like
AstSize, NOT optimal with sharing (DAG cost). `lp_extract.rs` does
ILP extraction for that. Planner analogy: greedy extraction ≈
picking the cheapest subplan per group in a memo — which is exactly
what a Cascades optimizer does, and e-graph ≈ Cascades **memo**
discovered independently.

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
