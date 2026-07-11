# Reading guide — "Ligra: A Lightweight Graph Processing Framework for Shared Memory" (Shun & Blelloch, PPoPP 2013) ([`~/repos/ligra`](https://github.com/jshun/ligra))

Two functions — `vertexMap` and `edgeMap` — and every frontier
algorithm in ~50 lines each (`apps/`). Ligra's contribution is
making direction switching (topic 20's Beamer trick, invented for
BFS) a FRAMEWORK property every algorithm inherits for free.

## The whole framework

```
  vertexSubset: a frontier, physically EITHER
      sparse: array of vertex ids        (small frontiers)
      dense:  boolean array of size n    (big frontiers)

  edgeMap(G, frontier, F, threshold):
      if |frontier| + Σ out_degrees(frontier) > m/20:   ← ligra.h:238,261
          DENSE: for each v ∈ V, scan IN-edges, stop early
                 (pull; reads frontier bitmap)             ligra.h:59
      else:
          SPARSE: for each u ∈ frontier, push OUT-edges   ligra.h:111
      F(u,v) does the algorithm-specific update, returns
      whether v joins the next frontier
```

Anchors: `ligra/ligra.h:235-272` `edgeMapData` — the switch;
`:238` the m/20 default threshold; `:59` `edgeMapDense` vs `:111`
`edgeMapSparse`; `:85` `edgeMapDenseForward` (push in dense clothing,
when early-exit doesn't apply).

## Reading the apps (each is a one-pager)

| app | F(u,v) | frontier evolution |
|---|---|---|
| `apps/BFS.C` | CAS parent[v] | classic expanding→shrinking wave |
| `apps/BC.C` | add σ contributions; TWO passes (forward + Brandes backward, both as edgeMaps) | dense mid-BFS — direction switch fires |
| `apps/Components.C` | label-propagation min | frontier = "changed last round" |
| `apps/BellmanFord.C` | writeMin dist | stays dense on low-diameter graphs |
| `apps/PageRank.C` | sum contributions | ALWAYS dense — edgeMap degenerates to SpMV |

The lesson in the table's last row: for whole-graph kernels (PR),
Ligra ≡ SpMV and the algebraic formulation is identical. Frontiers
only earn their complexity when they SHRINK — Ligra generalizes the
case where they do.

## Ligra vs GraphBLAS, honestly

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
