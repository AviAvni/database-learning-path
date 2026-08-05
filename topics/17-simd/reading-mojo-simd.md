# Mojo's `SIMD[dtype, size]`: width as a type parameter

What does SIMD look like when the TYPE SYSTEM, not a library, owns
it? In Mojo, scalars are literally width-1 vectors, and vector width
is a compile-time parameter you abstract over — the ergonomic ceiling
that `std::simd` and `wide` approximate from below. Before the docs,
this chapter builds the idea step by step — the ergonomics ladder,
the width-1 unification, the parametric `vectorize`, and what none of
it solves. Read it for the language-design angle; it's the contrast
that explains why our Rust experiments hand-write what Mojo's
`vectorize` generates.

**Read the version number before anything else.** Mojo is a moving
target: the documentation moved from `docs.modular.com/mojo/…` to
`mojolang.org/docs/…` (the old stdlib URLs now 404), the stdlib
namespace is `std.…`, the compile-time keyword is `comptime`, and
`simdwidthof` has been renamed `simd_width_of`. Every quotation below
is from **Mojo 1.0.0b2** — the version string the docs site reports
in its own page class (`docs-version-1.0.0b2`) — and every doc page
cited is reachable as clean Markdown by appending `.md` to its URL,
which is how the quotes here were extracted. If you are reading this
against a later release, re-check the names before trusting the
translation table in Step 8.

One naming caveat worth stating up front: many older write-ups —
including this repo, until the H1 above was corrected alongside its
`SUMMARY.md` link — spell the type `SIMD[type, width]`. At 1.0.0b2 the
declaration is `struct SIMD[dtype: DType, size: Int]`: the second
parameter is **`size`**, not `width`, and the first is a `DType`
*value* rather than a type. Expect the old spelling in any tutorial
written before the rename.

## The problem in one sentence

The same dot-product kernel must be written once for NEON's 4 f32
lanes, again for AVX-512's 16, and again as a scalar remainder loop
— polars literally ships its AVX-512 filter twice for two element
types — because in today's languages vector width is a property of
the *code you wrote*, not a *parameter the compiler fills in*.

## The concepts, step by step

### Step 1 — the ladder of SIMD ergonomics

> **In:** the same operation — a fused multiply-add across a vector of
> f32 — expressed three ways.
> **Out:** the axis those three ways vary along, which is *who chooses
> the width*.

SIMD (single instruction, multiple data — one instruction operating
on a vector of W values, its **lanes**) can be programmed at three
levels of abstraction, each trading control for portability:

```
 raw intrinsics      vfmaq_f32(acc, a, b)          per-ISA names, unsafe,
 (core::arch)                                      exact instruction control
        │
 portable library    acc = a.mul_add(b, acc)       one vocabulary, library
 (std::simd, wide)   Simd<f32, 4> / f32x4          picks instructions; width
        │                                          is a const generic bolted on
        │
 language type       SIMD[DType.float32, 4]        scalars ARE SIMD[T, 1];
 (Mojo 1.0.0b2)      fn f[w: Int](x: SIMD[T, w])   width is a first-class
                                                   compile-time parameter
```

**Intrinsics** are compiler-provided functions that map 1:1 to machine
instructions — NEON's `vfmaq_f32` *is* the `FMLA` in
`include/numkong/dot/neon.h:14`. Maximal control, zero portability:
`experiments/src/dot.rs:62` can only exist behind
`#[cfg(target_arch = "aarch64")]` at line 61.

A **portable library** gives one vocabulary and lets the library pick
instructions. `wide::f32x4` and `std::simd::Simd<f32, 4>` both do
this. The width is still a number *you* typed, though: polars writes
`const STRIPE: usize = 16;` (`float_sum.rs:13`) and that 16 is 16 on
every machine polars will ever run on.

Mojo moves the whole idea into the language. Here is the declaration,
verbatim:

```mojo
# mojolang.org/docs/std/builtin/simd/SIMD — Mojo 1.0.0b2, verbatim
struct SIMD[dtype: DType, size: Int]
```

Both parameters are compile-time. The doc's own summary of the
consequences is worth quoting because it is the language's thesis
statement:

> **Hardware-mapped**: Directly maps to CPU vector registers ·
> **Type-safe**: Data types and vector sizes are checked at compile
> time · **Zero-cost**: No runtime overhead compared to hand-optimized
> intrinsics · **Portable**: Same code works across different CPU
> architectures (x86, ARM, etc.)

And one hard constraint, stated in the same page under
**Constraints**: "The size of the SIMD vector must be positive and a
power of 2." That is not a stylistic rule — it is what lets the
compiler decompose an over-wide vector into whole registers, which is
Step 6's subject.

### Step 2 — scalars are width-1 vectors

> **In:** Mojo's `Float32`, `Int8`, and the rest of the ordinary-looking
> numeric types.
> **Out:** the observation that none of them are scalar types, and the
> two practical consequences.

Mojo's unification move is stated in the manual, in three lines of
`comptime` aliases:

```mojo
# mojolang.org/docs/manual/types — "Scalar values", Mojo 1.0.0b2, verbatim
comptime Scalar = SIMD[size=1]
comptime Int8 = Scalar[DType.int8]
comptime Float32 = Scalar[DType.float32]
```

`Scalar` is `SIMD` with `size` bound to 1 and `dtype` still open;
`Float32` closes `dtype` too. There is no separate scalar world. The
manual draws the conclusion itself: "whether you're working with a
single `Float32` value or a vector of float32 values, the math
operations go through exactly the same code path."

Two consequences you can use tomorrow, in Rust, without Mojo.

**No duplicate implementations to keep in sync.** Vectorizing an
algorithm means changing a parameter, not rewriting a body. Contrast
`experiments/src/dot.rs`, where the same reduction exists four times —
`dot_naive` (scalar, lines 10-17), `dot_unrolled8` (scalar, 8 named
accumulators, 22-37), `dot_wide` (portable, 46-49), `dot_neon`
(intrinsics, 62-65) — with four separate bodies that must agree to
within float rounding. The `#[cfg(target_arch = "aarch64")]` at line
61 is the fourth body's admission that it is not portable.

**The width-1 instance is a free test oracle.** If the scalar version
is literally the `size=1` instantiation of the vector version, then
running the same body at 1 and at 4 and diffing is a *tautologically
correct* test — you are testing the compiler's monomorphization, not
your algorithm. In Rust you have to build that oracle by hand, and
`dot.rs` does: its test module (`dot.rs:67` onward) has a `rel_err`
helper comparing each rung against `dot_naive`. Note that the
comparison there is *approximate* (`rel_err`), because in Rust the two
bodies really are different code with different summation orders. In
Mojo the reduce-order difference is still real — `reduce_add()` on a
4-lane vector is not the same order as four scalar adds — so the
oracle is exact only for order-independent kernels. Name that
distinction before you steal the idea: it holds for `filter`, not for
`dot`.

### Step 3 — `simd_width_of`: the machine's width as a queryable constant

> **In:** a target machine and an element dtype.
> **Out:** the natural lane count, as a compile-time `Int` — computed
> here for both this host and an AVX-512 server.

With width as a parameter, the machine's natural width becomes a
compile-time query rather than a number you hardcode:

```mojo
# mojolang.org/docs/std/sys/info/simd_width_of — Mojo 1.0.0b2, verbatim
def simd_width_of[dtype: DType, target: __mlir_type.`!kgen.target` = _current_target()]() -> Int
```

"Returns the vector size of the type on the host system." The
`target` parameter defaults to `_current_target()`, so it is a
*compile-time* property of what you are building for — and it can be
overridden, which is how you cross-compile without editing the
kernel. (There is a second overload taking a `type: RegisterPassable`
rather than a `DType`, on the same page.)

Do the arithmetic, because the whole argument is in the numbers. The
value is register bits divided by element bits:

```
 this host (Apple M5, aarch64, NEON, 128-bit vector registers):
   simd_width_of[DType.float32]()  = 128 / 32  =  4
   simd_width_of[DType.float64]()  = 128 / 64  =  2
   simd_width_of[DType.int8]()     = 128 /  8  = 16

 an AVX-512 x86 server (512-bit vector registers):
   simd_width_of[DType.float32]()  = 512 / 32  = 16
   simd_width_of[DType.float64]()  = 512 / 64  =  8
   simd_width_of[DType.int8]()     = 512 /  8  = 64

 ratio: the SAME source text yields 4x more lanes per instruction
```

Now compare what this repo does. `dot.rs:41-45` instructs you to use
`wide::f32x4` — the 4 is baked into the type name, so the byte
`4` in `f32x4` is this machine's `simd_width_of[DType.float32]()`
frozen at authoring time. On an AVX-512 box that same code still
issues 128-bit instructions and leaves three quarters of each register
idle. There is no portable, stable-Rust way to write "the native width
for `f32` on the target"; you write 4 and you write a comment.

The four is not automatically wrong, mind. Step 6 shows a case where
polars deliberately picks a width *larger* than any register, and
`reading-simsimd.md` Step 1 shows why: lanes and chains are different
resources, and only one of them is what `simd_width_of` returns.

### Step 4 — `vectorize`: the remainder loop, generated

> **In:** a length that is not a multiple of the vector width, and a
> width-generic closure.
> **Out:** the exact iteration schedule Mojo emits — including a
> surprise about the tail — and the cost of that tail, computed.

`simd_width_of` gives you a constant; `vectorize` turns it into a
loop. The signature, reduced to its load-bearing parts:

```mojo
# mojolang.org/docs/std/algorithm/backend/vectorize/vectorize — Mojo 1.0.0b2
def vectorize[func: ..., //, simd_width: Int, /, *, unroll_factor: Int = 1](size: Int, closure: func)
```

The one-line description is the whole contract: it maps "a function
across a range from 0 to `size`, incrementing by `simd_width` at each
step. The remainder of `size % simd_width` will run in separate
iterations."

The doc's own worked example is better than anything this guide could
invent, because it prints the schedule. It sets `comptime size = 10`
and `comptime simd_width = simd_width_of[DType.int32]()`, "assumed to
be 4 in this example", with a closure that prints its own width and
position. The documented output:

```
storing 4 els at pos 0
storing 4 els at pos 4
storing 1 els at pos 8
storing 1 els at pos 9
[0, 0, 0, 0, 4, 4, 4, 4, 8, 9]
```

Work through it:

```
 size = 10, simd_width = 4
 full iterations : floor(10 / 4) = 2, covering elements 0..7
 remainder       : 10 % 4        = 2, elements 8 and 9

 what got emitted for the remainder:
   NOT one width-2 iteration  (2 is a legal SIMD size — a power of 2)
   BUT two width-1 iterations ("1 els at pos 8", "1 els at pos 9")

 trip count: 2 vector iterations + 2 scalar = 4 iterations for 10 elements
   50 % of the trip count handles 20 % of the data
```

That the tail is width-1 and not width-2 is the detail worth carrying
away. Mojo *could* emit a descending ladder of widths (4, 2, 1) and
finish the tail in one step, but each distinct width is another
monomorphization of the closure — more code, more compile time — and
width 1 is the one instance Step 2 guarantees already exists for free.
The unification of scalars and vectors is what makes the cheap tail
strategy also the *only* tail strategy you need.

`unroll_factor` is the other knob. The doc shows `unroll_factor=2`
producing, in pseudocode, `closure[4](0); closure[4](4);` straight-line
instead of a loop, and warns that "the remainder loop won't unroll
unless `size` is passed as a parameter". This is the same lever as
`dot.rs`'s hand-written four accumulator vectors, with one crucial
difference covered in Step 7: unrolling a loop is not the same as
creating independent accumulator chains, and only the second one buys
you `reading-simsimd.md`'s 12.

Now price the tail on this repo's actual workload:

```
 our benches: N = 4M f32 = 4,194,304 elements (notes.md), width 4
   4,194,304 % 4 = 0   ->   the remainder loop never runs

 an odd length, e.g. a 1,537-dim embedding at width 4:
   1537 % 4 = 1  ->  1 scalar iteration out of 385  =  0.26 % of trips
```

Which is exactly why remainders are the classic source of SIMD bugs:
they cost nothing and they run almost never, so they are the code path
your benchmark never touches and your fuzzer finds first. Two ways out
appear in this topic: generate the tail (Mojo), or make the tail
impossible. simdjson takes the second route — see
`reading-simdjson.md` on its padded input buffer, which lets every
kernel read a full vector past the logical end without a tail at all.

### Step 5 — the four separable layers

> **In:** a slow loop and the four independent things you could do to
> it.
> **Out:** the order they must be applied in, and which of them this
> repo has actually measured.

The structural claim behind Mojo's design is that four optimizations
are *separable* — each a parameter or a decorator rather than a
rewrite — and that they stack in a fixed order:

```
 layer                     what changes                    which topic
 1  scalar semantics       types + compilation             (the baseline)
 2  lanes + chains         SIMD width, accumulator count    17 (this one)
 3  threads                cores                            14
 4  tiles                  cache blocking                   13
```

Types first, lanes second, cores third, cache blocking last. The
ordering is not arbitrary: you cannot vectorize a loop whose element
type is decided at runtime, you cannot usefully parallelize a loop
that is already memory-bound, and tiling only pays once the inner
kernel is fast enough that the cache is the constraint.

The earlier version of this guide illustrated the ladder with a
GFLOPS progression from Modular's matmul blog post. **That source is
gone** — the notebook and blog URLs all return 404 as of this
revision, and the brief's rule is that an unverifiable number does not
go in. So the ladder above carries no borrowed numbers. What it
carries instead is this repo's own measurement of layer 2, which you
can rerun:

```
 layer 2, measured here (notes.md, dot lane, N = 4M f32,
   release, Apple Silicon, measured 2026-07-10):
     dot_naive      (1 accumulator chain)     10.89 GB/s
     dot_unrolled8  (8 accumulator chains)    42.12 GB/s
     ratio 42.12 / 10.89                    =  3.9x

 the same lane in FINDINGS.md row 17, a different run:
     8.88 -> 26.32 GB/s                     =  3.0x

 layers 3 and 4 are NOT measured in topic 17 — they are topics
 14 and 13, and this topic's framing is deliberately "the last
 10x on a single core".
```

Cite whichever run you use by name; never average them. And note what
the 3.9× is *not*: it is not a lane-width effect. `dot_unrolled8`
(`dot.rs:22-37`) has eight `f32` accumulators, no vector types
anywhere, and no intrinsics — it is FINDINGS.md's "eight accumulators
and no intrinsics". Layer 2 is two things wearing one name, and this
repo's headline measures the chain half of it. Step 7 returns to why
the compiler could not have done that for you.

### Step 6 — where the type system stops caring: the 2× register rule

> **In:** Mojo's documented advice about over-wide vectors, and polars'
> deliberate violation of it.
> **Out:** the resolution — which is that the advice is right for maps
> and wrong for reduces, with the register counts to prove it.

The `SIMD` page carries an explicit caution, verbatim:

> **Caution:** If you declare a SIMD vector size larger than the
> vector registers of the target hardware, the compiler will break up
> the SIMD into multiple vector registers for compatibility. However,
> you should avoid using a vector that's more than 2x the hardware's
> vector register size because the resulting code will perform poorly.

Now hold that against `reading-polars-compute.md` Step 2, where
`float_sum.rs:13` sets `const STRIPE: usize = 16;` and the reduce runs
on `Simd<T, STRIPE>` (`float_sum.rs:82-83`):

```
 NEON vector register (this host)                      = 128 bits
 Mojo's ceiling: 2x the register                       = 256 bits
   at that ceiling:  f32 <= 8 lanes,  f64 <= 4 lanes

 polars ships:
   Simd<f32, 16> = 16 x 32 = 512 bits  = 4x the register
   Simd<f64, 16> = 16 x 64 = 1024 bits = 8x the register

 polars exceeds Mojo's stated ceiling by 2x (f32) and 4x (f64)
```

Both documents are right, about different kernels, and the difference
is what the extra registers are *for*.

Mojo's caution is about **element-wise maps**: `c = a * b + 1.0` over
a `SIMD[DType.float32, 32]`. Splitting that into 8 physical registers
buys nothing — the 8 multiply-adds were already independent, the
hardware would have pipelined 4-lane versions of them just as well,
and you have spent 8 of ~32 architectural vector registers to say so.
The compiler-generated spills are the "perform poorly".

polars' 16 is about **reduces**, where the extra registers *are* the
point. A reduction is a dependency chain, and
`reading-simsimd.md` Step 1 gives the rule: you need
`latency × ports` independent chains, which on this host is 3 × 4 = 12
for f32 FMA (`include/numkong/dot/neon.h:14`, M5 column). Declaring an
over-wide type is a portable way to spell "give me N chains" in a
language that has no direct way to ask:

```
 Simd<f32, 16> on NEON = 4 physical float32x4_t
   -> 4 independent add chains  (against the 12 the core wants)
 Simd<f64, 16> on NEON = 8 physical float64x2_t
   -> 8 independent add chains  (against 16 for f64 FMA, 4cy x 4p)

 so even polars' "excessive" 4x reaches only 4/12 = 33 % of the
 chains this core can keep in flight
```

The resolution: Mojo's rule is right when the operation is a map and
wrong when it is a reduce, and neither document says which it means.
This is a real limit of parametric width as a *language* feature —
`size` conflates "how much data per instruction" with "how many
independent chains", and the compiler cannot tell which one you wanted
from the type alone. Measure before you follow either.

### Step 7 — what parametric width does NOT solve

> **In:** the two hardest things in this topic — data-dependent
> compaction, and accumulator count.
> **Out:** why neither is a width problem, so no type system dissolves
> them.

The type system dissolves boilerplate, not microarchitecture. Two
things stay yours.

**Compress- and gather-shaped problems still need per-ISA thought.**
AVX-512 has `vpcompressd`, which packs the selected lanes of a vector
to the front in one instruction; NEON has no such instruction. A
parametric width cannot conjure one. On this host you write the
16-entry shuffle-mask lookup table instead — `notes.md`'s
implementation log records exactly that (`filter.rs: count_neon +
compact_neon (LUT built, all 16 masks pass)`), and
`reading-simdjson.md` walks the same trick in simdjson's own
`arm64/simd.h`. A hypothetical Mojo version of `compact_neon` would
still contain a table, because the problem is that the instruction
does not exist, not that the width was hard to spell.

**Accumulator count stays an engineering decision.** Float addition is
not associative, so reassociating a reduction changes the answer, so
no compiler in any language may split your one accumulator into twelve
without permission. This is why `dot_naive` (`dot.rs:10-17`) stays at
10.89 GB/s: its doc comment at lines 8-9 says so — "LLVM cannot
vectorize this without `-ffast-math` (float reassociation changes the
answer)". The permission has to be in the source, and giving it is
what `dot_unrolled8` does with eight named accumulators. The
instruction is the same either way; the *chain count* changed.

Note that Step 4's `unroll_factor` does not solve this either.
Unrolling replicates the loop body; if every replica still accumulates
into the same variable, you have one chain and a bigger loop body.
`dot.rs:41` therefore says "FOUR independent accumulator vectors", not
"unroll by four" — and even that is 4 chains against a machine that
wants 12, which is the first thing to try changing when your rung 3
number disappoints.

The dividing line: anything expressible as "same operation, any width"
the language absorbs; anything about *which* instructions exist and
*how many independent chains* you keep in flight stays engineering.

### Step 8 — the translation table for our stack

> **In:** each Mojo construct above.
> **Out:** the hand-written stable-Rust line in `experiments/` that
> stands in for it, with the anchor.

Every Mojo construct has a stable-Rust equivalent somewhere in this
topic's crate. This table is the map from the ideal to what M17
actually ships:

| Mojo 1.0.0b2 | stable Rust in `topics/17-simd/experiments/` |
|---|---|
| `SIMD[DType.float32, 4]` | `wide::f32x4` (`dot.rs:41`) |
| `Scalar[DType.float32]` = `SIMD[size=1]` | a separate `fn`: `dot_naive` (`dot.rs:10-17`), kept for the tail and for dispatch |
| `simd_width_of[DType.float32]()` | the literal `4` inside the type name `f32x4`; 128-bit NEON ÷ 32 |
| `vectorize[simd_width](size, closure)` | `chunks_exact(16)` plus a hand-written scalar remainder (`dot.rs:42-45`) |
| `unroll_factor = 2` | writing the accumulators out by hand (`dot.rs:41`, `dot_unrolled8` at `dot.rs:22-37`) |
| `reduce_add()` | `f32x4::reduce_add` (`dot.rs:45`) / `vaddvq_f32` (`dot.rs:56`) |
| target-conditional codegen | `#[cfg(target_arch = "aarch64")]` (`dot.rs:61`) |

Read the table in both directions. Left to right it says how much
boilerplate the language absorbs. Right to left it says something less
flattering to Mojo: every one of those Rust lines is *visible*, so you
can see the width, the chunk size, the accumulator count and the
reduce instruction at a glance — and this topic's whole argument is
that those four numbers are the performance. The generated version is
easier to write and harder to audit, which is the trade every
abstraction in this course makes.

## How to read the docs (with the concepts in hand)

Everything below is a web page, not a clone — there is nothing to
check out and nothing in the pin table (`resources/codebases.md`) for
Mojo, which is exactly why the version caveat at the top of this guide
matters. Append `.md` to any URL to get clean Markdown, and start from
`https://mojolang.org/llms.txt` or `sitemap.xml` if a link has moved
again.

1. **`/docs/std/builtin/simd/SIMD`** — skim the type's surface with
   Step 2 in mind. The interesting part is what is a *parameter*
   (`dtype`, `size`) versus what is a method (`reduce_add`, `select`,
   `shuffle`, `cast`). Read the **Caution** block against Step 6 and
   decide, for your own kernel, whether it is a map or a reduce.
2. **`/docs/manual/types` § "Scalar values"** — three lines of
   `comptime` aliases and one sentence of consequence. This is Step 2
   in full; it is shorter than this paragraph.
3. **`/docs/std/algorithm/backend/vectorize/vectorize`** — read the
   worked example and predict its printed output *before* looking, per
   Step 4. If you predicted one width-2 tail iteration, reread Step 2
   and work out why width-1 is cheaper for the compiler.
4. **`/docs/std/sys/info/simd_width_of`** — one signature; note the
   `target` parameter and what it means for cross-compilation.
5. Then go back to `experiments/src/dot.rs` and mark every line that
   `vectorize` would have generated. That is Step 8 made concrete, and
   it is the point of reading a language you are not going to use.

## Questions for notes.md

1. `comptime Float32 = Scalar[DType.float32]` = `SIMD[DType.float32, 1]`:
   what does making scalars width-1 vectors buy for TESTING kernels?
   Then state the limit Step 2 names — for which of `dot` and `filter`
   is the width-1 instance an *exact* oracle rather than an
   approximate one, and why?
2. `vectorize` generates the remainder loop and runs `size %
   simd_width` as that many **width-1** iterations. Where in
   `filter.rs` is the equivalent code, what is the remainder for this
   repo's N = 4M at width 4, and why is a path that runs 0 % of the
   time in your benchmarks the classic source of SIMD bugs? Compare
   simdjson's answer (pad the input so there is no tail).
3. Rust's `std::simd` has had the parametric type `Simd<f32, N>` for
   years and is still nightly-only. Which piece is actually hard: the
   type, the portable operations (`compress`!), or stabilizing the
   ISA mapping? Use Step 7's NEON-has-no-`vpcompressd` example to
   argue one side.
4. Step 6's contradiction: Mojo says never exceed 2× the register
   width; polars ships 4× for `f32` and 8× for `f64`. Compute the
   physical register count and the chain count for both, decide which
   rule applies to `dot_wide` (`dot.rs:46`), and pick a `STRIPE` for
   your own version. Then measure and see if you were right.
5. Step 5's ladder gains more from tiling (topic 13) and threads
   (topic 14) than from lanes. Reconcile that with this topic's "last
   10× on a single core" framing: when is topic 13 the bigger lever
   than topic 17, and what does `notes.md`'s 42 GB/s ceiling tell you
   about which one you are up against at N = 4M?
6. For M17: our engine will hardcode NEON width 4. Write the one
   sentence justifying that (deployment target) and the one-line
   escape hatch if SVE servers arrive — say which of Step 8's table
   rows would have to change, and which would not.

## Done when

Answer each before unfolding it.

- [ ] You can state the actual 1.0.0b2 signature of `SIMD` and say which of its two parameters is not a type.

  <details><summary>Answer</summary>

  `struct SIMD[dtype: DType, size: Int]`. Neither parameter is a type
  in the usual sense: `dtype` is a **value** of the `DType` struct
  (`DType.float32` is an identifier, not a type — the manual is
  explicit that "you can't create a variable with the type
  `DType.float64`"), and `size` is an `Int`. Both are compile-time
  parameters, and `size` "must be positive and a power of 2". Note the
  spelling: `size`, not `width`, despite this page's own title.

  </details>

- [ ] You can explain what making scalars width-1 vectors buys in the type system, and where the free test oracle stops being free.

  <details><summary>Answer</summary>

  `comptime Scalar = SIMD[size=1]`, so `Float32` is
  `SIMD[DType.float32, 1]` and, per the manual, "the math operations
  go through exactly the same code path". Two wins: no duplicate
  scalar/vector bodies to keep in sync (contrast `dot.rs`'s four
  separate rungs), and the width-1 instantiation is a test oracle for
  the width-4 one. The limit: it is an *exact* oracle only for
  order-independent kernels. `filter` qualifies; `dot` does not,
  because `reduce_add()` over 4 lanes sums in a different order than
  four scalar adds and float addition is not associative — which is
  why `dot.rs`'s tests use a relative-error helper rather than
  equality.

  </details>

- [ ] You can compute `simd_width_of` for three dtypes on this host and on an AVX-512 server.

  <details><summary>Answer</summary>

  Register bits ÷ element bits. On this host (Apple M5, NEON,
  128-bit): f32 → 128/32 = **4**, f64 → 128/64 = **2**, i8 →
  128/8 = **16**. On a 512-bit AVX-512 machine: f32 → **16**, f64 →
  **8**, i8 → **64**. The same source text, 4× the lanes. In our Rust
  the value is frozen into the type name `f32x4` (`dot.rs:41`), so on
  an AVX-512 box that code leaves three quarters of every register
  idle. `simd_width_of` also takes an explicit `target` parameter,
  defaulting to `_current_target()`, so the query works for
  cross-compilation too.

  </details>

- [ ] You can say what `vectorize` generates that you write by hand, and predict its remainder schedule exactly.

  <details><summary>Answer</summary>

  It emits `floor(size / simd_width)` full-width iterations plus
  `size % simd_width` iterations **at width 1** — not one narrower
  vector iteration. The doc's example (size 10, width 4) prints
  "storing 4 els at pos 0 / 4 els at pos 4 / 1 els at pos 8 / 1 els
  at pos 9" and ends with `[0, 0, 0, 0, 4, 4, 4, 4, 8, 9]`. Width-1
  is chosen because Step 2 already guarantees that instantiation
  exists, so no extra monomorphization is needed. In Rust you write
  the `chunks_exact` loop and the scalar tail yourself
  (`dot.rs:42-45`). For this repo's N = 4M at width 4 the remainder is
  4,194,304 % 4 = **0**, so the tail never executes in any benchmark
  here — which is precisely why it is where bugs hide.

  </details>

- [ ] You can separate the four layers and say which one this repo has actually measured.

  <details><summary>Answer</summary>

  Scalar semantics/compilation → lanes and chains (topic 17) →
  threads (topic 14) → tiles and cache blocking (topic 13), in that
  order, because each layer needs the one before it to be settled.
  This repo has measured **only layer 2**: `notes.md`'s dot lane goes
  10.89 → 42.12 GB/s (3.9×) and `FINDINGS.md` row 17 records
  8.88 → 26.32 GB/s (3.0×) from a different run. Both come from
  `dot_unrolled8`, which has eight scalar `f32` accumulators and no
  intrinsics — so the headline is the *chain* half of layer 2, not the
  lane half. The GFLOPS ladder that used to sit here was taken from a
  Modular blog post that now 404s, and has been removed rather than
  quoted from memory.

  </details>

- [ ] You can resolve the 2× rule against polars' 4×, with register counts.

  <details><summary>Answer</summary>

  Mojo's `SIMD` page cautions against "a vector that's more than 2x
  the hardware's vector register size because the resulting code will
  perform poorly" — on 128-bit NEON that caps f32 at 8 lanes. polars
  ships `Simd<f32, 16>` (512 bits, 4×) and `Simd<f64, 16>` (1024 bits,
  8×) from `float_sum.rs:13`. Both are right, for different kernels.
  For an element-wise **map** the split into 4 or 8 physical registers
  buys nothing and costs register pressure — Mojo's case. For a
  **reduce** the physical registers *are* independent dependency
  chains, and the core wants latency × ports = 3 × 4 = 12 of them for
  f32 FMA (`dot/neon.h:14`) — polars' case, and its 4 chains are still
  only a third of what the machine would take. `size` conflates
  "data per instruction" with "chains in flight", and no type system
  yet distinguishes them.

  </details>

- [ ] You can name what parametric width does *not* solve, with a concrete example of each.

  <details><summary>Answer</summary>

  **Missing instructions.** AVX-512's `vpcompressd` has no NEON
  equivalent, so data-dependent compaction needs a 16-entry
  shuffle-mask table on this host regardless of language —
  `notes.md`'s log records `compact_neon` with all 16 masks passing,
  and simdjson does the same thing in `arm64/simd.h`. **Chain count.**
  Float addition is not associative, so no compiler may turn one
  accumulator into eight; `dot.rs:8-9` says LLVM cannot vectorize
  `dot_naive` "without `-ffast-math`". The permission must be written
  in the source, which is what `dot_unrolled8` does. `unroll_factor`
  does not help either: replicating a body that accumulates into one
  variable gives you one chain and a longer body.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the one-line justification for hardcoding NEON width 4 in M17.

  <details><summary>Answer</summary>

  The justification is a deployment-target claim, not a performance
  one: M17 ships to aarch64 hosts with 128-bit NEON and no SVE
  exposed, so `simd_width_of[DType.float32]()` would return 4 on every
  machine we run on, and a hardcoded 4 is that query constant-folded.
  The escape hatch is the last row of Step 8's table — the
  `#[cfg(target_arch = ...)]` boundary — behind which a second kernel
  can appear without touching the callers; what would *not* change is
  the accumulator count, because Step 7 shows that number is set by
  latency × ports rather than by register width.

  </details>

## References

**Docs (Mojo 1.0.0b2 — check the version before trusting any name)**
- `https://mojolang.org/docs/std/builtin/simd/SIMD` — the
  `struct SIMD[dtype: DType, size: Int]` declaration, the
  power-of-two constraint, the reduce/select/shuffle API surface, and
  the "avoid more than 2x the hardware's vector register size"
  caution quoted in Step 6.
- `https://mojolang.org/docs/manual/types` § "Scalar values" — the
  three `comptime` aliases of Step 2 and the "exactly the same code
  path" sentence.
- `https://mojolang.org/docs/std/sys/info/simd_width_of` — Step 3's
  signature. Note the rename: this was `simdwidthof` in earlier
  releases, and the sibling queries are `simd_bit_width` and
  `simd_byte_width`.
- `https://mojolang.org/docs/std/algorithm/backend/vectorize/vectorize`
  — Step 4's signature, worked example and printed schedule;
  `parallelize` lives one directory over
  (`/docs/std/algorithm/backend/cpu/parallelize/`) and is topic 14's
  business.
- Index pages: `https://mojolang.org/llms.txt` and
  `https://mojolang.org/sitemap.xml`. Appending `.md` to any doc URL
  returns Markdown.
- **Removed:** the earlier version of this guide illustrated Step 5
  with a GFLOPS ladder (Python → naive Mojo → vectorized →
  parallelized → tiled) from Modular's "Matrix Multiplication in Mojo"
  notebook. That page and its blog mirrors return 404 at the time of
  writing, so the figures could not be checked against a source and
  have been dropped rather than repeated. If it reappears, the numbers
  belong here with their hardware.

**This repo**
- `topics/17-simd/experiments/src/dot.rs` — the four rungs Step 8
  maps onto: `dot_naive` (10-17), `dot_unrolled8` (22-37), `dot_wide`
  (39-49), `dot_neon` (51-65).
- `topics/17-simd/notes.md` and `FINDINGS.md` row 17 — the layer-2
  measurements of Step 5, from two different runs of the same lane.
- `reading-simsimd.md` — the latency × ports rule Step 6 and Step 7
  lean on, and the port/latency table it is derived from.
- `reading-polars-compute.md` Step 2 — `STRIPE = 16`, the deliberate
  violation of Mojo's 2× advice.
- `reading-simdjson.md` — padding instead of a remainder loop
  (Step 4), and the shuffle-table approach to compaction (Step 7).
