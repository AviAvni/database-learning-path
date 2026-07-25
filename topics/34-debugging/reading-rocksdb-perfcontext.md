# RocksDB PerfContext: observability as a dial, not a switch

RocksDB answers "why was THIS Get slow?" without a profiler attached:
every query thread carries a thread-local struct of ~100 counters
(PerfContext), every hot scope is a macro that may or may not read the
clock depending on a runtime dial (PerfLevel), and the cross-query
picture lives in a second, global tier (Statistics) whose latency
histograms squeeze all of u64 into 109 fixed buckets. The repo is
cloned at `~/repos/rocksdb`; this is a code-read, ~1.5h, aimed at
capstone M34 — a per-query perf context for the Rust engine whose
level-0 cost is provably zero. Build the ideas in order first; the
anchor table maps each to an exact file:line.

## The problem in one sentence

Instrument every block read, key comparison, and mutex wait on a
storage engine's hot path so a slow query can be explained
counter-by-counter, while a production box that turns it all off pays
literally nothing — not one branch more than an uninstrumented build.

## The concepts, step by step

### Step 1 — two tiers: per-query thread-local vs per-DB global

RocksDB keeps two parallel metric systems, one per question:

```
 question              structure           scope        sync cost
 ─────────────────────────────────────────────────────────────────
 "why was THIS         PerfContext         one thread,  none — plain
  query slow?"         (~100 uint64_t)     one query    uint64_t adds
 "how is the DB        StatisticsImpl      all threads, per-core
  doing overall?"      (tickers + histos)  all queries  aggregation
```

PerfContext (`include/rocksdb/perf_context.h:305`, counters declared
in `PerfContextBase` at `:73`) is the per-query tier: block cache
hits, block read counts/bytes/nanos, `internal_key_skipped_count`
(iterator tombstone skips), memtable/WAL write times, mutex wait
nanos — `Reset()` before your query, run, read after. `StatisticsImpl`
(`monitoring/statistics_impl.h:42`) is the global tier: `recordTick`
(`monitoring/statistics.cc:549`) folds increments from every thread
into per-DB tickers and histograms. Why it matters: a p99 spike in the
global histogram tells you *that* something is slow; PerfContext on
one repro tells you *where its nanoseconds went*. M34 needs both
tiers, and conflating them (a mutex-protected global per-query map)
buys the worst of each.

### Step 2 — thread-local access: no synchronization, by construction

`PerfContext* get_perf_context()`
(`include/rocksdb/perf_context.h:342`) returns a pointer to a
`thread_local PerfContext` (`monitoring/perf_context_imp.h:19`):

```
 thread A ──▶ its own PerfContext ──▶ plain `metric += value`
 thread B ──▶ its own PerfContext     no atomics, no locks, no sharing
```

A counter bump is a non-atomic add to memory only this thread ever
touches; reading the results is the same thread inspecting its own
struct after the query returns. Why it matters: the per-query tier is
cheap *because* it never aggregates — aggregation is deferred to the
moment you copy the struct out. The Rust equivalent is a context owned
by the query task (or a `thread_local!` slot), not an `Arc<Mutex<_>>`.

### Step 3 — PerfLevel: the ladder, ordered by clock reads

Instrumentation cost is not uniform — a counter bump is ~1ns, a
timestamp is a `clock_gettime`/`rdtsc` pair per scope, CPU-time clocks
are syscalls — so the enable knob is a *ladder*
(`include/rocksdb/perf_level.h:27`), each rung admitting a more
expensive class of measurement:

```
 kDisable                          = 1   nothing
 kEnableCount                      = 2   counters only — no clock reads
 kEnableWait                       = 3   + time blocked inside RocksDB
 kEnableTimeExceptForMutex         = 4   + wall-clock timers everywhere
 kEnableTimeAndCPUTimeExceptForMutex = 5 + CPU-time clocks
 kEnableTime                       = 6   + mutex/condvar wait timing
```

Mutex timing is last: it adds clock reads *inside critical sections*,
lengthening the very contention it measures. Why it matters: this is
the tax topic 34's bench lane 3 measures (bare loop → +clock pair →
+histogram.record → +slowlog check) — PerfLevel exists so production
sits at level 2 (counts are nearly free) and a repro session dials to
4+ without a rebuild. M34's dial should copy the ordering principle:
rungs sorted by cost class, not by feature.

### Step 4 — macros: the zero position is provably zero

Instrumentation is written as macros, not calls, so absence can be
compiled (`monitoring/perf_context_imp.h`):

```c
#if defined(NPERF_CONTEXT)
#define PERF_TIMER_GUARD(metric)                 // nothing. at all.
#else
#define PERF_TIMER_GUARD(metric)                                  \
  PerfStepTimer perf_step_timer_##metric(&(perf_context.metric)); \
  perf_step_timer_##metric.Start();
#endif
```

With `NPERF_CONTEXT` every macro (`:27` onward) expands to empty —
zero cost is a preprocessor fact, not a benchmark claim. Without it,
cost is gated at runtime: `PERF_COUNTER_ADD` (`:81`) is
`if (perf_level >= kEnableCount) perf_context.metric += value;`, and
the timer guards (`:45` wall-clock, `:88` per-LSM-level variant) check
the level before ever touching a clock. Why it matters: M34's bar is
"level 0 provably free" — in Rust, a `#[cfg(feature = "perf")]` macro
for the compile-out, plus a runtime branch on a thread-local level for
everything the feature flag keeps.

### Step 5 — PerfStepTimer: RAII so a scope can't leak its time

`PerfStepTimer` (`monitoring/perf_step_timer.h:13`) is what the guard
macros drop on the stack: the constructor evaluates
`perf_level >= enable_level` once and caches it; `Start()` reads the
clock only if enabled; the destructor (`:29`) calls `Stop()`, adding
`now - start_` to the target `uint64_t*` — optionally `RecordTick`ing
the same duration into Statistics: two tiers, one clock read.

```
 { PERF_TIMER_GUARD(block_read_time);      ┐ ctor: level check
   ... read the block ...                  │ Start(): clock #1
 }  ◀── destructor fires on ANY exit ──────┘ Stop(): clock #2, +=
```

`Measure()` (`:37`) restarts the interval mid-scope for multi-step
timing. Why it matters: early return, `?`, exception — the scope
closes, the time lands; a branch that skips the stop call is
unrepresentable. In Rust this is `Drop`, the natural shape for M34's
parse/plan/execute/serialize step timers.

### Step 6 — 109 buckets for all of u64: O(1) percentiles, exact merges

The global tier's latency histograms never store samples.
`HistogramBucketMapper` (`monitoring/histogram.h:21`) precomputes a
fixed set of geometrically growing bucket limits covering all of u64;
`HistogramStat` (`:46`) is min/max/count/sum/sum-of-squares plus
`std::atomic_uint_fast64_t buckets_[109]` (`:84`) — a fixed array, the
comment explains, so the struct needs no dynamic allocation and can
live in thread-local storage. `Add` is: map value to bucket index, one
relaxed atomic increment. `Percentile(p)` walks 109 buckets and
interpolates inside the one holding the p-th sample.

```
 value ──IndexForValue──▶ bucket i ──fetch_add──▶ buckets_[i]
                                                     │
 p50/p99/p999 ◀── walk 109 counters, interpolate ────┘
```

Because bucket boundaries are fixed at compile time, merging two
histograms (`HistogramImpl::Merge`, class at `:110`) is 109 exact
additions — per-thread histograms combine losslessly; the price is
relative error bounded by the geometric growth ratio. Why it matters:
this is precisely topic 34's `LogHistogram` stub — HdrHistogram
generalizes it with a `sub_bits` error knob — and M34 should record
step latencies this way, never as samples.

## Where each step lives in the code

All paths relative to `~/repos/rocksdb`.

| Step | Anchor | What to see |
|---|---|---|
| 1 | `include/rocksdb/perf_context.h:73` | `PerfContextBase` — read 30 counters' comments; note count/byte/time naming |
| 1 | `include/rocksdb/perf_context.h:305` | `PerfContext` — `Reset()`, `ToString(exclude_zero_counters)`, per-level map |
| 1 | `monitoring/statistics_impl.h:42` + `monitoring/statistics.cc:549` | `StatisticsImpl` / `recordTick` — the global tier |
| 2 | `include/rocksdb/perf_context.h:342` | `get_perf_context()` — thread-local contract in the comment above it |
| 3 | `include/rocksdb/perf_level.h:27` | the `PerfLevel` ladder + naming-convention comments per rung |
| 4 | `monitoring/perf_context_imp.h:27,:45,:81,:88` | `NPERF_CONTEXT` empty expansions; `PERF_TIMER_GUARD`; level-gated counter adds |
| 5 | `monitoring/perf_step_timer.h:13,:29` | `PerfStepTimer` ctor's cached level check; destructor → `Stop()` |
| 6 | `monitoring/histogram.h:21,:46,:84,:110` | `HistogramBucketMapper`; `HistogramStat` with `buckets_[109]`; `HistogramImpl` |

Read order: perf_level.h (whole file, 60 lines) → perf_context_imp.h
(whole file — the macros ARE the design) → perf_step_timer.h (whole
file) → skim PerfContextBase's counters → histogram.h. Then grep
`PERF_TIMER_GUARD(get_from_memtable_time)` for a live call site.

## Questions to answer in notes.md

1. M34: which ~10 counters should a graph query engine's PerfContext
   carry first? Draft the struct — think GraphBLAS matrix ops (mxm/mxv
   count, flops, mask applications), index seeks, property fetches
   (count + bytes), per-operator intermediate-record counts, result
   serialization bytes — marking level-2 counts vs level-4 timers.
2. `PERF_COUNTER_ADD` branches on `perf_level >= kEnableCount` for a
   ~1ns add — is the branch cheaper than the unconditional add? Use
   bench lane 3's numbers to argue when the level check itself is the
   tax, and what a Rust `const LEVEL: u8` generic would change.
3. `PerfStepTimer::Stop()` can feed both tiers (the `uint64_t*` metric
   and `RecordTick` on a `Statistics*`) from one clock-read pair.
   Where in M34's step timers would you replicate this, and where must
   the tiers stay decoupled?
4. Mutex wait timing is the top rung (`kEnableTime = 6`) because
   clocks inside critical sections perturb contention. Which M34
   measurement has the same observer effect (hint: per-operator timers
   inside a tight matrix loop), and what is your rung 6?
5. RocksDB's 109 buckets vs HdrHistogram's `sub_bits`: compute the
   worst-case relative error of a geometric ladder spanning u64 in 109
   buckets, and pick the bucket count your `LogHistogram` needs to
   keep p99 error under 5% for latencies between 1us and 10s.

## Done when

- [ ] You can trace `PERF_TIMER_GUARD(block_read_time)` end to end —
      macro expansion → ctor's cached level check → `Start()` clock
      read → destructor `Stop()` adding into the thread-local struct —
      and name the two settings under which no clock is ever read
      (NPERF_CONTEXT; level < 4).
- [ ] Given a counter name (`*_count`, `*_time`, `*_cpu_*`, mutex/wait
      metrics), you can say from perf_level.h's naming conventions at
      which rung it becomes live.
- [ ] You can explain why `HistogramStat` merges are exact while its
      percentiles are approximate, in one sentence each.
- [ ] M34's design doc names its two tiers, its ladder rungs in cost
      order, and its provably-zero mechanism.

## References

**Code**
- [RocksDB](https://github.com/facebook/rocksdb) — cloned at
  `~/repos/rocksdb`; the anchors above are the read
- [HdrHistogram](https://github.com/HdrHistogram/HdrHistogram) — Step
  6 generalized, with an explicit precision knob (`sub_bits`)

**Docs**
- [RocksDB wiki: Perf Context and IO Stats Context](https://github.com/facebook/rocksdb/wiki/Perf-Context-and-IO-Stats-Context)
  — the usage pattern: SetPerfLevel → Reset → query → ToString

**Related**
- This topic's `experiments/` — the `LogHistogram` stub (Step 6 with
  `sub_bits`) and bench lane 3 (the observability tax Step 3 controls)
- Capstone M34 — per-query perf context for the Rust engine: step
  timers + operator counters behind a PerfLevel-style dial; level 0
  provably free, full level < 5% overhead on the bench suite
