# RocksDB PerfContext: observability as a dial, not a switch

RocksDB answers "why was THIS Get slow?" without a profiler attached:
every query thread carries a thread-local struct of ~100 counters
(PerfContext), every hot scope is a macro that may or may not read the
clock depending on a runtime dial (PerfLevel), and the cross-query
picture lives in a second, global tier (Statistics) whose latency
histograms squeeze all of u64 into 109 fixed buckets. The repo is
cloned at `~/repos/rocksdb`, pinned at `rocksdb@7c80a5a`; this is a
code-read, ~1.5h, aimed at capstone M34 — a per-query perf context for
the Rust engine whose level-0 cost is provably zero. Build the ideas in
order first; the anchor table maps each to an exact file:line, and
every snippet below is quoted from that pinned SHA with real gutters.

## The problem in one sentence

Instrument every block read, key comparison, and mutex wait on a
storage engine's hot path so a slow query can be explained
counter-by-counter, while a production box that turns it all off pays
literally nothing — not one branch more than an uninstrumented build.

## The concepts, step by step

### Step 1 — two tiers: per-query thread-local vs per-DB global

> **In:** the problem statement — "explain one query" *and* "watch the
> whole DB" are different questions.
> **Out:** the two structures (PerfContext, StatisticsImpl) that the
> rest of the chapter dissects; Steps 2–5 are the per-query tier, Step 6
> is the global one.

RocksDB keeps two parallel metric systems, one per question:

```
 question              structure           scope        sync cost
 ─────────────────────────────────────────────────────────────────
 "why was THIS         PerfContext         one thread,  none — plain
  query slow?"         (~100 uint64_t)     one query    uint64_t adds
 "how is the DB        StatisticsImpl      all threads, per-core
  doing overall?"      (tickers + histos)  all queries  aggregation
```

PerfContext (`include/rocksdb/perf_context.h:305`, counters declared in
`PerfContextBase` at `:73`) is the per-query tier: block cache hits,
block read counts/bytes/nanos, `internal_key_skipped_count` (the count
of internal keys skipped during iteration — previous-key entries and
*updates hidden by tombstones*, but explicitly **not** the tombstones
themselves; the comment at `:136` says "the tombstones are not included
in this counter", they land in `internal_delete_skipped_count` at
`:149`), memtable/WAL write times, mutex wait nanos — `Reset()` before
your query, run, read after. `StatisticsImpl`
(`monitoring/statistics_impl.h:42`) is the global tier: `recordTick`
(`monitoring/statistics.cc:549`) folds increments from every thread
into per-DB tickers and histograms. Why it matters: a p99 spike in the
global histogram tells you *that* something is slow; PerfContext on one
repro tells you *where its nanoseconds went*. M34 needs both tiers, and
conflating them (a mutex-protected global per-query map) buys the worst
of each.

### Step 2 — thread-local access: no synchronization, by construction

> **In:** the per-query tier named in Step 1 (PerfContext).
> **Out:** the reason it needs zero synchronization — each thread owns
> its struct — which Step 3 then makes *conditionally* cheap via the
> level dial.

`PerfContext* get_perf_context()`
(`include/rocksdb/perf_context.h:342`) returns a pointer to a
`thread_local PerfContext` (`monitoring/perf_context_imp.h:19`). A
**thread-local** is a variable with one independent instance per
thread, so no two threads ever touch the same bytes:

```
 thread A ──▶ its own PerfContext ──▶ plain `metric += value`
 thread B ──▶ its own PerfContext     no atomics, no locks, no sharing
```

A counter bump is a non-atomic add to memory only this thread ever
touches; reading the results is the same thread inspecting its own
struct after the query returns. (When RocksDB is built with
`NPERF_CONTEXT`, `get_perf_context()` instead returns one shared global
no-op object — see the contract comment at `:333`–`:341`.) Why it
matters: the per-query tier is cheap *because* it never aggregates —
aggregation is deferred to the moment you copy the struct out. The Rust
equivalent is a context owned by the query task (or a `thread_local!`
slot), not an `Arc<Mutex<_>>`.

### Step 3 — PerfLevel: the ladder, ordered by clock reads

> **In:** the thread-local counters from Step 2, which are cheap to
> bump but expensive to *time*.
> **Out:** the runtime `perf_level` dial whose rungs Step 4's macros and
> Step 5's timer branch on to decide whether to touch a clock at all.

Instrumentation cost is not uniform — a counter bump is ~1 ns, a
timestamp is a `clock_gettime`/`rdtsc` pair per scope, CPU-time clocks
are syscalls — so the enable knob is a *ladder*
(`include/rocksdb/perf_level.h:27`), each rung admitting a more
expensive class of measurement:

```
 kUninitialized                      = 0   (unset)
 kDisable                            = 1   nothing
 kEnableCount                        = 2   counters only — no clock reads
 kEnableWait                         = 3   + time blocked inside RocksDB
 kEnableTimeExceptForMutex           = 4   + wall-clock timers everywhere
 kEnableTimeAndCPUTimeExceptForMutex = 5   + CPU-time clocks
 kEnableTime                         = 6   + mutex/condvar wait timing
```

The rung *names* encode the naming convention `perf_level.h` documents:
a `*_count`/`*_byte` counter is live at `kEnableCount` (2); a
`*_[wait|delay]_*` metric needs `kEnableWait` (3); a plain `*_time`/
`*_nanos` needs `kEnableTimeExceptForMutex` (4); a `*_cpu_*_time` needs
`kEnableTimeAndCPUTimeExceptForMutex` (5); and a
`*_[mutex|condition]_*` metric needs the top rung `kEnableTime` (6).
Mutex timing is last deliberately: it adds clock reads *inside critical
sections*, lengthening the very contention it measures. Why it matters:
this is the tax topic 34's bench lane 3 measures (bare loop → +clock
pair → +histogram.record → +slowlog check) — PerfLevel exists so
production sits at level 2 (counts are nearly free) and a repro session
dials to 4+ without a rebuild. M34's dial should copy the ordering
principle: rungs sorted by cost class, not by feature.

### Step 4 — macros: the zero position is provably zero

> **In:** the `perf_level` dial from Step 3.
> **Out:** the two-layer gate — a compile-time `#if` for absence and a
> runtime `if (perf_level >= …)` for the cheap counters — that Step 5's
> timer completes for the expensive clock reads.

Instrumentation is written as macros, not calls, so absence can be
*compiled out* entirely:

```c
// monitoring/perf_context_imp.h:23
23  #if defined(NPERF_CONTEXT)
     // ... :25 every guard/counter macro expands to nothing ...
27  #define PERF_TIMER_GUARD(metric)
     // ...
34  #define PERF_COUNTER_ADD(metric, value)
35  #define PERF_COUNTER_BY_LEVEL_ADD(metric, value, level)
37  #else
     // real definitions follow:
45  #define PERF_TIMER_GUARD(metric)                                  \
46    PerfStepTimer perf_step_timer_##metric(&(perf_context.metric)); \
47    perf_step_timer_##metric.Start();
     // ... :80 the counter add is the runtime-gated one ...
80  #define PERF_COUNTER_ADD(metric, value)        \
81    if (perf_level >= PerfLevel::kEnableCount) { \
82      perf_context.metric += value;              \
83    }
```

Two layers of gating, and it is worth keeping them straight. First,
compile-time: with `NPERF_CONTEXT` defined, *every* macro (`:25`–`:35`)
expands to nothing — zero cost is a preprocessor fact, not a benchmark
claim. Second, runtime, for the builds that keep instrumentation:
`PERF_COUNTER_ADD` (`:80`) wraps its add in
`if (perf_level >= kEnableCount)`, and `PERF_COUNTER_BY_LEVEL_ADD`
(`:87`) is the per-LSM-level *counter* variant (a map keyed by level,
also gated at `>= kEnableCount`) — note it is a **counter**, not a
timer. The **timer** guard `PERF_TIMER_GUARD` (`:45`) does something
subtler: it unconditionally constructs a `PerfStepTimer` and calls
`Start()`, and the level check that decides whether a clock is read
lives *inside* that object (Step 5), not in this macro. Why it matters:
M34's bar is "level 0 provably free" — in Rust, a
`#[cfg(feature = "perf")]` macro for the compile-out, plus a runtime
branch on a thread-local level for everything the feature flag keeps.

### Step 5 — PerfStepTimer: RAII so a scope can't leak its time

> **In:** the `PERF_TIMER_GUARD` macro from Step 4, which drops a
> `PerfStepTimer` on the stack.
> **Out:** the RAII object whose constructor caches the level check and
> whose destructor guarantees the interval is recorded — the mechanism
> behind Step 4's claim that no clock is read below level 4.

`PerfStepTimer` (`monitoring/perf_step_timer.h:13`) is what the guard
macros drop on the stack. The constructor evaluates
`perf_level >= enable_level` exactly once and caches it; `Start()` reads
the clock only if that cache is set (or a `Statistics*` sink is
attached); the destructor calls `Stop()`, adding `now - start_` to the
target `uint64_t*`:

```c
// monitoring/perf_step_timer.h:15
15    explicit PerfStepTimer(
16        uint64_t* metric, SystemClock* clock = nullptr, bool use_cpu_time = false,
17        PerfLevel enable_level = PerfLevel::kEnableTimeExceptForMutex, ...)
19        : perf_counter_enabled_(perf_level >= enable_level),   // level check, cached once
     // ...
29    ~PerfStepTimer() { Stop(); }                               // fires on ANY scope exit
31    void Start() {
32      if (perf_counter_enabled_ || statistics_ != nullptr) {
33        start_ = time_now();                                   // clock read, only if enabled
34      }
35    }
```

The default `enable_level` is `kEnableTimeExceptForMutex` (4), so a
plain `PERF_TIMER_GUARD` reads *no clock* until the dial reaches 4 —
below that, `perf_counter_enabled_` is false and `Start()`/`Stop()` are
branches over nothing. `Stop()` optionally `RecordTick`s the same
duration into `Statistics`, so one clock-read pair can feed both tiers.
`Measure()` (`:37`) restarts the interval mid-scope for multi-step
timing.

```
 { PERF_TIMER_GUARD(block_read_time);      ┐ ctor: cache perf_level >= 4
   ... read the block ...                  │ Start(): clock #1 (iff enabled)
 }  ◀── destructor fires on ANY exit ──────┘ Stop(): clock #2, += into struct
```

Why it matters: early return, `?`, exception — the scope closes, the
time lands; a branch that skips the stop call is unrepresentable. In
Rust this is `Drop`, the natural shape for M34's
parse/plan/execute/serialize step timers.

### Step 6 — 109 buckets for all of u64: O(1) percentiles, exact merges

> **In:** the global tier (`StatisticsImpl`) named in Step 1, which must
> summarize latencies from all threads.
> **Out:** the fixed-bucket histogram that makes per-thread merges exact
> and percentiles O(1) — the structure M34's `LogHistogram` stub
> reimplements.

The global tier's latency histograms never store samples.
`HistogramBucketMapper` (`monitoring/histogram.h:21`) precomputes a
fixed set of geometrically growing bucket limits covering all of u64;
`HistogramStat` (`:46`) is min/max/count/sum/sum-of-squares plus
`std::atomic_uint_fast64_t buckets_[109]` (`:84`) — a fixed array, the
comment at `:76`–`:78` explains, so the struct needs no dynamic
allocation and can live in thread-local storage. `Add` maps the value
to a bucket index and does one relaxed atomic increment; `Percentile(p)`
walks the 109 buckets and interpolates inside the one holding the p-th
sample.

```
 value ──IndexForValue──▶ bucket i ──fetch_add──▶ buckets_[i]
                                                     │
 p50/p99/p999 ◀── walk 109 counters, interpolate ────┘
```

The bucket ratio is worth deriving, because it *is* the accuracy story
(and Question 5). To cover u64 (≈ `2^64`) in 109 geometric buckets
starting near 1, the ratio `r` satisfies `r^109 ≈ 2^64`, i.e.
`r ≈ 2^(64/109) ≈ 2^0.587 ≈ 1.5` — which is exactly the `1.5 × previous`
growth `histogram.cc:28` uses (rounded to nice values). A value lands in
some bucket `[L, ~1.5L)`, so representing it by the bucket limit is up
to `(1.5 − 1) = 50%` high at the low edge; using the geometric midpoint
(`≈ 1.22 L`) bounds the relative error near `±22%`, and the linear
interpolation `Percentile` does within the bucket brings the typical
error well below that. Because bucket boundaries are fixed at compile
time, merging two histograms (`HistogramImpl::Merge`, class at `:110`)
is 109 exact additions — per-thread histograms combine losslessly; the
price is that bounded relative error. Why it matters: this is precisely
topic 34's `LogHistogram` stub — HdrHistogram generalizes it with a
`sub_bits` knob that subdivides each power-of-two band into `2^sub_bits`
linear sub-buckets to drive the error down — and M34 should record step
latencies this way, never as samples.

## Where each step lives in the code

All paths relative to `~/repos/rocksdb` at `rocksdb@7c80a5a`.

| Step | Anchor | What to see |
|---|---|---|
| 1 | `include/rocksdb/perf_context.h:73` | `PerfContextBase` — read the counters' comments; note count/byte/time naming and `:136` (tombstones excluded from `internal_key_skipped_count`) |
| 1 | `include/rocksdb/perf_context.h:305` | `PerfContext` — `Reset()`, `ToString(exclude_zero_counters)`, per-level map |
| 1 | `monitoring/statistics_impl.h:42` + `monitoring/statistics.cc:549` | `StatisticsImpl` / `recordTick` — the global tier, per-core aggregation |
| 2 | `include/rocksdb/perf_context.h:342` | `get_perf_context()` — thread-local contract in the comment above it (`:333`–`:341`) |
| 3 | `include/rocksdb/perf_level.h:27` | the `PerfLevel` ladder + naming-convention comments per rung |
| 4 | `monitoring/perf_context_imp.h:25,:45,:80,:87` | `NPERF_CONTEXT` empty expansions; `PERF_TIMER_GUARD` (timer); `PERF_COUNTER_ADD` / `PERF_COUNTER_BY_LEVEL_ADD` (level-gated counters) |
| 5 | `monitoring/perf_step_timer.h:15,:19,:29` | `PerfStepTimer` ctor's cached `perf_level >= enable_level`; destructor → `Stop()` |
| 6 | `monitoring/histogram.h:21,:46,:84,:110` | `HistogramBucketMapper`; `HistogramStat` with `buckets_[109]`; `HistogramImpl`; ratio at `histogram.cc:28` |

Read order: perf_level.h (whole file, ~60 lines) → perf_context_imp.h
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
   ~1 ns add — is the branch cheaper than the unconditional add? Use
   bench lane 3's numbers to argue when the level check itself is the
   tax, and what a Rust `const LEVEL: u8` generic would change.
3. `PerfStepTimer::Stop()` can feed both tiers (the `uint64_t*` metric
   and `RecordTick` on a `Statistics*`) from one clock-read pair. Where
   in M34's step timers would you replicate this, and where must the
   tiers stay decoupled?
4. Mutex wait timing is the top rung (`kEnableTime = 6`) because clocks
   inside critical sections perturb contention. Which M34 measurement
   has the same observer effect (hint: per-operator timers inside a
   tight matrix loop), and what is your rung 6?
5. RocksDB's 109 buckets vs HdrHistogram's `sub_bits`: starting from
   `r ≈ 2^(64/109) ≈ 1.5`, compute the worst-case relative error of the
   geometric ladder, and pick the bucket count (or `sub_bits`) your
   `LogHistogram` needs to keep p99 error under 5% for latencies between
   1 µs and 10 s.

## Done when

Answer each before unfolding it.

- [ ] You can trace `PERF_TIMER_GUARD(block_read_time)` end to end — macro expansion → ctor's cached level check → `Start()` clock read → destructor `Stop()` adding into the thread-local struct — and name the two settings under which no clock is ever read (NPERF_CONTEXT; level < 4).

  <details><summary>Answer</summary>

  `PERF_TIMER_GUARD(block_read_time)` expands (`perf_context_imp.h:45`)
  to a stack `PerfStepTimer perf_step_timer_block_read_time(&perf_context.block_read_time)`
  followed by `.Start()`. The constructor (`perf_step_timer.h:15`)
  caches `perf_counter_enabled_ = (perf_level >= enable_level)`, whose
  default `enable_level` is `kEnableTimeExceptForMutex` (4). `Start()`
  (`:31`) reads the clock only if `perf_counter_enabled_ ||
  statistics_ != nullptr`. At scope exit the destructor (`:29`) runs
  `Stop()`, which adds `now - start_` into `perf_context.block_read_time`
  (the thread-local struct). No clock is read when the build defines
  `NPERF_CONTEXT` (the macro is empty) or when `perf_level < 4` (the
  cached flag is false and no `Statistics` sink is attached).

  </details>

- [ ] Given a counter name (`*_count`, `*_time`, `*_cpu_*`, mutex/wait metrics), you can say from perf_level.h's naming conventions at which rung it becomes live.

  <details><summary>Answer</summary>

  From `perf_level.h`'s per-rung naming comments: `*_count`/`*_byte`
  counters are live at `kEnableCount` (2); `*_wait_*` / `*_delay_*`
  metrics need `kEnableWait` (3); plain `*_time` / `*_nanos` timers need
  `kEnableTimeExceptForMutex` (4); `*_cpu_*_time` / `*_cpu_*_nanos` need
  `kEnableTimeAndCPUTimeExceptForMutex` (5); and
  `*_mutex_*` / `*_condition_*` wait metrics need the top rung
  `kEnableTime` (6). The ladder is ordered by cost class — count, wait,
  wall-time, CPU-time, mutex-time — so each rung admits a strictly more
  expensive measurement.

  </details>

- [ ] You can explain why `HistogramStat` merges are exact while its percentiles are approximate, in one sentence each.

  <details><summary>Answer</summary>

  Merges are exact because every histogram shares the same 109
  compile-time-fixed bucket boundaries, so combining two is just 109
  element-wise `uint64` additions with no re-bucketing and no lost
  counts (`HistogramImpl::Merge`). Percentiles are approximate because a
  bucket only records *how many* samples fell in `[L, ~1.5L)`, not their
  values, so `Percentile` must interpolate within the bucket — leaving a
  bounded relative error set by the ~1.5× geometric ratio (≈20% at
  midpoint before interpolation).

  </details>

- [ ] M34's design doc names its two tiers, its ladder rungs in cost order, and its provably-zero mechanism.

  <details><summary>Answer</summary>

  Two tiers: a per-query, thread-local **PerfContext** analogue (plain
  non-atomic counters, owned by the query task) and a per-DB global
  **Statistics** analogue (per-core tickers + fixed-bucket histograms).
  Ladder in cost order, mirroring PerfLevel: counts (≈free) → in-engine
  wait time → wall-clock timers → CPU-time → mutex/critical-section
  timing (last, because it perturbs contention). Provably-zero
  mechanism: a compile-time gate (`#[cfg(feature = "perf")]` in Rust,
  RocksDB's `NPERF_CONTEXT`) that expands all instrumentation to nothing,
  plus a runtime `perf_level` branch (or a `const LEVEL` generic) for the
  builds that keep it — so level 0 costs not one branch more than an
  uninstrumented build.

  </details>

## References

**Code** (pinned at `rocksdb@7c80a5a`)
- [RocksDB](https://github.com/facebook/rocksdb) — cloned at
  `~/repos/rocksdb`; the anchors above are the read.
- [HdrHistogram](https://github.com/HdrHistogram/HdrHistogram) — Step 6
  generalized, with an explicit precision knob (`sub_bits`).

**Docs**
- [RocksDB wiki: Perf Context and IO Stats Context](https://github.com/facebook/rocksdb/wiki/Perf-Context-and-IO-Stats-Context)
  — the usage pattern: SetPerfLevel → Reset → query → ToString.

**Related**
- This topic's `experiments/` — the `LogHistogram` stub (Step 6 with
  `sub_bits`) and bench lane 3 (the observability tax Step 3 controls).
- Capstone M34 — per-query perf context for the Rust engine: step
  timers + operator counters behind a PerfLevel-style dial; level 0
  provably free, full level < 5% overhead on the bench suite.
