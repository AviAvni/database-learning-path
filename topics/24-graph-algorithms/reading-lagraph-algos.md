# Analytics with four verbs: LAGraph's algorithm shelf

Topic 20's guide covered BFS and the framework; this chapter reads
LAGraph's ANALYTICS algorithms — CC, TC, BC, SSSP, PR — as answers to
"what does this look like when the only verbs are
mxv/mxm/semiring/mask". Plus the punchline: FalkorDB already ships
these (`proc_pagerank.c:197` calls `LAGr_PageRank`;
`proc_betweenness.c`, `proc_cdlp.c` likewise) — M24 is re-plumbing a
pattern that exists, not inventing one. Before the code, this chapter
rebuilds the four verbs, then walks the shelf one algorithm at a
time.

## The problem in one sentence

Can every whole-graph analytic be written with four bulk operations
and no per-vertex control flow — and what does that cost against
hand-tuned frontier code (concretely: our scalar triangle count does
**15.6M triangles in 376 ms**, and LAGraph's masked-multiply
formulation of the same count must pay for its generality somewhere)?

## The concepts, step by step

### Step 1 — the four verbs: mxv, mxm, semiring, mask

GraphBLAS expresses graph algorithms as sparse linear algebra: the
graph is its adjacency matrix A (A[u][v] nonzero iff edge u→v), and
exactly four verbs do all the work. **mxv** (matrix-vector multiply)
makes every vertex combine values from all its neighbors in one bulk
operation — one traversal round. **mxm** (matrix-matrix multiply)
does the same for many vectors, or composes two-hop relationships.
A **semiring** swaps the (+, ×) inside those multiplies for another
(combine, accumulate) pair — (min, +) turns mxv into a relaxation
round, (OR, AND) turns it into reachability — so the semiring choice
*is* the algorithm. A **mask** restricts where output is computed
("only these entries"), letting the runtime skip work instead of
discarding it. Why it matters: no atomics, no per-vertex loops —
parallelism, blocking, and direction choice all belong to the
runtime, and the algorithm is a handful of verb calls.

### Step 2 — connected components as algebra: FastSV

Connected components (CC — label every vertex with its reachable
island) is classically solved with union-find, a per-edge
pointer-chasing structure; FastSV instead keeps a **parent** array
(each vertex points toward a representative; following parents leads
to the island's root) and improves it in bulk rounds. Each round does
two things: **hooking** (every vertex adopts the minimum of its
neighbors' grandparents — one mxv with a MIN semiring) and
**shortcutting** (every vertex's pointer jumps to its grandparent —
halving all chains at once). One round, de-algebra'd — three bulk ops
where union-find does per-edge pointer chases:

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

Because shortcutting halves chain lengths every round, the round
count is O(log n) — on a diameter-2 RMAT giant component it converges
in a handful of rounds, each a full bulk pass. The `min_2nd` semiring
in hooking means "take the neighbor's gp value, ignore the edge
weight" — question 1 asks what breaks without it.

### Step 3 — sampling in two worlds: FastSV vs Afforest

Both modern CC codes exploit the same observation — most edges are
inside the giant component and inspecting them teaches you nothing —
but each expresses "skip edges" in its own world's vocabulary. FastSV
samples COLUMNS per row *inside* the matrix ops (`FASTSV_SAMPLES` per
row, enabled when `nvals > n*samples*2 && n > 1024`) and stays
bulk-synchronous; our Afforest stub samples per-vertex neighbor
OFFSETS with a union-find — asynchronous, with per-edge early exit,
then a final sweep that skips the already-identified giant component
entirely. Frontier-vs-algebra in one algorithm: same idea, and the
test for which mechanism wins is edges inspected (Afforest's test
demands <50% of m) vs rounds × bulk-pass cost (FastSV) — question 2
makes you count both.

### Step 4 — triangle counting as masked multiplication

A triangle is a wedge (path u–v–w) whose endpoints are also directly
connected — so counting triangles is "multiply the adjacency by
itself (enumerate wedges), then keep only entries where A also has an
edge". That "keep only" is Step 1's mask, and it is the entire
performance story: the masked SpGEMM `(L*U').*L` never materializes
`L*U'` — the mask prunes the multiply (Azad & Buluç) so only wedges
that can close are ever computed. LAGraph ships six spellings of this
one count (L and U are the lower/upper triangles of A, which
deduplicate the 6 orderings of each triangle):

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

Our scalar `triangle_count` is Sandia's formulation with rank-ordered
adjacency instead of tril: "orient by degree, intersect forward
lists" IS `(L*L).*L` read row-wise. One measured point: rmat 15.6M
triangles in 376 ms vs uniform 5.4K in 158 ms — method choice (:44)
flips exactly because urand has ~no triangles to prune with: a mask
with nothing in it saves nothing.

### Step 5 — the rest of the shelf, rapid-fire

The remaining algorithms are the same move — pick a semiring, pick a
mask, iterate — plus one benchmarking lesson:

- `LAGr_PageRankGAP.c` vs `LAGr_PageRank.c`: GAP-spec PR (dangling
  handled gapbs-style, L1 stop) vs textbook. Benchmark specs fork
  implementations — topic 22's lesson in filenames.
- `LAGr_SingleSourceShortestPath.c:151-185`: MIN_PLUS delta-stepping
  (see reading-delta-stepping.md) — the bucket is a masked sparse
  vector, one vxm per inner iteration.
- `LAGr_Betweenness.c:110-164`: batched-source matrix Brandes (see
  reading-brandes.md) — the frontier is an ns×n matrix, so 32
  sources cost the same number of graph passes as one.
- `LG_CC_Boruvka.c` exists as the "simple" CC — compare its mxv
  count per round against FastSV7's three.

### Step 6 — the FalkorDB tie-in: the flush boundary is the cost

FalkorDB's procedure layer is exactly this shelf behind a Cypher
surface, and its shape names M24's real design question.
`proc_pagerank.c`: parse args → get the delta-matrix-backed A →
**flush/export to a GrB_Matrix** → `LAGr_PageRank` (:197) → map
scores back to node ids → stream results. The costs to attack in
falkordb-rs-next-gen: the export/flush boundary (can algorithms run
masked over DM/DP directly?) and result materialization (stream
top-k instead of full vectors?). M24 is re-plumbing this pattern
over the M20 core — the algorithms are solved; the boundary isn't.

## Where each step lives in the code

Each file's header comment states the formulation before the code —
read it first.

- **Step 2 — `LG_CC_FastSV7.c`**:

| anchor | what |
|---|---|
| `:69-71` | the state: `mngp` (min neighbor grandparent), `gp`, `gp_new` — SV's hooking/shortcutting as three vectors |
| `:102` | hooking = ONE mxv: `mngp = min_2nd(A, gp)` — every vertex reads its neighbors' grandparents in one masked matrix op |
| `:145-158` | shortcutting: `parent = min(parent, mngp)` via mxv on a PARENT MATRIX + `gp_new = parent(parent)` (extract = pointer chase as assign) |
| `:335-338` | sampling: `FASTSV_SAMPLES` per row, `sampling = nvals > n*samples*2 && n > 1024` — Afforest's idea imported (Step 3) |
| `:231-235` | built-in timing printfs: sample phase vs hash phase vs final mxv — SuiteSparse's authors profile like topic 0 |

- **Step 4 — `LAGr_TriangleCount.c`**: the six methods at `:33-37`,
  the LUT-vs-LL crossover note at `:44-47`.
- **Step 5 — the shelf**: `LAGr_PageRankGAP.c`, `LAGr_PageRank.c`,
  `LAGr_SingleSourceShortestPath.c:151-185`,
  `LAGr_Betweenness.c:110-164`, `LG_CC_Boruvka.c`.
- **Step 6 — FalkorDB**: `src/procedures/proc_pagerank.c` (:197 calls
  `LAGr_PageRank`), `proc_betweenness.c`, `proc_cdlp.c` — trace the
  parse → flush → call → materialize pipeline in any one of them.

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
