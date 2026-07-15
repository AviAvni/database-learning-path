# Ligra: two functions, every frontier algorithm

Two functions — `vertexMap` and `edgeMap` — and every frontier
algorithm in ~50 lines each (`apps/`). Ligra's contribution is
making direction switching (topic 20's Beamer trick, invented for
BFS) a FRAMEWORK property every algorithm inherits for free. Before
the code, this chapter builds the machine step by step: what a
frontier is, its two physical representations, the push/pull choice,
the one threshold that automates it, and what a whole algorithm looks
like when it's reduced to a single edge function.

## The problem in one sentence

Every frontier algorithm faces the same per-round choice — push from
the frontier's out-edges or pull over all vertices' in-edges — and
getting it wrong costs up to 10× per round (topic 20's BFS numbers);
Ligra moves that choice out of every algorithm and into one framework
function with one threshold: **|frontier| + its out-degree sum vs
m/20**.

## The concepts, step by step

### Step 1 — the frontier: the set of vertices that matter this round

A **frontier** is the set of active vertices in the current round of
a graph algorithm — in BFS, the vertices discovered last round whose
edges must be explored next. Frontier algorithms proceed in rounds:
apply a per-edge update from the current frontier, collect the
vertices that changed, and that collection *is* the next frontier. In
BFS on a social graph the frontier starts as 1 vertex, explodes to a
large fraction of the graph within 2–3 hops, then shrinks to
stragglers — a wave. The frontier's size, relative to the whole
graph, is the single quantity everything in Ligra keys off.

### Step 2 — two physical representations: id array vs bitmap

A set of vertices can be stored two ways, and the right one depends
on its size. Ligra's `vertexSubset` is physically EITHER:

```
  vertexSubset: a frontier, physically EITHER
      sparse: array of vertex ids        (small frontiers)
      dense:  boolean array of size n    (big frontiers)
```

Concretely, on a 65,536-vertex graph a 100-vertex frontier is 400
bytes as an id array but 8 KB as a bitmap (and iterating it means
scanning all 65,536 slots); a 40,000-vertex frontier is 160 KB as an
id array but still 8 KB as a bitmap with O(1) membership tests. The
representation is a cost decision, not a style decision — and Ligra
converts between them automatically as the wave grows and shrinks.

### Step 3 — push vs pull: whose edges do you traverse?

There are two ways to run one round of updates, with different cost
shapes. **Push** iterates the frontier and follows each member's
out-edges — work proportional to the frontier's out-degree sum, ideal
when the frontier is small. **Pull** iterates *every* vertex in the
graph and scans its in-edges asking "is any neighbor in the
frontier?" — work bounded by m (total edges), but with a decisive
trick: once one in-neighbor claims the vertex, stop scanning
(early exit). When the frontier is huge, most vertices get claimed by
an early in-edge, so pull touches far fewer than m edges — while push
would faithfully traverse the frontier's entire (huge) out-degree sum
and fight write contention doing it. Small frontier: push wins. Big
frontier: pull wins. Same asymptotics, ~10× apart in constants.

### Step 4 — the switch: edgeMap and the m/20 threshold

Ligra's `edgeMap` packages Step 3's choice behind one comparison, so
every algorithm inherits direction switching without asking:

```
  edgeMap(G, frontier, F, threshold):
      if |frontier| + Σ out_degrees(frontier) > m/20:   ← ligra.h:238,261
          DENSE: for each v ∈ V, scan IN-edges, stop early
                 (pull; reads frontier bitmap)             ligra.h:59
      else:
          SPARSE: for each u ∈ frontier, push OUT-edges   ligra.h:111
      F(u,v) does the algorithm-specific update, returns
      whether v joins the next frontier
```

The switch, as code — everything else in Ligra is plumbing around it:

```rust
fn edge_map(g: &Graph, front: &VertexSubset, f: &impl Fn(u32, u32) -> bool)
    -> VertexSubset {
    if front.len() + front.out_degree_sum(g) > g.m / 20 {
        // PULL: scan every vertex's IN-edges, early-exit once claimed
        let mut next = DenseBits::new(g.n);
        for v in 0..g.n {
            for u in g.in_edges(v) {
                if front.contains(u) && f(u, v) { next.set(v); break; }
            }
        }
        next.into()
    } else {
        // PUSH: only frontier vertices' OUT-edges; f returns "v joins next"
        front.iter().flat_map(|u| g.out_edges(u)
             .filter(|&v| f(u, v)).map(move |v| v)).collect()
    }
}
```

The estimate is crude — frontier size plus out-degree sum against
m/20 — and it cannot see how effective pull's early exit will be
(that depends on how full the *next* frontier is). It's a heuristic
that's right often enough to be a framework default; question 1 below
constructs the case where it's wrong. Note also the hidden cost: pull
reads in-edges, so the graph AND its transpose must both be resident.

### Step 5 — an algorithm is just F: reading the apps

With frontier, representation, and switch all owned by the framework,
an algorithm shrinks to its per-edge update function F(u, v) — which
does the algorithm-specific write and returns whether v joins the
next frontier. Each app is a one-pager:

| app | F(u,v) | frontier evolution |
|---|---|---|
| `apps/BFS.C` | CAS parent[v] | classic expanding→shrinking wave |
| `apps/BC.C` | add σ contributions; TWO passes (forward + Brandes backward, both as edgeMaps) | dense mid-BFS — direction switch fires |
| `apps/Components.C` | label-propagation min | frontier = "changed last round" |
| `apps/BellmanFord.C` | writeMin dist | stays dense on low-diameter graphs |
| `apps/PageRank.C` | sum contributions | ALWAYS dense — edgeMap degenerates to SpMV |

(CAS = compare-and-swap, the atomic instruction that lets parallel
pushers race safely for the same vertex; writeMin is its
take-the-minimum cousin.) The lesson in the table's last row: for
whole-graph kernels (PR), the frontier is always everything, so Ligra
≡ SpMV (sparse matrix–vector multiply) and the algebraic formulation
is identical. Frontiers only earn their complexity when they SHRINK —
Ligra generalizes the case where they do.

### Step 6 — Ligra vs GraphBLAS, honestly

The two frameworks in this topic's dichotomy trade expressiveness for
fusability, and neither dominates:

- edgeMap's F is an arbitrary function with CAS — semirings must be
  (monoid, binop) pairs. Afforest's "link only the r-th neighbor"
  fits neither cleanly (it's not an edgeMap either — it's a strided
  edge SAMPLE; frameworks leak).
- Ligra's dense mode reads IN-edges: it needs both G and Gᵀ resident
  — same memory doubling FalkorDB pays for its transposed twin
  (topic 20). Nobody escapes the transpose.
- The m/20 threshold vs Beamer's α/β vs SuiteSparse's dot-vs-saxpy
  auto-switch: three names for one decision — work(push) ∝ frontier
  out-degree sum vs work(pull) ∝ m with early exit.

## Where each step lives in the code

- **Steps 2–4 — the framework core**: `ligra/ligra.h:235-272`
  `edgeMapData` — the switch itself; `:238` the m/20 default
  threshold; `:59` `edgeMapDense` (pull with early exit) vs `:111`
  `edgeMapSparse` (push); `:85` `edgeMapDenseForward` (push in dense
  clothing, for when early-exit doesn't apply — question 2).
- **Step 5 — the apps**: `apps/BFS.C`, `apps/BC.C`,
  `apps/Components.C`, `apps/BellmanFord.C`, `apps/PageRank.C` — read
  each one's F against the table above; every file is ~50 lines.
- Navigation advice: read `edgeMapData` first and treat everything
  else in `ligra.h` as plumbing around it; then each app reads in
  minutes because you already know who calls F and when.

## Questions (answer in notes.md)

1. Derive when m/20 is the wrong threshold: construct a frontier
   whose out-degree sum is just under m/20 but whose PUSH cost
   exceeds pull's (hint: early-exit effectiveness depends on how
   FULL the next frontier will be, which the threshold can't see).
2. edgeMapDenseForward (:85) pushes from ALL vertices without
   early exit. When does it beat edgeMapDense (pull with break)?
3. BC.C runs Brandes' backward pass as edgeMaps over the TRANSPOSE.
   Map each Ligra construct onto the LAGr_Betweenness matrix ops —
   which of the two batches sources, and why can't Ligra?
4. Components.C is label propagation (frontier = changed vertices);
   our Afforest stub is sampling+union-find. Compare edges touched
   on a graph that's one giant component vs 18K components.
5. M24: should the capstone's algorithm library expose an edgeMap-
   style callback API to users (arbitrary Rust closures over edges)
   or a fixed algorithm menu like FalkorDB's procedures? What does
   Ligra's F-with-CAS cost a SAFE embedding (Rust: Send+Sync bounds,
   no UDF aborts mid-frontier)?

## References

**Papers**
- Shun & Blelloch — "Ligra: A Lightweight Graph Processing Framework
  for Shared Memory" (PPoPP 2013) — §3-4 for the two primitives and
  the threshold; the apps section reads faster as code

**Code**
- [ligra](https://github.com/jshun/ligra) — `ligra/ligra.h`
  (:235-272 `edgeMapData`, the switch) and `apps/` (each algorithm is
  a one-pager)
