# FastLanes: bit-unpacking at memory bandwidth

Topic 12 decoded bit-packed integers one value at a time. FastLanes
(Afroozeh & Boncz, VLDB 2023) redesigns the **storage layout** so that
decoding any bit width is the same straight-line sequence of loads,
shifts, masks and ORs — no shuffles, no cross-lane traffic, no
per-width special cases — and so that scalar C compiled with
`-O3` auto-vectorises to match hand-written intrinsics. Before the
paper, this chapter builds the argument: what bit-packing is, why the
obvious layout cannot vectorise, and how interleaving the values
dissolves every obstacle. The punchline for the whole topic: layout,
not intrinsics, is the win.

There is no pinned clone for FastLanes, so every claim below is
anchored to the paper — "The FastLanes Compression Layout: Decoding
> 100 Billion Integers per Second with Scalar Code", PVLDB 16(9),
pp. 2132-2144 — by section, listing or figure number, and the local
measurements come from this topic's own `notes.md`. Where the paper
and this machine disagree, both numbers are given with their source.

## The problem in one sentence

Bit-packed columns are how analytical databases fit in RAM, but the
standard sequential ("horizontal") layout decodes serially — value
*i*'s bit position depends on all the values before it — leaving a
memory-bandwidth-class job running at scalar ALU speed.

## The concepts, step by step

### Step 1 — bit-packing: pay only the bits you need

> **In:** 1024 integers known to fit in W bits, currently stored as
> `uint32`.
> **Out:** the same 1024 integers in `1024·W` bits, and a decode step
> that now sits on the hot path of every scan.

Bit-packing stores each integer in exactly the bits its range
requires. Work the case the paper uses throughout (§2.1, Figure 1):
W = 3, so values in 0..=7.

```
 1024 values as uint32 : 1024 * 32 bits = 32768 bits = 4096 bytes
 1024 values at W = 3  : 1024 *  3 bits =  3072 bits =  384 bytes
 compression ratio     : 4096 / 384 = 10.67x
```

For an analytical scan the compression *is* the performance (topic
12): the scan pulls 10.67× fewer bytes through the topic-0 memory
ladder. The catch is that decoding — turning packed bits back into
usable `uint32`s — is now a per-value cost on the read path, and the
whole question is whether it can be made to cost nothing.

Note the block size, because everything downstream depends on it. The
paper's footnote 2 (in §2.1) puts it exactly: "a chunk of 1024·W
(bit-width) encoded values fit in exactly W FLMM1024 registers", and
warns that larger chunks compress worse (the bit width is set by the
widest value in the chunk) and coarsen scan granularity. **1024 values
per block, at every bit width.**

### Step 2 — why the sequential layout cannot vectorise

> **In:** the "horizontal" layout — value 1's bits immediately follow
> value 0's, and so on.
> **Out:** a decode loop with a serial dependency and a data-dependent
> shift amount, i.e. both of README §2's autovectorisation failures at
> once.

Two things break. Values straddle word boundaries (a 3-bit value can
start at bit 62 of a `uint64` and finish in the next word), and — the
fatal one — value *i* sits at bit position `i·W mod 64`, so each
decode step's shift amount is a function of where the previous one
ended:

```
 3-bit values packed horizontally in a uint64:
 |v0 |v1 |v2 |v3 |v4 ... v20|v21 <- spans the word boundary
 decode v21: load TWO words, shift both, OR, mask   <- branchy, serial
 and lane i+1's shift depends on lane i's end       <- unvectorisable
```

The paper's §1.1 lists this under "Value-interleaving": the naive
layouts lead to "lack of parallel work and unused lanes or expensive
compensating actions such as PERMUTE and BITSHUFFLE". SIMD lanes want
to execute the identical operation on independent data; horizontal
packing guarantees neither.

### Step 3 — the fix: round-robin the values over 1024/T lanes

> **In:** 1024 W-bit values and a chosen lane width T ∈ {8,16,32,64}.
> **Out:** the same values distributed over S = 1024/T lanes so that
> every lane applies the *same* shift and mask at every step.

FastLanes targets a **virtual** register. §1.1: "we preempt further
widening of SIMD registers and propose a layout optimized for a
virtual 1024-bits register FLMM1024 that gets the best performance out
of any existing ISA, and even from scalar code."

The layout inside that register is the load-bearing idea, and it is
**not** bit-planes. §1.1 again: FastLanes "distributes all logically
subsequent e.g., 3-bit values round-robin over 128 separate 8-bit
lanes." So the unit that is spread is the **whole value**, not one of
its bits. §2.1 fixes the parameters: "To maximize decoding performance
we use the smallest lane-width that fits that, i.e. 8-bits (T = 8),
and therefore we have 128 (S = 1024/T = 128) lanes in our FLMM1024
word."

Derive the placement rule and then check it against the paper's own
figure:

```
 T = 8  -> S = 1024 / T = 128 lanes per FLMM1024 word
        -> each lane holds 1024 / S = 8 of the 1024 values
        -> lane s holds values  s, s+128, s+256, ..., s+896
        -> those 8 values need 8 * W = 24 bits, but a lane in ONE
           word is only T = 8 bits, so a lane's 24 bits are spread
           over B = 1024*W/1024 = W = 3 consecutive FLMM1024 words

 lane 0, concatenated across the 3 words (24 bits), W = 3:
   bit offset  0  3  6  9 12 15 18 21
   value     | 0|128|256|384|512|640|768|896|
   word boundaries fall at bit 8 and bit 16
     -> the value at offset 6..8  (position 256) is SPLIT: 2 bits in
        word 0, 1 bit in word 1
     -> the value at offset 15..17 (position 640) is SPLIT: 1 bit in
        word 1, 2 bits in word 2
```

That is exactly Figure 1's caption: "In the first word, only the first
two bits (yellow,pink) of the value at position 256 fit, so it is
continued in the second word (blue bit). The value at position 640 is
also split. This happens in all lanes." If your arithmetic reproduces
256 and 640 you have the layout right; if it does not, re-read the
round-robin rule before going further.

The crucial consequence: the split happens **in all lanes, at the same
bit offset**. Every lane is doing the identical work, so the fix-up is
one extra shift and one extra OR applied to the whole register — never
a permute, never a lane-crossing move.

### Step 4 — the kernel: a pseudo-ISA of six operations

> **In:** the interleaved layout of Step 3.
> **Out:** a straight-line kernel of loads, masked shifts and ORs,
> generated once per (W, T) pair, with no branches and no shuffles.

Listing 1 (§2.2) defines the whole instruction set on FLMM1024:
`LOAD<T>`, `STORE<T>`, `AND_LSHIFT<T>`, `AND_RSHIFT<T>`, `AND<T>`,
`OR<T>`, `XOR<T>`, `ADD<T>`, `SET<T>`. §2.2 explains the choice:
"FastLanes only uses simple operators, such as load/store,
left/right-shift, and/or/xor, addition and set instructions; supported
for all lane-widths, T ∈ {8, 16, 32, 64}… This instruction set can be
trivially mapped to intrinsics in all previously mentioned thinner
ISAs, just by using multiple identical instructions on independent
registers."

Listing 2 is the W = 3, T = 8 unpack kernel in that pseudo-ISA. Read
it against the bit offsets you just derived:

```
 Listing 2 (paper p. 2134), lines 1-15, abridged:
  1  uint<8> MASK1 = (1<<1)-1, MASK2 = (1<<2)-1, MASK3 = (1<<3)-1;
  3  r0 = LOAD<8>(in+0);
  4  r1 = AND_RSHIFT<8>(r0,0,MASK3); STORE<8>(out+0,r1);   <- offset 0
  5  r1 = AND_RSHIFT<8>(r0,3,MASK3); STORE<8>(out+1,r1);   <- offset 3
  6  r1 = AND_RSHIFT<8>(r0,6,MASK2);                       <- offset 6, 2 bits
  7  r0 = LOAD<8>(in+1); STORE(out+2,OR<8>(r1,
  8      AND_LSHIFT<8>(r0,2,MASK1)));                      <- + 1 bit, <<2
  9  r1 = AND_RSHIFT<8>(r0,1,MASK3); STORE<8>(out+3,r1);   <- offset 9
 10  r1 = AND_RSHIFT<8>(r0,4,MASK3); STORE<8>(out+4,r1);   <- offset 12
 11  r1 = AND_RSHIFT<8>(r0,7,MASK1);                       <- offset 15, 1 bit
 12  r0 = LOAD<8>(in+2); STORE(out+5,OR<8>(r1,
 13      AND_LSHIFT<8>(r0,1,MASK2)));                      <- + 2 bits, <<1
 14  r1 = AND_RSHIFT<8>(r0,2,MASK3); STORE<8>(out+6,r1);   <- offset 18
 15  r1 = AND_RSHIFT<8>(r0,5,MASK3); STORE<8>(out+7,r1);   <- offset 21
```

Every shift constant is an immediate. Lines 6-8 and 11-13 are the two
splits you predicted, stitched with one `AND_LSHIFT` and one `OR`
rather than by padding — and note that the two halves come from
*different loads of the same lane position*, so nothing crosses a lane.
The paper generates 116 such kernels statically, one for each
(W, T) with W < T and T ∈ {8,16,32,64} (§2.2, above Listing 2).

Now count what the kernel costs, because this is the number the title
is made of:

```
 Listing 2 per 1024 values:  3 LOAD + 8 STORE + 8 AND_RSHIFT
                           + 2 AND_LSHIFT + 2 OR = 23 FLMM1024 ops

 mapping FLMM1024 (1024 bits) onto real registers:
   AVX-512 (512-bit): 1024/512 = 2 real instrs per FLMM1024 op
                      23 * 2  =  46 instrs -> 1024/46  = 22.3 values/instr
   NEON    (128-bit): 1024/128 = 8 real instrs per FLMM1024 op
                      23 * 8  = 184 instrs -> 1024/184 =  5.6 values/instr
   uint64 scalar    : 1024/64  = 16 real instrs per FLMM1024 op
                      23 * 16 = 368 instrs -> 1024/368 =  2.8 values/instr
```

Compare with §1.1's headline: decoding "delivers a vector of 1024
tuples at-a-time, in sometimes as little as 17 CPU cycles (an
astonishing 70 values per CPU core cycle)". 1024/17 = 60.2, so the "70
values per cycle" is the best point of Figure 8 rather than the 17-cycle
case; both are W = 8 on the widest machine in Table 2. Your Mac is the
middle row of the arithmetic above.

Here is the same skeleton in Rust, so you can see the shape the
autovectoriser is being handed:

```rust
// ILLUSTRATION — not quoted from the paper; the real kernel is Listing 2,
// p. 2134, whose control flow this reproduces. Compare with the topic's own
// scalar decoder at topics/17-simd/experiments/src/unpack.rs:7 — that one is
// horizontal, and this one is interleaved.
fn unpack_interleaved(words: &[[u64; 16]], w: u32, out: &mut [[u64; 16]]) {
    let mask = (1u64 << w) - 1;
    let (mut word, mut shift) = (0usize, 0u32);
    for group in out.iter_mut() {
        for lane in 0..16 {              // <- the vectorised dimension: 16 u64
            group[lane] = (words[word][lane] >> shift) & mask;
        }
        shift += w;
        if shift + w > 64 { word += 1; shift = 0; }
        // the real kernel stitches the straddling value with one extra
        // AND_LSHIFT + OR instead of restarting the shift; still no branch
        // inside the lane loop, and still the same shift for ALL lanes
    }
}
```

The inner loop has a compile-time trip count, no lane-dependent index,
no branch, and one shift amount for all 16 lanes. That is the entire
list of things an autovectoriser needs. §3.1's answer to Q4: "clang++
can auto-vectorize our Scalar code, matching the performance of
explicit intrinsics — denoted SIMD", and the recommendation that
follows it: "when incorporating FastLanes in future systems, we
recommend just using the Scalar code paths."

### Step 5 — the Unified Transposed Layout, and what it buys DELTA

> **In:** a DELTA-encoded column, whose decode is a 1024-long serial
> prefix sum, and a table whose columns have different widths.
> **Out:** one tuple order that works for every lane width, and a
> per-lane chain of length T instead of 1023.

§2.3 states the dependency problem and its fix. In the default layout
"adding the values at position 0 and position 1 correspond to different
lanes"; the transposed layout stores values out of order — "The order
for the first 16 values here is 0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14,
3, 7, 11, 15" — and §2.3 reports the payoff for 16 values in a 128-bit
register: "only 4 additions are needed."

Do the general version:

```
 sequential DELTA over 1024 values:
   chain = 1023 dependent adds, each waiting on the previous
 transposed, S = 1024/T lanes each carrying its own base:
   values per lane = 1024 / S = 1024 / (1024/T) = T
   chain = T dependent adds, S of them running in parallel

   T = 64 -> chain 64,  1023/64 = 16.0x shorter
   T = 32 -> chain 32,  1023/32 = 32.0x shorter
   T =  8 -> chain  8,  1023/8  = 127.9x shorter
```

That is the same move as the eight-accumulator dot product this topic
measures (`reading-simsimd.md`, and `notes.md`'s 10.89 → 42.12 GB/s):
break one long dependency chain into many short independent ones by
restructuring the data rather than the instructions.

§2.4 then solves the problem that makes it usable in a real scan.
Different columns have different widths, "and different columns will
have different widths. However, when we reorder tuples, we should use
the same order for all columns, because a scan needs to create a
consistent stream of tuples." The construction: "The basic building
block are transposed tiles of 8x16 values. We have eight such tiles for
each vector of 1024 tuples", ordered **04261537**, with DELTA
processing order per width:

```
 §2.4, verbatim processing orders:
   8-bit  : bases -> 04261537
   16-bit : bases -> 0426 -> 1537
   32-bit : bases -> 04 -> 15 -> 26 -> 37
   64-bit : bases -> 0 -> 1 -> .. -> 7
```

§2.4 proves 04261537 is the *only* order with the two required
properties (start at tile 0; successive SIMD operations touch directly
subsequent tile numbers in the same lane position). Read that proof —
it is nine lines and it is the answer to "why this permutation". One
caution when you do: the running text on p. 2137 writes "04261357"
once, while the abstract, §1.1 and the proof's conclusion all give
**04261537**. The proof is the authority; the single "1357" is a typo.

### Step 6 — results, with their measurement conditions attached

> **In:** the paper's Table 2 platforms and §3.1/§3.2 methodology.
> **Out:** which numbers describe a CPU kernel, which describe a query,
> and which of them can be expected on this Mac.

Table 2 lists six machines. Two matter here: **Intel Ice Lake 8375C at
3.5 GHz with AVX-512**, which produces most of the headline figures,
and **Apple M1 at 3.2 GHz with 128-bit NEON** — the closest thing in
the paper to the machine you are reading on. §3.1 also notes that on
Graviton3 "SVE is slower than NEON", so every ARM number in the paper
is a NEON number.

The methodology matters more than usual. §3.1: "These micro-benchmarks
aim to characterize pure CPU cost and decompress a single vector 30M
times; hence **all data is L1 resident**." The scalar baselines were
de-vectorised on purpose with `-O3 -mno-sse -fno-slp-vectorize
-fno-vectorize`. So the micro-benchmark speedups are *ALU* ratios, not
bandwidth ratios:

| claim | value | where |
|---|---|---|
| SIMD vs de-vectorised Scalar | 40×–70× at T = 8, 3×–4× at T = 64 | §3.1 (Q1), Fig. 8 |
| `Scalar_T64` vs Scalar | 64/T ×, i.e. 8× at T = 8 | §3.1 (Q3), Fig. 8/9 |
| autovectorised Scalar vs intrinsics | matches | §3.1 (Q4) |
| peak decode rate | 70 values per core cycle at W = 8 | §3.1 (Q1) |
| interleaving's cost to plain scalar | none — "performance is equal to the naive horizontal layout" | §3.1, Fig. 9 |
| M1 specifically | "just 128-bit NEON, but clearly has more instruction level paralellism"; "In terms of scalar performance, M1 tops Ice Lake clock-for-clock" | §3.1 |

The honest end-to-end claim is **not** "decoding is free". It is §3.2's
crossover, measured with `SELECT SUM(COL) FROM TAB` over `10 · 2^28`
uint32 values (10 GB, RAM-resident) on Ice Lake, from Figure 12's
caption: "The crossover point where decompressing scans (plots)
outperform plain array scans (horizontal lines), moves from a minimal
compression ratio of 4x (≈8bits) with Scalar decoding to just 25%
compression (≈24bits) with FastLanes… FastLanes can then improve
end-to-end performance up to 7x vs. uncompressed and 4x vs. scalar."

Read that as the real result: decoding is not free, it is cheap enough
that *almost any* compression now pays for itself in a scan. Figure 11
adds the last few percent by fusing bit-unpacking with the FOR / DELTA
/ DICT / RLE decode, which removes an intermediate STORE + LOAD.

### Step 7 — our baby version: `unpack.rs`

> **In:** this topic's 4-bit unpacking bench, which keeps the
> horizontal layout.
> **Out:** an understanding of exactly which width family lets you get
> away with that, and what FastLanes buys at every other width.

```rust
// topics/17-simd/experiments/src/unpack.rs:7-14 — the provided scalar rung
     7  pub fn unpack4_scalar(packed: &[u8], out: &mut Vec<u32>) {
     8      out.clear();
     9      out.reserve(packed.len() * 2);
    10      for &b in packed {
    11          out.push((b & 0x0F) as u32);
    12          out.push((b >> 4) as u32);
    13      }
    14  }
```

That is the horizontal layout of Step 2 — and it is fine, because W = 4
divides 8. No value ever straddles a byte, so the shift amounts are the
constant pair (0, 4) rather than a running position. The whole family
`W ∈ {1, 2, 4, 8}` has this property; `W = 3, 5, 6, 7` does not, and
that is precisely the gap FastLanes' interleaving closes.

`notes.md` records this rung at **10.20 GB/s of output** (provided
rungs, release, Apple Silicon, measured 2026-07-10). Before you write
the NEON version, predict it, then reconcile with the paper: if
clang has already autovectorised the loop above, the intrinsics rung
should win little or nothing — which is FastLanes' own Q4 result
arriving on your desk.

## How to read the paper (with the concepts in hand)

- **§2.1 + Figure 1 — read first, with a pencil.** Reproduce Step 3's
  offset table for W = 3, T = 8 and confirm you get positions 256 and
  640 as the split values. Do not move on until you do; §2.2 onwards is
  bookkeeping on top of this.
- **§2.2, Listing 1 and Listing 2.** Nine pseudo-instructions and one
  15-line kernel. Match every shift constant in Listing 2 to a bit
  offset from your table.
- **§2.3 then §2.4.** §2.3 for why transposition breaks the DELTA
  chain (and the "only 4 additions" figure), §2.4 for why one order has
  to serve every column width, plus the nine-line uniqueness proof of
  04261537.
- **§3.1 — read the methodology paragraph before the results.** "All
  data is L1 resident" and the `-fno-vectorize` flags on the baseline
  decide what the 40×–70× means.
- **§3.2 and Figure 12** are the numbers to quote when someone asks
  whether compression pays: the crossover moves from 4× compression to
  25 %.
- The [FastLanes repo](https://github.com/cwida/FastLanes) is optional;
  it is not in this repo's pin table, and the paper's kernels are
  self-contained.

## Questions for notes.md

1. Redo Step 3's offset table for W = 5, T = 8. How many FLMM1024
   words does one lane's values span, how many of the 8 values in a
   lane are split across a word boundary, and how many extra
   `AND_LSHIFT` + `OR` pairs does the kernel therefore need compared
   with the W = 3 case's two?
2. Step 1 fixes the block at 1024 values. Footnote 2 in §2.1 gives two
   reasons against making it larger. State both, and say which one
   would bite a Cypher property scan hardest.
3. Interleaved decode reads W words that are 128 bytes apart in the
   T = 8 case. Is that stride still prefetcher-friendly (topic 13)?
   Compute the number of distinct cache lines one 1024-value decode
   touches at W = 3 and at W = 32.
4. Step 5 gives the chain length as T. On 128-bit NEON a `uint64` lane
   width means 2 lanes per physical register. Work out how many
   physical NEON registers one FLMM1024 DELTA step occupies at
   T = 64, and whether that fits the 32 architectural `v` registers.
5. Random access to value *i* now needs the lane index `i mod S` and
   the within-lane index `i / S`, plus up to two word reads if the
   value is split. Write the formula, then say why an analytic scan
   never pays it (topic 12's block-granularity access).
6. For M17's "SIMD-ize one topic 12 decoder": ours is W = 4,
   horizontal, measured at 10.20 GB/s of output. Predict the NEON rung
   *before* running the bench, then reconcile your result with §3.1's
   Q4 claim that autovectorised scalar already matches intrinsics.

## Done when

Answer each before unfolding it.

- [ ] You can explain why the horizontal bit-packed layout cannot vectorise, naming both failures.

  <details><summary>Answer</summary>

  Value *i* starts at bit `i·W mod 64`, so the shift amount for lane
  *i+1* depends on where lane *i* ended — a serial dependency — and
  values that straddle a word boundary need a second load, a second
  shift and an OR, which is a data-dependent branch. SIMD lanes must
  execute the identical operation on independent data; horizontal
  packing supplies neither. §1.1 lists the usual escape routes —
  PERMUTE and BITSHUFFLE — as the expensive compensating actions the
  layout is designed to avoid.

  </details>

- [ ] You can state what FastLanes interleaves, and place a specific value in a specific lane and word.

  <details><summary>Answer</summary>

  It interleaves **whole values**, not bits: §1.1 says it "distributes
  all logically subsequent e.g., 3-bit values round-robin over 128
  separate 8-bit lanes." With T = 8, S = 1024/T = 128 lanes, each lane
  holds 1024/S = 8 values — lane *s* holds positions
  s, s+128, …, s+896 — and those 8·W = 24 bits span W = 3 consecutive
  FLMM1024 words. In lane 0 the values sit at bit offsets 0, 3, 6, 9,
  12, 15, 18, 21, so positions 256 (offset 6) and 640 (offset 15)
  straddle the word boundaries at bits 8 and 16 — which is exactly what
  Figure 1's caption says.

  Anyone who tells you plane *b* holds bit *b* of many values is
  describing a bit-plane / BITSHUFFLE layout, which is not this.

  </details>

- [ ] You can name the block size and say what fixes it.

  <details><summary>Answer</summary>

  1024 values, at every bit width. §2.1 footnote 2: "a chunk of 1024·W
  (bit-width) encoded values fit in exactly W FLMM1024 registers."
  Larger chunks are rejected for two reasons given there — worse
  compression, because the bit width is set by the value domain of the
  whole chunk, and a coarser minimum vector size "imposed to the scan
  subsystem".

  </details>

- [ ] You can explain what the Unified Transposed Layout is for, and why the order is 04261537 rather than anything else.

  <details><summary>Answer</summary>

  A scan reads several columns of different widths and must emit one
  consistent tuple stream, so all columns need the *same* reordering
  (§2.4). The building block is eight transposed 8×16 tiles per
  1024-tuple vector. The order must start at tile 0 (the 64-bit case
  processes one tile at a time and the header holds bases for tile 0)
  and must make successive SIMD operations touch directly subsequent
  tile numbers in the same lane position; §2.4's proof shows those two
  requirements admit **04261537** alone. Processing orders: 8-bit
  `04261537`; 16-bit `0426` then `1537`; 32-bit `04, 15, 26, 37`;
  64-bit `0…7`.

  </details>

- [ ] You can quote the paper's speedups *with* the conditions that produced them, and say which one to cite for "does compression pay?".

  <details><summary>Answer</summary>

  §3.1's 40×–70× (T = 8) down to 3×–4× (T = 64) is a **pure-CPU,
  L1-resident** micro-benchmark that decompresses one vector 30M times,
  against a baseline compiled with `-mno-sse -fno-slp-vectorize
  -fno-vectorize`. `Scalar_T64` is 64/T× faster than Scalar; clang++
  auto-vectorises the scalar path to intrinsic speed (Q4); peak is 70
  values per cycle at W = 8. Most figures are Ice Lake 8375C at
  3.5 GHz with AVX-512; the M1 row of Table 2 is 128-bit NEON at
  3.2 GHz.

  For "does compression pay?" cite **§3.2 / Figure 12** instead: on a
  10 GB RAM-resident `SELECT SUM(COL)`, the crossover where a
  decompressing scan beats a plain array scan moves from 4×
  compression (≈8 bits) with scalar decoding to just 25 % compression
  (≈24 bits) with FastLanes, and up to 7× vs uncompressed / 4× vs
  scalar at 8 threads.

  </details>

- [ ] You can say what random access to value *i* now costs, and why the scan does not care.

  <details><summary>Answer</summary>

  Value *i* is in lane `i mod S` at within-lane index `i / S`, at bit
  offset `(i / S) · W` inside that lane's `T·W`-bit run — so up to two
  word reads plus a shift/mask/OR, versus one or two reads in the
  horizontal layout, and the address arithmetic is no longer monotone
  in *i*. An analytic scan decodes whole 1024-value blocks and consumes
  them in whatever order they arrive; §2.3 argues the reordering is
  free in the relational setting because "query operator semantics
  typically do not depend on order", and where it does the order can be
  restored or carried in a selection vector.

  </details>

- [ ] You wrote answers to all six questions in notes.md, and can state how `unpack.rs` differs from the paper's layout.

  <details><summary>Answer</summary>

  `unpack4_scalar` (`experiments/src/unpack.rs:7-14`) is horizontal, not
  interleaved — and gets away with it because W = 4 divides 8, so no
  value straddles a byte and the shifts are the constants 0 and 4. The
  whole family W ∈ {1,2,4,8} is like this; W ∈ {3,5,6,7} is where the
  horizontal layout's running bit position reappears and where
  FastLanes' interleaving earns its keep. `notes.md` records the scalar
  rung at **10.20 GB/s of output** (Apple Silicon, 2026-07-10).

  </details>

## References

**Papers**
- Azim Afroozeh, Peter Boncz — "The FastLanes Compression Layout:
  Decoding > 100 Billion Integers per Second with Scalar Code",
  *PVLDB* 16(9), 2023, pp. 2132-2144.
  <https://www.vldb.org/pvldb/vol16/p2132-afroozeh.pdf> — §2.1 and
  Figure 1 for the interleaved layout, §2.2 with Listings 1-2 for the
  kernel, §2.3-§2.4 for the transposed and Unified Transposed layouts
  (including the 04261537 uniqueness proof), §3.1 for the L1-resident
  micro-benchmarks and Table 2 for the hardware, §3.2 and Figure 12 for
  the end-to-end crossover.

**Code**
- [FastLanes](https://github.com/cwida/FastLanes) — CWI's reference
  implementation. Not pinned in `resources/codebases.md`, so nothing in
  this guide is anchored to it; the paper's listings are self-contained.
- This topic's own `experiments/src/unpack.rs` — the horizontal W = 4
  decoder to contrast against.
