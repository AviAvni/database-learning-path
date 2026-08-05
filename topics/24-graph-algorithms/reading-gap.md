# The GAP benchmark suite: five graphs so the wrong winner can't win

The yardstick for graph analytics: 6 kernels × 5 graphs, plus
REFERENCE IMPLEMENTATIONS that are themselves state-of-the-art
single-node code. Read the paper for the methodology (it's topic 22's
fair-benchmarking argument specialized to graphs), read `src/` for
the algorithms — each .cc file opens with a mini-paper. Before
either, this chapter builds the suite's ideas one at a time: what a
kernel is, then the three graph properties — degree skew, diameter,
and source luck — that let a single benchmark graph crown the wrong
winner.

*Source pinned (resources/codebases.md): gapbs @`b5e3e19`; anchors
re-checked with `tools/pinned-source.py`. Trial counts are the GAP
paper (arXiv:1508.03619, Table 1); the triangle figures are this
topic's `notes.md`.*

## The problem in one sentence

Graph-algorithm performance depends on the input graph's *shape* so
strongly that one benchmark graph ranks implementations backwards —
on our own bench, two graphs with identical n=65,536 and m=1,819,338
contain **15,645,988 vs 5,428 triangles — a ≈2,882× difference**
(15,645,988 / 5,428; notes.md), so a triangle counter tuned on one is
being measured on a different job on the other.

## The concepts, step by step

### Step 1 — a kernel: the unit of fair comparison

> **In:** the goal of comparing graph implementations fairly.
> **Out:** the *kernel* — an algorithm pinned by input/output only — and
> the 6-kernel × 5-graph matrix GAP runs so no data structure is
> privileged.

A **kernel** is a self-contained algorithm with a precisely specified
input and output — specified tightly enough that any implementation,
in any language over any data structure, can be timed on the same
task. GAP picks six kernels, five graphs, and runs the full matrix
(the diagram's annotations are unpacked in Steps 2–4):

```
  kernels: BFS  SSSP  PR  CC  BC  TC
  graphs:  twitter (skew)  web (locality)  road (diameter!)
           kron (RMAT synthetic)  urand (uniform synthetic)
                     │
   every kernel × every graph, many trials from random sources
   (Table 1: BFS/SSSP 64, PR/CC 16, BC 16×4 sources, TC 3) —
   because ONE graph shape crowns the wrong winner:
   road kills delta-stepping's parallelism (long diameter),
   urand kills direction-optimizing BFS (no hubs),
   kron/twitter kill anything O(max_degree²)
```

The six, in plain terms: BFS (breadth-first search — explore the
graph level by level from one source vertex), SSSP (single-source
shortest paths — BFS's weighted cousin), PR (PageRank — iterate a
per-vertex score until it stabilizes), CC (connected components —
label every vertex with which reachable island it belongs to), BC
(betweenness centrality — score vertices by how many shortest paths
pass through them), TC (triangle counting — count 3-cycles). Three
are per-source traversals, three are whole-graph iterations; together
they cover every memory-access pattern a graph analytics engine has.
Why it matters: drop any one pattern from the suite and an engine can
over-fit to the rest.

### Step 2 — degree skew: hubs change the work, not just the clock

> **In:** two graphs with identical n and m.
> **Out:** why *degree skew* (a power-law hub distribution) changes
> which algorithm you are effectively running, not just how fast — and
> why GAP ships both skewed and uniform graphs.

A vertex's **degree** is its edge count, and real-world graphs are
**skewed**: degree follows a power law, so a handful of hub vertices
carry a huge fraction of the edges while most vertices have a few.
Our RMAT scale-16 graph (RMAT is the standard skewed-graph generator,
what GAP calls "kron") and our uniform graph have the *same* n=65,536
and m=1.82M — but max degree 9,751 vs 59. The consequence is not a
constant factor; it changes which algorithm you are running:

- Triangle counting intersects neighbor lists, so hub neighborhoods
  intersecting each other is where triangles live: 15.6M triangles on
  RMAT vs 5.4K on uniform. Any TC benchmark on uniform data measures
  a different algorithm.
- Anything with a per-vertex cost proportional to degree² detonates
  on a 9,751-degree hub (9,751² ≈ 95M operations for one vertex).
- Conversely, urand's *lack* of hubs kills direction-optimizing BFS:
  frontiers never get dense enough for the pull side of the switch to
  pay off (topic 20's Beamer trick needs hubs to shine).

That is why GAP includes both skewed (twitter, kron) and uniform
(urand) graphs: each disqualifies a different class of over-fitted
winner.

### Step 3 — diameter: how many rounds the algorithm must take

> **In:** a frontier algorithm that advances one distance-level per
> round.
> **Out:** why *diameter* sets the round count (and per-round frontier
> size the parallelism), so a road graph starves the parallelism that
> twitter/kron hand out.

The **diameter** is the longest shortest-path distance in the graph,
measured in hops — and for any algorithm that advances a **frontier**
(the set of vertices discovered in the current round of a traversal)
one distance-level per round, the diameter *is* the round count, and
the frontier size per round is all the parallelism there is:

```
  twitter/kron:  diameter ~10-20   → frontiers of millions of
                                      vertices per round: parallel
  road (USA):    diameter ~1000s   → frontiers of a few hundred:
                                      1000s of tiny sequential rounds
```

Road networks are in the suite precisely because they starve
frontier parallelism: delta-stepping's buckets (its unit of parallel
work) hold almost nothing at any bucket width, so an SSSP
implementation that looks great on twitter can crawl on road. One
graph family flips the SSSP ranking — that's the suite's argument in
one row.

### Step 4 — source luck: why many trials from random sources

> **In:** the per-source kernels (BFS, SSSP, BC) on a skewed graph.
> **Out:** why source choice can swamp most optimizations, and GAP's
> defense — many trials from random non-zero-degree sources, all
> reported (Table 1: BFS/SSSP 64 trials/64 sources; BC 16 trials
> averaging 4 sources; whole-graph PR/CC 16, TC 3).

Per-source kernels (BFS, SSSP, BC) start from a chosen vertex, and on
a skewed graph the choice is worth more than most optimizations:
starting at a hub reaches the giant component in ~2 hops; starting at
a degree-1 leaf adds rounds and shrinks early frontiers — the same
work, wildly different clock. GAP's rule (paper Table 1): BFS and SSSP
run **64 trials from 64 random sources**, BC runs **16 trials, each
averaging 4 sources**, and the whole-graph kernels run enough trials to
catch non-determinism (PR 16, CC 16, TC 3). Report ALL trials — not
the mean, not the best. Our bench uses 3 fixed sources — upgrade when
it matters. The cost of skipping this: one lucky hub source silently
overstates a per-source headline number.

### Step 5 — the spec binds: kernel specification ≠ implementation

> **In:** a kernel specified by input/output only.
> **Out:** how the spec *forks* implementations — e.g. GAP's PR spec
> (L1-error stop, ignore dangling vertices) is why `LAGr_PageRankGAP`
> exists as a separate function from textbook PR.

GAP specifies each kernel by input and output only, so algebraic
codes (LAGraph runs GAP too) and frontier codes compete honestly —
no data structure is privileged. But a spec is an interface, and
interfaces bind implementations: `LAGr_PageRankGAP` exists as a
separate function because GAP's PR spec differs from textbook PR — it
stops when the summed score change drops below 10⁻⁴, and it **ignores
dangling (zero-out-degree) vertices** rather than redistributing their
rank, exactly as gapbs's `pr.cc` does. Textbook PR (and LAGraph's other
entry, `LAGr_PageRank`) instead redistributes sink rank each iteration
to keep the scores summing to 1. Benchmark specs fork implementations —
remember that when you write M22's lanes: whatever you specify is what
everyone will build.

### Step 6 — the baseline problem: reference code that is itself state of the art

> **In:** the classic benchmarking sin (beating a strawman baseline).
> **Out:** how GAP forecloses it — shipping gapbs, whose reference
> kernels are themselves state-of-the-art, each opening with a
> mini-paper header comment.

The classic benchmarking sin (topic 22) is beating a strawman
baseline. GAP forecloses it by shipping gapbs: reference
implementations that are themselves state-of-the-art single-node
code — direction-optimizing BFS, delta-stepping with thread-local
bins, Brandes with a successor bitmap, Afforest's sampling CC. A
claimed win over gapbs means something. And each `src/*.cc` opens
with a header comment that is a mini-paper on its trick — required
reading before the code.

## Where each step lives in the code

Each file's header comment = required reading (Step 6):

| file | algorithm | the trick |
|---|---|---|
| `src/bfs.cc` | direction-optimizing | topic 20's guide covers it — α=15, β=18 here |
| `src/sssp.cc:87` | `DeltaStep` | thread-local bins (`:32` comment); `:44`: redundant relaxation is CHEAPER than removing stale entries — same lazy-deletion bet as our Dijkstra oracle |
| `src/bc.cc:51` | Brandes | `PBFS` records a `succ` BITMAP (:76) so backprop tests "is w my BFS successor" in one bit — no depth recheck |
| `src/cc.cc:95` | Afforest | `:106` neighbor_rounds=2 link sweeps, `:69` SampleFrequentElement (1024 samples), `:127` final sweep skips the giant component (`if (comp[u]==c) continue;`) |
| `src/pr.cc:31-57` | pull PR | kDamp .85, L1-error stop; `pr_spmv.cc` is the same as one SpMV per iter — the algebraic identity made explicit |
| `src/tc.cc:52-99` | ordered TC | `OrderedCount` after `RelabelByDegree` if `WorthRelabelling` (:75 samples degree skew) |

## How to read the paper (with the concepts in hand)

- The graph-selection discussion is Steps 2–3: for each of the five
  graphs, name the property (skew, locality, diameter) and which
  kernel ranking it exists to flip.
- The methodology section is Step 4 — many trials from random sources,
  all reported (Table 1: 64 for BFS/SSSP, 16 for PR/CC/BC, 3 for TC).
  Steal it for M22/M24's lanes.
- The kernel specifications are Step 5 — notice how tightly PR's
  stopping condition is pinned, and why (specs bind implementations).
- Then go to `src/` with the table above; the header comments (Step
  6) are faster than the paper for the per-algorithm tricks.

## Questions (answer in notes.md)

1. Why does GAP include road networks at all — which of the 6
   kernels ranks implementations DIFFERENTLY on road vs twitter,
   and what property (diameter, degree variance) drives each flip?
2. sssp.cc:44 argues redundant relaxations beat precise bucket
   removal. Under what edge-weight distribution does that bet fail?
3. gapbs's `bc.cc` runs 1 source by default (`CLIterApp(..., 1)` at
   bc.cc:234); the GAP spec approximates BC from 4 sources per trial,
   16 trials (paper Table 1). On our RMAT (18,844 components!), what
   systematic error does source sampling introduce and how would you
   stratify?
4. pr.cc vs pr_spmv.cc: same math, different memory access. Which
   wins on kron and why (hint: pull = gather = topic 20's SpMV
   16-19 GB/s lane)?
5. GAP has no Louvain/Leiden kernel. What makes community detection
   benchmark-hostile (hint: nondeterminism, tie-breaking,
   quality-vs-speed frontier)?

## Done when

Answer each before unfolding it.

- [ ] You can explain what a kernel is and why the spec binds the kernel rather than the implementation.
  <details><summary>Answer</summary>

  A kernel is an algorithm pinned by input and output only, so any
  implementation over any data structure can be timed on the identical
  task. The spec is an interface: it forks implementations (GAP's
  L1-stop, ignore-dangling PR spec is why `LAGr_PageRankGAP` is a
  separate function), so whatever you specify is what everyone builds.

  </details>
- [ ] You can explain why degree skew changes the work and not just the clock — this topic measures max degree 9751 on RMAT against 59 on uniform, with triangle counts of 15.6 M against 5428.
  <details><summary>Answer</summary>

  Same n and m, but a degree-9,751 hub concentrates edges, so
  neighbour-list intersections (triangles) and any O(degree²) per-vertex
  cost explode where they were trivial on uniform data (15,645,988 vs
  5,428 triangles). The counter is running a different job, not the same
  job slower.

  </details>
- [ ] You can explain why diameter sets the round count and why road networks are therefore in the suite.
  <details><summary>Answer</summary>

  A frontier algorithm advances one distance-level per round, so the
  diameter *is* the round count and per-round frontier size is the only
  parallelism. Twitter/kron (diameter ~10-20) give million-vertex
  frontiers; road (diameter ~1000s) gives thousands of tiny sequential
  rounds — flipping the SSSP ranking.

  </details>
- [ ] You can say why many trials from random sources are required, and what source luck does to a single measurement.
  <details><summary>Answer</summary>

  On a skewed graph, a hub source reaches everything in ~2 hops while a
  leaf source adds rounds — same work, very different clock. GAP runs 64
  trials/64 sources for BFS/SSSP and 16 trials of 4 sources for BC
  (Table 1) and reports all of them, so one lucky source can't silently
  inflate the headline number.

  </details>
- [ ] You can state the baseline problem: reference code that is itself state of the art.
  <details><summary>Answer</summary>

  The classic sin is beating a strawman. GAP ships gapbs — reference
  kernels that are themselves state-of-the-art (direction-optimizing
  BFS, delta-stepping, Brandes with a successor bitmap, Afforest) — so a
  claimed win over gapbs actually means something.

  </details>
- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>Answer</summary>

  Done when notes.md answers Q1 (which kernels flip on road vs twitter),
  Q2 (when redundant-relaxation loses), Q3 (BC source-sampling error and
  stratification), Q4 (pr.cc vs pr_spmv.cc on kron), and Q5 (why
  community detection is benchmark-hostile).

  </details>

## References

**Papers**
- Beamer, Asanović, Patterson — "The GAP Benchmark Suite"
  ([arXiv:1508.03619](https://arxiv.org/abs/1508.03619)) — read for
  the methodology: why these 5 graphs, why 64 trials from random
  sources

**Code**
- [gapbs](https://github.com/sbeamer/gapbs) `src/` — each kernel's
  header comment is a mini-paper; required reading before the code
