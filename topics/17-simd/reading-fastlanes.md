# Reading guide — FastLanes ("The FastLanes Compression Layout", VLDB '23)

Azim Afroozeh & Peter Boncz. Topic 12 decoded bit-packed integers
one value at a time; this paper redesigns the STORAGE LAYOUT so that
decoding any bit width is the same straight-line SIMD kernel — no
shuffles, no per-width special cases — and hits memory bandwidth on
every ISA from NEON to AVX-512, *including scalar code that
autovectorizes*.

## 1. The problem with sequential bit-packing (topic 12's layout)

```
 3-bit values packed sequentially in a u64:
 |v0 |v1 |v2 |v3 |v4 ... v20|v21⟨spans the word boundary⟩
 decode v21: load TWO words, shift both, OR, mask  ← branchy, serial,
 and lane i+1 depends on where lane i ended        ← unvectorizable
```

Values straddle word boundaries and each value's position depends on
all previous widths — a serial dependency chain, the enemy from
README §1.

## 2. The fix: interleaved (transposed) layout

FastLanes packs a block of 1024 values as if the machine had 1024
bit-serial lanes ("the 1024-bit virtual ISA"):

```
 1024 values, width w → w × 128-byte "bit-planes":
 word j of plane b holds bit b of values {j, j+64, j+128, ...}
 (transposed order, via the "unified 04261537" permutation)

 decode = for each output vector:
   acc = (plane_word >> shift) & mask   ← same shift for ALL lanes
   no value ever crosses a lane boundary
   no cross-lane shuffle, EVER
```

Because every lane does the identical shift+mask at every step, the
kernel is the same for NEON's 128-bit vectors, AVX-512's 512-bit, or
a u64 scalar loop — the vector width just decides how many of the
1024 virtual lanes you process per instruction. Wider ISA = same
code, fewer iterations.

## 3. The unified transposed order

The permutation 04261537 reorders values so that ALL of {8,16,32,64}-
bit lane types see a consistent order — so you can bit-unpack u8s,
then delta-decode as u16s, without re-permuting between kernels.
Question: why does delta (a PREFIX dependency) need this at all?
Their answer: delta is computed per-lane over the transposed order —
each lane keeps its own running base, turning a serial prefix-sum
into 1024/W independent short chains. Same trick as multi-
accumulator dot: break the chain by restructuring the data.

## 4. Results worth remembering

- Decode at RAM bandwidth: unpacking is FREE relative to the memory
  it saves — the final word on topic 12's "compression IS
  performance" table.
- Scalar Rust/C compiled with autovec reaches ~the intrinsic
  version, BECAUSE the layout removed everything autovec chokes on
  (README §2's four failures — all four absent by construction).
- The same layout accelerates delta, RLE, dictionary, and FOR —
  it's a compression *layout*, not a codec.

## 5. Our baby version: `unpack.rs`

The experiments' 4-bit unpack keeps topic 12's sequential layout
(values don't straddle bytes at w=4 — the one width where sequential
is already SIMD-friendly):

```
 16 bytes = 32 nibbles:  lo = bytes & 0x0F,  hi = bytes >> 4
 → interleave/widen to u32 lanes.  No LUT. Two ops + widening.
```

Question: at which widths does the sequential layout stop being
this easy (hint: w ∤ 8), and what does FastLanes' transposition buy
exactly there?

## Questions for notes.md

1. Block = 1024 values regardless of width. What two constraints
   pick 1024 (largest vector ISA lanes × smallest type, and
   cacheline alignment of every plane)?
2. Interleaved decode touches w planes 128B apart — is that still
   sequential enough for the prefetcher (topic 13's stride limits)?
3. Delta-decode with per-lane bases: what's the ratio of chain
   length, 1024 sequential vs transposed on 128-bit NEON (16 u64
   lanes... derive it)?
4. Random access to value i now needs w bit-plane reads — what did
   we trade away vs sequential packing, and why doesn't an analytic
   scan care (topic 12's block-granularity access)?
5. For M17's checklist item "SIMD-ize one topic 12 decoder": ours is
   w=4 sequential. Predict GB/s scalar vs NEON before running
   simd_bench — then reconcile with FastLanes' claim that layout,
   not intrinsics, is the win.
