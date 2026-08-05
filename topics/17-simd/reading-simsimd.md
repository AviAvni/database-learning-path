# SimSIMD: the port/latency table is the design doc

This is M14's vector-distance layer done by someone who read the CPU
manuals: every NEON file opens with a per-instruction port/latency
table, and every kernel's accumulator count follows from it. Before
the headers, this chapter builds the microarchitecture vocabulary —
ports, latency, dependency chains — then walks each design decision
as a consequence of the table. The through-line: ports × latency
decides everything, and fancy instructions lose to plain FMAs that
spread across ports.

Two things make this guide unusually concrete. First, the headers
carry a column literally labelled `M5`, and this repo's host is an
Apple M5 (`sysctl -n machdep.cpu.brand_string`) — so for once the
vendor-independent latency table you are reading *is your machine's*.
Second, that means you can use it to predict `notes.md`'s numbers
instead of admiring them, which Step 2 does.

Every anchor below is SimSIMD at the pinned revision
`ashvardanian/SimSIMD@63a254f` (`resources/codebases.md`), quoted with
the line numbers the code occupies in that revision. The headers live
under `include/numkong/` — the project renamed its internal namespace
to `numkong`/`nk_`, so `SimSIMD` the repo ships `numkong` the C
library, and every symbol below is `nk_*`.

## The problem in one sentence

A dot-product loop with one accumulator runs at one twelfth of an
Apple M5 core's floating-point issue rate — and SimSIMD's own
benchmark shows the "obvious" specialized instruction (FCMLA) losing
2.3× to plain FMAs (17.1 vs 39.7 GiB/s) — so every kernel here is
shaped by two numbers from the CPU manual, latency and port count,
not by instruction counts.

## The concepts, step by step

### Step 1 — ports, latency, and why one accumulator is 1/12 of the machine

> **In:** a loop that does one fused multiply-add per element into one
> running sum.
> **Out:** the number of *independent* such loops the core needs before
> it stops idling — computed from two numbers, latency and port count.

Three definitions, because the rest of the guide is arithmetic on
them.

An **execution port** (also "pipe") is an independent hardware unit
that can *start* one instruction per cycle. A core with 4 vector
floating-point pipes can begin 4 vector FMAs in the same cycle. Port
count sets **throughput**: how many operations can be *started* per
cycle.

**Latency** is the number of cycles between an instruction starting
and its result being usable by a *dependent* instruction. An FMA
(**fused multiply-add**: `acc = acc + a*b`, computed as one
instruction with one rounding) has a latency of a few cycles.

A **dependency chain** is a sequence where each operation consumes the
previous one's result. `acc += a[i]*b[i]` is a chain: iteration `i+1`
cannot start its FMA until iteration `i`'s FMA has retired its result
into `acc`. A chain therefore advances at exactly one operation per
`latency` cycles, no matter how many ports are idle.

Little's law does the rest. To keep `ports` operations starting every
cycle when each takes `latency` cycles to complete, you need

```
 in-flight operations = latency x ports
```

independent chains. Nothing about lane width appears in that formula.
Widening from 4 lanes to 16 lanes multiplies the work each chain does
but does not add a single chain. This is the sentence the whole topic
turns on, and `experiments/src/dot.rs` says it in the module doc:

```rust
// topics/17-simd/experiments/src/dot.rs:1-5
     1  //! Dot product: the reduction kernel. Four rungs.
     2  //!
     3  //! The lesson (README §1): lanes don't make you fast, independent
     4  //! dependency chains do. M-series wants ~12 FMA chains in flight;
     5  //! one accumulator uses 1/12 of the machine.
```

Step 2 turns "~12" into a number you can derive rather than quote.

### Step 2 — the table is the design doc, and it predicts your own baseline

> **In:** the comment block at the top of `include/numkong/dot/neon.h`
> and the `dot` row of this topic's `notes.md`.
> **Out:** the chain count this core wants, and the core's clock —
> solved for, not asserted.

Every SimSIMD NEON header opens with the numbers Step 1 needs,
measured per microarchitecture. The `dot` header's table is the
longest:

```c
// include/numkong/dot/neon.h:11-27 (comment block, verbatim)
    11   *  Key NEON instructions for dot products:
    12   *
    13   *      Intrinsic     Instruction                  A76       M5
    14   *      vfmaq_f32     FMLA (V.4S, V.4S, V.4S)      4cy @ 2p  3cy @ 4p
    15   *      vfmaq_f64     FMLA (V.2D, V.2D, V.2D)      4cy @ 2p  4cy @ 4p
    16   *      vfmsq_f64     FMLS (V.2D, V.2D, V.2D)      4cy @ 2p  4cy @ 4p
    17   *      vmulq_f32     FMUL (V.4S, V.4S, V.4S)      3cy @ 2p  3cy @ 4p
    18   *      vmulq_f64     FMUL (V.2D, V.2D, V.2D)      3cy @ 2p  3cy @ 4p
    19   *      vaddvq_f32    FADDP+FADDP (reduce)         5cy @ 1p  8cy @ 1p
    20   *      vaddvq_f64    FADDP (V.2D to scalar)       3cy @ 1p  3cy @ 1p
    21   *      vpaddq_f32    FADDP (V.4S, V.4S, V.4S)     2cy @ 2p  3cy @ 4p
    22   *      vpaddq_f64    FADDP (V.2D, V.2D, V.2D)     2cy @ 2p  3cy @ 4p
    23   *      vcvt_f64_f32  FCVTL (V.2D, V.2S)           3cy @ 2p  3cy @ 2p
    24   *      vld2_f32      LD2 ({Vt.2S, Vt2.2S}, [Xn])  4cy @ 1p  4cy @ 1p
    25   *
    26   *  FMA throughput doubles on cores with 4 SIMD pipes (Apple M4+, Graviton3+, Oryon), but
    27   *  horizontal reductions remain at 1/cy on all cores and become the main bottleneck.
```

Read `3cy @ 4p` as "3 cycles of latency, 4 ports". Now apply Step 1's
formula to the two rows that matter, using the **M5** column, which is
this machine:

```
 f32 FMA (line 14):  latency 3 x ports 4 = 12 independent chains
 f64 FMA (line 15):  latency 4 x ports 4 = 16 independent chains

 peak f32 flops: 4 pipes x 4 lanes x 2 flops/FMA = 32 flops/cycle
 peak f64 flops: 4 pipes x 2 lanes x 2 flops/FMA = 16 flops/cycle

 A76 for contrast (2 pipes): 4 x 2 = 8 chains, 16 f32 flops/cycle
```

So "M-series wants ~12 chains" is line 14's `3cy @ 4p` multiplied out,
and the A76 wants 8. The same source file, one column over, is a
different design.

Now the part that makes the table yours. `notes.md` records the `dot`
lane at N = 4M f32 with a single accumulator (`dot_naive`,
`experiments/src/dot.rs:10-17`, one scalar `f32` chain) at
**10.89 GB/s**. That kernel is pure Step 1: one chain, so it must
advance at one element per FMA latency. Solve for the clock:

```
 notes.md, dot lane: naive 10.89 GB/s, counting BOTH inputs
 bytes per element-pair: 4 (a[i]) + 4 (b[i])            = 8 B
 element-pairs per second: 10.89e9 / 8                  = 1.361e9 /s
 one chain at FMLA latency 3 cy (dot/neon.h:14, M5 col)
   => 3 cycles per element-pair
   => clock >= 1.361e9 x 3                              = 4.08 GHz
```

4.08 GHz is a lower bound (it charges the whole 3-cycle latency to
useful work and none to loop overhead), and it is a plausible M5
P-core boost clock. The table predicted the shape of a measurement
taken on a different day by a different tool, to within a napkin. Use
that 4.08 GHz figure wherever a cycle count is needed for this
machine; `reading-simdjson.md` and `reading-sigmod15-vectorization.md`
both cite it rather than guessing a clock.

The second rung checks the other half of the model. `notes.md` has
`dot_unrolled8` (8 accumulators, `dot.rs:22-37`) at **42.12 GB/s**:

```
 8 chains vs 1 chain  =>  model predicts up to 8x
 measured 42.12 / 10.89                                 = 3.87x
 fraction of the chain ceiling reached: 3.87 / 8        = 48 %
 absolute rate: 42.12e9 / 8 B = 5.27e9 pairs/s
   at 4.08 GHz                                          = 1.29 pairs/cycle
 chain ceiling with 8 chains at latency 3: 8/3          = 2.67 pairs/cycle
```

Halfway to the ceiling and no further — because at N = 4M the two
input arrays are 16.78 MB each and the loop is now moving 42 GB/s
through memory, which is single-core DRAM territory on this class of
part. That is the honest reading of `notes.md`'s 3.9×: the first
rung was latency-bound and the model explains it exactly; the second
rung escaped latency and hit bandwidth, so the model only bounds it.
Keep that distinction — Step 3 depends on it.

### Step 3 — precision by wider accumulators, not by reordering

> **In:** `nk_dot_f32_neon`, the f32 dot product every Python/Rust
> caller of this library actually reaches.
> **Out:** why it accumulates in f64 at a 16× cost in peak issue rate,
> and why that 16× is nearly free at the sizes it runs on.

Summing millions of f32 products accumulates rounding error: once the
running sum is large, small addends fall off the bottom of the
24-bit significand. polars' answer (`reading-polars-compute.md`,
Step 5) was pairwise recursion — restructure the *addition order*.
SimSIMD's answer is to restructure the *precision*:

```c
// include/numkong/dot/neon.h:126-146
   126  NK_PUBLIC void nk_dot_f32_neon(nk_f32_t const *a_scalars, nk_f32_t const *b_scalars, nk_size_t count_scalars,
   127                                 nk_f64_t *result) {
   128      // Upcast f32 to f64 via FCVTL/FCVTL2, two independent FMA chains for ILP
   129      float64x2_t sum_low_f64x2 = vdupq_n_f64(0);
   130      float64x2_t sum_high_f64x2 = vdupq_n_f64(0);
   131      nk_size_t idx_scalars = 0;
   132      for (; idx_scalars + 4 <= count_scalars; idx_scalars += 4) {
   133          float32x4_t a_f32x4 = vld1q_f32(a_scalars + idx_scalars);
   134          float32x4_t b_f32x4 = vld1q_f32(b_scalars + idx_scalars);
   135          float64x2_t a_low_f64x2 = vcvt_f64_f32(vget_low_f32(a_f32x4));
   136          float64x2_t a_high_f64x2 = vcvt_high_f64_f32(a_f32x4);
   137          float64x2_t b_low_f64x2 = vcvt_f64_f32(vget_low_f32(b_f32x4));
   138          float64x2_t b_high_f64x2 = vcvt_high_f64_f32(b_f32x4);
   139          sum_low_f64x2 = vfmaq_f64(sum_low_f64x2, a_low_f64x2, b_low_f64x2);
   140          sum_high_f64x2 = vfmaq_f64(sum_high_f64x2, a_high_f64x2, b_high_f64x2);
   141      }
   142      nk_f64_t sum_f64 = vaddvq_f64(vaddq_f64(sum_low_f64x2, sum_high_f64x2));
   143      for (; idx_scalars < count_scalars; ++idx_scalars)
   144          sum_f64 += (nk_f64_t)a_scalars[idx_scalars] * (nk_f64_t)b_scalars[idx_scalars];
   145      *result = sum_f64;
   146  }
```

Lines 129-130 are the two accumulators; 135-138 are the four FCVTL
upcasts (`vcvt_f64_f32` takes the low half, `vcvt_high_f64_f32` the
high half); 139-140 are the two FMAs, one per chain; 142 folds the
chains and reduces; 143-144 is the scalar tail for `count_scalars % 4`.
Note the signature: `nk_f64_t *result` at line 127. The f64-ness is
not an internal detail, it is in the API.

Now price it, using the M5 column and the loop's own shape:

```
 per iteration (dot/neon.h:132-141): 4 f32 element-pairs consumed
   2 loads, 4 FCVTL (line 23: 3cy @ 2p), 2 vfmaq_f64 (line 15: 4cy @ 4p)

 chains: 2 (lines 129-130), each advancing 1 FMA per 4 cy
   => 1 iteration per 4 cycles => 4 pairs / 4 cy       = 1.00 pair/cycle

 what the core could do with 12 f32 chains and no upcast:
   12 chains / 3 cy latency = 4 FMAs/cy x 4 lanes      = 16 pairs/cycle

 peak-issue cost of this design: 16 / 1                = 16x
```

Sixteen times slower than the machine's f32 ceiling — and SimSIMD
ships it anyway. Step 2 explains why that is defensible:

```
 1.00 pair/cycle at 4.08 GHz x 8 B/pair                = 32.6 GB/s
 this topic's measured bandwidth ceiling (notes.md,
   dot_unrolled8, 8 chains, no upcast)                 = 42.1 GB/s
 shortfall at DRAM-resident sizes: 1 - 32.6/42.1       = 23 %
```

A 16× penalty in peak issue becomes a ~23 % penalty in delivered
bandwidth, because at these sizes nothing is issue-bound. That is the
whole argument for the f64 upcast: it costs almost nothing where the
library actually runs, and it removes an error mode that is very hard
to debug from Python. It would be a bad trade for L1-resident data,
which is exactly where Step 6's batching applies.

One more detail worth naming: the FMAs are not the busiest
instruction here. Line 23 gives FCVTL 2 ports; the loop issues 4 of
them and only 2 FMAs, so the *converts* consume 2 cycles of issue
against the FMAs' 0.5. If you ever try to beat this kernel, the
converts are what you have to remove, not the FMAs.

### Step 4 — the horizontal reduction is the other bottleneck

> **In:** the two `vaddvq_*` rows of the table and line 142 of
> `nk_dot_f32_neon`.
> **Out:** the vector length below which the reduce dominates the
> kernel, computed.

Line 27 of the header states the problem in one sentence: "horizontal
reductions remain at 1/cy on all cores and become the main
bottleneck." A **horizontal reduction** sums the lanes *within* one
vector register down to a scalar; unlike lane-wise arithmetic it
cannot be spread across ports, because each stage feeds the next.

The table prices two of them, and the gap is startling:

```
 vaddvq_f32 (line 19): FADDP+FADDP     A76 5cy @ 1p    M5 8cy @ 1p
 vaddvq_f64 (line 20): FADDP           A76 3cy @ 1p    M5 3cy @ 1p
```

On M5 the f32 reduce is **8/3 = 2.7× more expensive** than the f64
one, and it is the only row in the whole table that got *worse* from
A76 to M5 (5 → 8 cycles) while everything else got better. Line 142
therefore reduces with `vaddvq_f64`, which it gets for free because
Step 3 already chose f64 accumulators. The precision decision paid a
performance dividend one line later — that is not a coincidence, it is
what reading your own table buys you.

Now compute when it matters. Take the reduce at line 142 as one
`vaddq_f64` plus one `vaddvq_f64` ≈ 3 + 3 = 6 cycles, and the loop at
1 iteration per 4 cycles from Step 3:

```
 n = 1536 (a typical embedding dimension):
   iterations = 1536 / 4 = 384;  loop = 384 x 4 cy      = 1536 cy
   reduce                                               =    6 cy
   overhead = 6 / 1542                                  = 0.39 %

 n = 8 (a tiny vector, e.g. a quantized codebook entry):
   iterations = 2;  loop = 2 x 4 cy                     =    8 cy
   reduce                                               =    6 cy
   overhead = 6 / 14                                    = 43 %

 break-even at 5 % overhead: loop >= 6 / 0.05 = 120 cy
   => 30 iterations => n >= 120 elements
```

Below ~120 dimensions the reduction is a first-order cost and the
kernel shape should change — which is precisely the case the streaming
API in Step 6 is built for, because it reduces four vectors at once.
Above it, reduce once at the end and forget about it. Note also what
this does to Step 8's dispatch argument: a kernel that costs 14 cycles
cannot afford a feature test.

### Step 5 — FSQRT vs. estimate-and-refine, and the doubling rule

> **In:** the `spatial` header's table and its two reciprocal-square-root
> helpers, one for f32 and one for f64.
> **Out:** why neither ever issues FSQRT, how many Newton-Raphson
> rounds each needs, and one place where SimSIMD's own comment
> overstates the case.

The `spatial` header (L2, cosine/angular distances) opens with its own
table, and its bottom two rows are the story:

```c
// include/numkong/spatial/neon.h:13-26 (comment block, verbatim)
    13   *      Intrinsic     Instruction              A76        M5
    14   *      vfmaq_f32     FMLA (V.4S, V.4S, V.4S)  4cy @ 2p   3cy @ 4p
    15   *      vmulq_f32     FMUL (V.4S, V.4S, V.4S)  3cy @ 2p   3cy @ 4p
    16   *      vaddq_f32     FADD (V.4S, V.4S, V.4S)  2cy @ 2p   2cy @ 4p
    17   *      vsubq_f32     FSUB (V.4S, V.4S, V.4S)  2cy @ 2p   2cy @ 4p
    18   *      vrsqrteq_f32  FRSQRTE (V.4S, V.4S)     2cy @ 2p   3cy @ 1p
    19   *      vsqrtq_f32    FSQRT (V.4S, V.4S)       12cy @ 1p  9cy @ 1p
    20   *      vrecpeq_f32   FRECPE (V.4S, V.4S)      2cy @ 2p   3cy @ 1p
    21   *
    22   *  FRSQRTE provides ~8-bit precision; two Newton-Raphson iterations via vrsqrtsq_f32 achieve
    23   *  ~23-bit precision, sufficient for f32. This is much faster than FSQRT (0.25/cy).
    24   *
    25   *  Distance computations (L2, angular) benefit from 2x throughput on 4-pipe cores (Apple M4+,
    26   *  Graviton3+, Oryon), but FSQRT remains slow on all cores. Use rsqrt+NR when precision allows.
```

`FSQRT` at `9cy @ 1p` with a reciprocal throughput of 0.25/cy (line
23 — one result every 4 cycles) is the worst instruction in the file.
`FRSQRTE` is a **reciprocal square root estimate**: a table lookup
that returns an approximation of `1/sqrt(x)` in 3 cycles. It is not
accurate enough on its own, so it is refined:

```c
// include/numkong/spatial/neon.h:50-61
    50   *  @brief Reciprocal square root of 4 floats with Newton-Raphson refinement.
    51   *
    52   *  Uses `vrsqrteq_f32` (~8-bit initial estimate) followed by two Newton-Raphson iterations
    53   *  via `vrsqrtsq_f32`, achieving ~23-bit precision — sufficient for f32.
    54   *  Much faster than `vsqrtq_f32` (2 cy vs 9-12 cy latency, 2/cy vs 0.25/cy throughput).
    55   */
    56  NK_INTERNAL float32x4_t nk_rsqrt_f32x4_neon_(float32x4_t x) {
    57      float32x4_t rsqrt_f32x4 = vrsqrteq_f32(x);
    58      rsqrt_f32x4 = vmulq_f32(rsqrt_f32x4, vrsqrtsq_f32(vmulq_f32(x, rsqrt_f32x4), rsqrt_f32x4));
    59      rsqrt_f32x4 = vmulq_f32(rsqrt_f32x4, vrsqrtsq_f32(vmulq_f32(x, rsqrt_f32x4), rsqrt_f32x4));
    60      return rsqrt_f32x4;
    61  }
```

**Two** refinement rounds (lines 58 and 59), not three. Newton-Raphson
on this function converges quadratically, so each round roughly
doubles the number of correct bits, and the significand width caps it:

```
 f32 path (lines 57-59), 2 rounds:
   8 bits -> 16 -> 32, capped by f32's 24-bit significand
   header line 53 records                                ~23 bits

 f64 path (lines 110-115), 3 rounds:
   8 bits -> 16 -> 32 -> 64, capped by f64's 53-bit significand
   header line 67 records                                ~48 bits
```

The f64 sibling is a separate piece of code, inside the angular-distance
helper, and it is where the third round lives:

```c
// include/numkong/spatial/neon.h:105-115
   105      // Unlike x86, Arm NEON manuals don't explicitly mention the accuracy of their `rsqrt` approximation.
   106      // Third-party research suggests that it's less accurate than SSE instructions, having an error of 1.5×2⁻¹².
   107      // One or two rounds of Newton-Raphson refinement are recommended to improve the accuracy.
   108      // https://github.com/lighttransport/embree-aarch64/issues/24
   109      // https://github.com/lighttransport/embree-aarch64/blob/3f75f8cb4e553d13dced941b5fefd4c826835a6b/common/math/math.h#L137-L145
   110      float64x2_t rsqrts_f64x2 = vrsqrteq_f64(squares_f64x2);
   111      // Perform three rounds of Newton-Raphson refinement for f64 precision (~48 bits):
   112      rsqrts_f64x2 = vmulq_f64(rsqrts_f64x2, vrsqrtsq_f64(vmulq_f64(squares_f64x2, rsqrts_f64x2), rsqrts_f64x2));
   113      rsqrts_f64x2 = vmulq_f64(rsqrts_f64x2, vrsqrtsq_f64(vmulq_f64(squares_f64x2, rsqrts_f64x2), rsqrts_f64x2));
   114      rsqrts_f64x2 = vmulq_f64(rsqrts_f64x2, vrsqrtsq_f64(vmulq_f64(squares_f64x2, rsqrts_f64x2), rsqrts_f64x2));
```

Line 106 is a second, better-sourced estimate of FRSQRTE's starting
accuracy, and it disagrees with line 22's "~8-bit":

```
 error 1.5 x 2^-12  =>  correct bits = 12 - log2(1.5) = 12 - 0.585 = 11.4 bits
 header line 22 claims                                            ~8 bits
```

Both numbers are in the same file, 84 lines apart, and 11.4 is the one
with a citation attached (line 108). Nothing downstream breaks —
starting from 11.4 bits, two rounds still saturate f32 — but it is a
good habit to notice when a file's summary table is rounder than its
own footnotes.

Two more places where the prose is looser than the table, both
checkable from what you have already read:

1. Line 54's "2 cy vs 9-12 cy latency, 2/cy vs 0.25/cy throughput" is
   an **A76** comparison. Line 18's M5 column says FRSQRTE is
   `3cy @ 1p` — the *same single port* as FSQRT, and one cycle slower
   than the A76 it is being contrasted with. On M5 the port advantage
   the comment advertises does not exist; the win is the 3-vs-9 latency
   plus the fact that the six refinement instructions (lines 58-59:
   four FMUL, two FRSQRTS) are ordinary vector arithmetic that spreads
   over 4 pipes.
2. That same "2 cy" compares a *bare* FRSQRTE against a *complete*
   FSQRT. The honest comparison includes lines 58-59, and the
   refinement is itself a dependency chain — each round's FMUL needs
   the previous round's result. SimSIMD's table does not list a
   latency for FRSQRTS, so the total cannot be computed from this
   source, and this guide will not invent one. What you *can* say from
   the table alone is the throughput claim, which is the one that
   matters for a loop over many vectors.

The kernels that use all this are compact. `nk_sqeuclidean_f32_neon`
is the L2-squared workhorse:

```c
// include/numkong/spatial/neon.h:123-140
   123  NK_PUBLIC void nk_sqeuclidean_f32_neon(nk_f32_t const *a, nk_f32_t const *b, nk_size_t n, nk_f64_t *result) {
   124      // Accumulate in f64 for numerical stability (2 f32s per iteration, avoids slow vget_low/high)
   125      float64x2_t sum_f64x2 = vdupq_n_f64(0);
   126      nk_size_t i = 0;
   127      for (; i + 2 <= n; i += 2) {
   128          float32x2_t a_f32x2 = vld1_f32(a + i);
   129          float32x2_t b_f32x2 = vld1_f32(b + i);
   130          float32x2_t diff_f32x2 = vsub_f32(a_f32x2, b_f32x2);
   131          float64x2_t diff_f64x2 = vcvt_f64_f32(diff_f32x2);
   132          sum_f64x2 = vfmaq_f64(sum_f64x2, diff_f64x2, diff_f64x2);
   133      }
   134      nk_f64_t sum_f64 = vaddvq_f64(sum_f64x2);
   135      for (; i < n; ++i) {
   136          nk_f64_t diff_f64 = (nk_f64_t)a[i] - (nk_f64_t)b[i];
   137          sum_f64 += diff_f64 * diff_f64;
   138      }
   139      *result = sum_f64;
   140  }
```

Count the chains: **one** (line 125), consuming **two** f32 per
iteration (`float32x2_t` at 128-129, half a register). That is
2 elements per 4 cycles = 0.5 elements/cycle, against a 16-chain f64
ceiling. It is the least aggressive kernel in the library, and the
comment at 124 explains the width choice ("avoids slow
vget_low/high") but not the chain count. Whether that is a real
oversight or a judgement that L2 is memory-bound anyway is question 2
below — and after Step 2 you can predict the answer before you test
it. The Euclidean distance is then one line, reusing it:

```c
// include/numkong/spatial/neon.h:142-145
   142  NK_PUBLIC void nk_euclidean_f32_neon(nk_f32_t const *a, nk_f32_t const *b, nk_size_t n, nk_f64_t *result) {
   143      nk_sqeuclidean_f32_neon(a, b, n, result);
   144      *result = nk_f64_sqrt_neon(*result);
   145  }
```

One square root for the whole vector — which is why Step 5's FSQRT
analysis matters less than it looks for L2, and much more for
normalization, where you take one per *element*.

### Step 6 — batch candidates, don't unroll pairs

> **In:** the streaming-state API documented at the top of
> `dot/neon.h`, and the chain deficit left over from Step 3.
> **Out:** where the missing chains come from, and how much load
> traffic batching saves — both computed.

Step 3 left `nk_dot_f32_neon` with 2 chains where the machine wants
16. The missing parallelism does not come from unrolling the loop
further over *one* pair of vectors — it comes from scoring one query
against *several* targets at once. SimSIMD documents the pattern in
the header, as runnable example code:

```c
// include/numkong/dot/neon.h:40-60 (doc-comment example, verbatim)
    40   *  @code{c}
    41   *  nk_dot_f32x2_state_neon_t state_first, state_second, state_third, state_fourth;
    42   *  float32x2_t query_f32x2, target_first_f32x2, target_second_f32x2, target_third_f32x2, target_fourth_f32x2;
    43   *  nk_dot_f32x2_init_neon(&state_first);
    // ... 44-46: three more init calls, one per state ...
    47   *  for (nk_size_t idx = 0; idx + 2 <= depth; idx += 2) {
    48   *      query_f32x2 = vld1_f32(query_ptr + idx);
    49   *      target_first_f32x2 = vld1_f32(target_first_ptr + idx);
    50   *      target_second_f32x2 = vld1_f32(target_second_ptr + idx);
    51   *      target_third_f32x2 = vld1_f32(target_third_ptr + idx);
    52   *      target_fourth_f32x2 = vld1_f32(target_fourth_ptr + idx);
    53   *      nk_dot_f32x2_update_neon(&state_first, query_f32x2, target_first_f32x2, idx, 2);
    // ... 54-56: three more updates, same query vector, different targets ...
    57   *  }
    58   *  float32x4_t results_f32x4;
    59   *  nk_dot_f32x2_finalize_neon(&state_first, &state_second, &state_third, &state_fourth, depth, &results_f32x4);
    60   *  @endcode
```

Line 48 loads the query **once**; lines 49-52 load four different
targets; lines 53-56 feed four independent states. Line 59 reduces all
four together into a single `float32x4_t` — four distances, one
register. That signature is the shape of an HNSW inner loop: score one
query against a neighbour list.

The state is exactly what Step 1 predicts it should be — one
accumulator, so one chain per candidate:

```c
// include/numkong/dot/neon.h:230-246
   230  typedef struct nk_dot_f32x2_state_neon_t {
   231      float64x2_t sum_f64x2;
   232  } nk_dot_f32x2_state_neon_t;
   // ... 234: init zeroes sum_f64x2 ...
   236  NK_INTERNAL void nk_dot_f32x2_update_neon(nk_dot_f32x2_state_neon_t *state, nk_b64_vec_t a, nk_b64_vec_t b,
   237                                            nk_size_t depth_offset, nk_size_t active_dimensions) {
   // ... 238-239: unused-parameter shims ...
   240      // Upcast 2 f32s to f64s for high-precision accumulation
   241      float32x2_t a_f32x2 = vreinterpret_f32_u32(a.u32x2);
   242      float32x2_t b_f32x2 = vreinterpret_f32_u32(b.u32x2);
   243      float64x2_t a_f64x2 = vcvt_f64_f32(a_f32x2);
   244      float64x2_t b_f64x2 = vcvt_f64_f32(b_f32x2);
   245      state->sum_f64x2 = vfmaq_f64(state->sum_f64x2, a_f64x2, b_f64x2);
   246  }
```

Price the two arrangements over the same work — one query against four
targets, `depth` dimensions each:

```
 A. four separate nk_dot_f32_neon calls (Step 3's kernel)
      per call: 2 chains, 4 pairs per 4 cy      = 1.00 pair/cycle
      the query array is streamed 4 times       = 4x query load traffic
      chains in flight at any moment            = 2 of 16

 B. the batched streaming loop (lines 47-57)
      loads per iteration: 1 query + 4 targets  = 5 loads
      element-pairs per iteration: 4 states x 2 = 8 pairs
      chains: 4 states x 1 accumulator          = 4 of 16
      each chain: 1 vfmaq_f64 per 4 cy
        => 8 pairs per 4 cycles                 = 2.00 pairs/cycle

 speedup B/A                                    = 2.0x
 load traffic: naive would need 4 x (1 query + 1 target) = 8 loads
   batched needs 5                              = 37.5 % fewer
```

Twice the throughput and a third fewer loads, from restructuring the
*call*, not the kernel. And notice what the arithmetic also says:
4 chains of 16 is still 25 % of the machine, so batching **eight**
targets instead of four would double it again — the API's choice of
four is a register-pressure decision, not a ceiling. That is a
concrete thing to try in `experiments/`.

Why batching rather than unrolling? Because unrolling needs
iterations to unroll *over*. At `depth = 128`, Step 3's kernel gets
128/4 = 32 iterations, so an 8-wide unroll leaves 4 iterations per
chain — barely enough to fill the pipeline before the loop ends, and
the Step 4 reduce then costs 6 of the ~134 cycles. Batching adds
chains without needing a single extra iteration, which is the only
option when `depth` is small and the candidate list is long. Short
vectors, many of them: that is the vector-search workload exactly.

### Step 7 — the FCMLA lesson: a specialized instruction must beat the table

> **In:** ARMv8.3's `FCMLA`, an instruction that computes a complex
> multiply-accumulate in one go, and SimSIMD's benchmark of it.
> **Out:** why fewer instructions lost to more instructions, by 2.3×,
> and what that means for your own kernel choices.

ARMv8.3-A added `FCMLA` (fused complex multiply-add), which looks
purpose-built for complex dot products: it does the rotate-and-multiply
dance of `(a+bi)(c+di)` in hardware. SimSIMD measured it and rejected
it, recording the result in the code that replaced it:

```c
// include/numkong/dot/neon.h:154-159 (inside nk_dot_f32c_neon, 148-187)
   154      // ARMv8.3-A FCMLA (`vcmlaq_rot0/rot90_f32`) was benchmarked as an alternative to the
   155      // deinterleave+4FMA pattern below. FCMLA processes only 2 complex pairs per iteration
   156      // (interleaved 128-bit operands, 2x `vcmlaq`), while `vld2_f32` deinterleaves 2 pairs
   157      // with 4 independent FMA instructions that fully utilize M4's 4 SIMD pipes. Result on
   158      // Apple M4 at n=4096: manual f32 39.7 GiB/s, FCMLA 17.1 GiB/s (2.3x slower).
   159      // The f64 upcast here trades throughput for precision — FCMLA offers neither advantage.
```

Read the instruction counts before the conclusion:

```
 per 2 complex pairs:
   FCMLA path        : 2 x vcmlaq            = 2 instructions
   deinterleave path : 1 x vld2_f32 + 4 FMA  = 5 instructions

 measured (line 158, Apple M4, n=4096):
   deinterleave (manual f32)                 = 39.7 GiB/s
   FCMLA                                     = 17.1 GiB/s
   ratio 39.7 / 17.1                         = 2.32x
```

The path with **2.5× more instructions is 2.3× faster.** Instruction
count is simply not the metric. Two `vcmlaq` are two operations on the
critical path; four FMAs are four independent chains that Step 1's
formula says the 4-pipe core wants. Ports × latency decided it, as it
decides everything else in this file.

Three cautions on the number itself, because it is easy to misquote:

- **39.7 GiB/s is not what the shipped code does.** Line 159 says so
  explicitly: the benchmark's fast variant accumulated in f32, and the
  shipped `nk_dot_f32c_neon` upcasts to f64 (lines 165-168) for the
  Step 3 reason. The 39.7 is the *rejected alternative's* speed, kept
  as evidence about FCMLA, not as a performance claim for the library.
- **It is an M4 number, and this machine is an M5.** The port counts
  are the same (4 pipes, `dot/neon.h:26` lists "Apple M4+"), so the
  argument transfers; the absolute figure need not.
- **GiB/s, not GB/s** — 2^30, while `notes.md` and `FINDINGS.md` use
  decimal 10^9. Never compare the two without converting
  (39.7 GiB/s = 42.6 GB/s).

The generalization for your own work: a new instruction earns its
place only if it improves `chains x lanes / latency`, and "it exists
and it is named after my problem" is not evidence. The same test
applies to the NEON table lookup in this topic's own filter kernel —
`notes.md`'s implementation log records `count_neon + compact_neon`
with all 16 mask cases passing, and question 5 asks whether `vqtbl1q`
compression actually beats the branchless store it replaces, or
whether it just looks more like SIMD.

### Step 8 — dispatch: one file per ISA, function pointers filled at load

> **In:** the same kernel written once per instruction set, and a
> caller that must not care.
> **Out:** the third of this topic's three binding times, and the cost
> model that picks between them.

Each kernel family exists once per ISA — `dot/neon.h`, `dot/sve.h`,
`dot/haswell.h`, `dot/skylake.h` — so the directory layout *is* the
dispatch table's shape. What connects them is a struct of function
pointers, one field per (kernel, dtype):

```c
// include/numkong via c/dispatch.h:10-12, 23-27, 36-45
    10  #define NK_DYNAMIC_DISPATCH 1
    // ... 14-22: NK_TARGET_* defines come from the build system ...
    23   *  OS/compiler capabilities summary:
    24   *  - Linux: everything available in GCC 12+ and Clang 16+.
    // ... 25-26: FreeBSD and Windows/MSVC rows ...
    27   *  - macOS - Apple Clang: only Arm NEON and x86 AVX2 Haswell extensions.
    // ... 30-34: includes and extern "C" ...
    36  // Forward declaration of dispatch table type (same structure as in numkong.c)
    37  typedef struct {
    38      // Dot products
    39      nk_metric_dense_punned_t dot_f64c;
    40      nk_metric_dense_punned_t dot_f32c;
    41      nk_metric_dense_punned_t dot_bf16c;
    42      nk_metric_dense_punned_t dot_f16c;
    43      nk_metric_dense_punned_t dot_f64;
    44      nk_metric_dense_punned_t dot_f32;
```

Line 27 is worth pausing on: on this Mac, Apple Clang supports only
NEON, so the SVE and Skylake files are not merely unselected, they are
not compiled. The `#[cfg]`-style layer removes the impossible options
before the runtime layer chooses among the possible ones — the same
two-layer structure `reading-polars-compute.md` Step 6 finds in polars.

The table is filled exactly once, at library load:

```c
// include/numkong via c/numkong.c:832-843 and 915-919
   832  NK_DYNAMIC nk_capability_t nk_capabilities(void) {
   833      //! The latency of the CPUID instruction can be over 100 cycles, so we cache the result.
   834      static nk_capability_t static_capabilities = nk_cap_any_k;
   835      if (static_capabilities != nk_cap_any_k) return static_capabilities;
   836
   837      static_capabilities = nk_capabilities_();
   838
   839      // Initialize the central dispatch table with the detected capabilities
   840      nk_dispatch_table_init();
   841
   842      return static_capabilities;
   843  }
   // ... 844-914: per-ISA capability probes ...
   915  // Auto-initialization for dynamic libraries - ensures dispatch table is populated on library load
   916  #if defined(__GNUC__) || defined(__clang__)
   917  __attribute__((constructor)) static void nk_auto_init(void) {
   918      nk_capabilities(); // Triggers dispatch table initialization
   919  }
```

Line 917's `__attribute__((constructor))` runs `nk_auto_init` before
`main`; line 835 memoizes so the second call is a load and a compare;
line 840 fills the table. Line 833 gives the reason in the file's own
words — CPUID can cost over 100 cycles.

That comment is the whole cost model, and Step 4 already gave you the
other side of it:

```
 detection cost, per c/numkong.c:833            > 100 cycles
 a short-vector dot kernel (Step 4, n = 8)      =  14 cycles

 detecting per call: 100 / 14                   = 7x the kernel itself
 detecting once at load, amortized over 1e6 calls
   100 / 1e6                                    = 0.0001 cycles/call
 residual per-call cost of init-time binding: one indirect call
   (unpredictable target on first use, then predicted; never inlined)
```

Which gives this topic's three binding times, and why each is right
for its own dispatched unit:

| binding time | who | mechanism | cost per use | dispatched unit |
|---|---|---|---|---|
| compile | hashbrown, memchr | `cfg_if!` picks a backend module (`group/mod.rs:8-45`) | zero | a handful of instructions |
| init | SimSIMD | `__attribute__((constructor))` fills a fn-pointer table (`c/numkong.c:917-919`) | one indirect call | one whole vector distance |
| call | polars | `is_x86_feature_detected!` per array (`filter/primitive.rs:33`) | one predictable branch | one whole column |

SimSIMD sits in the middle because it ships a C library whose
compilation unit cannot know the target, but whose dispatched unit —
a full distance over hundreds of dimensions — is large enough to
absorb an indirect call. Change either fact and the answer changes:
that is what makes this a cost model rather than a style preference.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| `dot/neon.h:11-27` | 2 | THE table: latency and port counts, A76 vs Apple M5; line 27 names the reduce as the bottleneck |
| `experiments/src/dot.rs:1-5` | 1 | this topic's own statement of the chain rule |
| `dot/neon.h:126-146` | 3, 4 | `nk_dot_f32_neon` — FCVTL upcasts (135-138), TWO f64 chains (129-130), `vaddvq_f64` reduce (142), scalar tail (143-144) |
| `dot/neon.h:19-20` | 4 | `vaddvq_f32` 8cy@1p vs `vaddvq_f64` 3cy@1p on M5 — the reduce that got worse |
| `spatial/neon.h:13-26` | 5 | the distance table; FSQRT `9cy @ 1p`, FRSQRTE `3cy @ 1p` |
| `spatial/neon.h:50-61` | 5 | `nk_rsqrt_f32x4_neon_` — FRSQRTE + **two** NR rounds → ~23 bits |
| `spatial/neon.h:105-115` | 5 | the f64 path — **three** NR rounds → ~48 bits, and the `1.5×2⁻¹²` citation |
| `spatial/neon.h:123-140` | 5 | `nk_sqeuclidean_f32_neon` — ONE chain, 2 f32/iteration |
| `spatial/neon.h:142-145` | 5 | `nk_euclidean_f32_neon` — one sqrt for the whole vector |
| `dot/neon.h:40-60` | 6 | the streaming example: one query load, FOUR target loads, four states, one `float32x4_t` of results |
| `dot/neon.h:230-246` | 6 | the state struct (one `float64x2_t`) and its 2-element update |
| `dot/neon.h:154-159` | 7 | the FCMLA comment: 39.7 vs 17.1 GiB/s on M4 at n=4096 |
| `c/dispatch.h:10, 23-27, 36-45` | 8 | `NK_DYNAMIC_DISPATCH`, the macOS/Apple-Clang capability row, the fn-pointer struct |
| `c/numkong.c:832-843, 915-919` | 8 | memoized detection, the >100-cycle CPUID comment, the load-time constructor |

Reading order: the table at the top of `dot/neon.h` first — it is the
real reading assignment — then `nk_dot_f32_neon` and the streaming
doc-comment above it, then `spatial/neon.h`'s table and its two rsqrt
helpers, then the FCMLA comment, then the two dispatch files. Finish
by opening one x86 sibling (`dot/haswell.h`) and finding the same
kernel re-derived from a different table; you should be able to
predict its accumulator count before you read it.

## Questions for notes.md

1. From `dot/neon.h:14-15`, compute peak f32 and f64 FMA throughput on
   M5 in flops/cycle, then compute what fraction of the f64 figure
   `nk_dot_f32_neon` reaches with its two chains. Step 3 shows the
   arithmetic; redo it for a hypothetical four-chain version and say
   what stops SimSIMD from shipping that.
2. `nk_sqeuclidean_f32_neon` (`spatial/neon.h:123-140`) uses ONE f64
   chain over `float32x2_t` loads — 0.5 elements/cycle by Step 4's
   method. Sloppy, or is L2 memory-bound before it is issue-bound at
   the sizes it runs on? Predict from Step 2's 42 GB/s ceiling, then
   check with `dot.rs`.
3. Newton-Raphson: `spatial/neon.h:57-59` uses two rounds and
   `105-115` uses three. Write out the doubling ladder from both
   claimed starting accuracies (~8 bits at line 22, 11.4 bits from
   line 106's `1.5×2⁻¹²`) and say which round count each justifies for
   f32 and f64. Then say why line 68's advice — "for full 52-bit
   mantissa fidelity, prefer `vsqrtq_f64`" — is consistent with the
   ladder rather than a contradiction of it.
4. The streaming API returns a `float32x4_t` of 4 results
   (`dot/neon.h:59`). Sketch M14's candidate-scoring loop signature
   around it: what does the caller do with a neighbour list of 32,
   and where does the Step 4 reduce cost land in that loop?
5. Apply Step 7's test to this topic's own kernel: `notes.md`'s
   implementation log records `count_neon + compact_neon` (LUT built,
   all 16 masks pass). Does the `vqtbl1q` table lookup beat the
   branchless store it replaces, by the ports-and-latency argument
   rather than by instruction count? Predict, then measure.
6. For M17's dispatch: sketch the fn-pointer table for
   {dot, l2sq, filter} × {neon, scalar}, say where
   `is_aarch64_feature_detected!` runs exactly once, and compute the
   break-even call count against Step 8's >100-cycle detection cost
   for a kernel of your chosen size.

## Done when

Answer each before unfolding it.

- [ ] You can read the port/latency table and compute both the chain count and the peak f32 FMA throughput for M5 from it.

  <details><summary>Answer</summary>

  From `dot/neon.h:14`, M5 column, `vfmaq_f32` is `3cy @ 4p`. Chains
  needed = latency × ports = 3 × 4 = **12**. Peak f32 flops = 4 pipes
  × 4 lanes × 2 flops per FMA = **32 flops/cycle**. For f64
  (line 15, `4cy @ 4p`): 4 × 4 = **16 chains**, and 4 × 2 × 2 =
  **16 flops/cycle**. The A76 column gives 8 chains and 16 f32
  flops/cycle from the same rows — same file, different machine,
  different design.

  </details>

- [ ] You can derive this machine's clock from `notes.md` rather than assuming one.

  <details><summary>Answer</summary>

  `notes.md`'s naive dot rung is 10.89 GB/s over both inputs, i.e.
  8 bytes per element-pair, so 1.361e9 pairs/s. `dot_naive`
  (`experiments/src/dot.rs:10-17`) is one scalar accumulator = one
  chain, so it advances one pair per FMA latency = 3 cycles
  (`dot/neon.h:14`, M5). Clock ≥ 1.361e9 × 3 = **4.08 GHz**. It is a
  lower bound because loop overhead is charged to useful work. The
  host really is an M5 (`sysctl -n machdep.cpu.brand_string`), so the
  table's `M5` column applies directly.

  </details>

- [ ] You can explain why precision is bought with wider accumulators rather than with reordering, and price the choice.

  <details><summary>Answer</summary>

  polars restructures the addition *order* (pairwise summation);
  SimSIMD restructures the *precision*, upcasting f32 to f64 with
  FCVTL (`dot/neon.h:135-138`) and accumulating in `float64x2_t`
  (129-130). Cost: 2 chains × 4 f32 per iteration per 4 cycles =
  1 pair/cycle, against 16 pairs/cycle for a 12-chain pure-f32 kernel
  — **16× in peak issue**. But 1 pair/cycle at 4.08 GHz × 8 B =
  32.6 GB/s, against this topic's measured 42.1 GB/s ceiling, so the
  real cost at DRAM-resident sizes is about **23 %**. It buys error
  control that no reordering can match, in a library whose callers
  cannot debug float cancellation from Python.

  </details>

- [ ] You can say which instruction in the table got *worse* from A76 to M5, and what the kernels do about it.

  <details><summary>Answer</summary>

  `vaddvq_f32`, the f32 horizontal reduce (`dot/neon.h:19`): 5cy@1p on
  A76, **8cy@1p** on M5 — every other row improved. `vaddvq_f64`
  (line 20) stayed at 3cy@1p, so it is 2.7× cheaper on M5, and
  `nk_dot_f32_neon:142` uses it — a free dividend from the Step 3
  precision choice. Line 27 states the general rule: reductions stay
  at 1/cy on all cores and become the main bottleneck. Concretely, at
  n = 8 the ~6-cycle reduce is 43 % of a 14-cycle kernel; at n = 1536
  it is 0.39 %; break-even at 5 % overhead is around n = 120.

  </details>

- [ ] You can say why batching candidates beats unrolling pairs, with the load count and the chain count.

  <details><summary>Answer</summary>

  The streaming loop (`dot/neon.h:47-57`) issues 5 loads per iteration
  (1 query at line 48, 4 targets at 49-52) for 4 states × 2 elements =
  8 element-pairs, versus 8 loads for the same 8 pairs when the four
  dots run separately — **37.5 % fewer loads**, because the query is
  read once instead of four times. Chains go from 2 to **4** (one
  `float64x2_t` per state, `dot/neon.h:230-232`), so throughput goes
  from 1.0 to **2.0 pairs/cycle**. Unrolling cannot do this when
  `depth` is small: at depth 128 there are only 32 iterations to
  unroll over, while batching adds chains without needing any. Four
  candidates is still 4 of 16 chains, so eight would help again.

  </details>

- [ ] You can state the FCMLA lesson precisely, including whose number 39.7 GiB/s actually is.

  <details><summary>Answer</summary>

  `dot/neon.h:154-159`: on an **Apple M4 at n = 4096**, the manual
  deinterleave path (`vld2_f32` + 4 independent FMAs, 5 instructions
  per 2 complex pairs) hit **39.7 GiB/s** while FCMLA (2 `vcmlaq`,
  2 instructions) hit **17.1 GiB/s** — 2.32× slower with 2.5× fewer
  instructions, because 4 FMAs are 4 chains that fill 4 pipes.
  Crucially, 39.7 belongs to the *rejected f32 variant*, not to the
  shipped kernel: line 159 says the shipped code upcasts to f64 and
  "FCMLA offers neither advantage". Also GiB/s (2^30), not the
  decimal GB/s that `notes.md` uses — 39.7 GiB/s = 42.6 GB/s.

  </details>

- [ ] You can sketch the function-pointer dispatch table, say when the ISA choice is made, and justify it against the alternatives.

  <details><summary>Answer</summary>

  `c/dispatch.h:37-45` declares a struct with one
  `nk_metric_dense_punned_t` per (kernel, dtype) — `dot_f32`,
  `dot_f64`, `dot_bf16c`, and so on. `c/numkong.c:917-919` marks
  `nk_auto_init` as `__attribute__((constructor))`, so it runs at
  library load; it calls `nk_capabilities()`
  (`c/numkong.c:832-843`), which memoizes in a `static` at line 834
  and calls `nk_dispatch_table_init()` at 840. Line 833 gives the
  motive: CPUID can cost over 100 cycles. That is **init-time**
  binding — between hashbrown's compile-time `cfg_if!` (zero cost,
  but the dispatched unit is a few instructions) and polars'
  per-call `is_x86_feature_detected!` (one branch, but the dispatched
  unit is a whole column). SimSIMD's unit is one distance over
  hundreds of dimensions, big enough to absorb an indirect call and
  small enough that a >100-cycle probe per call would be 7× the
  kernel at n = 8.

  </details>

- [ ] You found at least one place where SimSIMD's prose is looser than its own table, and can say what the code actually does.

  <details><summary>Answer</summary>

  Three candidates, all checkable. (a) `spatial/neon.h:22` says
  FRSQRTE gives "~8-bit precision" while line 106 cites third-party
  measurement of `1.5×2⁻¹²` error = 11.4 bits, with the issue link at
  108. (b) Line 54's "2 cy vs 9-12 cy latency, 2/cy vs 0.25/cy
  throughput" is the **A76** comparison; line 18's M5 column has
  FRSQRTE at `3cy @ 1p`, the same single port as FSQRT. (c) The same
  line compares a bare FRSQRTE against a complete FSQRT, when the
  usable f32 result needs the two refinement rounds at lines 58-59;
  FRSQRTS has no row in the table, so the honest total cannot be
  computed from this file. None of these change a design decision —
  the throughput argument survives all three — but the f32 path is
  **two** rounds to ~23 bits (57-59), not three, and the three-round
  ~48-bit version is the separate f64 helper at 110-115.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the Newton-Raphson round counts for f32 and f64.

  <details><summary>Answer</summary>

  The round counts: **two** rounds for f32 (`spatial/neon.h:57-59`),
  because 8 → 16 → 32 bits saturates the 24-bit f32 significand and
  the header records ~23 (line 53); **three** rounds for f64
  (`spatial/neon.h:110-115`), because 8 → 16 → 32 → 64 is needed to
  approach the 53-bit f64 significand and the header records ~48
  (line 67). Each round roughly doubles the correct bits because
  Newton's method on this function converges quadratically. Line 68
  is consistent: if you need the full 52-bit mantissa rather than ~48,
  the ladder cannot get you there cheaply and you should just issue
  `vsqrtq_f64`.

  </details>

## References

**Code**
- [SimSIMD](https://github.com/ashvardanian/SimSIMD) at the pinned
  revision `63a254f` (`resources/codebases.md`) — headers under
  `include/numkong/`, one file per ISA per kernel family
  (`dot/neon.h`, `spatial/neon.h`, and their `sve`/`haswell`/`skylake`
  siblings). The port/latency tables at the top of each NEON header
  (`dot/neon.h:11-27`, `spatial/neon.h:11-26`) are the real reading
  assignment; the dispatch machinery is `c/dispatch.h` and
  `c/numkong.c:829-843, 915-919`.

**This repo**
- `topics/17-simd/notes.md` — the `dot` lane (naive 10.89 GB/s,
  unrolled-8 42.12 GB/s, N = 4M f32, Apple Silicon, measured
  2026-07-10) that Steps 2 and 3 compute against. `FINDINGS.md` row 17
  records a different run of the same bench (8.88 → 26.32 GB/s); cite
  whichever you use by name and never average them.
- `topics/17-simd/experiments/src/dot.rs` — `dot_naive` (one chain)
  and `dot_unrolled8` (eight) are the two rungs Step 2 uses.
- `reading-polars-compute.md` (pairwise summation; per-call dispatch),
  `reading-hashbrown-simd.md` (compile-time dispatch),
  `reading-sigmod15-vectorization.md` and `reading-simdjson.md` (both
  cite Step 2's derived 4.08 GHz for their cycle counts).

**Hardware**
- Host for every "this machine" claim above: Apple M5
  (`sysctl -n machdep.cpu.brand_string`), aarch64, 128-bit NEON, no
  SVE exposed and no AVX-512 — so the `M5` column of SimSIMD's tables
  is the one that applies, and `c/dispatch.h:27` explains why only the
  NEON kernels are even compiled here.
