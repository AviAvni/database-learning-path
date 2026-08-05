# polars-compute: shipping SIMD in stable Rust

The production-Rust answer to "how do I ship SIMD without a nightly
compiler or per-CPU binaries": autovectorisation-friendly scalar
bodies, `std::simd` where it pays, and raw intrinsics only for the one
instruction no portable abstraction can express. Before the anchors,
this chapter builds the two kernels every engine needs — the reduction
and the filter — as polars actually ships them, and then works out
what your machine gets from each.

Every anchor below is `pola-rs/polars@f8bcc3d` (`resources/codebases.md`),
quoted with the line numbers the code occupies in that revision. Read
it with one fact in front of you: **on aarch64 the entire AVX-512 half
of the filter is `#[cfg]`-ed out of existence**, so the code your Mac
runs is the scalar path — which turns out to be the more interesting
one anyway.

## The problem in one sentence

`xs.iter().sum::<f32>()` leaves most of the FPU idle because one
accumulator is one serial dependency chain, and the naive filter loop
mispredicts a branch on every unpredictable element; polars-compute
fixes both in stable Rust, in two files you can read in an afternoon.

## The concepts, step by step

### Step 1 — the reduction problem: one accumulator, one chain

> **In:** `n` floats and a loop that does `acc += x[i]`.
> **Out:** a serial dependency chain of length `n`, whose length —
> not the machine's throughput — sets the runtime.

`acc += x[i]` cannot start until `acc += x[i-1]` has finished, because
the second add consumes the first add's result. That is a **dependency
chain**, and its cost is `n × latency`, independent of how many adders
the machine has.

Put this topic's own numbers on it. `notes.md` (provided rungs,
release, Apple Silicon, 2026-07-10, N = 4M f32, 20 reps):

| dot rung | GB/s | ms | vs naive |
|---|---|---|---|
| naive, 1 chain | 10.89 | 3.081 | 1.0× |
| unrolled-8, autovectorised | 42.12 | 0.797 | 3.9× |

3.9× from zero intrinsics — just writing eight accumulators so LLVM is
permitted to reassociate. (`FINDINGS.md` row 17 records 8.88 → 26.32
GB/s from a different run of the same bench; cite whichever run you are
quoting, and do not average them.)

The compiler cannot do this for you unasked. Float addition is not
associative — `(a+b)+c` and `a+(b+c)` round differently — so LLVM will
not turn one chain into four without `-ffast-math`. Restructuring the
order of additions has to be written down. polars writes it down, and
its comment says exactly why:

```rust
// polars crates/polars-compute/src/float_sum.rs:44-63 — the final reduce
    44  fn vector_horizontal_sum<V, T>(mut v: V) -> T
// ... 45-48: bounds ...
    49      // We have to be careful about this reduction, floating
    50      // point math is NOT associative so we have to write this
    51      // in a form that maps to good shuffle instructions.
    52      // We fold the vector onto itself, halved, until we are down to
    53      // four elements which we add in a shuffle-friendly way.
    54      let mut width = STRIPE;
    55      while width > 4 {
    56          for j in 0..width / 2 {
    57              v[j] = v[j] + v[width / 2 + j];
    58          }
    59          width /= 2;
    60      }
    62      (v[0] + v[2]) + (v[1] + v[3])
    63  }
```

Line 62 is the tell: the last four elements are added in a fixed
*shuffle-friendly* pairing, not left to right. The summation order is
part of the API.

### Step 2 — STRIPE = 16: many accumulators, reduced once at the end

> **In:** a block of 128 elements.
> **Out:** 16 lanes of partial sums held across the whole block, folded
> to one scalar exactly once.

```rust
// polars crates/polars-compute/src/float_sum.rs:13-14 — the two constants
    13  const STRIPE: usize = 16;
    14  const PAIRWISE_RECURSION_LIMIT: usize = 128;
```

```rust
// polars crates/polars-compute/src/float_sum.rs:79-85 — the SIMD block sum
    79      fn sum_block_vectorized(&self) -> F {
    80          let vsum = self
    81              .chunks_exact(STRIPE)
    82              .map(|a| Simd::<T, STRIPE>::from_slice(a).cast_generic::<F>())
    83              .sum::<Simd<F, STRIPE>>();
    84          vector_horizontal_sum(vsum)
    85      }
```

Line 81 cuts the 128-element block into 8 chunks of `STRIPE = 16`; line
83 sums them *as vectors*, so lane *j* accumulates elements
j, j+16, j+32, … — 16 independent chains of length 8 instead of one
chain of 128. Line 84 collapses them, once per 128 elements.

Now size it on real registers, because "16 accumulators" is not what
the hardware sees:

```
 STRIPE = 16 lanes.  Simd<T, 16> occupies 16 * size_of::<T>() * 8 bits.

  f32 : 16 * 32 = 512 bits
     NEON    (128-bit regs): 512 / 128 = 4 physical registers
                             -> 4 independent add chains per lane group
     AVX-512 (512-bit regs): 512 / 512 = 1 physical register

  f64 : 16 * 64 = 1024 bits
     NEON: 1024 / 128 = 8 physical registers -> 8 chains

 how many chains do you need?  chains = latency x issue ports
   FMLA on an Apple M-class P-core: 3 cycles, 4 pipes  (SimSIMD's own
   table, include/numkong/dot/neon.h:13-24)          -> 12
```

So `Simd<f32, 16>` on NEON is 4 chains against a model that wants ~12,
and `Simd<f64, 16>` is 8. Deliberately **wider than the vector
register** — the accumulator count is set by latency × ports, not by
register width. Hold onto that number: Mojo's own documentation
(`reading-mojo-simd.md`) advises never exceeding 2× the hardware
register size, and polars ships 4× for `f32` and 8× for `f64`. Both
positions are defensible and they cannot both be right for your
kernel; measure.

The same file contains this topic's headline trick with no SIMD at all:

```rust
// polars crates/polars-compute/src/float_sum.rs:155-169 — the non-SIMD build
   155  #[cfg(not(feature = "simd"))]
   156  impl<T, F> SumBlock<F> for [T; PAIRWISE_RECURSION_LIMIT]
// ... 157-160: bounds ...
   161      fn sum_block_vectorized(&self) -> F {
   162          let mut vsum = [F::default(); STRIPE];
   163          for chunk in self.chunks_exact(STRIPE) {
   164              for j in 0..STRIPE {
   165                  vsum[j] = vsum[j] + chunk[j].as_();
   166              }
   167          }
   168          vector_horizontal_sum(vsum)
   169      }
```

A plain `[F; 16]` array, indexed lane-wise, with a compile-time trip
count and no intrinsic anywhere — and it is still called
`sum_block_vectorized`, because that is exactly the shape LLVM
autovectorises. This is `notes.md`'s "3.9× from ZERO intrinsics" as a
library ships it: the win came from writing 16 accumulators, not from
naming a register.

One caution before you quote the 3.9× as a compute result: at N = 4M
each input is 16 MB, well out of L2, and 42.12 GB/s may be a bandwidth
ceiling rather than an ALU one. `notes.md`'s own prediction worksheet
asks precisely this ("rerun at N=64K in-cache to see compute limit") —
run it before you decide.

### Step 3 — pairwise recursion: the same tree fixes accuracy

> **In:** an array whose length is a multiple of 128.
> **Out:** a sum with `O(log n)` rounding error instead of `O(n)`,
> using the tree the SIMD blocking already built.

```rust
// polars crates/polars-compute/src/float_sum.rs:196-209 — the recursion
   196      let block: Option<&[T; PAIRWISE_RECURSION_LIMIT]> = f.try_into().ok();
   197      if let Some(block) = block {
   198          return block.sum_block_vectorized();
   199      }
// ... 201-204: the safety argument for the split ...
   206          let blocks = f.len() / PAIRWISE_RECURSION_LIMIT;
   207          let left_len = (blocks / 2) * PAIRWISE_RECURSION_LIMIT;
   208          let (left, right) = (f.get_unchecked(..left_len), f.get_unchecked(left_len..));
   209          pairwise_sum(left) + pairwise_sum(right)
```

Below 128 elements, the striped block of Step 2; above it, recursive
halving. One design, two wins. Work the error bound for a realistic
column, using ε for the unit roundoff and counting the longest chain of
dependent additions any single value passes through:

```
 n = 1e8, PAIRWISE_RECURSION_LIMIT = 128, STRIPE = 16

 naive left-to-right worst case            ~ n * eps = 1e8 * eps

 polars, longest path for one value:
   inside its lane, within a block : 128/16     =  8 adds
   the horizontal fold             : log2(16)   =  4 adds
   across blocks                   : log2(1e8/128)
                                   = log2(781250) = 19.6 -> 20 adds
   total                                        ~ 32 * eps

 ratio: 1e8 / 32 = 3.1e6 times tighter
```

The masked variant is the same tree with a `select`:

```rust
// polars crates/polars-compute/src/float_sum.rs:87-97 — nulls, without a branch
    87      fn sum_block_vectorized_with_mask(&self, mask: BitMask<'_>) -> F {
    88          let zero = Simd::default();
// ... 89-92: the same chunks_exact(STRIPE) pipeline, enumerated ...
    93                  let m: Mask<T::Mask, STRIPE> = mask.get_simd(i * STRIPE);
    94                  m.select(Simd::from_slice(a).cast_generic::<F>(), zero)
// ... 96-97: same sum, same horizontal fold ...
```

Line 94 turns "is this value null?" into "add zero", which is topic
11's validity-mask philosophy in one line: nulls never become control
flow. The non-SIMD sibling at line 175 says so out loud —
"Unconditional add with select for better branch-free opts."

### Step 4 — the filter: two scalar kernels and a selectivity threshold

> **In:** 64 values and their 64-bit mask word.
> **Out:** the survivors, packed, using one of two kernels chosen by
> the popcount — with no branch on any individual element.

This is the file to read closely, because it is the one your machine
runs. `scalar.rs` has *two* inner kernels, not one. The sparse kernel
iterates set bits:

```rust
// polars crates/polars-compute/src/filter/scalar.rs:9-25 — the sparse kernel
     9  unsafe fn scalar_sparse_filter64<T: Pod>(v: &[T], mut m: u64, out: *mut T) {
    10      let mut written = 0usize;
    12      while m > 0 {
    13          // Unroll loop manually twice.
    14          let idx = m.trailing_zeros() as usize;
    15          *out.add(written) = *v.get_unchecked(idx);
    16          m &= m.wrapping_sub(1); // Clear least significant bit.
    17          written += 1;
    19          // tz % 64 otherwise we could go out of bounds
    20          let idx = (m.trailing_zeros() % 64) as usize;
    21          *out.add(written) = *v.get_unchecked(idx);
    22          m &= m.wrapping_sub(1); // Clear least significant bit.
    23          written += 1;
    24      }
    25  }
```

Cost is proportional to the popcount, not to 64 — and note line 13's
manual ×2 unroll, which is why the safety contract at line 8 demands
room for "`m.count_ones() + 1` writes": the second half of an unrolled
iteration may run with `m == 0` and write one junk element. This is
simdjson's flatten-bits idiom (`json_structural_indexer.h:93-121`)
applied to values instead of indices.

The dense kernel never branches at all:

```rust
// polars crates/polars-compute/src/filter/scalar.rs:30-46 — the dense kernel
    30  unsafe fn scalar_dense_filter64<T: Pod>(v: &[T], mut m: u64, out: *mut T) {
// ... 31-33: comment — pointer form generates better code ...
    34      let mut written = 0usize;
    35      let mut src = v.as_ptr();
    37      // We hope the outer loop doesn't get unrolled, but the inner loop does.
    38      for _ in 0..16 {
    39          for i in 0..4 {
    40              *out.add(written) = *src;
    41              written += ((m >> i) & 1) as usize;
    42              src = src.add(1);
    43          }
    44          m >>= 4;
    45      }
    46  }
```

Lines 40-41 are exactly this topic's `compact_branchless`
(`experiments/src/filter.rs:21-22`): store unconditionally, advance the
cursor by the predicate. 64 iterations regardless of the data.

And the caller chooses between them by counting bits:

```rust
// polars crates/polars-compute/src/filter/scalar.rs:102-124 — the four paths
   102          // Fast-path: empty mask.
   103          if m == 0 {
   104              continue;
   105          }
// ... 107-110: safety comment ...
   111              // Fast-path: completely full mask.
   112              if m == u64::MAX {
   113                  core::ptr::copy_nonoverlapping(value_chunk.as_ptr(), out, 64);
   114                  out = out.add(64);
   115                  continue;
   116              }
   118              let m_popcnt = m.count_ones();
   119              if m_popcnt <= 16 {
   120                  scalar_sparse_filter64(value_chunk, m, out)
   121              } else {
   122                  scalar_dense_filter64(value_chunk, m, out)
   123              };
   124              out = out.add(m_popcnt as usize);
```

Four paths, and the decision is made **once per 64 elements** on a
value (`m`) that is already in a register — never per element:

```
 m == 0        (0% in this word)  -> skip 64 elements entirely
 m == u64::MAX (100%)             -> one 64-element memcpy
 popcnt <= 16  (<= 25%)           -> sparse: cost ~ popcount
 popcnt >  16  (>  25%)           -> dense: fixed 64 iterations
```

Why 16 and not something else? Count operations and you will not get
16: sparse costs about 5 ops per survivor (tzcnt, load, store, blsr,
increment), dense about 4 per element regardless, so a pure op count
would put the crossover near 50 elements. The real cost is the shape,
not the count — sparse's load address depends on `tzcnt(m)`, so each
survivor sits behind a serial `m → tzcnt → address → load` chain, and
the `while m > 0` trip count is data-dependent. Dense's loads are a
sequential stream and its trip count is a constant. The code offers no
justification comment for 16, so read it as polars' measured choice
(25 % of 64) rather than a derivation.

Note also what is *absent*: there is no `is_simple` helper in this
file. The fast paths are the two literal comparisons at lines 103 and
112.

### Step 5 — the compress instruction: the one place intrinsics are needed

> **In:** a 512-bit vector and a 64-bit mask, on x86 hardware that has
> AVX-512.
> **Out:** the selected lanes packed to the left in one instruction —
> and a comment explaining why polars still does not use the
> compress-*store* form.

```rust
// polars crates/polars-compute/src/filter/avx512.rs:50-62 — the u8 kernel
    50  pub unsafe fn filter_u8_avx512vbmi2<'a>(
// ... 51-54: signature ...
    55      simd_filter!(values, mask_bytes, out, |vchunk, m: u64| {
    56          // We don't use compress-store instructions because they are very slow
    57          // on Zen. We are allowed to overshoot anyway.
    58          let v = _mm512_loadu_si512(vchunk.as_ptr().cast());
    59          let filtered = _mm512_maskz_compress_epi8(m, v);
    60          _mm512_storeu_si512(out.cast(), filtered);
    61          out = out.add(m.count_ones() as usize);
    62      })
```

Lines 56-57 are the whole lesson: the ISA offers a fused
compress-and-store, and polars declines it because it is slow on one
vendor's implementation, preferring compress-to-register (line 59) plus
a full-width store (line 60) and a pointer bump by popcount (line 61).
The overshoot is safe because `filter_values_generic` over-allocated —
`Vec::with_capacity(mask_bits_set + pad)` at `primitive.rs:75`, with
`pad` = 64/32/16/8 for u8/u16/u32/u64, i.e. one 512-bit vector's worth
of elements — and then truncates with `out.set_len(mask_bits_set)` at
`primitive.rs:80`.

The `simd_filter!` macro (`avx512.rs:7-43`) is the shared skeleton:
`avx512.rs:12` chunks by 64 for the same `m64 == 0` sparse fast path as
the scalar file (line 20), then `avx512.rs:24` walks sub-chunks of
`MASK_BITS` elements. Three kernels instantiate it — `epi8` under
`avx512vbmi2` (line 59), `_mm512_maskz_compress_epi32` under `avx512f`
(line 94), and `_mm512_maskz_compress_epi64` (line 104+).

This is the **only** place polars drops to raw intrinsics for filtering,
and the reason is structural: compress is data-dependent lane
*movement*. `std::simd` has no portable operation for it, and no
autovectoriser will invent one.

### Step 6 — what your machine actually runs

> **In:** `target_arch = "aarch64"`.
> **Out:** `nop_filter`, and 100 % of the work in `scalar_filter`.

The dispatch is not in `mod.rs`. `mod.rs` only decides whether the
AVX-512 module *exists*:

```rust
// polars crates/polars-compute/src/filter/mod.rs:2-7 — module gating
     2  mod boolean;
     3  mod primitive;
     4  mod scalar;
     6  #[cfg(all(target_arch = "x86_64", feature = "simd"))]
     7  mod avx512;
```

The runtime test lives one file over, and — correcting a claim that is
easy to make from the shape of the code — it happens **once per array**,
not once per 64-element block:

```rust
// polars crates/polars-compute/src/filter/primitive.rs:49-56, 67-80
    49  fn filter_values_u32(values: &[u32], mask: &Bitmap) -> Vec<u32> {
    50      #[cfg(all(target_arch = "x86_64", feature = "simd"))]
    51      if is_avx512_enabled() {
    52          return filter_values_generic(values, mask, 16, avx512::filter_u32_avx512f);
    53      }
    55      filter_values_generic(values, mask, 1, nop_filter)
    56  }
// ... 58-65: the u64 twin ...
    67  fn filter_values_generic<T: Pod>(
// ... 68-74: signature and set_bits() ...
    75      let mut out = Vec::with_capacity(mask_bits_set + pad);
    77          let (values, mask_bytes, out_ptr) = scalar_filter_offset(values, mask, out.as_mut_ptr());
    78          let (values, mask_bytes, out_ptr) = bulk_filter(values, mask_bytes, out_ptr);
    79          scalar_filter(values, mask_bytes, out_ptr);
    80          out.set_len(mask_bits_set);
```

On aarch64 lines 50-53 do not compile at all, so `filter_values_u32` is
line 55 and nothing else. Trace it through: `pad = 1`,
`bulk_filter = nop_filter`, and `nop_filter` (`primitive.rs:13-19`)
returns its three arguments unchanged. Line 78 is a no-op; line 79 does
everything. **Your Mac's polars filter is Step 4's two scalar kernels,
start to finish** — which is why Step 4 is the long one.

If you want the vector version on NEON you have to write it, and this
topic's `experiments/src/filter.rs` is where. The options are
simdjson's LUT-shuffle compress (`arm64/simd.h:246-278`, an 8-lane
`vqtbl1q_u8` per half) or the branchless append polars already uses.
`notes.md` records branchless at 12.73 GB/s at 50 % selectivity against
branchy's 1.19 — so the bar your NEON kernel has to clear is a scalar
one, and it is not low.

### Step 7 — dispatch, and the three binding times

> **In:** one binary that must run on machines with different ISAs.
> **Out:** a choice between compile-time, init-time and call-time
> binding — and a cost model for picking one.

This topic shows all three:

| binding time | who | mechanism | cost per use |
|---|---|---|---|
| compile | hashbrown, memchr | `cfg_if!` / `#[cfg]` selects a backend file (`group/mod.rs:8-45`) | zero |
| init | SimSIMD | `__attribute__((constructor))` fills a function-pointer table (`c/numkong.c:917-919`) | one indirect call per kernel invocation |
| call | polars | `is_avx512_enabled() && is_x86_feature_detected!(…)` (`primitive.rs:33`) | one predictable branch per array |

polars' choice is right for its shape: the dispatched unit is a whole
column, so a branch per array is unmeasurable, and shipping source
means the `#[cfg]` layer above it already removed the impossible
options. hashbrown's is right because its dispatched unit is a
handful of instructions. SimSIMD's is right because it ships a C
library as a binary and cannot recompile per host.

## Where each step lives in the code

polars at `f8bcc3d`, under `crates/polars-compute/src/`.

| anchor | step | what it is |
|---|---|---|
| `float_sum.rs:2-3` | 2 | `std::simd` imported only under `feature = "simd"` |
| `float_sum.rs:13-14` | 2-3 | `STRIPE = 16`, `PAIRWISE_RECURSION_LIMIT = 128` |
| `float_sum.rs:44-63` | 1-2 | `vector_horizontal_sum` — the once-per-block fold, and the non-associativity comment |
| `float_sum.rs:79-85` | 2 | `sum_block_vectorized` — `chunks_exact(STRIPE)` into `Simd<T,16>` |
| `float_sum.rs:87-97` | 3 | the masked variant: `m.select(v, zero)`, nulls without branches |
| `float_sum.rs:155-186` | 2 | the **non-SIMD** fallback: a plain `[F; 16]` accumulator array |
| `float_sum.rs:189-211` | 3 | `pairwise_sum` — recursive halving down to 128 |
| `filter/mod.rs:6-7` | 6 | `mod avx512` exists only on `x86_64` |
| `filter/mod.rs:30-52` | 6 | `filter_with_bitmap` — leading/trailing-zero trim and all-empty/all-full paths |
| `filter/primitive.rs:11-19` | 6 | `FilterFn` and `nop_filter` — what aarch64 gets |
| `filter/primitive.rs:21-65` | 6-7 | dispatch by element size, then one feature test **per array** |
| `filter/primitive.rs:67-83` | 5-6 | `filter_values_generic`: over-allocate by `pad`, offset, bulk, scalar, `set_len` |
| `filter/scalar.rs:9-25` | 4 | `scalar_sparse_filter64` — `trailing_zeros` + `m &= m-1`, unrolled ×2 |
| `filter/scalar.rs:30-46` | 4 | `scalar_dense_filter64` — store-always, advance by bit |
| `filter/scalar.rs:102-124` | 4 | the four paths and the `popcnt <= 16` threshold |
| `filter/avx512.rs:7-43` | 5 | `simd_filter!` — the shared 64-element skeleton |
| `filter/avx512.rs:50-63` | 5 | `filter_u8_avx512vbmi2` and the "slow on Zen" comment |
| `filter/avx512.rs:87-98` | 5 | `_mm512_maskz_compress_epi32` under plain AVX-512F |
| `min_max/` | — | the same pattern for min/max (`scalar.rs`, `simd.rs`) — an optional second lap |

Reading order: `float_sum.rs` top to bottom (Steps 1-3 in about 210
lines, and read the `#[cfg(not(feature = "simd"))]` impl at 155 as
carefully as the SIMD one), then `filter/primitive.rs` to see where
your machine lands, then `filter/scalar.rs` because that is where it
lands, then `filter/avx512.rs` for the road not taken.

## Questions for notes.md

1. Step 2 puts `Simd<f32, 16>` at 4 NEON registers and `Simd<f64, 16>`
   at 8, against a latency × ports target of about 12. Work out the
   STRIPE that would hit 12 chains for `f32` on NEON, then say what it
   would cost on AVX-512 — and why one constant has to serve both.
2. Derive Step 3's error bound for your own N = 4M dot product rather
   than n = 1e8, and compare it with `notes.md`'s open question about
   "max f32 dot error vs naive at N=4M".
3. `scalar_sparse_filter64`'s contract asks for `count_ones() + 1`
   writes and `filter_values_generic` allocates `mask_bits_set + pad`.
   Trace one 64-element word at popcount 1 and say exactly which write
   is the extra one.
4. The `popcnt <= 16` threshold does not fall out of an operation
   count (Step 4 shows it would predict ~50). Design the experiment
   that would find the real crossover on your machine, and predict
   which way it moves for `u64` values versus `u8`.
5. `filter_with_bitmap` (`filter/mod.rs:30-52`) trims leading and
   trailing zero runs before doing anything. For a Cypher `WHERE` over
   a sorted-ish column, what fraction of the work does that remove,
   and which of Step 4's four paths does it make redundant?
6. For M17: polars uses `std::simd` for the sum but *not* for the
   filter. State the property of compress that defeats portable SIMD,
   and name one other operation in this topic with the same property.

## Done when

Answer each before unfolding it.

- [ ] You can explain the reduction problem, and connect STRIPE = 16 to this topic's own measured accumulator win.

  <details><summary>Answer</summary>

  One accumulator is one dependency chain of length n, costing
  `n × latency` no matter how many adders exist. `float_sum.rs:81-83`
  accumulates into `Simd<T, 16>` so lane j sums elements j, j+16, … —
  16 lanes, folded to a scalar once per 128-element block by
  `vector_horizontal_sum` (`:44-63`), which is careful about ordering
  because float addition is not associative (`:49-51`).

  `notes.md` measures the same idea in this topic's dot product:
  10.89 → 42.12 GB/s (3.9×) from eight accumulators and no intrinsics.
  (`FINDINGS.md` row 17 has 8.88 → 26.32 from another run.) Caveat: at
  N = 4M the fast rung may be bandwidth-bound, which `notes.md`'s
  worksheet asks you to check at N = 64K.

  </details>

- [ ] You can size STRIPE = 16 in physical registers on this machine, and state the tension it creates.

  <details><summary>Answer</summary>

  `Simd<f32, 16>` is 512 bits = **4** NEON registers; `Simd<f64, 16>`
  is 1024 bits = **8**. On AVX-512 the f32 case is a single register.
  So polars deliberately runs 4× the hardware vector width for f32,
  because accumulator count is set by latency × ports (about 3 × 4 = 12
  for FMLA on an Apple M-class P-core, per SimSIMD's table at
  `dot/neon.h:13-24`), not by register width.

  Mojo's documentation gives the opposite advice — never exceed 2× the
  register size, because the compiler splits the vector and "the
  resulting code will perform poorly". Both are real positions; the
  difference is that polars is accumulating, where extra registers buy
  independent chains, and Mojo's warning is about straight-line
  element-wise code, where they buy nothing.

  </details>

- [ ] You can explain how pairwise recursion fixes accuracy, with the arithmetic.

  <details><summary>Answer</summary>

  `pairwise_sum` (`float_sum.rs:189-211`) halves the array down to
  128-element blocks (line 207) and sums each block with Step 2's
  striped kernel. For n = 1e8 the longest chain of dependent additions
  any one value passes through is 128/16 = 8 within its lane, plus
  log2(16) = 4 for the horizontal fold, plus log2(1e8/128) ≈ 20 across
  blocks — about 32ε against a naive left-to-right bound of 1e8·ε, a
  factor of ~3.1e6.

  The SIMD blocking and the accuracy fix are the same tree.

  </details>

- [ ] You can describe both scalar filter kernels and the rule that picks between them.

  <details><summary>Answer</summary>

  `scalar_sparse_filter64` (`filter/scalar.rs:9-25`) iterates set bits
  with `trailing_zeros` and `m &= m.wrapping_sub(1)`, manually unrolled
  ×2 — cost proportional to popcount, and the reason its contract
  demands `count_ones() + 1` writes. `scalar_dense_filter64` (`:30-46`)
  stores every element and advances the cursor by
  `((m >> i) & 1)` — 64 fixed iterations, no branch.

  `scalar_filter` (`:102-124`) picks per 64-element word: skip if
  `m == 0`, one 64-element `copy_nonoverlapping` if `m == u64::MAX`,
  sparse if `popcnt <= 16` (25 %), dense otherwise. The threshold is
  empirical — an operation count would predict about 50 — and reflects
  sparse's data-dependent load address and trip count.

  </details>

- [ ] You can say what your machine actually executes, and prove it from the `#[cfg]`s.

  <details><summary>Answer</summary>

  Only the scalar path. `filter/mod.rs:6-7` gates `mod avx512` on
  `target_arch = "x86_64"`, so on aarch64 it does not exist;
  `filter_values_u32` (`primitive.rs:49-56`) therefore compiles to just
  `filter_values_generic(values, mask, 1, nop_filter)`, and `nop_filter`
  (`primitive.rs:13-19`) returns its arguments unchanged. In
  `filter_values_generic` (`:67-83`) line 78 is a no-op and line 79's
  `scalar_filter` does all the work, with `pad = 1`.

  So there is no compress instruction, no runtime feature test that can
  succeed, and no vector code in polars' primitive filter on this Mac.

  </details>

- [ ] You can explain why compress needs intrinsics, and place polars' dispatch among the three binding times.

  <details><summary>Answer</summary>

  Compress is data-dependent lane *movement*: which source lane feeds
  which destination lane is a function of the mask, not of the program.
  `std::simd` has no portable operation for that and no autovectoriser
  will synthesise one, so `avx512.rs` is the one file in
  polars-compute that uses raw intrinsics. (simdjson answers the same
  problem on NEON with a lookup table of shuffle patterns.)

  Binding times: compile-time in hashbrown/memchr (`cfg_if!`),
  init-time in SimSIMD (a constructor filling a function-pointer table
  at `c/numkong.c:917-919`), call-time in polars
  (`primitive.rs:33`, once per array). polars' unit of dispatch is a
  whole column, so a branch per array costs nothing measurable.

  </details>

- [ ] You wrote answers to all six questions in notes.md.

  <details><summary>Answer</summary>

  Self-check. Question 3 has one right answer worth verifying: at
  popcount 1 the ×2 unroll runs its second half with `m == 0`, so
  `trailing_zeros() % 64` gives index 0 and one junk element is written
  past the survivor — which is exactly the "+1" in both the contract at
  `filter/scalar.rs:8` and the `+ pad` at `primitive.rs:75`.

  </details>

## References

**Code**
- [polars](https://github.com/pola-rs/polars) at `f8bcc3d` —
  `crates/polars-compute/src/float_sum.rs` for the reduction (read the
  `#[cfg(not(feature = "simd"))]` impl too), `filter/primitive.rs` for
  the dispatch, `filter/scalar.rs` for the two kernels your machine
  runs, `filter/avx512.rs` for the intrinsic path. `min_max/` repeats
  the same structure if you want a second lap.

**Cross-references in this topic**
- `reading-simdjson.md` — the NEON answer to compress
  (`arm64/simd.h:246-278`) and the flatten-bits loop that
  `scalar_sparse_filter64` mirrors.
- `reading-sigmod15-vectorization.md` — the selectivity curve these two
  kernels are sitting on, and where the crossover comes from.
- `reading-mojo-simd.md` — the "never exceed 2× the register width"
  advice that Step 2 contradicts on purpose.
