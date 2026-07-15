# FastLanes: bit-unpacking at memory bandwidth

Topic 12 decoded bit-packed integers one value at a time; FastLanes
(Afroozeh & Boncz) redesigns the STORAGE LAYOUT so that decoding any
bit width is the same straight-line SIMD kernel — no shuffles, no
per-width special cases — and hits memory bandwidth on every ISA
from NEON to AVX-512, *including scalar code that autovectorizes*.
Before the paper, this chapter builds the argument step by step:
what bit-packing is, why the obvious layout can't vectorize, and how
transposing the data dissolves every obstacle. The punchline for
this whole topic: layout, not intrinsics, is the win.

## The problem in one sentence

Bit-packed columns are how analytical databases fit in RAM, but the
standard sequential layout decodes serially — one value's position
depends on all previous values — leaving a memory-bandwidth-class
job (100+ billion integers/second in the title) running at scalar
ALU speed.

## The concepts, step by step

### Step 1 — bit-packing: pay only the bits you need

Bit-packing stores integers in exactly the bits their range requires:
values 0–7 need 3 bits each, so 1024 of them take 384 bytes instead
of 4096 as u32s — a 10.7× compression. For an analytical scan the
compression IS the performance (topic 12): a scan reads 10× fewer
bytes through the topic-0 memory ladder. The catch is decoding —
turning packed bits back into usable u32s — which now sits on the
hot path of every scan.

### Step 2 — why the sequential layout can't vectorize

The obvious layout packs values back-to-back: value 1's bits
immediately follow value 0's. Two things break. Values straddle word
boundaries (a 3-bit value can start at bit 62 of a u64 and end in
the next word), and — worse — value i's bit position is
`i × w mod 64`, so each decode step's shift amount depends on where
the previous one ended:

```
 3-bit values packed sequentially in a u64:
 |v0 |v1 |v2 |v3 |v4 ... v20|v21⟨spans the word boundary⟩
 decode v21: load TWO words, shift both, OR, mask  ← branchy, serial,
 and lane i+1 depends on where lane i ended        ← unvectorizable
```

That's a serial dependency chain (each step needing the previous
step's result — the enemy from README §1) *plus* data-dependent
control flow (README §2's failure #2). SIMD lanes want to execute
the identical operation; sequential packing guarantees they can't.

### Step 3 — the fix: transpose into 1024 virtual bit-serial lanes

FastLanes packs a block of 1024 values as if the machine had 1024
one-bit-wide lanes ("the 1024-bit virtual ISA"): instead of value
after value, it stores bit-plane after bit-plane — plane b holds bit
b of many values, arranged so that every real SIMD lane always
applies the SAME shift and mask:

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
code, fewer iterations:

```rust
// 1024 values as 16 u64 lanes advancing in LOCKSTEP — every lane runs the
// identical shift+mask, which is all autovectorization needs to see
fn unpack(planes: &[[u64; 16]], w: u32, out: &mut [[u64; 16]; 64]) {
    let mask = (1u64 << w) - 1;
    let (mut word, mut shift) = (0usize, 0u32);
    for group in out.iter_mut() {
        for lane in 0..16 {                 // ← the vectorized dimension
            group[lane] = (planes[word][lane] >> shift) & mask;
        }
        shift += w;
        if shift + w > 64 { word += 1; shift = 0; }
        // (real FastLanes stitches the boundary bits with one extra
        //  OR instead of padding — still the same shift for ALL lanes)
    }
}
```

The cost of the transpose: values are no longer stored in logical
order, so random access to value i needs reads from w separate
planes. Analytic scans, which decode whole blocks anyway, never
notice.

### Step 4 — the unified transposed order: one permutation for every lane width

The permutation 04261537 reorders values so that ALL of
{8,16,32,64}-bit lane types see a consistent order — so you can
bit-unpack u8s, then delta-decode as u16s, without re-permuting
between kernels. And delta (each value stored as the difference from
its predecessor — a PREFIX dependency, inherently serial) is
computed *per-lane over the transposed order*: each lane keeps its
own running base, turning one 1024-long serial prefix-sum into
1024/W independent short chains. Same trick as multi-accumulator dot
(reading-simsimd.md): break the chain by restructuring the data.
Question: why does delta need the unified order at all?

### Step 5 — results worth remembering

- Decode at RAM bandwidth: unpacking is FREE relative to the memory
  it saves — the final word on topic 12's "compression IS
  performance" table.
- Scalar Rust/C compiled with autovec reaches ~the intrinsic
  version, BECAUSE the layout removed everything autovec chokes on
  (README §2's four failures — all four absent by construction).
- The same layout accelerates delta, RLE, dictionary, and FOR —
  it's a compression *layout*, not a codec.

### Step 6 — our baby version: `unpack.rs`

The experiments' 4-bit unpack keeps topic 12's sequential layout —
legitimately, because at w=4 values never straddle bytes (4 divides
8), the one family of widths where sequential is already
SIMD-friendly:

```
 16 bytes = 32 nibbles:  lo = bytes & 0x0F,  hi = bytes >> 4
 → interleave/widen to u32 lanes.  No LUT. Two ops + widening.
```

Question: at which widths does the sequential layout stop being
this easy (hint: w ∤ 8), and what does FastLanes' transposition buy
exactly there?

## How to read the paper (with the concepts in hand)

- **§3–4 — read carefully.** The interleaved layout (Step 3) and the
  unified transposed order (Step 4). Draw the bit-planes for w=3,
  16 lanes, by hand — once you can place value 17's bits yourself,
  the rest of the paper is bookkeeping.
- **Delta section** — check that the per-lane-base trick really is
  Step 4's chain-breaking, then compute the chain-length ratio
  (question 3 below).
- **Evaluation** — the claim to verify is the scalar-autovec one
  (Step 5, second bullet): find the table where scalar compiled code
  matches intrinsics, and note on which ISA the gap is largest.
- The FastLanes repo (CWI's reference implementation) is optional;
  the paper's kernels are self-contained.

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

## References

**Papers**
- Afroozeh & Boncz — "The FastLanes Compression Layout: Decoding
  > 100 Billion Integers per Second with Scalar Code" (VLDB 2023)
  — read §3-4 for the interleaved layout and the unified transposed
  order; the eval confirms the autovectorization claim

**Code**
- [FastLanes](https://github.com/cwida/FastLanes) — CWI's reference
  implementation of the layout (optional; the paper's kernels are
  self-contained)
