# Topic 17 notes — SIMD & hardware-conscious processing

## Baseline (provided rungs, release, Apple Silicon, measured 2026-07-10)

N = 4M f32 (16 MB per input — out of L2, into memory), 20 reps.

### dot product

| rung | GB/s | ms | vs naive |
|---|---|---|---|
| naive (1 chain) | 10.89 | 3.081 | 1.0× |
| unrolled-8 (autovec) | 42.12 | 0.797 | 3.9× |
| wide f32x4 ×4 acc | stub | | |
| neon vfmaq ×4 acc | stub | | |

3.9× from ZERO intrinsics — just writing 8 accumulators so LLVM may
reassociate. The serial FMA chain (3cy latency) was the bottleneck,
exactly as the ports×latency model predicts.

### filter compact (GB/s of input)

| sel% | branchy | branchless | neon-compress |
|---|---|---|---|
| 1 | 10.95 | 12.70 | stub |
| 25 | 2.13 | 13.32 | stub |
| 50 | **1.19** | 12.73 | stub |
| 75 | 2.11 | 12.38 | stub |
| 99 | 6.65 | 11.98 | stub |

The SIGMOD '15 curve, live: branchy collapses 9× at 50% selectivity
(mispredict wall — ~symmetric around 50%, recovering toward the
ends); branchless is FLAT within ±5% across the whole sweep because
its control flow is data-independent. Note branchy never actually
wins here even at 1%/99% — the paper's crossover needs even more
extreme selectivities (<1%) on this core.

### 4-bit unpack

| rung | GB/s (output) |
|---|---|
| scalar | 10.20 |
| neon shift/mask | stub |

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| dot_wide (4 × f32x4 = 16 partials) vs unrolled-8 — faster, same, slower? | | |
| dot_neon vs dot_wide — does hand-written vfmaq beat `wide`'s codegen? | | |
| is dot at N=4M memory-bound? (32 MB / 3.081 ms ≈ 10 GB/s naive; what's the memory ceiling — rerun at N=64K in-cache to see compute limit) | | |
| neon-compress at 50% sel vs branchless 12.73 GB/s | | |
| neon-compress at 99% — does always-store-16B beat branchless? | | |
| unpack4_neon vs scalar 10.20 GB/s (is the scalar already autovectorized? check `cargo asm` or the 10 GB/s hint) | | |
| max f32 dot error vs naive at N=4M, 4-acc f32 (SimSIMD upcasts to f64 — do we need to?) | | |

## Implementation log

- [ ] dot.rs: dot_wide + dot_neon — 4 tests green
- [ ] filter.rs: count_neon + compact_neon (LUT built, all 16 masks
      pass) — 3 tests green
- [ ] unpack.rs: unpack4_neon — 2 tests green
- [ ] full simd_bench table recorded above (replace "stub")
- [ ] rerun dot at N=64K (in-cache) — record compute-bound ceiling

Surprises / dead ends:

## Questions from the reading guides

### simdjson (reading-simdjson.md)

1. Why 64-byte blocks over 16-byte NEON width:
2. Compress LUT size/indexing, why 8 lanes max per shuffle:
3. Amdahl on branchy stage 2 (fraction of bytes reaching it):
4. Why nibble tables suffice for UTF-8 validation:
5. RESP stage-1 mask sketch for M7:

### polars-compute (reading-polars-compute.md)

1. Why STRIPE=16 (wider than one NEON vector) helps (ports×latency):
2. Pairwise error bound vs left-to-right at n=10⁸:
3. Why compress trailing garbage is safe (who truncates):
4. Bit-level validity-bitmap filtering:
5. Why compress defeats portable SIMD abstractions:

### hashbrown + memchr (reading-hashbrown-simd.md)

1. Why 16B NEON Group lost to u64 SWAR in hashbrown:
2. vshrn nibble-mask >>2 vs byte-mask >>3 — the newtype guard:
3. Why SWAR false positives are acceptable in match_tag:
4. 7-bit tags + sign-bit encoding — cost/benefit for M2:
5. Compile vs init vs call-time dispatch for our engine:

### SimSIMD (reading-simsimd.md)

1. Fraction of 32 flops/cy peak that f64-accumulating dot reaches:
2. Is single-chain sqeuclidean_f32 actually a bottleneck:
3. Newton-Raphson rounds → bits (8 → 16 → 32 → 48):
4. 4-target stateful API → M14 scoring loop signature:
5. M17 fn-pointer dispatch table sketch:

### SIGMOD '15 (reading-sigmod15-vectorization.md)

1. Why branchless loses at 99% in their data (store traffic):
2. Vertical probing vs join output order:
3. Why bloom filters vectorize better than probes:
4. Their permutation-LUT compress vs simdjson's:
5. Rank filter/probe/partition/bloom by engine-level win (Amdahl):

### FastLanes (reading-fastlanes.md)

1. Why 1024-value blocks:
2. Interleaved planes vs prefetcher:
3. Chain-length ratio for transposed delta on NEON:
4. Random access cost we traded away:
5. Predicted vs measured unpack4 GB/s reconciliation:

### Mojo (reading-mojo-simd.md)

1. w=1-as-oracle testing trick (adopt in dot.rs? already done —
   scalar IS the oracle):
2. Remainder-loop bugs vs simdjson's padding:
3. What's actually blocking std::simd stabilization:
4. When topic 13 (tiling) out-levers topic 17 (lanes):
5. One-sentence NEON-width-4 justification for M17:

## Cross-topic threads

- The filter selectivity curve is topic 11's selection-vector
  decision at lane level; branchless = the vectorized engine's
  default for exactly this flatness.
- ports × latency ⇒ accumulator count is topic 13's MLP argument
  (memory-level parallelism) transposed to FLOPs.
- unpack4 is topic 12's decoder; FastLanes says the LAYOUT, not the
  intrinsics, decides whether decode rides at RAM bandwidth.
- hashbrown group matching is topic 2's probe loop; SwissTable = 
  SIMD as a hash-table DESIGN constraint, not an optimization.
- SimSIMD's 4-target streaming states are M14's candidate scoring
  loop, pre-shaped.

## M17 log (capstone)

- [ ] NEON kernels behind M11 vectorized runtime: filter compact,
      hash probe, dot/l2 (M14)
- [ ] scalar fallbacks + `is_aarch64_feature_detected!` dispatch
      (once, at init — fn-pointer table)
- [ ] engine-level speedup per kernel; record where Amdahl eats the 4×
- [ ] SIMD-ize topic 12 bit-unpack; re-run compression-IS-performance

## Done when

- All stub tests green; simd_bench tables above fully populated;
  prediction table reconciled; reading-guide questions answered.
