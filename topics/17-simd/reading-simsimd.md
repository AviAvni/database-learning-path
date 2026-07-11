# Reading guide — SimSIMD distance kernels

Clone: [`~/repos/SimSIMD`](https://github.com/ashvardanian/SimSIMD) — note the headers live under
`include/numkong/` (the project's internal rename). This is M14's
vector-distance layer done by someone who read the CPU manuals: every
NEON file opens with a port/latency table, and every kernel's
accumulator count follows from it.

## Anchor map

| anchor | what it is |
|---|---|
| numkong/spatial/neon.h:10-20 | THE table: per-instruction latency/ports on A76 vs Apple M-series |
| numkong/spatial/neon.h:123-140 | `nk_sqeuclidean_f32_neon` — f64 accumulation, 2 f32/iter |
| numkong/dot/neon.h:126-146 | `nk_dot_f32_neon` — FCVTL upcast, TWO independent FMA chains |
| numkong/dot/neon.h:37-45 | stateful streaming API: FOUR `nk_dot_f32x2_state_neon_t`s |
| numkong/dot/neon.h:~150 | the FCMLA comment: measured 39.7 vs 17.1 GiB/s — why they said no |
| numkong/spatial/neon.h:~100 | rsqrt by `vrsqrteq` + 3 Newton-Raphson rounds (no FSQRT) |
| include/numkong/*/ | one file per ISA per kernel family: neon, sve, haswell, skylake... |

## 1. The table is the design doc (spatial/neon.h:10-20)

```
 Intrinsic     Instruction   A76        Apple M5
 vfmaq_f32     FMLA          4cy @ 2p   3cy @ 4p
 vaddq_f32     FADD          2cy @ 2p   2cy @ 4p
 vsqrtq_f32    FSQRT         12cy @ 1p  9cy @ 1p
 vrsqrteq_f32  FRSQRTE       2cy @ 2p   3cy @ 1p
```

Read it as: on M-series, FMA needs latency(3) × ports(4) = 12
in-flight independent FMAs to saturate; on A76 only 8. And FSQRT is
a 1-port 9-cycle disaster — hence `vrsqrteq` + Newton-Raphson
(3 rounds ≈ f64 precision) instead of `vsqrtq`. Question: the README
said "≥12 chains" but nk_dot_f32_neon uses only 2 — where do the
other 10 come from in practice (see §3)?

## 2. Precision is why the kernels look "slow" (dot/neon.h:126)

```c
float64x2_t sum_low  = vdupq_n_f64(0);   // chain 1
float64x2_t sum_high = vdupq_n_f64(0);   // chain 2
for (; i + 4 <= n; i += 4) {
    a_f32x4 = vld1q_f32(a+i);  b_f32x4 = vld1q_f32(b+i);
    // FCVTL / FCVTL2: upcast each half to f64x2
    sum_low  = vfmaq_f64(sum_low,  a_low_f64,  b_low_f64);
    sum_high = vfmaq_f64(sum_high, a_high_f64, b_high_f64);
}
```

f32 inputs, f64 accumulators — half the lane width, deliberately.
Contrast polars: pairwise recursion (restructure the ADDITION ORDER)
vs SimSIMD: wider accumulator type (restructure the PRECISION).
Question: for M14's l2 distance over 1536-dim embeddings, which
error-control strategy is cheaper on M-series, and does recall@10
even care?

## 3. The 4-state streaming API (dot/neon.h:37-45)

The header's doc-comment shows the intended use: ONE query against
FOUR targets, four `nk_dot_f32x2_state_neon_t`s updated per
iteration:

```
 for idx:                      chains in flight:
   q = load(query+idx)           state1 += q·t1   ┐
   t1..t4 = load(4 targets)      state2 += q·t2   │ 4 FMA chains,
                                 state3 += q·t3   │ shared q load
                                 state4 += q·t4   ┘
 finalize(4 states) → one f32x4 of results
```

The ILP comes from BATCHING CANDIDATES, not unrolling one pair —
the query load is amortized 4×, and 4 states × dual-issue ≈ the 12
chains the machine wants. This is exactly M14's HNSW inner loop
shape (score one query against a neighbor list). Question: why is
this better than 4 accumulators over a single pair for the
short-vector case (n=128 dims: how many iterations does each scheme
get to overlap)?

## 4. The FCMLA lesson (dot/neon.h ~:150)

ARMv8.3 has a complex-multiply instruction (FCMLA). They benchmarked
it: 17.1 GiB/s vs 39.7 GiB/s for plain deinterleave (`vld2`) + 4
independent FMAs on M4. The fancy instruction LOST 2.3× because it
serializes work that 4 plain FMAs spread over 4 ports. The meta-
lesson for M17: newer/specialized instruction ≠ faster; ports ×
latency decides. Question: what's the NEON analogue in our filter
kernel (is `vqtbl1q` compress always better than branchless stores)?

## 5. Dispatch: one file per ISA, chosen at init

Directory layout is the dispatch table: `dot/neon.h`, `dot/sve.h`,
`dot/haswell.h`, `dot/skylake.h`... At init, capability detection
fills function pointers once; call sites pay an indirect call, not
a feature test. Middle binding time between hashbrown (compile) and
polars (per-call detect). Question: an indirect call can't inline —
when does THAT cost exceed the runtime-check cost it saves (think
n=8 dims vs n=4096)?

## Questions for notes.md

1. From the table: peak f32 FMA throughput on M-series = 4 ports ×
   4 lanes × 2 flops = 32 flops/cy. What fraction does
   nk_dot_f32_neon reach, given f64 accumulation halves lanes?
2. sqeuclidean_f32 uses ONE f64x2 chain (spatial/neon.h:123) —
   sloppy, or is L2-distance latency-bound elsewhere? Predict, then
   check with your dot.rs bench.
3. Newton-Raphson: why 3 rounds for f64 (~48 bits) — how many bits
   does each round double from FRSQRTE's ~8-bit estimate?
4. The stateful API returns `float32x4_t` of 4 results — how does
   this shape M14's candidate-scoring loop signature?
5. For M17 dispatch: sketch the fn-pointer table for
   {dot, l2sq, filter} × {neon, scalar} and where
   `is_aarch64_feature_detected!` runs exactly once.
