# Topic 0 — Notes

Numbers from this machine (Apple Silicon, macOS). Record *why*, not just what.

## Talk: Gil Tene — "How NOT to Measure Latency" (watched ✅)

Core thesis: almost everyone measures latency wrong, and the errors all point the
same direction — making systems look *better* than they are.

1. **Latency is a distribution, never a number.** Means and standard deviations are
   meaningless for latency (it's multi-modal, heavy-tailed, not normal). Always report
   percentiles — and the *whole* curve, not just p50/p99.
2. **The tail is what users experience.** A page load touching ~100 resources hits the
   p99 almost every time (1 − 0.99¹⁰⁰ ≈ 63%). "p99.9 doesn't matter" is backwards:
   the more requests per user interaction, the deeper the percentile that dominates UX.
3. **Coordinated omission** — the big one. If the load generator waits for a response
   before sending the next request, a server stall silences the generator exactly when
   things go bad: the bad results are *omitted* from the data, coordinated with the
   stall. A 100s test with one 50s pause can report "p99 < 1ms" while reality is
   ~25s average during half the test. The error is ~1000x+, not a rounding issue.
   - Fix: measure against the **intended send schedule** (constant-rate arrival), not
     the actual send time. If a request should have gone out at t=5s but went out at
     t=55s, its latency includes those 50s of wait.
   - This is why HdrHistogram has correction modes and why wrk2/redis-benchmark grew
     constant-throughput modes.
4. **Service time ≠ response time.** Service time = how long the server took once it
   started; response time = what the client experiences, including queueing. Load
   generators that back off measure service time and *call* it response time.
   Throughput-vs-latency plots made this way are fiction beyond saturation.
5. **"Sustainable throughput" framing.** Don't ask "what's the max throughput?" — ask
   "what's the max throughput at which we still meet the latency requirements?"
   Test by stating requirements first (e.g. p99.9 < 20ms, max < 200ms), then finding
   the highest load that passes. A benchmark without a latency requirement is a
   throughput benchmark, and throughput alone is easy to game.
6. **Beware the hockey stick you can't see.** Plotted percentile curves always bend up
   hard somewhere ("the hockey stick"); tests that stop at p99 just hide where. Plot to
   the max recorded value — the max is a real event that happened, not an outlier to trim.
7. **Never average percentiles** across intervals/machines — p99s don't average. Merge
   the histograms (HdrHistogram), then read percentiles off the merged data.

**Rules for this repo's benchmarks (from the talk):**
- Capstone server benches (M7+) must use a constant-rate open-loop load generator with
  coordinated-omission correction (HdrHistogram), never closed-loop request→wait→request.
- Report p50/p90/p99/p99.9/max + full percentile plot; never mean latency.
- Criterion is fine for CPU microbenches (throughput of kernels), but *not* an oracle
  for request latency — different tool for a different question.

## Experiment 3 — branch_misprediction (done, first pass)

| Variant | Sorted | Shuffled |
|---------|--------|----------|
| branchy | 338 µs (3.1 Gelem/s) | 2.75 ms (0.38 Gelem/s) |
| branchless | 70 µs (15.0 Gelem/s) | 71 µs (14.7 Gelem/s) |

- **8.1x** sorted→shuffled gap for the branchy version — the classic misprediction penalty.
  1M elements, ~50% unpredictable taken rate ⇒ ~500K flushes; (2750−338)µs / 500K ≈
  ~4.8 ns ≈ 15 cycles per miss at ~3.2 GHz. Matches the §3 estimate.
- **Surprise:** with plain `sum += x` in the branch, LLVM if-converts + auto-vectorizes
  and the gap *vanishes* (both ~70µs). Had to put `black_box(x)` inside the taken path
  to keep a real branch. Lesson: on modern compilers the famous StackOverflow
  sorted-array effect only reproduces if vectorization is defeated — always check the asm.
- Branchless = data dependence instead of control dependence ⇒ NEON select, 4.8x faster
  than even the perfectly-predicted branchy loop.

## Experiment 1 — cache_ladder (todo)

## Experiment 2 — lookup_shootout (todo)

## Flamegraph (todo)

`samply record ./target/release/deps/lookup_shootout-* --bench --profile-time 10`

## M0 workload generator

- Seeded `StdRng` + `rand_distr::Zipf` (s=0.99, YCSB default), skewed toward low ids
  (oldest nodes = hubs, matching preferential attachment).
- Generation throughput: **~11 M ops/s** (9.1 ms / 100K ops) — fine for now; if engine
  benches ever exceed ~10 M ops/s, pre-generate op vectors outside the timed loop.
- Zipf sampling dominates cost (rejection sampling per draw). Alias-table or
  precomputed CDF is the known fix — note for later, not needed yet.
