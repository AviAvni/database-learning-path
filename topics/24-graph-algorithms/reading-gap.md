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

## The problem in one sentence

Graph-algorithm performance depends on the input graph's *shape* so
strongly that one benchmark graph ranks implementations backwards —
on our own bench, two graphs with identical n=65,536 and m=1.82M
contain **15,645,988 vs 5,428 triangles (a 2,883× difference)**, so a
triangle counter tuned on one is being measured on a different job on
the other.

## The concepts, step by step

### Step 1 — a kernel: the unit of fair comparison

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
   every kernel × every graph, 64 trials from random sources —
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

### Step 4 — source luck: why 64 trials from random sources

Per-source kernels (BFS, SSSP, BC) start from a chosen vertex, and on
a skewed graph the choice is worth more than most optimizations:
starting at a hub reaches the giant component in 2 hops; starting at
a degree-1 leaf adds rounds and shrinks early frontiers. Source
choice changes BFS/SSSP/BC time by **more than 10×** on skewed
graphs. GAP's rule: 64 trials from random sources, report ALL of
them — not the mean, not the best. Our bench uses 3 fixed sources —
upgrade when it matters. The cost of skipping this: a lucky source is
a silent 10× overstatement in your headline number.

### Step 5 — the spec binds: kernel specification ≠ implementation

GAP specifies each kernel by input and output only, so algebraic
codes (LAGraph runs GAP too) and frontier codes compete honestly —
no data structure is privileged. But a spec is an interface, and
interfaces bind implementations: `LAGr_PageRankGAP` exists as a
separate function because GAP's PR spec (stop on L1 error, handle
dangling vertices the way gapbs does) differs from textbook PR.
Benchmark specs fork implementations — remember that when you write
M22's lanes: whatever you specify is what everyone will build.

### Step 6 — the baseline problem: reference code that is itself state of the art

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
| `src/cc.cc:95` | Afforest | `:106` neighbor_rounds=2 link sweeps, `:69` SampleFrequentElement (1024 samples), `:129` final sweep skips the giant component |
| `src/pr.cc:31-57` | pull PR | kDamp .85, L1-error stop; `pr_spmv.cc` is the same as one SpMV per iter — the algebraic identity made explicit |
| `src/tc.cc:52-99` | ordered TC | `OrderedCount` after `RelabelByDegree` if `WorthRelabelling` (:75 samples degree skew) |

## How to read the paper (with the concepts in hand)

- The graph-selection discussion is Steps 2–3: for each of the five
  graphs, name the property (skew, locality, diameter) and which
  kernel ranking it exists to flip.
- The methodology section is Step 4 — 64 trials from random sources,
  all reported. Steal it verbatim for M22/M24's lanes.
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
3. bc.cc approximates with 16 sources by default. On our RMAT
   (18,844 components!), what systematic error does source sampling
   introduce and how would you stratify?
4. pr.cc vs pr_spmv.cc: same math, different memory access. Which
   wins on kron and why (hint: pull = gather = topic 20's SpMV
   16-19 GB/s lane)?
5. GAP has no Louvain/Leiden kernel. What makes community detection
   benchmark-hostile (hint: nondeterminism, tie-breaking,
   quality-vs-speed frontier)?

## References

**Papers**
- Beamer, Asanović, Patterson — "The GAP Benchmark Suite"
  ([arXiv:1508.03619](https://arxiv.org/abs/1508.03619)) — read for
  the methodology: why these 5 graphs, why 64 trials from random
  sources

**Code**
- [gapbs](https://github.com/sbeamer/gapbs) `src/` — each kernel's
  header comment is a mini-paper; required reading before the code
