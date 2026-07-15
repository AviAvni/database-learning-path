# SimSIMD: the port/latency table is the design doc

This is M14's vector-distance layer done by someone who read the CPU
manuals: every NEON file opens with a per-instruction port/latency
table, and every kernel's accumulator count follows from it. Before
the headers, this chapter builds the microarchitecture vocabulary —
ports, latency, dependency chains — then walks each design decision
as a consequence of the table. The through-line: ports × latency
decides everything, and fancy instructions lose to plain FMAs that
spread across ports. (Note: the headers live under
`include/numkong/`, the project's internal rename.)

## The problem in one sentence

A dot-product loop with one accumulator runs at 1/12th of an Apple
M-series core's floating-point throughput — and SimSIMD's own
benchmark shows the "obvious" specialized instruction (FCMLA) losing
2.3× to plain FMAs (17.1 vs 39.7 GiB/s) — so every kernel here is
shaped by two numbers from the CPU manual, not by instruction
counts.

## The concepts, step by step

### Step 1 — ports and latency: the machine's real currency

A modern core doesn't execute one instruction at a time — it has
multiple **execution ports** (independent hardware units; M-series
has 4 that can each start a vector floating-point op every cycle),
and each instruction has a **latency** (cycles until its result is
usable — ~3 for an FMA, fused multiply-add: `acc = acc + a*b` in one
instruction). Peak throughput needs every port starting a new op
every cycle — but an op whose *input* is a previous op's *output*
must wait out the latency. A **dependency chain** (each op needing
the last one's result) therefore runs at 1 op per `latency` cycles,
using one port a third of the time. To saturate the machine you need
`latency × ports` = 3 × 4 = **12 independent chains** in flight. One
accumulator = one chain = 1/12 of the machine. Data-parallel loops
don't make you fast; independent chains do (README §1).

### Step 2 — the table is the design doc (spatial/neon.h:10-20)

Every SimSIMD NEON header opens with the numbers Step 1 needs,
measured per microarchitecture:

```
 Intrinsic     Instruction   A76        Apple M5
 vfmaq_f32     FMLA          4cy @ 2p   3cy @ 4p
 vaddq_f32     FADD          2cy @ 2p   2cy @ 4p
 vsqrtq_f32    FSQRT         12cy @ 1p  9cy @ 1p
 vrsqrteq_f32  FRSQRTE       2cy @ 2p   3cy @ 1p
```

Read it as: on M-series, FMA needs latency(3) × ports(4) = 12
in-flight independent FMAs to saturate; on A76 only 8. And FSQRT is
a 1-port 9-cycle disaster — hence the kernels use `vrsqrteq` (a fast
~8-bit reciprocal-square-root *estimate*) refined by 3 Newton-Raphson
rounds (each round roughly doubles the correct bits: 8 → 16 → 32 →
~48, f64-grade) instead of ever issuing FSQRT. Every choice below is
a row of this table.

### Step 3 — precision by wider accumulators, not reordering (dot/neon.h:126)

Summing millions of f32 products accumulates rounding error. polars'
answer was pairwise recursion (restructure the ADDITION ORDER);
SimSIMD's answer is accumulate in f64 (restructure the PRECISION) —
upcast each half of the f32 vector with FCVTL and FMA into two f64
accumulators:

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

f32 inputs, f64 accumulators — half the lane width, deliberately:
the kernel trades throughput for error control that costs no extra
instructions per element. But notice: only TWO chains, when Step 1
demanded 12. The missing parallelism comes from Step 4. Question:
for M14's l2 distance over 1536-dim embeddings, which error-control
strategy is cheaper on M-series, and does recall@10 even care?

### Step 4 — batch candidates, don't unroll pairs (dot/neon.h:37-45)

The streaming API's doc-comment shows where the other chains come
from: score ONE query against FOUR targets simultaneously, keeping
four independent accumulator states:

```
 for idx:                      chains in flight:
   q = load(query+idx)           state1 += q·t1   ┐
   t1..t4 = load(4 targets)      state2 += q·t2   │ 4 FMA chains,
                                 state3 += q·t3   │ shared q load
                                 state4 += q·t4   ┘
 finalize(4 states) → one f32x4 of results
```

The instruction-level parallelism comes from BATCHING CANDIDATES,
not from unrolling one pair — the query load is amortized 4×, and
4 states × 2 chains each (Step 3's low/high split) ≈ the 12 chains
the machine wants. This is exactly M14's HNSW inner loop shape
(score one query against a neighbor list). Question: why is this
better than 4 accumulators over a single pair for the short-vector
case (n=128 dims: how many iterations does each scheme get to
overlap)?

### Step 5 — the FCMLA lesson: specialized instructions must beat the table

ARMv8.3 added FCMLA, a complex-multiply instruction that looks
purpose-built for complex dot products. SimSIMD benchmarked it
(comment near dot/neon.h:150): 17.1 GiB/s vs 39.7 GiB/s for the
"dumb" alternative — deinterleave with `vld2` + 4 independent FMAs
on M4. The fancy instruction LOST 2.3× because it serializes work
that 4 plain FMAs spread over 4 ports (Step 1's arithmetic: fewer,
longer chains). The meta-lesson for M17: newer/specialized
instruction ≠ faster; ports × latency decides. Question: what's the
NEON analogue in our filter kernel (is `vqtbl1q` compress always
better than branchless stores)?

### Step 6 — dispatch: one file per ISA, function pointers at init

Each kernel family exists once per ISA — `dot/neon.h`, `dot/sve.h`,
`dot/haswell.h`, `dot/skylake.h` — and the directory layout IS the
dispatch table. At startup, capability detection fills a table of
function pointers once; call sites pay an indirect call, never a
feature test. That's the middle binding time of this topic's three:
hashbrown binds at compile time (`cfg_if!`), polars at call time
(runtime detect per kernel invocation), SimSIMD at init. Question:
an indirect call can't inline — when does THAT cost exceed the
runtime-check cost it saves (think n=8 dims vs n=4096)?

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| numkong/spatial/neon.h:10-20 | 2 | THE table: per-instruction latency/ports on A76 vs Apple M-series |
| numkong/spatial/neon.h:~100 | 2 | rsqrt by `vrsqrteq` + 3 Newton-Raphson rounds (no FSQRT) |
| numkong/dot/neon.h:126-146 | 3 | `nk_dot_f32_neon` — FCVTL upcast, TWO independent FMA chains |
| numkong/spatial/neon.h:123-140 | 3 | `nk_sqeuclidean_f32_neon` — f64 accumulation, 2 f32/iter |
| numkong/dot/neon.h:37-45 | 4 | stateful streaming API: FOUR `nk_dot_f32x2_state_neon_t`s |
| numkong/dot/neon.h:~150 | 5 | the FCMLA comment: measured 39.7 vs 17.1 GiB/s — why they said no |
| include/numkong/*/ | 6 | one file per ISA per kernel family: neon, sve, haswell, skylake... |

Reading order: the table at the top of `spatial/neon.h` first — it's
the real reading assignment — then `dot/neon.h` top to bottom (the
streaming-state doc-comment, the kernel, the FCMLA comment), then
skim one x86 sibling to see the same kernel re-derived from a
different table.

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

## References

**Code**
- [SimSIMD](https://github.com/ashvardanian/SimSIMD) —
  `include/numkong/` — one file per ISA per kernel family
  (`dot/neon.h`, `spatial/neon.h`, sve/haswell/skylake siblings);
  the port/latency tables at the top of each NEON header are the
  real reading assignment
