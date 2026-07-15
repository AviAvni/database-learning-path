# Brandes betweenness: restructure the sum, not the data structure

Betweenness centrality by definition is an all-pairs O(n³) sum; Brandes
turned it into O(V·E) with one algebraic observation — and it's the
cleanest example of speeding an algorithm up by restructuring the SUM
rather than the data structure. This chapter builds the algorithm from
the definition up: what the sum measures, why it's hopeless as written,
how to count shortest paths cheaply, and the one recurrence that
collapses the whole thing. Our `bc::brandes` stub implements it against
the O(n³) definitional oracle; gapbs's `bc.cc` and LAGraph's
`LAGr_Betweenness.c` show the two production shapes.

## The problem in one sentence

Betweenness as defined sums over all (source, target) pairs — on our
65,536-vertex RMAT that is **~2.8 × 10¹⁴ elementary operations
(n³)**, and Brandes gets the identical numbers for roughly
n × m ≈ 1.2 × 10¹¹, three orders of magnitude less, without
approximating anything.

## The concepts, step by step

### Step 1 — what betweenness measures: traffic through a vertex

Betweenness centrality scores a vertex by how many shortest paths pass
through it — a bridge vertex connecting two clusters lies on *every*
cross-cluster shortest path and scores enormously; a leaf lies on none
and scores zero. Two counting quantities make it precise: **σ_st**
(sigma) is the number of distinct shortest paths from s to t (there
can be many of equal length), and **σ_st(v)** is how many of those
pass through v. The score is the sum of fractions:

```
  bc(v) = Σ_{s≠v≠t}  σ_st(v) / σ_st
```

The fraction matters: if s→t has 4 equally-short paths and 2 go
through v, v gets credit 0.5 for that pair — betweenness counts
*share of traffic*, not path existence. Why it matters: this is the
standard "who is the broker" measure in fraud rings, network
resilience, and social analysis — and it is defined as a sum over all
n² vertex pairs.

### Step 2 — the definitional cost: O(n³), and why we keep it anyway

Computing bc directly means: for every pair (s, t), find all shortest
paths, attribute fractions to every interior vertex — an all-pairs
computation with a triple loop, O(n³) time and O(n²) memory for the
all-pairs depths and σ. Our `bc_brute` does exactly this, and it is
hopeless at scale (Step 1's 2.8 × 10¹⁴ for n=65,536). But a slow,
obviously-correct transcription of the definition is the perfect
**oracle** (a reference implementation used only to check a fast one) —
the stub must reproduce `bc_brute`'s numbers exactly before it earns
the right to sample.

### Step 3 — counting paths with one BFS: σ flows along the BFS DAG

The number of shortest paths from a fixed source s to every vertex
comes out of a single BFS (breadth-first traversal that labels each
vertex with its **depth** — hop distance from s). The edges that go
from depth d to depth d+1 form the **BFS DAG** (directed acyclic
graph) — precisely the edges that shortest paths from s may use. Path
counts accumulate along it:

```
  σ_s(s) = 1
  σ_s(v) = Σ  σ_s(u)   over DAG predecessors u of v
           (u at depth[v]-1 with an edge u→v)

        s            σ: s=1
       / \              a=1, b=1        two length-2 paths reach c:
      a   b             c = σ(a)+σ(b) = 2
       \ /
        c
```

One BFS gives depths and σ for *all* targets at once — O(E) per
source. That kills the "for every t" half of the pair sum: what
remains expensive is attributing fractions to interior vertices, which
is Step 4's job.

### Step 4 — the dependency: fold the sum over targets

Brandes' move is to fix the source s and give a name to the entire
inner sum over targets — the **dependency** of s on v:

```
  δ_s(v) = Σ_t  σ_st(v) / σ_st        so that   bc(v) = Σ_s δ_s(v)
```

Nothing is computed yet; this is pure regrouping. But the regrouped
quantity turns out to satisfy a recurrence over the BFS DAG — meaning
δ_s(v) for *all* v can be computed in one backward sweep, without ever
enumerating targets t. That is the restructuring in the chapter title:
the data structures are unchanged (a BFS queue, some arrays); only the
order of summation moved.

### Step 5 — the recurrence: one backward sweep per source

Every shortest path from s through v continues into exactly one DAG
successor w of v — so partition the paths-through-v by that successor,
and δ_s(v) becomes a sum over v's successors of already-computed
quantities:

```
  definition:  bc(v) = Σ_{s≠v≠t}  σ_st(v) / σ_st
               (our bc_brute: all-pairs BFS + triple loop, O(n³))

  Brandes' observation: fix s and define the DEPENDENCY
               δ_s(v) = Σ_t σ_st(v)/σ_st
  then δ_s satisfies a recurrence over the BFS DAG, deepest first:

               δ_s(v) =  Σ_{w : v ∈ pred_s(w)}  (σ_sv / σ_sw) · (1 + δ_s(w))

  so per source: one forward BFS (depths + σ) + one backward sweep.
  bc(v) = Σ_s δ_s(v).   n sources × O(E) each = O(V·E).
```

The recurrence is the entire paper — derive it once by hand
(partition shortest s→t paths through v by v's DAG successor w; the
1 accounts for t=w itself: paths *ending at* w also pass through v).
The factor σ_sv/σ_sw is v's share of the traffic entering w. Because
δ of a vertex needs δ of its successors (which are deeper), the sweep
must run deepest-first. Transcribed:

```rust
// after a forward BFS from s: depth[], sigma[] (path counts),
// and order = vertices sorted by depth
fn accumulate(bc: &mut [f64], order: &[u32], g: &Csr,
              depth: &[i32], sigma: &[f64]) {
    let mut delta = vec![0.0; g.n];
    for &w in order.iter().rev() {                    // deepest FIRST
        for v in g.in_edges(w) {
            if depth[v] + 1 == depth[w] {             // (v,w) is a DAG edge
                delta[v] += sigma[v] / sigma[w]       // split w's paths...
                          * (1.0 + delta[w]);         // ...the 1 = t=w itself
            }
        }
        if w != s { bc[w as usize] += delta[w as usize]; }
    }
}
```

Per source: one forward BFS + one backward sweep, both O(E). Over all
n sources: O(V·E) time, O(V) extra memory per source — the n²
all-pairs tables of Step 2 never exist. When even n sources is too
many, sample k of them and scale — gapbs defaults to 16.

### Step 6 — the two production shapes: a bitmap vs a batch

Both production codes implement Steps 3–5; they diverge on how the
backward sweep answers "is (v, w) a DAG edge" and on how many sources
run at once:

| | gapbs bc.cc | LAGraph LAGr_Betweenness.c |
|---|---|---|
| forward | `PBFS` (:51): CAS on depths, records `succ` BITMAP (:76) — "is (u,v) a DAG edge" = one bit | `frontier`/`paths` are ns×n MATRICES (:110-164) — a BATCH of sources advances as one masked mxm |
| σ | `path_counts` accumulated at depth boundaries (`depth_index` slices the BFS queue by level) | `paths += frontier` per level, FP64 semiring |
| backward | deepest-first over `depth_index`, reads `succ` | transposed mxm per level with `bc_update` matrix |
| sampling | k sources, scores scaled | `sources` array — batch size = ns |
| wins | per-edge constants, one bitmap read per edge | no atomics; 4-32 sources amortize each matrix pass |

The batched-matrix trick is the one to remember for M24: BC over 32
sampled sources = the SAME number of graph passes as one source,
just with 32-row frontier matrices — SpGEMM amortizes what frontier
code cannot (it would need 32 separate BFS queues).

### Step 7 — what breaks in practice: the four traps

The stub's failure modes are all boundary conditions of Steps 3–5:

1. σ must be accumulated ONLY along depth+1 edges (BFS DAG), and
   backprop must iterate strictly deepest-first — bucket vertices by
   depth after `bfs_sigma`, don't re-walk the queue out of order.
2. σ overflows u64 fast on dense graphs (σ multiplies along
   diamonds) — that's why everyone (gapbs `CountT`, LAGraph FP64,
   us) uses floats for path COUNTS. Exactness of the RATIO survives.
3. Disconnected sources: unreachable v has depth -1 — contribute
   nothing, don't divide by σ=0 (our RMAT has 18,844 components;
   the test will catch you).
4. Convention check: directed-sum over ordered (s,t) on a symmetric
   graph double-counts undirected pairs. Fine — but halve if you
   ever compare against NetworkX's undirected numbers.

## How to read the paper (with the concepts in hand)

- The definition section is Steps 1–2; the notation (σ_st, pair
  dependencies) maps one-to-one onto this chapter's.
- The path-counting lemma is Step 3 — one BFS per source, σ along
  DAG edges.
- The main theorem is Steps 4–5. Do the partition-by-successor
  derivation by hand *before* reading the proof; then the proof reads
  as confirmation. This recurrence is the whole paper.
- Then the two implementations: gapbs `src/bc.cc` (find where `succ`
  is set in PBFS at :76 and where backprop reads it), and LAGraph's
  `LAGr_Betweenness.c:110-164` (watch `frontier` be a matrix — Step
  6's batch — and find where the transpose enters the backward pass).

## Questions (answer in notes.md)

1. Derive the recurrence from the definition (the partition-by-
   successor argument). Where does the "+1" come from?
2. bc_brute is O(n³) time but also O(n²) MEMORY (all-pairs depths+σ).
   Brandes is O(V·E) time, O(V) extra memory per source. At what
   n/m does the brute oracle stop fitting in LLC, and does that
   matter for a CORRECTNESS oracle?
3. gapbs's succ bitmap vs re-checking depth[w]==depth[v]+1: count
   memory touches per backprop edge for both. Why does the bitmap
   win despite costing a bit per EDGE?
4. LAGraph batches ns sources into one matrix. What limits ns
   (memory = ns×n FP64 dense rows in `paths`) and where's the sweet
   spot on our 65K-node RMAT?
5. FalkorDB has `proc_betweenness.c` calling LAGraph. M24: what
   should `CALL algo.betweenness(samples: 32)` return when the graph
   changed under a delta matrix that hasn't been flushed (topic 20's
   wait) — flush first, or compute on the stale main matrix?

## References

**Papers**
- Brandes — "A Faster Algorithm for Betweenness Centrality"
  (J. Math. Sociology 2001) — the dependency recurrence is the whole
  paper; derive it by hand once

**Code**
- [gapbs](https://github.com/sbeamer/gapbs) `src/bc.cc` — frontier
  Brandes with the `succ` bitmap trick
- [LAGraph](https://github.com/GraphBLAS/LAGraph)
  `src/algorithm/LAGr_Betweenness.c` — batched-source matrix
  formulation (:110-164)
