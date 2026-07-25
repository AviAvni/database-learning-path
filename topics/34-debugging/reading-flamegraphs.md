# The Flame Graph: folding a million stacks into one picture

Brendan Gregg's CACM article (Vol 59 No 6, June 2016; also ACM Queue)
is the canonical write-up of the visualization you will stare at for
the rest of your performance career. In topic 34's diagnosis triad —
wrong answers / too slow / crashed — this is the "too slow" leg:
profiler samples are perishable evidence, and the flame graph
aggregates them into one durable artifact. This chapter builds the one
structural idea (merging identical stacks) and the reading discipline
(width, not height) before you open the article — with the database
question in view: for a query engine under load, the off-CPU variant
is usually where the story is.

## The problem in one sentence

A CPU profiler at 99 Hz across many cores emits hundreds of thousands
of stack traces per minute — far too many to read as text — and the
flame graph compresses them into one interactive picture by merging
identical stacks and drawing width proportional to sample count.

## The concepts, step by step

### Step 1 — the raw material: sampled stacks, not traced calls

Everything starts with a sampling profiler: interrupt the CPU at a
fixed rate (e.g. `perf record -g -F 99`) and record the full call
stack of whatever is running. Two properties matter. First, sampling
is statistical: a function's share of samples estimates its share of
CPU time. Second, it is *zero code change*: you attach to the live
database when the pager fires, detach when done — the deliberate
complement to bench lane 3, which prices the observability tax of
always-on instrumentation. The output, though, is a wall of stacks.

```
  worker-3             worker-3             worker-3
  main                 main                 main
  run_query            run_query            run_query
  executor::pull       executor::pull       parser::parse
  expand_op            filter_op
  GrB_mxm              eval_predicate
  ─ sample 184,001 ─   ─ sample 184,002 ─   ─ sample 184,003 ─ ...×10⁵
```

### Step 2 — the merge: identical stacks become one column

The whole trick of the flame graph is one aggregation step: two
samples with byte-identical stacks are the same evidence, so merge
them and keep a count. In the classic pipeline, `stackcollapse-perf.pl`
folds `perf script` output into one line per unique stack:

```
main;run_query;executor::pull;expand_op;GrB_mxm 41203
main;run_query;executor::pull;filter_op;eval_predicate 8931
main;run_query;parser::parse 1204
```

Hundreds of thousands of samples collapse into a few hundred unique
stacks. `flamegraph.pl` renders these as an SVG: shared stack prefixes
become shared boxes, unique suffixes branch off above the last common
frame. Why it matters: the folded file is a greppable, diffable text
artifact — the SVG is a view; the folded file is the data.

### Step 3 — reading the geometry: width is everything, height is nothing

In the rendered graph, the y-axis is stack depth (root at the bottom,
leaves at the top) and each box is one function frame. Box **width**
is the fraction of samples in which that frame appeared — relative
time, inclusive of everything called above it. Crucially, the x-axis
is **not** time: after merging, sibling frames sort alphabetically, so
left-to-right order carries no temporal meaning. The samples above:

```
        ┌──────────┐┌──┐
        │ GrB_mxm  ││ev│                ev = eval_predicate
        ├──────────┤├──┤
        │expand_op ││fi│                fi = filter_op
        ├──────────┴┴──┤─┐
        │executor::pull│p│              p = parser::parse
        ├──────────────┴─┤
        │   run_query    │
        ├────────────────┤
        │      main      │
        └────────────────┘
   width ∝ samples ──────────▶   (x-order alphabetical, NOT time)
```

The reading discipline: hunt for **wide plateaus** — flat-topped boxes
where samples terminate, i.e. functions actually on-CPU — not tall
spikes. Depth is just call-path length; a 60-frame tower 2 pixels wide
costs nothing, while a squat 40%-wide `memcpy` plateau is your bug.
The top edge, left to right, is the histogram of on-CPU leaf functions.

### Step 4 — on-CPU vs off-CPU: two graphs, two questions

The on-CPU flame graph answers "where is CPU time spent." Its dual,
the **off-CPU flame graph**, samples the stacks of threads that are
*blocked* — waiting on locks, disk I/O, page faults, the scheduler —
weighted by time spent off-CPU. For databases this is often the
interesting one: a write-heavy engine's on-CPU graph can look
innocently flat while every worker spends most of wall time parked in
`futex_wait` under a latch or `fdatasync` behind the WAL. Those stacks
never run, so the on-CPU graph *cannot* show them — the two graphs
partition wall-clock time:

```
  wall time of one worker thread
  ├── on-CPU ──┤├────────── off-CPU ──────────┤
   GrB_mxm,      futex_wait (latch),  fdatasync (WAL),
   eval_pred     read() (page miss)
```

Why it matters: "the database is slow but CPUs are idle" is exactly
where an on-CPU profile shrugs; off-CPU names the lock and the fsync.

### Step 5 — differential (red/blue) flame graphs for regressions

Given two folded profiles — before/after a commit, or a good node and
a bad node — a differential flame graph draws the second profile's
shape and colors each frame by its change in sample count: red grew,
blue shrank. The regression is literally the red patch. Capture a
folded file per release, and "what got slower?" is a one-command diff.

### Step 6 — pitfalls: broken stacks make lying graphs

The graph is only as good as the stack walks. If the workload is
built with `-fomit-frame-pointer` (long the compiler default), the
frame walker cannot follow the chain and you get truncated one- or
two-frame stacks — a wide lawn of "grass" at the bottom, or towers
floating on `[unknown]`. Fixes: rebuild with frame pointers
(`-fno-omit-frame-pointer`), or use DWARF or LBR-based unwinding.
Similarly, JIT and interpreted frames show as anonymous addresses
unless the runtime exports a symbol map (e.g. `/tmp/perf-PID.map`).
Rule of thumb: before believing any plateau, check that stacks reach
a plausible root (`main`, a thread entry) — a broken-stack graph
misattributes time with total confidence.

## How to read the paper (with the concepts in hand)

CACM 59(6) / ACM Queue, ~10 pages of prose and figures; budget ~1h.

- **Opening problem statement** (10 min) — the MySQL mystery that raw
  profiler text couldn't crack; this motivates Step 2's merge.
- **The visualization definition** (15 min) — box/width/ordering
  semantics; make sure "x-axis is not time" survives the figures.
- **Implementation / pipeline** (10 min) — stackcollapse + flamegraph
  SVG generation (Step 2); note how many profilers have collapsers.
- **Variants** (15 min) — off-CPU, memory, differential (Steps 4–5);
  read the off-CPU part twice with your WAL-fsync hat on.
- **Challenges** (10 min) — Step 6's broken stacks and symbol
  problems, straight from the source.
- Then do, don't just read (~15 min): `perf record -g` against
  FalkorDB under a benchmark, render the SVG, find the widest plateau.

## Questions to answer in notes.md

1. Why is "the x-axis is not time" the load-bearing design decision?
   Name one question a time-ordered stack chart answers that a flame
   graph cannot, and the far more common converse.
2. Predict FalkorDB's flame graphs under a read-heavy workload: which
   plateaus dominate on-CPU (GraphBLAS matrix ops? filter eval?
   result serialization? allocator?), and what appears off-CPU that
   the on-CPU graph hides entirely? Then measure and score yourself.
3. A frame occupying 30% width at mid-height but with almost no
   samples terminating in it — what does that tell you, and where do
   you look next?
4. Bench lane 3 prices always-on instrumentation; sampling costs
   ~nothing until attached. What can built-in counters tell you that a
   flame graph cannot (per-query attribution, tail latencies, rare
   events between samples)?
5. Your nightly profile of a Rust binary shows a wide floor of
   two-frame "grass" stacks. Give the diagnosis steps and the two
   fixes from Step 6; which fits a production FalkorDB build, and why?

## Done when

- [ ] You can explain why width = fraction of samples containing the
      frame, and why alphabetical order (not time) enables the merge.
- [ ] You have generated one on-CPU flame graph of FalkorDB under load
      via perf → stackcollapse → flamegraph.pl and named its widest
      plateau.
- [ ] You can state which class of database slowness (locks, fsync,
      page faults) is invisible on-CPU and needs the off-CPU variant.
- [ ] You can spot broken stack walks (grass, floating towers) before
      trusting any plateau.

## References

**Article**
- Brendan Gregg — "The Flame Graph" (Communications of the ACM 59(6),
  June 2016; also ACM Queue) —
  [queue.acm.org/detail.cfm?id=2927301](https://queue.acm.org/detail.cfm?id=2927301)

**Code & further material**
- [FlameGraph repo](https://github.com/brendangregg/FlameGraph) —
  `stackcollapse-*.pl` and `flamegraph.pl`; the pipeline in Step 2
- [Gregg's flame graphs page](https://www.brendangregg.com/flamegraphs.html)
  — index of variants (off-CPU, memory, differential) and per-profiler
  instructions
- This topic's bench lane 3 — the always-on instrumentation cost that
  sampling profilers complement
