# Analytics with four verbs: LAGraph's algorithm shelf

Topic 20's guide covered BFS and the framework; this chapter reads
LAGraph's ANALYTICS algorithms — CC, TC, BC, SSSP, PR — as answers to
"what does this look like when the only verbs are
mxv/mxm/semiring/mask". Plus the punchline: FalkorDB already ships
these (`proc_pagerank.c:197` calls `LAGr_PageRank`;
`proc_betweenness.c`, `proc_cdlp.c` likewise) — M24 is re-plumbing a
pattern that exists, not inventing one.

## LG_CC_FastSV7.c — connected components, algebraically

| anchor | what |
|---|---|
| `:69-71` | the state: `mngp` (min neighbor grandparent), `gp`, `gp_new` — SV's hooking/shortcutting as three vectors |
| `:102` | hooking = ONE mxv: `mngp = min_2nd(A, gp)` — every vertex reads its neighbors' grandparents in one masked matrix op |
| `:145-158` | shortcutting: `parent = min(parent, mngp)` via mxv on a PARENT MATRIX + `gp_new = parent(parent)` (extract = pointer chase as assign) |
| `:335-338` | sampling: `FASTSV_SAMPLES` per row, `sampling = nvals > n*samples*2 && n > 1024` — Afforest's idea imported |
| `:231-235` | built-in timing printfs: sample phase vs hash phase vs final mxv — SuiteSparse's authors profile like topic 0 |

FastSV vs our Afforest stub: same "don't touch every edge" goal,
different mechanism — FastSV samples COLUMNS per row inside matrix
ops (still bulk-synchronous), Afforest samples per-vertex neighbor
OFFSETS with a union-find (asynchronous, per-edge early exit).
Frontier-vs-algebra in one algorithm.

One FastSV round, de-algebra'd — three bulk ops where union-find
does per-edge pointer chases:

```rust
fn fastsv_round(a: &SparseMat, parent: &mut [u32], gp: &mut [u32]) -> bool {
    // hooking: every vertex reads all neighbors' grandparents AT ONCE
    let mngp = a.mxv_min_2nd(gp);              // mngp[v] = min gp[u] over u∈N(v)
    let mut changed = false;
    for v in 0..parent.len() {                 // shortcutting, elementwise
        let m = mngp[v].min(gp[v]);
        if m < parent[v] { parent[v] = m; changed = true; }
    }
    for v in 0..gp.len() { gp[v] = parent[parent[v] as usize]; } // = extract
    changed
}
```

## LAGr_TriangleCount.c — six formulations of one count

```
  :33-37   0 default    (currently Sandia_LUT)
           2 Cohen:      ntri = sum((L*U) .* A) / 2
           3 Sandia_LL:  ntri = sum((L*L) .* L)
           4 Sandia_UU:  ntri = sum((U*U) .* U)
           5 Sandia_LUT: ntri = sum((L*U') .* L)   ← dot product form
           6 Sandia_ULT: ntri = sum((U*L') .* U)
  :44-47   LUT fastest on large graphs EXCEPT GAP-urand, where
           saxpy-based LL wins — the dot-vs-saxpy split (topic 20)
           decided by TRIANGLE DENSITY, not just matrix shape
```

The masked SpGEMM `(L*U').*L` never materializes L*U' — the mask
prunes the multiply (Azad & Buluç). Our scalar `triangle_count` is
Sandia's formulation with rank-ordered adjacency instead of tril:
"orient by degree, intersect forward lists" IS `(L*L).*L` read
row-wise. One measured point: rmat 15.6M triangles in 376 ms vs
uniform 5.4K in 158 ms — method choice (:44) flips exactly because
urand has ~no triangles to prune with.

## The rest, rapid-fire

- `LAGr_PageRankGAP.c` vs `LAGr_PageRank.c`: GAP-spec PR (dangling
  handled gapbs-style, L1 stop) vs textbook. Benchmark specs fork
  implementations — topic 22's lesson in filenames.
- `LAGr_SingleSourceShortestPath.c:151-185`: MIN_PLUS delta-stepping
  (see reading-delta-stepping.md).
- `LAGr_Betweenness.c:110-164`: batched-source matrix Brandes (see
  reading-brandes.md).
- `LG_CC_Boruvka.c` exists as the "simple" CC — compare its mxv
  count per round against FastSV7's three.

## FalkorDB tie-in (M24's actual shape)

`proc_pagerank.c`: parse args → get the delta-matrix-backed A →
**flush/export to a GrB_Matrix** → `LAGr_PageRank` (:197) → map
scores back to node ids → stream results. The costs to attack in
falkordb-rs-next-gen: the export/flush boundary (can algorithms run
masked over DM/DP directly?) and result materialization (stream
top-k instead of full vectors?).

## Questions (answer in notes.md)

1. FastSV7:102's min_2nd semiring: why 2nd (take the neighbor's gp,
   ignore edge values) — and what breaks with plain MIN_TIMES on a
   weighted graph?
2. Count matrix ops per FastSV round vs pointer-chases per Afforest
   round. On a diameter-2 RMAT giant component, which converges in
   fewer ROUNDS, and why does Afforest still win wall-clock?
3. Sandia_LUT (dot) vs Sandia_LL (saxpy) — connect :44-47's
   urand exception to topic 20's dot3-vs-saxpy3 rule. What property
   of urand (no hubs, no triangles) starves the dot-form's mask?
4. LAGr_PageRankGAP handles dangling vertices with an extra
   reduction per iteration. Our pull PR ignores them — quantify the
   error on a graph with 18K single-node components.
5. M24 API: `CALL algo.wcc()` on a graph with pending deltas —
   enumerate the three options (flush first / run on main / run on
   main+DP-DM masked) and their consistency semantics (topic 8's
   read-your-writes for procedures).

## References

**Code**
- [LAGraph](https://github.com/GraphBLAS/LAGraph) `src/algorithm/` —
  `LG_CC_FastSV7.c`, `LAGr_TriangleCount.c`, `LAGr_PageRankGAP.c`,
  `LAGr_SingleSourceShortestPath.c`, `LAGr_Betweenness.c`,
  `LG_CC_Boruvka.c` — each file's header comment states the
  formulation before the code
- [FalkorDB](https://github.com/FalkorDB/FalkorDB)
  `src/procedures/proc_pagerank.c` (:197 calls `LAGr_PageRank`),
  `proc_betweenness.c`, `proc_cdlp.c` — M24's shape, already shipping
