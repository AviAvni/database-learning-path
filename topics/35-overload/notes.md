# Topic 35 notes — overload control & resource governance

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin overload_bench`:

```
== lane 1: metastable failure — one trigger, two loads ==
   capacity 300 QPS, timeout 1 s, 1 retry, 10 s outage at t=30 s
           ── load 280 QPS ──   ── load 140 QPS ──
   t(s)    offered  goodput      offered  goodput
      0        281      280          141      140
     20        280      280          140      140
     29        280      280          140      140
     35        560        0          280        0
     45        560        0          280        0
     60        560        0          280        0
     90        560        0          280        0
    120        560        0          280        0
    160        560        0          140      251
    199        560        0          140      140
   280 QPS: goodput never recovers — the outage ended at t=40 s
   140 QPS heals at t=161 s

== lane 2: retry budget on the 280 QPS scenario ==
   headroom = 300 − 280 = 20 QPS; budgets straddle it:
[stub — implement the todo!()s to unlock lane 2]

== lane 3: DAGOR-lite under sustained 2x overload ==
   600 QPS offered, 300 QPS capacity, 4 priorities, no retries
[stub — implement the todo!()s to unlock lane 3]
```

Lane 1 is a seeded virtual-clock simulation, so every figure — 280 QPS never
recovers; 140 QPS heals at t=161 s with a partial window of 251 at t=160 — is
identical on every run.

The lane-1 mechanics, worth memorizing: the 10 s outage queues
10 s × 280 = 2,800 requests ≈ 9.3 s of work. Every request arriving
during or after the outage waits > 1 s, times out, and fires a retry
1 s after its own arrival — so from t≈31 s the offered load is
280 + 280 = 560 QPS against 300 QPS of capacity. The queue grows
260 requests per second *forever*; removing the trigger changes
nothing because the storm is now self-sustaining. At 140 QPS the storm
is 280 < 300: the backlog (1,400 + retries) drains at the 20 QPS
surplus, which is why healing takes ~2 minutes for a 10 s outage —
recovery time is backlog / headroom, and headroom is thin by design.

## Predictions (fill BEFORE implementing the stubs)

| question | predicted | measured |
|---|---|---|
| lane 1: 280 QPS after 10 s outage | | **goodput 0 at t=199 s, offered locked at 560 QPS**; outage ended t=40 s |
| lane 1: 140 QPS after same outage | | **heals at t=161 s**; partial window (251) at t=160 |
| lane 1: hidden capacity, 1 retry | | consistent: 280 dies, 140 heals (bisect in exercise 2) |
| lane 2: budget 15 vs 25 QPS | | |
| lane 3: goodput at 2× overload, no control | | |
| lane 3: goodput with DAGOR-lite | | |

<details>
<summary>Author's prediction</summary>

| question | author's prediction |
|---|---|
| lane 1: 280 QPS after 10 s outage | goodput 0 forever (storm 560 > 300) |
| lane 1: 140 QPS after same outage | heals (storm 280 < 300) |
| lane 1: hidden capacity, 1 retry | capacity/2 = 150 QPS |
| lane 2: budget 15 vs 25 QPS | 15 heals (fits 20 QPS headroom), 25 never |
| lane 3: goodput at 2× overload, no control | → 0 (FIFO queue grows, all miss 1 s timeout) |
| lane 3: goodput with DAGOR-lite | ≈ capacity, prio 0-1 near 100% |

</details>

## Reference numbers for the exercise lanes

No reference run of the stub lanes is recorded for this topic; the stub tests
are the specification.

## Guide-question checklist

- [ ] reading-metastable.md Q1–Q5
- [ ] reading-dagor.md Q1–Q5
- [ ] reading-redis-backpressure.md Q1–Q5
- [ ] reading-cockroach-admission.md Q1–Q5

## Cross-topic threads (worked)

- Topic 34 ↔ 35: same virtual-clock open-loop simulator; topic 34
  proved closed-loop clients can't *see* a stall, this topic shows the
  stall seeding a failure that outlives it. The metastable paper cites
  Tene on coordinated omission for exactly this reason.
- Topic 4 ↔ 35: RocksDB write stalls = per-store backpressure;
  cockroach io_load_listener = the same L0 file/sub-level signals
  driving node-wide admission tokens.
- Topics 5/8 ↔ 35: fork checkpoint and GC pauses are *triggers*; their
  cost isn't the pause, it's the storm the pause seeds at loads above
  hidden capacity.

## Capstone M35 log

- Surface: per-query priority + DAGOR-style cursor on executor queuing
  time (1 s / 2,000-query windows, 20 ms threshold as the starting
  point); fast `-BUSY`-style reject with retry-after hint; plan-time
  memory gate (redis DENYOOM per query).
- Targets: ≥ 80% of saturated throughput at 2× overload; metastable
  scenario reproduced on the real engine (induced stall + open-loop
  client + client-side retry), then fixed by the admission layer.
- Order of work: queuing-time measurement first (it's also M34's perf
  context arrival timestamp), then reject path, then priorities.

## Infra notes

- No new clones: redis and cockroach already under ~/repos.
- Redis anchors (`e87dda2a4`): evict.c:36 (EVPOOL_SIZE 16), :42
  (EVICTION_OVERUSE_DIVISOR), :139 (evictionPoolPopulate), :395
  (getMaxmemoryState), :436 (overMaxmemoryAfterAlloc), :543
  (performEvictions), :813 (bounded overshoot when only blessed keys
  remain); server.c:4785 (is_denyoom_command), :4880
  (out_of_memory = performEvictions() == EVICT_FAIL), :4893
  (rejectCommand oomerr), :2278 (-BUSY), :882
  (isInsideYieldingLongCommand), :5245 (pauseActions during shutdown);
  networking.c:5487/:5551 (output buffer limits), :4794
  (unpauseActions); config.c:3629 (maxmemory-samples default 5);
  script.c:150 (busy_reply_threshold).
- Cockroach anchors verified: admission.go:1 (package doc), :54
  (grantKind comment), :178 (requester), :198 (granter);
  work_queue.go:303 (WorkQueue), :813 (Admit), :1196
  (AdmittedWorkDone); kv_slot_adjuster.go:16 (threshold: runnable
  goroutines per CPU), :29, :46 (CPULoad), :99 (decrease), :103
  (increase at half threshold); io_load_listener.go:69
  (L0FileCountOverloadThreshold), :77 (L0SubLevelCountOverloadThreshold);
  admissionpb/admissionpb.go:23 (WorkPriority int8 ladder).
- Papers verified from PDFs: metastable failures (HotOS'21,
  sigops hotos21-s11-bronson.pdf, all 7 pp.) — Fig 2's 300 QPS /
  280 QPS / 1 s timeout / 1 retry / stable <150 / retries <20 QPS
  numbers, cache 10× amplification, link-imbalance case, CoDel and
  Tene citations; DAGOR (SoCC'18, arXiv:1806.04075, pp. 1–11) —
  20 ms threshold, 1 s / 2000-request window, α=5% β=1%, 128 user
  levels, DAGOR_q 750 vs DAGOR_r 630 QPS, service M saturation ~750
  QPS on 3 servers, subsequent-overload 25% example.
- Crate: 3 provided tests green (sim.rs — vulnerable-no-trigger,
  collapse-above-hidden-capacity with exact offered=1600 work
  amplification, heal-below-hidden-capacity). 6 stub tests fix
  contracts for tokenbucket.rs (3) and admission.rs (3). Lanes 2-3
  print `[stub …]` banners via catch_unwind until implemented.

## Done when

- [ ] All 9 tests pass; lanes 2-3 print real numbers.
- [ ] Hidden-capacity bisection done for retries = 1, 2, 3
      (exercise 2) and recorded here.
- [ ] Trigger-intensity curve (exercise 3) sketched: max survivable
      outage vs load.
- [ ] Healing-time-vs-budget table (exercise 4) with the
      backlog/(headroom − budget) explanation.
- [ ] Composition experiment (exercise 5): DAGOR + retry budget
      together vs each alone, numbers here.
- [ ] All 20 guide questions answered in writing.
- [ ] M35 sketch (exercise 6) upgraded to a design note with the
      fast-reject cost measured.
