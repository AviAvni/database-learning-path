# Worst-case optimal Datalog on a GPU and on a cluster: what the 2026 papers claim, and what their tables say

Soufflé, Ascent and Cozo (the previous chapters) evaluate a rule as a chain of
**binary** joins — nested loops, one atom at a time. Two 2026 papers from
one group replace that with **worst-case optimal joins** inside the same
semi-naive loop, and then spread the loop over hardware that punishes
intermediate results: a GPU with 48 GB of memory, and an MPI cluster where
every intermediate tuple crosses a network. This chapter reads both, checks
the arithmetic in their headline claims against their own tables, and says
where the lane-3 matrix formulation and these engines meet.

- **"Scaling Worst-Case Optimal Datalog to GPUs"** — Yihao Sun, Kunting Qi,
  Thomas Gilray, Sidharth Kumar, Kristopher Micinski.
  [arXiv 2604.20073](https://arxiv.org/abs/2604.20073) (v2, 23 Apr 2026; the
  PDF carries a template conference stamp — "Conference acronym 'XX" — so it
  is a preprint). The engine is **SRDatalog**.
- **"Distributed Datalog at Scale with Worst-Case Optimal Joins"** — Kunting
  Qi, Yihao Sun, Kristopher Micinski, Thomas Gilray, Sidharth Kumar.
  [arXiv 2610.12692](https://arxiv.org/abs/2610.12692) (v1, 8 Oct 2026).

Both exist and say what their abstracts say; where a number in the text does
not match the tables, Step 5 says so. They are young preprints, so every
figure below is "the authors report", and the CPU-versus-GPU comparison in
the first has a confound spelled out in Step 5.

Topic 44's [reading-relational-ematching.md](../44-egraphs-egglog/reading-relational-ematching.md)
Step 8 works the AGM bound on the triangle, and its
[reading-free-join.md](../44-egraphs-egglog/reading-free-join.md) is the
CPU-side bridge between binary and worst-case optimal joins; topic 54
(`topics/54-join-algorithms/`) is the main-memory join-algorithm treatment.
FlowLog (VLDB 2026, summarized in topic 27's README) is the CPU Differential
Dataflow engine both papers use as a baseline.

## The problem in one sentence

A multi-way recursive rule evaluated as a chain of binary joins can
materialize an intermediate relation far bigger than its inputs *and* its
output in every round — fatal when memory is 48 GB or the intermediate must
be shuffled across a network — so can the recursion be driven by a join that
never does, without losing the load balance a GPU needs?

## The concepts, step by step

### Step 1 — the blow-up a binary plan cannot avoid

> **In:** the triangle rule `triangle(x,y,z) :- r(x,y), s(y,z), t(z,x)`.
> **Out:** the AGM bound, a concrete star-graph blow-up, and what a
> worst-case optimal join does instead.

A **worst-case optimal join** (WCOJ) is a join algorithm whose running time
is bounded by the largest output the query can *possibly* produce on inputs
of the given sizes — the **AGM bound** (Atserias–Grohe–Marx). For the
triangle with each relation holding N tuples the AGM bound is N^1.5.
(Defined and worked in topic 44, Step 8 of
[reading-relational-ematching.md](../44-egraphs-egglog/reading-relational-ematching.md).)
A binary plan joins two relations first, `r ⋈ s`, and that intermediate can
be much bigger than N^1.5 *whatever order you pick*. Concretely, a **star**:
m edges `a_i → h` into a hub h, and m edges `h → b_j` out of it.

```
inputs:       r = s = t = the 2m edges            2m = 2,000 tuples each (m = 1,000)
r ⋈ s on y=h: m × m two-hop paths                 1,000 × 1,000 = 1,000,000 intermediate tuples
triangles:    need t(z,x) closing each path; with no such edge, output = 0
```

A million intermediate tuples for a 2,000-edge input and an empty answer.
WCOJ evaluates **one variable at a time**: choose a value for x, then the
values of y consistent with every relation that mentions y, then z — each
step is an **intersection** of sorted lists, and a candidate that cannot
extend to a full answer dies before anything is materialized. The cost of
this is the programming model of Step 3: intersections over sorted data, not
hash probes.

In Datalog the blow-up repeats every round. Lane 1's same-generation rule is
three atoms (`arc(a,x), sg(a,b), arc(b,y)`); lane 3's two-product form has
to materialize `Aᵀ·D` — the intermediate — before multiplying by `A`
(notes.md compares the work of the two). The GPU paper's §3 puts the claim
at program-analysis scale: frameworks like DOOP and ddisasm have "hundreds of
cyclic, 6-to-8-way join rules", for which "binary evaluation guarantees
asymptotic blowups in intermediate relation sizes regardless of the chosen
join order" and the intermediates "exhaust the VRAM capacity of any modern
GPU" (§3, *The VRAM wall of binary joins*). That is an argument, supported
by their finding that the GPU binary-join engines GDlog and VFLog fail on
those workloads (§7.1, "reliably Out-Of-Memory"); the paper does not print
the OOM runs.

### Step 2 — why not simply use an existing WCOJ?

> **In:** Step 1. **Out:** the three requirements the GPU paper derives from
> the failures of earlier systems.

§3 lists three failures. **Static WCOJ engines** (HoneyComb, cuMatch) build
hierarchical indexes once for one-shot queries; semi-naive evaluation inserts
a delta every round, so the index must be rebuilt, and the paper's
microbenchmark finds the rebuild "consumes up to 93% of the total merge time"
(§3). **Dynamic WCOJ** (ALFTJ) balances load by work stealing, which needs
global atomic counters to allocate output — fatal on a GPU, where every
thread must know its write offset before it writes. **Binary-join GPU
Datalog** (GDlog, VFLog) fully materializes intermediates in device memory
(Step 1). From them the paper takes three imperatives (§3): a two-phase
count-then-materialize pipeline with deterministic write offsets; flat sorted
column storage with cheap delta merges; and a launch-time load balancer.

### Step 3 — SRDatalog's design: sorted flat columns, count then materialize

> **In:** Step 2's imperatives. **Out:** the per-iteration pipeline (§4,
> Figure 1) and the two parts that connect to lanes 2 and 3.

Each relation is **flat, sorted columns** in a structure-of-arrays layout,
"sorted lexicographically in radix order according to the column access
sequence dictated by the query plan" (§4), so all tuples sharing a prefix are
contiguous and a **prefix** lookup is a binary search; no separate index
structure exists. One semi-naive iteration is a fixed pipeline (§4, Figure 1):

```
Histogram         bound the work of the outermost join variable           (Step 4)
Count kernel      multi-way column intersections, writing only counts
  prefix sum      per-thread output offsets, one bulk allocation
Materialize       re-run the intersections, writing tuples at known offsets
Compute Delta     filter against the full relations → new tuples
Build Index       sort the new tuples into the column layout
Merge             integrate the delta
```

Two things connect to this topic's lanes. **The Merge phase is lane 2's
sorted merge, with an amortization**: a relation is split into a small *head
buffer* and a large fully sorted *body*; each delta merges into the head, and
the head flushes into the body "when a capacity threshold is reached"; joins
treat head and body as a logical union (§4). Lane 2's reference rewrites the
whole closure each round and loses to hash on a long chain for exactly that
reason (notes.md); the head/body split is the standard fix.
**Count-then-materialize is the symbolic/numeric split of sparse matrix
multiplication** — run the loop on the pattern to size the output, then again
to fill it — the structure topic 20 reads in
[reading-gustavson-spgemm.md](../20-graphblas/reading-gustavson-spgemm.md)
Step 5, arrived at here for the same reason (unknown output size,
no atomics).

### Step 4 — skew: the histogram and the helper relation

> **In:** a power-law graph, where one join key has millions of matches.
> **Out:** how the engine splits a skewed key, and what the ablation says.

If each thread group takes one value of the outermost join variable, a hub
traps one group while the rest idle. SRDatalog computes a **histogram** of
work per root value first, prefix-sums it, and partitions the root range into
equal-work slices at kernel launch — no work stealing (§5). When the skew is
*inside* the join (an inner variable binding to a ubiquitous base type), it
**splits the rule** with a helper relation so the hot variable moves to the
root, where the histogram can see it. The ablation (§7.3, Figure 5, six DOOP
rules, NVIDIA Nsight kernel times) reports speedups from the balancing of
"1.1× to 35.8×", the largest (HeapAllocHelper) from the rule split, the two
virtual-dispatch rules 3.1×–3.3× and CastTo 1.8× — where "a single key
encapsulates 75.5% of the workload". The authors flag where it does not help:
unmitigated inner skew keeps the speedup low. §6 adds *stream-order rule
multiplexing*, running independent rules on separate CUDA streams, to fill
the GPU during the long tail of small deltas (§7.4, Figure 6: 1.27×–1.66× on the five DOOP datasets, 1.23× on galen).

### Step 5 — auditing the headline: what Table 2 says

> **In:** §7.1, Table 2: 17 workload–dataset rows. **Out:** a geometric mean
> recomputed from the raw times, and the confounds.

The abstract claims "geometric-mean speedups of 21× to 47×"; §7.1 says "21×
over Ascent, 14× over FlowLog, and 26× over Soufflé". The geometric mean of
n ratios is the n-th root of their product. Recomputing from Table 2's
17 rows of raw seconds (SRDatalog 1 GPU vs Ascent, FlowLog, Soufflé on 32
cores) as the geometric mean of time ratios:

```
vs Ascent    17.6×     (§7.1 text: 21×)
vs FlowLog   13.4×     (§7.1 text: 14×)
vs Soufflé   26.4×     (§7.1 text: 26×)
```

— from the raw columns; from the table's rounded "speedup" columns the same
means are 17.9×, 13.4× and 26.5×. Soufflé and FlowLog agree with the text; the
**Ascent figure does not: 17.6× from the table against the stated 21×**, and
the abstract's 47× appears nowhere in the 17 rows. The per-row ratios (2.7× to
214× across the 51 of them) are as printed; the
summary statistics are internally inconsistent in this version. Treat the
rows, not the abstract, as the evidence.

Three confounds belong beside any such number. **CPU versus GPU**: one RTX
6000 Ada (48 GB, 300 W) against 32 cores of an EPYC 9655 (~400 W), "matched
by commodity cloud rental cost (≈$1/hour)" (§7); the ratio mixes the
algorithm (WCOJ against nested loops) with the hardware (memory bandwidth),
and only the SG-versus-VFLog and LSQB-versus-cuMatch rows compare GPU with
GPU. Those are the clean ones: **2.3×–7.1×** over VFLog on same-generation
(Table 3; 513 iterations on vsp_finan, 35.5 s → 5.01 s) and **2.1×–4.0×**
over cuMatch on LSQB Q6/Q7/Q9 at SF 100 (Figure 4). **Iteration count**: the
smallest ratios are in ddisasm (530–1,542 iterations; 2.7×–23× across
the three baselines), and the authors say why: those programs
"generate only a microscopic trickle of new delta tuples" in most rounds,
exposing "the sequential launch overhead of the semi-naïve evaluation loop"
(§7.1). The largest are in Andersen (24–25 iterations; 32×–214×) and
CSPA and galen (31–34 iterations): dense and shallow. **Loading is excluded** from every
measurement (§7.1), including Soufflé's, which is derived by subtraction.

### Step 6 — the cluster: HyperCube partitioning, iterated

> **In:** Step 3's engine idea moved to shared-nothing memory. **Out:** the
> partition scheme, Table I's replication arithmetic, and what the scaling
> tables show.

On a cluster the cost of a join is communication. **HyperCube** (also called
Shares) assigns each join variable a dimension of a logical process grid; a
relation is hash-partitioned along the grid dimensions of the variables it
contains and **replicated** along the others, so that every rank has, locally,
every tuple combination it needs and the whole join is **one communication
round followed by rank-local WCOJ**. The paper's contribution is iterating
it: in every semi-naive round only the *newly derived* tuples are routed to
their owner ranks (`MPI_Alltoall(v)`), replicated along missing dimensions
(`MPI_Allgather(v)` within a subcube communicator), and deduplicated against
local state to form the next delta; the same layout is reused every round so
the intersections stay local (§IV). Table I of the paper prices the layouts
for the Peer Influence rule on 64 ranks (storage in multiples of N_E, the
edge relation's size, and N_I, the Influenced relation's):

```
                 1D  x(64)         2D  x(8) z(8)       3D  x(4) y(4) z(4)
Influenced(x)    1·N_I             8·N_I               16·N_I
R(x,y) edges     1·N_E             8·N_E               4·N_E
S(x,z) edges     1·N_E             1·N_E               4·N_E
T(z,y) edges     64·N_E            8·N_E               4·N_E
edges, total     66·N_E            17·N_E              12·N_E
```

The 1D layout replicates one relation 64 times; 2D spreads replication
(17 N_E for edges); 3D has the least edge replication (12 N_E) but replicates
the unary relation 16 times. The empirical trade (Tables II–III, runtimes in
seconds; Table IV, share of time in communication): 2D is faster when
feasible — Peer Influence (Orkut) on Polaris at 512 ranks is 10.087 s in 2D
against 39.556 s in 3D — but **2D runs out of memory at 32 ranks** on
Polaris for the two Orkut workloads where 3D completes (68.247 s), and the
communication share is higher in 3D (86.8% against 63.9% at 2,048 ranks for
Peer Influence, Orkut, Polaris). Scaling is real but saturating: "up to 14.3×
on Polaris and 13.4× on Aurora" from 32 to 2,048 ranks (§1), most of it
before 512, and TC on USRoad *slows down* past 512 ranks (2.495 s → 3.515 s →
7.336 s on Polaris, 2D).

### Step 7 — the single-node honesty check

> **In:** Table V (32-way parallelism on one Polaris node) and Table VI.
> **Out:** which comparisons support the paper's claim and which do not.

Table V compares one node (32 MPI ranks) with Soufflé, FlowLog and DuckDB
(32 threads). The abstract-level reading, "faster than established
single-node engines on most benchmarks", holds for Peer Influence (23.9 s
against FlowLog's 165.7 s), Triangle (28.5 s against DuckDB's 34.1 s on
LiveJournal) and TC (20.2 s against FlowLog's 68.6 s). It does **not** hold
for same-generation on G10 — ours 78.2 s, Soufflé 57.3 s, FlowLog 31.9 s — or
for Andersen (Large) — ours 17.5 s, FlowLog 9.4 s. Table VI compares with
distributed systems on **TC over USRoad** only: 8.9×–11.2× faster than Slog
at every rank count from 32 to 512, and faster than a multi-node multi-GPU
engine by 3.7× at 32 ranks narrowing to 1.5× at 512. TC is a two-atom join:
a WCOJ has nothing to optimize in it, so that win is evidence for the
HyperCube-and-MPI engineering, not for worst-case optimality — which the paper
supports on its cyclic benchmarks (Peer Influence, Triangle) and says
nothing about TC. The authors' own reading is deliberately modest: "accelerator-
based systems may become more competitive at larger scales" (§V).

## How to read the papers (with the concepts in hand)

GPU paper: **§3** (the three failures) and **§4–§5** (Figure 1, Figure 2) are
the design; **§7.1 Table 2 with Step 5's recomputation beside it**, then
**§7.2** (the clean GPU-versus-GPU rows) and **§7.4**. Distributed paper:
**§III** with Table I and Figure 3 (partition and replication, draw the
4-rank 2 × 2 grid), **§IV** with Figure 4, then **Tables II–VI** with Step 6
and 7 beside them.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | lane 1, nonlinear TC and SG rows; lane 3's `sg_algebra` | the intermediate a pairwise plan materializes |
| 3 | lane 2 (sorted merge); lane 3 (count then fill = symbolic then numeric) | head/body merge; `mxm_complement_mask` |
| 6 | topic 36/37's sharding chapters | communication as the cost of a join |

## Questions (answer in notes.md)

1. Generalize Step 1's star to a hub with m in-edges and m out-edges plus
   k closing edges `b_j → a_i`. How many triangles, and what is the binary
   plan's intermediate size?
2. Recompute the geometric mean of Table 2's "vs FL" column from its rounded
   ratios, then from the raw times; why do they differ in the third digit?
3. 4 ranks as a 2 × 2 grid, Peer Influence's `R(x,y)` partitioned on x and y.
   Where does the tuple (x=7, y=9) live and how many ranks hold a copy of an
   `Influenced(7)` tuple?
4. TC on USRoad speeds up to 512 ranks then slows (Table II). What
   communication-share trend in Table IV would you expect to see for TC?

## Done when

Answer each before unfolding it.

- [ ] You can state the AGM bound for the triangle and give a graph where a binary plan blows up.

  <details><summary>Answer</summary>

  With N tuples per relation the triangle has at most N^1.5 results (AGM).
  A hub with m in-edges and m out-edges makes `r ⋈ s` produce m² two-hop
  tuples — 1,000,000 for m = 1,000 on 2,000 edges — while the triangle
  output can be empty; a WCOJ intersects per variable and never builds them.

  </details>

- [ ] You can name the three failures SRDatalog is designed against and its answer to each.

  <details><summary>Answer</summary>

  Binary GPU Datalog materializes intermediates in VRAM → WCOJ. Static WCOJ
  rebuilds hierarchical indexes per delta (up to 93% of merge time) → flat
  sorted columns with a head buffer and body. Dynamic WCOJ needs atomic output
  allocation under work stealing → histogram-guided launch-time balancing plus
  two-phase count-then-materialize with deterministic offsets (§3–§5).

  </details>

- [ ] You can say which of the GPU paper's comparisons are clean and recompute one headline.

  <details><summary>Answer</summary>

  Clean (GPU versus GPU): 2.3×–7.1× over VFLog on same-generation (Table 3)
  and 2.1×–4.0× over cuMatch on LSQB (Figure 4). The CPU-versus-GPU rows
  mix algorithm and hardware. From Table 2's raw times the geometric mean
  over Ascent is 17.6×, not the text's 21×; over FlowLog 13.4× (text 14×); over
  Soufflé 26.4× (text 26×).

  </details>

- [ ] You can compute the replication cost of a HyperCube layout from Table I.

  <details><summary>Answer</summary>

  Sum the per-relation replication factors: 64 ranks, edges R, S, T —
  1D 1 + 1 + 64 = 66·N_E; 2D 8 + 1 + 8 = 17·N_E; 3D 4 + 4 + 4 = 12·N_E — while
  the unary Influenced relation goes 1, 8, 16 × N_I. 3D minimizes edge
  replication but moves the burden to Influenced, and 2D was faster
  (10.087 s against 39.556 s at 512 ranks) wherever it fit in memory.

  </details>

- [ ] You can say which single-node and distributed results support the claim and which do not.

  <details><summary>Answer</summary>

  Table V supports it for Peer Influence, Triangle and TC but not for SG
  (G10: 78.2 s against FlowLog 31.9 s, Soufflé 57.3 s) or Andersen Large
  (17.5 s against FlowLog 9.4 s). Table VI wins (8.9×–11.2× over Slog) are
  on TC, a two-way join where worst-case optimality is irrelevant, so they
  credit the HyperCube/MPI design rather than WCOJ.

  </details>

## References

- Yihao Sun, Kunting Qi, Thomas Gilray, Sidharth Kumar, Kristopher Micinski,
  **"Scaling Worst-Case Optimal Datalog to GPUs"**, arXiv 2604.20073v2 (Apr
  2026) — §3 (failures), §4–§6 (design), §7.1 Table 2, §7.2 Table 3 and
  Figure 4, §7.3 Figure 5, §7.4.
- Kunting Qi, Yihao Sun, Kristopher Micinski, Thomas Gilray, Sidharth Kumar,
  **"Distributed Datalog at Scale with Worst-Case Optimal Joins"**, arXiv
  2610.12692v1 (Oct 2026) — §III (HyperCube, Table I), §IV (iteration),
  §V Tables II–VI.
- Atserias, Grohe, Marx, **"Size bounds and query plans for relational
  joins"**, FOCS 2008 — the AGM bound; named, not read for this chapter.
- Hangdong Zhao et al., **FlowLog**, PVLDB 19(3), 2026 — the CPU baseline
  both papers share; summarized in topic 27's README.
