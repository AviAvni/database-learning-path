# Compaction is four axes, not two strategies

"Leveled vs tiered" is a false binary: a compaction policy is an independent
choice on four design axes — trigger, layout, granularity, movement — and
every system you've read in this topic sits somewhere in that grid. Before
the paper, this chapter builds the four axes one at a time, with the systems
you already know as coordinates. This is the taxonomy chapter; read it LAST
of the four papers, because it organizes the other three.

## The problem in one sentence

After three papers and two codebases you have seen at least five distinct
compaction behaviors described with only two words ("leveled", "tiered") —
and decisions that dominate p99.9 write latency, like whether a compaction
moves one 64 MB file or one 25 GB level, don't even have a name in that
vocabulary.

## The concepts, step by step

### Step 1 — a compaction policy is a bundle of independent decisions

Every compaction, in every engine, answers the same four questions: *when*
do we compact, *what shape* must the levels have, *how much* data does one
job move, and *does the data actually get rewritten*. "Leveled" and
"tiered" are bundles — prepackaged answers to all four at once — which hides
the fact that the answers are independently choosable:

```
 a compaction policy = choice on each axis:

 1. TRIGGER      when?    level saturation / #runs / staleness / space amp
 2. DATA LAYOUT  what shape?   leveling / tiering / 1-leveling / L-leveling / hybrid
 3. GRANULARITY  how much at once?   whole level / one file (RocksDB) / few files
 4. DATA MOVEMENT who moves?   full merge / trivial move (relink non-overlapping)
```

Unbundling matters because the axes control *different* observable costs —
Steps 2–5 take them one at a time.

### Step 2 — axis 1, the trigger: what event starts a compaction

The trigger is the predicate that fires a compaction job. The familiar one
is **saturation** — a level exceeds its size target (RocksDB's score ≥ 1.0
from the compaction chapter). But nothing forces that choice: you can
trigger on **run count** (tiered's "K runs accumulated"), on **staleness**
(data untouched for N hours gets merged — useful for TTL workloads), or
directly on **space amplification** (compact when dir size / live data
exceeds 1.5). The paper's empirical finding worth flagging now: at low write
rates, the *trigger* choice moves point-lookup latency more than the layout
does — because the trigger decides how long overlapping runs linger before
being merged away.

### Step 3 — axis 2, the layout: what shape the levels are kept in

The layout is the invariant about runs per level — the axis Dostoevsky
already turned into a dial. **Leveling** = 1 run per level; **tiering** = up
to T runs per level; **1-leveling / L-leveling** = tiering with a leveled
first or last level (L-leveling is exactly lazy leveling); hybrids mix per
level. This is the only axis the "leveled vs tiered" vocabulary ever named,
and Steps 4–5 are the two whole axes it left silent.

### Step 4 — axis 3, granularity: how much data one job moves

Granularity is the size of a single compaction job's input. **Whole-level**
compaction (your mini-LSM, the 1996 paper's rolling merge in spirit) merges
an entire level at once: with a 2.5 GB L2 that is one job occupying the disk
for tens of seconds, and every one of those seconds is back-pressure —
foreground writes stall in bursts. **File-granularity** compaction
(RocksDB: pick *one* ~64 MB file plus its next-level overlaps) does the
same total work as many small jobs spread over time. Same throughput,
radically different p99.9: granularity is a **tail-latency knob, not a
throughput knob** — the paper's cleanest finding, and topic 2's
rehash-spike lesson (one big pause vs many amortized ones) at LSM scale.

### Step 5 — axis 4, data movement: merge bytes or relink them

Data movement asks whether a compaction physically rewrites data or merely
re-labels it. A **full merge** reads, merges, and rewrites every input byte
— the default assumption. A **trivial move** applies when an input file
does not overlap anything at the destination level: the engine just edits
metadata to say the file now belongs to the next level — **zero bytes of
IO** (lsm-tree's `Choice::Move`, RocksDB's trivial move). For sequential or
bulk-load ingest this axis dominates everything: a sorted snapshot can
cascade to the bottom level entirely by relinking, write amp 1.0 — which is
your M4 graph-snapshot question answered by an axis the two-word vocabulary
couldn't even express.

### Step 6 — using the grid: place every system, then trust only same-engine data

With four axes, every policy you've met becomes a coordinate — your
mini-LSM is (trigger = level size, layout = leveled or tiered, granularity
= whole level, movement = full merge, + trivial move if you stole
`Choice::Move`); RocksDB leveled is (saturation, leveling, one-file,
merge+trivial-move). The paper's second contribution is methodological: it
implements the *whole grid inside one engine* so comparisons vary one axis
at a time — the Fair Benchmarking lesson (topic 0) applied, because
cross-engine comparisons confound all four axes with everything else. And
the headline empirical result across the grid: **no policy wins everywhere**
— the RUM conjecture, empirically, again.

## How to read the paper (with the concepts in hand)

1. §3 — the taxonomy (Steps 1–5 in the authors' terms). Make the table for:
   your mini-LSM, lsm-tree crate, RocksDB leveled, RocksDB universal, FIFO.
2. §4 — the benchmark methodology (Step 6): they implement the design space
   inside one engine to compare fairly — same engine, one variable.
3. **§5 findings** — the ones worth keeping:
   - file-granularity compaction (RocksDB style) smooths write stalls vs
     whole-level (spikes) — granularity is a *tail latency* knob, not a
     throughput knob (Step 4);
   - trigger choice dominates point-lookup latency more than layout at low
     write rates (Step 2);
   - no policy wins everywhere (the RUM conjecture, empirically, again).
4. Skim the workload sensitivity plots — note which finding you'll test.

## Questions to answer in notes.md

1. Your write_amp experiment compacts whole levels. Predict, then measure if
   time allows: what does per-insert p99.9 look like vs a per-file granularity
   variant? (This is topic 2's rehash-spike lesson at LSM scale.)
2. Which axis does Dostoevsky's lazy leveling move on? (Layout only — trigger/
   granularity/movement orthogonal.) Which does Monkey move on? (None — it's
   a filter-memory axis the taxonomy doesn't cover; where would you add it?)
3. For M4's graph-snapshot SSTs: bulk-loading a snapshot is one giant sorted
   run. Which axis choices make ingest cheap? (Trivial move into the bottom
   level — no merge at all.)

## Done when

Your notes contain the 5-system × 4-axis table and one prediction you could
test with the mini-LSM.

## References

**Papers**
- Sarkar, Papon, Staratzis, Athanassoulis — "Constructing and Analyzing
  the LSM Compaction Design Space" (VLDB 2021) — §3 taxonomy and §5
  findings are the keepers; §4's one-engine methodology is the Fair
  Benchmarking lesson applied
