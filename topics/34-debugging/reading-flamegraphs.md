# The Flame Graph: folding a million stacks into one picture

Brendan Gregg's "The Flame Graph" is the canonical write-up of the
visualization you will stare at for the rest of your performance
career. In topic 34's diagnosis triad — wrong answers / too slow /
crashed — this is the "too slow" leg: profiler samples are perishable
evidence, and the flame graph aggregates them into one durable
artifact. This chapter builds the one structural idea (merging
identical stacks) and the reading discipline (width, not height)
before you open the article — with the database question in view: for a
query engine under load, the off-CPU variant is usually where the story
is.

This is an *article*, not code, so every claim below is anchored to a
**section** of Gregg's paper rather than a `file:line`. The version
cited is **ACM Queue, Vol 14, Issue 2 (April 2016)** — republished as
**Communications of the ACM, Vol 59, No 6 (June 2016)**, DOI
`10.1145/2927299.2927301`. Numbers quoted are the ones the article
itself prints, in the section named.

## The problem in one sentence

A CPU profiler at 99 Hz across many cores emits tens of thousands of
stack traces per minute — far too many to read as text — and the flame
graph compresses them into one interactive picture by merging identical
stacks and drawing width proportional to sample count.

## The concepts, step by step

### Step 1 — the raw material: sampled stacks, not traced calls

> **In:** nothing yet — this step names the raw evidence every later
> step consumes.
> **Out:** a wall of **stack traces**, one per timer interrupt, that
> Step 2 will merge.

A **sampling profiler** interrupts the CPU at a fixed rate and records
what is running; it does *not* instrument every call. A **stack trace**
(call stack) is the list of nested function calls active at that
instant — the leaf (on-CPU function) at the top, its caller beneath,
down to the thread entry at the root. A single frame is one function in
that list.

Two properties make sampling the right raw material. First, it is
**statistical**: a function's *share of samples* estimates its *share
of CPU time*, so you never need to catch every call, only enough
samples to be representative. Second, it is **zero code change** — you
attach to the live database when the pager fires and detach when done.
That is the deliberate complement to this topic's bench lane 3, which
prices the observability tax of *always-on* instrumentation: sampling
costs nothing until you attach.

Gregg's §"CPU Profiling" gives the canonical rate: stack traces are
sampled at **99 times per second** — "not 100, to avoid lock-step
sampling", i.e. to avoid beating against any 100 Hz periodic activity.
Over 30 seconds on a 16-CPU box that is `16 × 99 × 30 = 47,520`
samples; "as text, this would be hundreds of thousands of lines." The
output is unreadable precisely because it is complete:

```
  worker-3             worker-3             worker-3
  main                 main                 main
  run_query            run_query            run_query
  executor::pull       executor::pull       parser::parse
  expand_op            filter_op
  GrB_mxm              eval_predicate
  ─ sample 184,001 ─   ─ sample 184,002 ─   ─ sample 184,003 ─ ...×10⁴
```

Why it matters: any tool that dumps raw samples buries the signal. The
next step is the one idea that makes 47,520 stacks legible.

### Step 2 — the merge: identical stacks become one column

> **In:** the wall of stack traces from Step 1.
> **Out:** a **folded profile** — one line per *unique* stack with a
> count — which Step 3 renders as geometry.

The whole trick of the flame graph is one aggregation step: two samples
with byte-identical stacks are the same evidence, so merge them and
keep a count. In Gregg's §"Instructions", the pipeline's middle stage,
`stackcollapse-perf.pl`, folds `perf script` output into the **folded
format** — each stack on one line, functions separated by semicolons,
then a space and a count:

```
main;run_query;executor::pull;expand_op;GrB_mxm 41203
main;run_query;executor::pull;filter_op;eval_predicate 8931
main;run_query;parser::parse 1204
```

The article's own case study is the scale argument: the MySQL profile
in §"The Problem" was `591,622` lines of DTrace output holding `27,053`
unique stacks — collapsing merges the samples down to those unique
paths, and the flame graph makes the whole thing readable on one
screen. The renderer, `flamegraph.pl`, turns the folded file into an
SVG: shared stack *prefixes* become shared boxes, unique suffixes
branch off above the last common frame.

The full three-step pipeline (§"Instructions"), worth memorizing
because you will type it:

```
# perf record -F 99 -a -g -- sleep 60
# perf script | ./stackcollapse-perf.pl | ./flamegraph.pl > out.svg
```

Why it matters: the folded file is a greppable, diffable *text*
artifact — the SVG is only a view of it. Step 5's regression diff and
your own `grep`/`awk` post-processing both operate on the folded file,
not the picture.

### Step 3 — reading the geometry: width is everything, height is nothing

> **In:** the folded profile from Step 2, rendered as an SVG.
> **Out:** the reading discipline — which boxes to trust and which to
> ignore — that the rest of the chapter applies.

Gregg's §"Flame Graphs Explained" fixes the semantics precisely. The
**y-axis** is stack depth, root at the bottom and leaf at the top; each
box is one function frame, and the box beneath a box is its caller. The
**x-axis** "does *not* show the passage of time" — after merging,
sibling frames are sorted **alphabetically** by function name, "which
maximizes box merging" (identical adjacent frames fuse), so
left-to-right order carries no temporal meaning at all.

Box **width** is the load-bearing dimension. It is the fraction of
samples in which that frame appeared, counting every sample where the
frame was anywhere in the stack — its own time *plus* everything it
called. That "plus everything above it" is why width is called
**inclusive**: a caller is always at least as wide as its widest child.
As a formula, for frame `f`:

```
width(f) = (samples whose stack contains f) / (total samples)
```

Worked on the article's own numbers. Total samples for a 16-CPU, 30 s,
99 Hz profile is `16 × 99 × 30 = 47,520`. A frame that shows up in
`11,880` of those stacks renders at `11,880 / 47,520 = 25%` of the full
width. And the MySQL red herring from §"The Problem": its `status`
command appeared in `5,530` of `348,427` samples, so
`5,530 / 348,427 = 1.59% ≈ 1.6%` — visibly a sliver, which is exactly
why the eye skips it and lands on the real culprit (`join`).

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

The reading discipline (§"Flame Graph Interpretation"): hunt for **wide
plateaus** — flat-topped boxes along the top edge where samples
*terminate*, i.e. functions actually on-CPU — not tall spikes. A
**plateau** is a wide box whose top edge is exposed; that width is
on-CPU time. Depth is just call-path length: a 60-frame tower two
pixels wide costs nothing, while a squat 40%-wide `memcpy` plateau is
your bug.

Why it matters: every beginner reads height (deep = important) and gets
it backwards. The top edge, left to right, is the histogram of on-CPU
leaf functions; that is the whole profile.

### Step 4 — on-CPU vs off-CPU: two graphs, two questions

> **In:** the reading discipline from Step 3, applied to *two different*
> sample sources.
> **Out:** the split — which of two flame graphs answers "where is CPU
> spent" and which answers "where is wall-clock spent waiting."

The **on-CPU flame graph** (everything so far) answers "where is CPU
time spent." Its dual, the **off-CPU flame graph** (§"Other Targets" →
Off-CPU), samples the stacks of threads that are *blocked* — not
running — with the box width proportional to time spent blocked rather
than to sample count. Gregg lists the reasons a thread goes off-CPU:
"waiting on I/O, locks, timers, a turn on-CPU, and waiting for paging
or swapping." Because those stacks were captured *when the thread was
descheduled*, the width is blocked time.

For databases this is often the interesting graph. A write-heavy engine
can show an innocently flat on-CPU profile while every worker spends
most of wall-clock time parked in `futex_wait` under a latch or
`fdatasync` behind the WAL. Those stacks never run, so the on-CPU graph
*cannot* show them — the two graphs partition wall-clock time:

```
  wall time of one worker thread
  ├── on-CPU ──┤├────────── off-CPU ──────────┤
   GrB_mxm,      futex_wait (latch),  fdatasync (WAL),
   eval_pred     read() (page miss)
```

Why it matters: "the database is slow but the CPUs are idle" is exactly
where an on-CPU profile shrugs. Off-CPU names the lock and the fsync —
and off-CPU is only possible *because* Step 3's rule "width can measure
anything, not just sample count" is baked into the format
(§"Flame Graphs Explained": "widths can reflect measures other than
sample counts").

### Step 5 — differential (red/blue) flame graphs for regressions

> **In:** *two* folded profiles from Step 2 — a "before" (A) and an
> "after" (B).
> **Out:** one flame graph colored by the per-frame delta, so the
> regression is a visible patch.

Given two folded profiles — before/after a commit, or a healthy node
and a sick one — a **differential flame graph** (§"Differential Flame
Graphs") draws the **B** profile's shape and colors each frame by its
change in sample count from A to B: "red colors indicate functions that
increased, and blue colors indicate those that decreased." The
regression is literally the red patch.

The article is honest about the failure mode, and you should carry it:
because the drawing uses B's shape, "some code paths present in the A
profile may be missing entirely in the B profile, and so will be
missing from the final visualization" — a path that vanished shows up
as *nothing*, not as blue. Gregg's `flamegraphdiff` fixes this by
drawing three graphs (A, B, and the delta). Capture a folded file per
release and "what got slower?" becomes a one-command diff — the reason
Netflix generates these nightly.

Why it matters: this is the payoff of Step 2's insistence that the
folded file *is* the data. You cannot diff two SVGs; you diff two text
files and render the result.

### Step 6 — pitfalls: broken stacks make lying graphs

> **In:** any flame graph from Steps 3–5.
> **Out:** the two ways the picture can be confidently wrong, and how to
> spot them before you trust a plateau.

The graph is only as good as the stack walks, and §"Challenges" names
the two ways they break. First, **incomplete stack traces**. A **frame
pointer** is the register (`%rbp` on x86-64) that chains each stack
frame to its caller; when "the software compiler reuses the frame
pointer register as a compiler optimization" — the historical default
under `-fomit-frame-pointer` — the frame walker cannot follow the chain
and you get truncated one- or two-frame stacks: a wide lawn of "grass"
at the bottom, or towers floating on `[unknown]`. The fix is "a
different compiled binary (e.g., using gcc's `-fno-omit-frame-pointer`)
or a different stack-walking technique" (DWARF or LBR unwinding). At
Netflix the Java fix was the JVM's `-XX:+PreserveFramePointer`.

Second, **missing function names**. Here the stack is complete but many
frames "are represented as hexadecimal addresses" — the JIT/interpreted
case, "which may not create a standard symbol table for profilers." The
fix is a supplemental symbol file (Linux `perf_events` reads one; the
Java fix is `perf-map-agent`).

Rule of thumb: before believing any plateau, check that stacks reach a
plausible root (`main`, a thread entry). A broken-stack graph
misattributes time with total confidence — it is the flame-graph
equivalent of a benchmark that measured the wrong thing.

Why it matters: a lying flame graph looks exactly like a truthful one.
The only defense is checking the roots before you read the widths.

## How to read the article (with the concepts in hand)

ACM Queue 14(2) / CACM 59(6), ~10 pages of prose and figures; budget
~1h. Read it section by section, mapping each to a step above:

- **§"CPU Profiling"** (10 min) — sampling, the 99 Hz convention, the
  wall-of-text problem (Step 1).
- **§"The Problem"** (10 min) — the MySQL 40%-CPU mystery on Joyent that
  raw profiler text couldn't crack; the numbers (591,622 lines, 27,053
  stacks, `join` was the culprit) motivate Step 2's merge.
- **§"Flame Graphs Explained" + "Flame Graph Interpretation"** (15 min)
  — box/width/ordering semantics; make sure "x-axis is not time"
  survives the figures (Step 3).
- **§"Other Targets" → Off-CPU** (15 min) — read it twice with your
  WAL-fsync hat on (Step 4).
- **§"Differential Flame Graphs"** (10 min) — red/blue, and the missing-
  path caveat (Step 5).
- **§"Challenges"** (10 min) — broken stacks and missing symbols
  (Step 6), straight from the source.
- Then do, don't just read (~15 min): `perf record -g` against FalkorDB
  under a benchmark, render the SVG, find the widest plateau.

## Questions to answer in notes.md

1. Why is "the x-axis is not time" the load-bearing design decision?
   Name one question a time-ordered stack chart (Gregg's "flame chart")
   answers that a flame graph cannot, and the far more common converse.
2. Predict FalkorDB's flame graphs under a read-heavy workload: which
   plateaus dominate on-CPU (GraphBLAS matrix ops? filter eval? result
   serialization? allocator?), and what appears off-CPU that the on-CPU
   graph hides entirely? Then measure and score yourself.
3. A frame occupying 30% width at mid-height but with almost no samples
   terminating in it — what does that tell you, and where do you look
   next?
4. Bench lane 3 prices always-on instrumentation; sampling costs
   ~nothing until attached. What can built-in counters tell you that a
   flame graph cannot (per-query attribution, tail latencies, rare
   events between samples)?
5. Your nightly profile of a Rust binary shows a wide floor of
   two-frame "grass" stacks. Give the diagnosis steps and the two fixes
   from Step 6; which fits a production FalkorDB build, and why?

## Done when

Answer each before unfolding it.

- [ ] You can explain why box width is the fraction of samples containing the frame, and why alphabetical (not time) ordering is what enables the merge.

  <details><summary>Answer</summary>

  Width is inclusive sample share: `width(f) = (samples whose stack
  contains f) / (total samples)` (§"Flame Graphs Explained"), so a
  caller is always at least as wide as its widest child, and the
  visible top edge is the histogram of on-CPU leaf functions. On the
  article's own 16-CPU/30 s/99 Hz profile that denominator is
  `16 × 99 × 30 = 47,520`; a frame in 11,880 of them draws at 25%.

  The merge only works because sibling frames are sorted
  *alphabetically* by name, not by time. Alphabetical order guarantees
  that two identical frames which belong side by side end up
  horizontally adjacent, so they fuse into one wide box; a time-ordered
  x-axis would scatter the same function across the width and destroy
  the merge (that is exactly why Gregg's time-ordered "flame chart"
  merges poorly, especially across threads).

  </details>

- [ ] You have generated one on-CPU flame graph of FalkorDB under load via perf → stackcollapse → flamegraph.pl and named its widest plateau.

  <details><summary>Answer</summary>

  The three-step pipeline is `perf record -F 99 -a -g -- sleep 60`, then
  `perf script | ./stackcollapse-perf.pl | ./flamegraph.pl > out.svg`
  (§"Instructions"). `perf record` samples stacks at 99 Hz across all
  CPUs; `stackcollapse-perf.pl` folds them to one line per unique stack
  with a count; `flamegraph.pl` renders that folded file to SVG.

  The widest plateau is whatever flat-topped box owns the largest slice
  of the top edge — for a read-heavy FalkorDB run, likely a GraphBLAS
  kernel (`GrB_mxm`/`GrB_mxv`) or filter evaluation. Its width, read off
  the SVG's mouse-over as "N samples, P percent", is that function's
  share of on-CPU time.

  </details>

- [ ] You can state which class of database slowness is invisible on-CPU and needs the off-CPU variant.

  <details><summary>Answer</summary>

  Anything the thread does *while descheduled*: waiting on I/O, on
  locks, on timers, for a turn on-CPU, or for paging/swapping
  (§"Other Targets" → Off-CPU). For a database that is lock contention
  (`futex_wait` under a latch) and durable-write stalls (`fdatasync`
  behind the WAL). These stacks never execute, so an on-CPU profile —
  which only samples running threads — cannot contain them. The off-CPU
  flame graph captures the stack at the moment the thread blocks and
  weights each box by blocked time, so the two graphs together partition
  wall-clock time.

  </details>

- [ ] You can spot broken stack walks (grass, floating towers) before trusting any plateau.

  <details><summary>Answer</summary>

  Two symptoms from §"Challenges". Truncated stacks — a wide floor of
  one- or two-frame "grass", or towers floating on `[unknown]` — mean
  the frame pointer was omitted (`-fomit-frame-pointer`) so the walker
  lost the chain; the fix is `-fno-omit-frame-pointer` (or DWARF/LBR
  unwinding, or `-XX:+PreserveFramePointer` for the JVM). Frames shown
  as bare hex addresses mean missing symbols, typically from JIT'd code;
  the fix is a supplemental symbol map (`perf-map-agent` for Java).

  The check is mechanical: before believing a plateau, confirm its stack
  reaches a plausible root (`main` or a thread entry). A broken-stack
  graph misattributes time with total confidence, so a plateau on top of
  grass is worthless until the walk is fixed.

  </details>

## References

**Article**
- Brendan Gregg — "The Flame Graph" (ACM Queue 14(2), April 2016;
  republished in Communications of the ACM 59(6), June 2016; DOI
  `10.1145/2927299.2927301`) —
  [queue.acm.org/detail.cfm?id=2927301](https://queue.acm.org/detail.cfm?id=2927301).
  Sections cited: CPU Profiling; The Problem; Flame Graphs Explained;
  Instructions; Flame Graph Interpretation; Other Targets (Off-CPU);
  Differential Flame Graphs; Challenges.

**Code & further material**
- [FlameGraph repo](https://github.com/brendangregg/FlameGraph) —
  `stackcollapse-*.pl` and `flamegraph.pl`; the pipeline in Step 2.
  (Not in this repo's pin table; treat script names as stable
  interface, not pinned line numbers.)
- [Gregg's flame graphs page](https://www.brendangregg.com/flamegraphs.html)
  — index of variants (off-CPU, memory, differential) and per-profiler
  instructions.
- This topic's bench lane 3 — the always-on instrumentation cost that
  sampling profilers complement.
