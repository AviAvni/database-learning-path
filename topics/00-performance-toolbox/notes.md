# Topic 0 — Notes

Numbers from this machine (Apple Silicon, macOS). Record *why*, not just what.

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
