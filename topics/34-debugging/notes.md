# Topic 34 notes — debugging & production diagnosis

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| lane 1: closed-loop p99 under 100 ms stalls | happy-path 1 µs | **1.0 µs** (even p99.99 = 1.0 µs; only max sees the stall) |
| lane 1: open-loop p99 | ~90 ms (stall minus one interval decade) | **90.0 ms**; p99.9 = 99.0 ms, p99.99 = 99.9 ms |
| lane 1: size of the lie at p99 | 4–5 orders of magnitude | **90,000×** (1 µs vs 90 ms) |
| lane 2: histogram record vs sort-everything ns/op | record ~2-5 ns, sort ~50+ | (stub — measure after implementing histogram.rs) |
| lane 2: p99.9 histogram error vs exact | ≤ 3.1% (sub_bits=5) | (stub) |
| lane 3: clock-pair tax on a ~2 ns op | dominates — 10-20 ns/op | (stub) |
| lane 3: slowlog check tax above clock+histogram | unmeasurable | (stub) |

The lane-1 shape is exact and worth memorizing: each 100 ms stall
queues ~10K arrivals (100 ms / 10 µs); 9 stalls in 1M ops → ~9% of all
requests carry queueing delay, decaying roughly linearly from 100 ms
as the backlog drains. The worst 1% (10K samples) are therefore the
ones delayed ≥ ~90 ms — that's why open-loop p99 lands at exactly
90.0 ms and climbs toward the full 100 ms by max. Closed-loop sees the
stalls in exactly 9 samples out of 1M, so even its p99.99 (rank
999,900) is clean. The bench is a virtual clock, so these are provable
numbers, not noise.

## Guide-question checklist

- [ ] reading-rr.md Q1–Q5
- [ ] reading-flamegraphs.md Q1–Q5
- [ ] reading-redis-doctors.md Q1–Q5
- [ ] reading-rocksdb-perfcontext.md Q1–Q5

## Cross-topic threads (worked)

- Topic 16 ↔ 34: simulation testing and rr are the same move —
  capture nondeterminism behind an interface (simulated network /
  syscall boundary) — applied before vs after ship.
- Topic 11's harness is closed-loop; every M11 p99 needs the lane-1
  asterisk until re-measured open-loop.
- The stall model IS topic 8's GC pause and topic 5's fork
  checkpoint; coordinated omission explains why they were invisible
  in the topic-level benches but will page someone in prod.

## Capstone M34 log

- Surface: `GRAPH.SLOWLOG` (redis semantics — ≥ threshold, negative
  disables, ring, monotonic ids surviving reset — exactly the stub's
  contract tests) + per-query perf context with
  parse/plan/execute/serialize step timers behind a PerfLevel dial.
- Budget: level 0 provably free via lane 3's harness; full
  instrumentation < 5% on the M11 bench suite; histograms merged
  across threads exactly (lane 2's merge contract).
- The before shot: reproduce lane 1 on the real Rust engine with an
  induced stall (e.g. artificial 100 ms pause every N queries),
  closed vs open loop.

## Infra notes

- No new clones: redis, rocksdb, FalkorDB already under ~/repos.
- Anchors verified by grep this session: redis slowlog.c:103/:28,
  latency.c:63/:182, latency.h:17/:50/:63, config.c:3271,
  object.c:1421, debug.c:2643/:2673/:2115; rocksdb
  perf_context.h:305/:73/:342, perf_level.h:27,
  perf_context_imp.h:27/:45/:81/:88, perf_step_timer.h:13/:29,
  histogram.h:21/:46/:84/:110, statistics_impl.h:42,
  statistics.cc:549; FalkorDB src/slow_log/slow_log.{c,h}.
- rr facts verified from the extended technical report (arXiv:1705.05937 — the
  ~21-page edition, not the shorter ATC'17 conference paper), pp. 1–8:
  RCB counter, seccomp-bpf in-process interception, RR page, < 2×
  slowdown, one-thread-at-a-time limitation.
- Crate: 3 provided tests green (workload.rs), 6 stub tests fix
  contracts for histogram.rs (3) and slowlog.rs (3). Lanes 2-3 print
  `[stub …]` banners via catch_unwind until implemented.

## Done when

- [ ] All 9 tests pass; lanes 2-3 print real numbers.
- [ ] Utilization argument written (README exercise 2) for both stall
      configs.
- [ ] Midpoint-vs-upper-bound bias question answered (exercise 3).
- [ ] rr session completed on a topic-16 test (exercise 5), with the
      reverse-continue-to-writer transcript in these notes.
- [ ] All 20 guide questions answered in writing.
- [ ] M34 perf-context sketch (exercise 6) upgraded to a design note
      with lane-3 numbers for each PerfLevel.
