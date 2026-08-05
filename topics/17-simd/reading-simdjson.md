# simdjson: parsing without branches

Parsing — the most branchy code imaginable — rebuilt as branch-free
bitmask algebra over 64-byte blocks. Before the paper and the headers,
this chapter builds the tricks one at a time: why branches kill
parsers, how masks replace them, the operator table, the prefix-XOR
ladder, the backslash-parity subtraction, the over-write/under-advance
flatten, and NEON's emulated compress. Every trick transfers to a DB
engine: RESP framing, CSV ingest, LIKE prefilters.

Every anchor below is simdjson at the pinned revision
`simdjson/simdjson@c783809` (`resources/codebases.md`), quoted with the
line numbers the code occupies in that revision. Your machine is
aarch64, so the files that actually run are `include/simdjson/arm64/`
and `src/arm64.cpp` — and several of them do **not** work the way the
x86 files (or the 2019 paper) describe. Where they differ, this guide
gives you both and says which one your CPU executes.

## The problem in one sentence

A conventional JSON parser asks a data-dependent question of every
input byte, so it takes a mispredicted branch on unpredictable data
roughly once per byte; simdjson replaces the per-byte question with
per-64-byte **bitmask** arithmetic that has no data-dependent branches
at all, and only lets the branchy code see the few bytes that matter.

How expensive is one such branch? Rather than quoting a number, use
this topic's own measurement. `notes.md`'s filter sweep (Apple
Silicon, measured 2026-07-10, N = 4M f32 = 16,777,216 input bytes) has
the branchy compaction loop at **1.19 GB/s** at 50 % selectivity and
the branchless one at **12.73 GB/s**:

```
 branchy    16,777,216 B / 1.19e9  B/s = 14.10 ms
 branchless 16,777,216 B / 12.73e9 B/s =  1.32 ms
 per element (4,194,304 of them): 3.361 ns  vs  0.314 ns
 gap = 3.047 ns/element
 clock (derived, see reading-simsimd.md Step 2): >= 4.08 GHz
 3.047 ns x 4.08 GHz          =  12.4 cycles per element
 half the elements mispredict at 50 % sel ⇒ ≈ 25 cycles per miss
   (an upper bound: it charges the entire gap to mispredicts)
```

(The clock is not asserted; it is solved for from this topic's own
naive dot rung — 10.89 GB/s over 8 bytes per element-pair on a
3-cycle FMA chain — in `reading-simsimd.md` Step 2. The host is an
Apple M5, `sysctl -n machdep.cpu.brand_string`.)

The paper is deliberately vaguer — §1 says only "several cycles of
penalty due to a mispredicted branch" — and that vagueness is correct,
because the penalty is a property of the pipeline you are on. What is
not vague is the shape: an order of magnitude, and it lands squarely
in the middle of the selectivity range.

## The concepts, step by step

### Step 1 — why parsers are slow: one branch per unpredictable byte

> **In:** a byte stream and a parser written as `if (c == '"') … else
> if (c == '{') …`.
> **Out:** an understanding of why that shape caps at hundreds of
> MB/s no matter how wide the machine's vectors are.

**SIMD** (single instruction, multiple data) is one CPU instruction
operating on a whole vector of values at once; **NEON** is ARM's SIMD
instruction set, with 128-bit vectors — 16 bytes, or four f32, per
instruction. SIMD is useless for a textbook parser, because a textbook
parser is a chain of data-dependent branches. The CPU predicts each
one and speculates past it; JSON bytes are effectively random from the
predictor's point of view, so it is wrong constantly, and each miss
throws away the pipeline's speculative work.

That is the same disease as this topic's README §2 failure #2
("data-dependent control flow"), at one-byte granularity — and it is
the same curve `notes.md` measures for `filter`, where the branchy
lane collapses 9× at 50 % selectivity while the branchless lane stays
flat within ±5 % across the whole sweep.

The paper frames the goal as a *cost model* rather than a speed:
§3 promises "a fixed number of instructions per input byte" for the
quoted-string detection, with no data-dependent branch at all. Fixed
cost per byte is what makes a parser's throughput a property of the
machine instead of a property of the document.

### Step 2 — the fix: classify 64 bytes into bitmasks, branch once per block

> **In:** 64 input bytes.
> **Out:** a handful of `uint64_t` masks, bit *i* answering one yes/no
> question about byte *i*, computed with zero data-dependent branches.

The core move: process input in 64-byte blocks and turn every per-byte
question into a **bitmask** — a `uint64_t` in which bit *i* answers the
question for byte *i*. "Is byte *i* a quote?" becomes one bit; the
answers for a whole block become one register; and questions about
bytes become bit arithmetic, which has no branches.

The architecture splits in two:

```
 stage 1: structural indexing (SIMD, branch-free)
   64 input bytes -> classify -> masks (one bit per byte):
   op {}[]:, whitespace, backslash, quote
   -> escape parity -> real quotes -> in-string regions
   -> structural positions -> flatten bits into an index array
 stage 2: tape building (branchy, but only visits flagged bytes)
   walk the structural indexes, parse numbers/strings, emit tape
```

Why 64 and not NEON's 16? Because the mask is the unit of work, and a
mask is a general-purpose register. The classifier still runs at
16 bytes per `vqtbl1q_u8`; it just runs four times and glues the four
16-bit results into one 64-bit mask (Step 3). The block size is set by
the *mask* width, not the *vector* width — which is why the arm64
kernel and the westmere (SSE) kernel both use 64 while haswell (AVX2)
and icelake (AVX-512) use 128:

| kernel | block size | anchor |
|---|---|---|
| arm64 (your machine) | 64 | `src/arm64.cpp:126` — `stage1::json_structural_indexer::index<64>` |
| westmere (SSE4.2) | 64 | `src/westmere.cpp:140` |
| haswell (AVX2) | 128 | `src/haswell.cpp:135` |
| icelake (AVX-512) | 128 | `src/icelake.cpp:181` |

The 128-byte variant is not a wider mask — it is *two* 64-byte blocks
pipelined together for instruction-level parallelism, which
`json_structural_indexer.h:220-229` (`step<128>`) spells out and the
PERF NOTES at `json_structural_indexer.h:176-191` justify.

### Step 3 — classification: how arm64 really does it (not nibble tables)

> **In:** four `uint8x16_t` chunks holding 64 bytes.
> **Out:** two `uint64_t` masks — `op` (is byte *i* one of `,:[]{}`?)
> and `whitespace` — costing a fixed 8 vector instructions each.

The tool is `vqtbl1q_u8`: NEON's table-lookup instruction, which uses
each byte of one vector as an index into a **16-entry table** (a LUT)
held in another vector — sixteen parallel lookups in one instruction.
A byte's value is 0–255 and the table has 16 entries, so *something*
has to shrink the index.

The famous textbook answer is "split the byte into **nibbles** (4-bit
halves) and AND two lookups." That is what the paper describes in
§3.1.2, and it is what the x86 kernel does (`src/haswell.cpp:43-72`
folds the high bits with `| 0x20` and looks up the low nibble). It is
**not** what your kernel does. `src/arm64.cpp:40-80` uses a single
table and an offset shift:

```c
// src/arm64.cpp:41-56 — json_character_block::classify (op half only)
    41    const uint8x16_t op_table = simd8<uint8_t>(
    42      0xff, 0, ',', ':', 0, '[', ']', '{', '}', 0, 0, 0, 0, 0, 0, 0
    43    );
// ... 44-52: ws_table and the four 16-byte chunks d0_0 .. d0_3 ...
    53    const uint8x16_t match_op_0 = vceqq_u8(vqtbl1q_u8(op_table, vshrq_n_u8(vaddq_u8(d0_0, vdupq_n_u8(3)), 4)), d0_0);
```

Read line 53 inside out: add 3 to every byte, shift right by 4 (so the
index is `(b + 3) >> 4`, a value in 0–15), look that up in `op_table`,
and compare the *result* back against the original byte. A lane
matches only if the table entry for its bucket **is** the byte itself.

Work the arithmetic for all six structural characters, and for one
near miss:

```
 byte   hex   b+3   (b+3)>>4   op_table[i]   equals b?
 ','    0x2C   47      2          ','          yes
 ':'    0x3A   61      3          ':'          yes
 '['    0x5B   94      5          '['          yes
 ']'    0x5D   96      6          ']'          yes
 '{'    0x7B  126      7          '{'          yes
 '}'    0x7D  128      8          '}'          yes
 '-'    0x2D   48      3          ':' (0x3A)   NO   <- rejected
 'a'    0x61  100      6          ']' (0x5D)   NO   <- rejected
```

The `+3` is the whole trick: without it `[` (0x5B) and `]` (0x5D) land
in the same bucket 5, and `{` (0x7B) and `}` (0x7D) both land in 7.
Adding 3 pushes `]` into bucket 6 and `}` into bucket 8, giving each of
the six characters a private bucket. The final `vceqq_u8` is what makes
the collisions harmless: every other byte that shares a bucket gets
compared against a character it is not.

Whitespace uses a different instruction on line 58 — `vqtbx1q_u8`,
table-lookup-*with-fallback*: indices ≥ 16 leave the destination
untouched instead of writing zero. So `ws_table` (`arm64.cpp:44-46`,
`0xff` at indices 9, 10 and 13) catches `\t` (0x09), `\n` (0x0A) and
`\r` (0x0D) directly by byte value, and space (0x20, index 32 ≥ 16)
falls through to the destination operand — which is
`vceqq_u8(d, vdupq_n_u8(' '))`, computed for exactly that purpose.
One instruction, four whitespace characters, no nibbles.

Then 64 lanes of `0x00`/`0xFF` have to become 64 bits. x86 has one
instruction for this (`PMOVMSKB`); NEON does not, so `arm64.cpp:63-77`
ANDs each lane with a repeating `0x01,0x02,…,0x80` pattern and folds
with a tree of `vpaddq_u8` (pairwise add): 4 ANDs + 4 pairwise adds
per mask, halving the lane count each time — 64 lanes → 32 → 16 → 8
bytes = one `uint64_t`, extracted at line 76. Count it: **8 vector
instructions to produce one 64-bit mask**, against x86's four
`PMOVMSKB` plus three shift-or. This is the third distinct answer to
"one bit per lane" you will meet in this topic; hashbrown's and
memchr's are the other two, and neither matches this one.

### Step 4 — quote parity: prefix-XOR, and why it is *not* PMULL here

> **In:** `quote`, a mask with a 1 at every real (unescaped) quote.
> **Out:** `in_string`, a mask with a 1 at every byte that lies inside
> a string — computed without a loop over the 64 positions.

Knowing where quotes are is not enough: a byte is inside a string if
an **odd** number of quotes precede it. That is a running parity over
64 positions — a serial scan, the exact shape SIMD cannot do.
`prefix_xor(m)` computes, for every bit position, the XOR of all lower
bits, which is precisely "odd number of quotes so far".

The paper's answer (§3.1.1) is one carry-less multiply: multiplying the
mask by all-ones in GF(2) makes each output bit the XOR of all input
bits below it, and the paper quotes `pclmulqdq` at 7 cycles latency,
1 per cycle throughput on Skylake. That is a real instruction and the
x86 kernels use it —
`include/simdjson/westmere/bitmask.h:22` (and the haswell and icelake
copies) call `_mm_clmulepi64_si128`.

Your kernel does something else, and says why:

```c
// include/simdjson/arm64/bitmask.h:17-38 — prefix_xor on aarch64
    17  simdjson_inline uint64_t prefix_xor(uint64_t bitmask) {
    19    // We could do this with PMULL, but it is apparently slow.
// ... 21-23: the vmull_p64 version, commented out ...
    24    // Analysis by @sebpop:
// ... 25-27: the eors interleave with vector code, so their latency hides ...
    28    // Also the PMULL requires two extra fmovs: GPR->FP (3 cycles in N1, 5 cycles in A72 )
    29    // and FP->GPR (2 cycles on N1 and 5 cycles on A72.)
    31    bitmask ^= bitmask << 1;
    32    bitmask ^= bitmask << 2;
    33    bitmask ^= bitmask << 4;
    34    bitmask ^= bitmask << 8;
    35    bitmask ^= bitmask << 16;
    36    bitmask ^= bitmask << 32;
    37    return bitmask;
    38  }
```

Six shift-XOR steps, because log₂ 64 = 6: after the `<< 1` step each
bit holds the XOR of itself and its neighbour; after `<< 2`, of a span
of 4; after `<< 32`, of all 64. Check the doc comment's own example at
line 15 by hand — `prefix_xor(0b00100100) == 0b00011100` — and note
that the ladder runs in **general-purpose registers**, which is exactly
@sebpop's argument: the GPR units are idle while the FP side does the
classification, so six cheap integer ops on the idle side beat one
"fast" FP instruction that needs a 3-cycle `fmov` in and a 2-cycle
`fmov` out (N1 numbers; 5 and 5 on A72).

The lesson is the one SimSIMD's FCMLA comment makes independently: a
specialised instruction has to beat the *whole sequence including the
data movement to reach it*, not just the arithmetic.

Where it is used (`src/generic/stage1/json_string_scanner.h:62-85`):

```c
// src/generic/stage1/json_string_scanner.h:62-78 — one block of string state
    62  simdjson_really_inline json_string_block json_string_scanner::next(const simd::simd8x64<uint8_t>& in) {
    63    const uint64_t backslash = in.eq('\\');
    64    const uint64_t escaped = escape_scanner.next(backslash).escaped;
    65    const uint64_t quote = in.eq('"') & ~escaped;
// ... 67-72: comment explaining the xor with the carry-in ...
    73    const uint64_t in_string = prefix_xor(quote) ^ prev_in_string;
// ... 75-77: comment ...
    78    prev_in_string = uint64_t(static_cast<int64_t>(in_string) >> 63);
```

Line 78 is the cross-block carry: an *arithmetic* right shift by 63
smears the top bit across all 64, producing 0 or `~0`, which line 73
XORs into the next block. Blocks stay independent except for one bit.

### Step 5 — the escaped-backslash problem: a subtraction, not an addition

> **In:** `backslash`, a mask of every `\` in the block.
> **Out:** `escaped`, a mask of every byte that a backslash escapes —
> so Step 4's `quote` can exclude `\"`.

Whether a quote is real depends on the **parity of the backslash run**
before it: in `\"` the backslash escapes the quote, in `\\"` the two
backslashes escape each other and the quote is real. Run-length parity
looks inherently sequential.

The paper's Fig. 3 (§3.1.1) resolves it with two *additions*, letting
an adder's carry ripple the length of each run and pop out at its end.
The shipped code does it with **one subtraction** and the constant
`ODD_BITS`:

```c
// src/generic/stage1/json_escape_scanner.h:127-142 — next_escape_and_terminal_code
   127      uint64_t maybe_escaped = potential_escape << 1;
// ... 129-133: comment — bring in all odd bits, for speed ...
   134      uint64_t maybe_escaped_and_odd_bits     = maybe_escaped | ODD_BITS;
   135      uint64_t even_series_codes_and_odd_bits = maybe_escaped_and_odd_bits - potential_escape;
// ... 137-141: comment — flip the odd bytes back ...
   142      return even_series_codes_and_odd_bits ^ ODD_BITS;
```

`ODD_BITS` is `0xAAAAAAAAAAAAAAAA` (`json_escape_scanner.h:74`) — every
odd-numbered bit. Line 135's borrow chain is the engine: subtracting
the run's own bits from a field of alternating 1s propagates a borrow
the length of the run, and where it stops encodes the run's parity.

Do it on paper in 8 bits, with `ODD_BITS = 0b10101010 = 0xAA` and bit 0
as the leftmost character:

```
 input `\\\n`  (odd run of 3, then 'n')     input `\\n` (even run of 2)
 potential_escape = 0b00000111 = 0x07       0b00000011 = 0x03
 maybe_escaped    = 0b00001110 = 0x0E       0b00000110 = 0x06
 | ODD_BITS       = 0b10101110 = 0xAE       0b10101110 = 0xAE
 - potential_esc  = 0b10100111 = 0xA7       0b10101011 = 0xAB
 ^ ODD_BITS       = 0b00001101 = 0x0D       0b00000001 = 0x01
```

Then `escaped = escape_and_terminal_code ^ backslash`
(`json_escape_scanner.h:67`, with the carry-in bit folded in):

```
 odd run:  0x0D ^ 0x07 = 0x0A = bits 1,3  -> the 2nd backslash and the 'n'
 even run: 0x01 ^ 0x03 = 0x02 = bit 1     -> only the 2nd backslash; 'n' is FREE
```

That is the whole answer: in `\\\n` the `n` is escaped, in `\\n` it is
not, and the difference fell out of one subtract. `escape` (line 68)
is the complementary mask of backslashes that *do* escape something,
and line 69 carries its top bit into the next block, exactly as Step 4
carries `in_string`.

Note the short circuit at `json_escape_scanner.h:53`: if the whole
block has no backslash, all of this is skipped. That is a
data-dependent branch — a well-predicted one, since most JSON blocks
contain no backslash at all.

### Step 6 — flatten: over-write, under-advance (and STEP is 4, not 8)

> **In:** a 64-bit `structural` mask and the block's base index.
> **Out:** `cnt` 32-bit positions appended to an array, with more than
> `cnt` slots written and only `cnt` counted.

Stage 2 wants positions, not a mask. The classic loop is
`while (bits) { *out++ = idx + ctz(bits); bits &= bits - 1; }` — and
the paper is explicit (§3.1.4) that this "introduces an unpredictable
branch; unless there is a regular pattern in our bitsets, we would
expect to have at least one branch miss for each word."

The paper's fix (Fig. 6) is to extract **8** indexes unconditionally
and overwrite the excess on the next iteration: "as long as the
frequency of our set bits is below 8 bits out of 64 we expect few
unpredictable branches," and the paper calls 8 "a heuristic based on
our experience with JSON documents."

The shipped code has moved on. It writes in steps of **4**, up to 24,
then falls back to a scalar loop:

```c
// src/generic/stage1/json_structural_indexer.h:93-121 — bit_indexer::write
    93    simdjson_inline void write(uint32_t idx, uint64_t bits) {
// ... 94-96: comment — this branch is sometimes mispredicted, sometimes vital ...
    97      if (bits == 0)
    98          return;
   100      int cnt = static_cast<int>(count_ones(bits));
   103      bits = reverse_bits(bits);          // #if SIMDJSON_PREFER_REVERSE_BITS
   108      static constexpr const int STEP = 4;
   110      static constexpr const int STEP_UNTIL = 24;
   112      write_indexes_stepped<0, STEP_UNTIL, STEP>(idx, bits, cnt);
// ... 113-119: scalar tail for cnt > 24, marked simdjson_unlikely ...
   121      this->tail += cnt;
```

Line 112 writes 4 slots at a time and only checks `cnt` every 4
(`write_indexes_stepped` at lines 71-80 recurses while
`START+STEP < cnt`); line 121 advances the cursor by the **real**
count. Slots past `cnt` hold garbage that the next block silently
overwrites — the doc comment at lines 85-86 says the buffer must be
oversized for exactly this reason. **Over-write, under-advance.**

Line 103 is another aarch64-specific choice. `write_index` has two
bodies: the ARM one at lines 44-48 uses `leading_zeroes` +
`zero_leading_bit`, the x86 one at lines 56-59 uses `trailing_zeroes` +
`clear_lowest_bit`. The comment at lines 31-43 gives the reason —
"ARM lacks a fast trailing zero instruction, but it has a fast bit
reversal instruction and a fast leading zero instruction" — so the
mask is reversed **once** (line 103) and then consumed from the top.
`include/simdjson/arm64/bitmanipulation.h:76` is what sets
`SIMDJSON_PREFER_REVERSE_BITS` to 1 for your build.

Now compute whether STEP=4 is enough for a real document. The paper's
Table 5 gives bytes-per-structural for each test file; convert to bits
set per 64-byte mask by dividing 64 by it:

```
 twitter        11.4 B/structural -> 64/11.4 =  5.6 set bits per 64-bit mask
 gsoc-2018      43.9              -> 64/43.9 =  1.5
 citm_catalog   12.7              -> 64/12.7 =  5.0
 marine_ik       4.6              -> 64/4.6  = 13.9   <- exceeds 8, and 4, and needs 24
 canada          6.7              -> 64/6.7  =  9.6
```

For twitter the paper's 8-wide unconditional write covers 5.6 on
average and the shipped 4-wide covers it in two rounds with one check;
for marine_ik neither does, and the `STEP_UNTIL = 24` ceiling is what
keeps the tail loop rare. The paper's own framing (§3.1.4) is that a
wider unconditional extraction is "more expensive due to having to use
more operations, but even less likely to cause a branch miss" — the
2026 code has simply re-tuned that trade-off downward, presumably
because the check at every 4 is cheaper than 4 extra writes.

### Step 7 — compress on NEON: the missing instruction, emulated

> **In:** 16 bytes in a vector plus a 16-bit mask.
> **Out:** the *unmasked* bytes packed to the left, one 16-byte store,
> with only `16 - popcount(mask)` of them meaningful.

Sometimes stage 1 must compact surviving bytes, not just index them.
AVX-512 has `vpcompressb`/`vpcompressd` for this. NEON has nothing.
simdjson emulates it with two table lookups:

```c
// include/simdjson/arm64/simd.h:246-277 — simd8<L>::compress
   246      simdjson_inline void compress(uint16_t mask, L * output) const {
// ... 247-251: using-declarations and the two-halves comment ...
   252        uint8_t mask1 = uint8_t(mask); // least significant 8 bits
   253        uint8_t mask2 = uint8_t(mask >> 8); // most significant 8 bits
   257        uint64x2_t shufmask64 = {thintable_epi8[mask1], thintable_epi8[mask2]};
// ... 258-265: reinterpret, and add 0x08 to the second half's indices ...
   267        uint8x16_t pruned = vqtbl1q_u8(*this, shufmask);
   270        int pop1 = BitsSetTable256mul2[mask1];
   275        uint8x16_t compactmask = vld1q_u8(reinterpret_cast<const uint8_t *>(pshufb_combine_table + pop1 * 8));
   276        uint8x16_t answer = vqtbl1q_u8(pruned, compactmask);
   277        vst1q_u8(reinterpret_cast<uint8_t*>(output), answer);
```

Read the semantics off the doc comment at lines 238-241 before the
code, because they are inverted from what you expect: it "copies to
`output` all bytes corresponding to a **0** in the mask", and "only the
first `16 - count_ones(mask)` bytes of the result are significant but
16 bytes get written". A set bit means *drop this byte*, and the store
is unconditionally 16 wide — over-write, under-advance again, this time
in bytes rather than indexes.

Why two halves? Because of the table size. Work it out:

```
 one table for all 16 mask bits: 2^16 entries x 16-byte shuffle pattern
   = 65,536 x 16 B = 1,048,576 B = 1 MB   (blows every cache)
 two tables of 8 bits each:      2^8 entries x 8-byte pattern
   = 256 x 8 B = 2,048 B = 2 KB           (internal/simdprune_tables.h:11)
 plus the stitcher pshufb_combine_table[272] bytes  (:13)
 plus the doubled popcount BitsSetTable256mul2[256] (:11)
```

Two 8-bit halves cost 512× less table for one extra shuffle and one
extra lookup. Line 267 prunes each half independently (which leaves a
gap in the middle, because half 1 kept only `pop1/2` bytes); line 270
reads the *doubled* popcount of the low half straight out of a table
rather than computing it; line 275 uses it to index a second shuffle
table that slides half 2 down onto half 1; line 276 applies it. Two
`vqtbl1q_u8`, three table reads, one store.

`compress_halves` at `arm64/simd.h:283-299` is the 8-lane sibling,
using `vqtbl1_u8` (64-bit table) twice. It is the closest thing on
NEON to a per-8-lane compress, and it is what a `f32x4` compact in
`filter.rs` will end up shaped like — with the important difference
that for four 32-bit lanes the mask has only 16 possible values, so the
whole LUT is 16 × 16 = 256 bytes and no stitching is needed at all.

### Step 8 — what stage 2 costs, and what transfers to a DB engine

> **In:** the paper's regression model and dataset table.
> **Out:** a defensible answer to "does the branchy half kill the
> speedup?", computed rather than asserted.

Stage 2 is still branchy. The Amdahl argument is usually waved at with
"it only touches 1/8 of bytes" — which is true for exactly one file.
Table 5 of the paper gives bytes-per-structural from **4.6**
(marine_ik) to **43.9** (gsoc-2018); the paper's own §3.1.4 phrasing is
"once every 40 characters or once every 4 characters."

The paper's §4.3 regression (R² ≥ 0.99) lets you check the split
directly. On its Skylake machine, with `B` input bytes, `S` structural
characters and `F` floating-point numbers:

```
 stage 1 = 1.7*S + 0.62*B      cycles
 stage 2 =  19*F + 8.7*S + 0.31*B
 total   =  19*F +  11*S + 0.92*B
```

Run it on twitter.json (Table 5: S = 55,264, F = 1; Table 6:
B = 631,514):

```
 stage 1 = 1.7*55,264 + 0.62*631,514 =  93,948.8 + 391,538.7 =   485,487 cy
 total   = 19 + 11*55,264 + 0.92*631,514
         = 19 +   607,904 +   580,993          =             = 1,188,916 cy
 stage 1 share = 485,487 / 1,188,916 = 40.8 %
 throughput    = 631,514 B / (1,188,916 cy / 3.4e9 Hz) = 1.81 GB/s
```

Two things to take from that. First, 40.8 % matches §4.3's prose
("about half the CPU cycles per input byte — between 0.5 and 3 cycles
— are spent in stage 1"), so the branchy half is *not* a rounding
error; it is the larger half. Second, the model predicts 1.81 GB/s
where Table 10 measures **2.2 GB/s** for twitter — the regression is
fitted across all files and under-predicts this one by 18 %. Quote the
measurement, not the model.

And now the headline, with its hardware attached, because it does not
transfer to your Mac. §4.1: **Intel i7-6700 Skylake at 3.4 GHz**
(3.7 GHz turbo), DDR4-2133, GCC 9.1 with `-O3 -march=native`, Linux.
§4.5: "our parser can achieve and even surpass 2 GB/s in six
instances, and for gsoc-2018, we reach 3 GB/s." Table 10, GB/s:

| file | simdjson | RapidJSON | sajson |
|---|---|---|---|
| gsoc-2018 | 3.2 | 0.68 | 1.2 |
| citm_catalog | 2.5 | 0.72 | 1.1 |
| twitter | 2.2 | 0.55 | 0.83 |
| canada | 1.1 | 0.38 | 0.62 |
| marine_ik | 0.94 | 0.42 | 0.66 |

The spread within simdjson's own column (0.94 to 3.2, a 3.4× range on
one CPU) is the real lesson: "gigabytes per second" is a property of
the *document's* structural density as much as of the parser. Your
machine has different vector widths, a different `prefix_xor`, a
different classifier and a different flatten step — treat every number
above as the paper's, not as a prediction.

What transfers to the engine you are building:

- RESP protocol framing (M7) = structural indexing over `\r\n$*:+-`;
  the op-table trick of Step 3 generalises to any six-ish byte set.
- CSV / JSON bulk ingest = the whole pipeline.
- string-escape scanning = LIKE and regex prefilters.
- the meta-lesson: turn per-byte branches into per-block masks, then
  branch once per block — topic 11's vectorization, byte edition.

## Where each step lives in the code

Every anchor is `simdjson/simdjson@c783809`.

| anchor | step | what it is |
|---|---|---|
| `src/arm64.cpp:40-80` | 3 | `json_character_block::classify` — `(b+3)>>4` op table, `vqtbx1q_u8` whitespace, `vpaddq_u8` bit-gather |
| `src/haswell.cpp:43-72` | 3 | the x86 contrast: low-nibble table plus an OR with `0x20`, with its own false-positive note at 57-66 |
| `include/simdjson/arm64/simd.h:123-136` | 3 | `to_bitmask` — the AND-plus-`vpaddq` fold, as a reusable helper |
| `include/simdjson/arm64/bitmask.h:17-38` | 4 | `prefix_xor` — six shift-XOR steps, *not* PMULL, with the reason at 19-29 |
| `include/simdjson/westmere/bitmask.h:22` | 4 | the CLMUL version the paper describes (x86 only) |
| `src/generic/stage1/json_string_scanner.h:62-85` | 4 | `next()` — backslash → escaped → quote → `prefix_xor` → `in_string`, carry-out at 78 |
| `src/generic/stage1/json_escape_scanner.h:96-143` | 5 | `next_escape_and_terminal_code` — OR with `ODD_BITS`, subtract, XOR back |
| `src/generic/stage1/json_escape_scanner.h:50-71` | 5 | `next()` — the short circuit at 53 and the block carry at 69 |
| `src/generic/stage1/json_structural_indexer.h:93-121` | 6 | `bit_indexer::write` — `STEP = 4`, `STEP_UNTIL = 24`, `tail += cnt` |
| `src/generic/stage1/json_structural_indexer.h:44-48` | 6 | the ARM `write_index`: reverse once, then leading-zeroes |
| `src/generic/stage1/json_structural_indexer.h:209-247` | 2 | the driver loop, `step<128>` at 220 and `step<64>` at 231 |
| `src/arm64.cpp:126` | 2 | which one your CPU runs: `index<64>` |
| `include/simdjson/arm64/simd.h:246-278` | 7 | `compress` — two `thintable_epi8` halves + `pshufb_combine_table` stitch |
| `include/simdjson/arm64/simd.h:283-299` | 7 | `compress_halves` — the 8-lane sibling |
| `src/generic/stage1/utf8_lookup4_algorithm.h:44,60,88,104` | 3 | where the nibble-AND trick really lives: `byte_1_high & byte_1_low & byte_2_high` |

Reading route: `src/arm64.cpp` first, because it is short (161 lines)
and shows which generic pieces your CPU instantiates. Then
`arm64/bitmask.h` (44 lines, one function). Then the string scanner
with the paper's §3.1.1 open beside it, then the escape scanner, then
the structural indexer. `arm64/simd.h` is a toolbox — read `compress`
and `to_bitmask`, skim the rest.

## Questions for notes.md

1. Step 2 shows arm64 and westmere both using 64-byte blocks while
   haswell and icelake use 128. Since the mask is a `uint64_t` in every
   case, what does `step<128>` actually buy (read the PERF NOTES at
   `json_structural_indexer.h:176-191`), and why would that pay off on
   AVX2 but not on NEON?
2. Redo Step 3's bucket table for a RESP framing classifier over
   `\r \n $ * : + -` (0x0D, 0x0A, 0x24, 0x2A, 0x3A, 0x2B, 0x2D). Does
   any constant offset `k` give all seven a private `(b+k)>>4` bucket?
   If not, what is the smallest set of extra comparisons you need?
3. `prefix_xor` costs 6 dependent XOR-shift pairs on your machine.
   That is a 12-instruction dependency chain per 64-byte block. At
   64 bytes per block, how many bytes/s does that chain alone permit if
   each pair is 2 cycles and nothing overlaps — and why is the real
   answer higher (re-read @sebpop's note at `bitmask.h:24-29`)?
4. The compress LUT arithmetic in Step 7 assumed byte lanes. Redo it
   for `f32x4`: how many mask values, how big is the shuffle table, and
   why does the two-halves stitching disappear?
5. For M7: sketch stage-1 masks for `*3\r\n$3\r\nSET\r\n...` — which
   characters are structural, what is your bytes-per-structural, and
   where does it sit in Step 6's table (closer to twitter or to
   marine_ik)?

## Done when

Answer each before unfolding it.

- [ ] You can explain the stage-1 idea — classify 64 bytes into bitmasks, branch once per block — and say what sets the block size.

  <details><summary>Answer</summary>

  Every per-byte question becomes one bit of a `uint64_t`, so questions
  about a whole block become branch-free bit arithmetic. The block size
  is set by the **mask** width (a general-purpose register), not the
  vector width: the arm64 classifier still works 16 bytes at a time and
  runs four times per block. That is why arm64 (`src/arm64.cpp:126`)
  and westmere (`src/westmere.cpp:140`) both use `index<64>` while
  haswell (`:135`) and icelake (`:181`) use 128 — and the 128 variant
  is two 64-byte blocks pipelined for ILP (`step<128>`,
  `json_structural_indexer.h:220-229`), not a wider mask.

  </details>

- [ ] You can describe what the arm64 classifier actually does, and why it is *not* the paper's two-nibble AND.

  <details><summary>Answer</summary>

  `src/arm64.cpp:53` indexes a single 16-entry `op_table` with
  `(byte + 3) >> 4` and then compares the looked-up value back against
  the original byte (`vceqq_u8`). The `+3` is what separates `[`/`]`
  (0x5B/0x5D → buckets 5 and 6) and `{`/`}` (0x7B/0x7D → 7 and 8); the
  final compare rejects every other byte that shares a bucket, e.g.
  `-` (0x2D) lands in bucket 3 and is compared against `:`.
  Whitespace uses `vqtbx1q_u8` at line 58 — table-lookup with fallback
  — so `\t\n\r` come from `ws_table` and space falls through to a
  `vceqq_u8(d, ' ')` destination.

  The two-nibble AND the paper describes in §3.1.2 is real, but it
  lives in UTF-8 validation, and there it is **three** tables:
  `utf8_lookup4_algorithm.h:104` returns
  `byte_1_high & byte_1_low & byte_2_high`. The x86 classifier
  (`src/haswell.cpp:43-72`) uses a low-nibble table plus `| 0x20`, and
  admits in its own comment (57-66) that it also matches two control
  characters, caught later in stage 2.

  </details>

- [ ] You can explain quote parity by prefix-XOR, and say which instruction computes it *on your machine*.

  <details><summary>Answer</summary>

  A byte is inside a string iff an odd number of quotes precede it, so
  the primitive needed is "XOR of all lower bits", per position.
  On x86 that is one carry-less multiply by all-ones
  (`westmere/bitmask.h:22`, `_mm_clmulepi64_si128`; the paper quotes
  `pclmulqdq` at 7-cycle latency, 1/cycle throughput on Skylake).

  On aarch64 it is **not**: `arm64/bitmask.h:31-36` is a six-step
  shift-XOR ladder (log₂ 64 = 6) in general-purpose registers, and
  lines 19-29 give the reason — PMULL needs a GPR→FP `fmov` in
  (3 cycles on N1, 5 on A72) and an FP→GPR `fmov` out (2 / 5), while
  the GPR units are idle anyway because the critical path is on the FP
  side doing classification.

  </details>

- [ ] You can describe the escaped-backslash problem and show, on numbers, why the shipped code subtracts rather than adds.

  <details><summary>Answer</summary>

  A quote is real only if the backslash run before it has even length,
  so the parser needs run-length parity without a loop. The paper's
  Fig. 3 uses two additions and a carry ripple. The shipped code
  (`json_escape_scanner.h:127-142`) does
  `((potential_escape << 1) | ODD_BITS) - potential_escape) ^ ODD_BITS`
  with `ODD_BITS = 0xAAAA…` (line 74): the *borrow* chain of the
  subtraction ripples the length of each run, and XORing the odd bits
  back off leaves 1s exactly on the escaped codes.

  In 8 bits, `\\\n` gives `0x07 → 0x0E → 0xAE → 0xA7 → 0x0D`, and
  `0x0D ^ 0x07 = 0x0A` = the second backslash and the `n`. For `\\n`
  it gives `0x03 → 0x06 → 0xAE → 0xAB → 0x01`, and
  `0x01 ^ 0x03 = 0x02` = only the second backslash — the `n` is free.

  </details>

- [ ] You can explain over-write / under-advance in `bit_indexer::write`, and say what the step width is in the code versus in the paper.

  <details><summary>Answer</summary>

  The loop writes a fixed number of index slots per round regardless of
  how many bits are actually set, and advances the output cursor by the
  true popcount (`json_structural_indexer.h:121`, `this->tail += cnt`).
  Garbage past `cnt` is overwritten by the next block; the buffer is
  oversized on purpose (doc comment, lines 85-86). The cost is a few
  redundant stores; the saving is the unpredictable branch the paper
  measures at "at least one branch miss for each word" (§3.1.4).

  The paper's Fig. 6 writes **8** at a time and calls it a heuristic.
  The pinned code writes **4** (`STEP`, line 108) up to
  `STEP_UNTIL = 24` (line 110), then a scalar tail. It also reverses
  the mask once (line 103) and consumes it with leading-zeroes, because
  ARM has no fast trailing-zero instruction (comment, lines 31-43).

  </details>

- [ ] You can say why stage 2 stays branchy, and give the Amdahl argument with a number you computed rather than one you were told.

  <details><summary>Answer</summary>

  Stage 2 does the irreducibly data-dependent work — number parsing,
  string unescaping, tape emission — on only the bytes stage 1 flagged.
  "It only sees 1/8 of the bytes" is true for twitter and nothing else:
  Table 5 spans 4.6 to 43.9 bytes per structural.

  Using §4.3's model on twitter (B = 631,514, S = 55,264, F = 1):
  stage 1 = 1.7·S + 0.62·B = 485,487 cycles, total = 19·F + 11·S +
  0.92·B = 1,188,916 cycles, so stage 1 is **40.8 %** — the branchy
  half is the larger one. It works anyway because stage 2's per-byte
  term (0.31·B) is small; its cost is concentrated in the 8.7·S and
  19·F terms, which scale with *structure*, not with input size.

  </details>

- [ ] You can state the paper's throughput result together with the machine it was measured on, and say why it does not predict your Mac.

  <details><summary>Answer</summary>

  §4.1: an **Intel i7-6700 (Skylake, 3.4 GHz, 3.7 GHz turbo)**,
  DDR4-2133, GCC 9.1, `-O3 -march=native`, Linux. §4.5 claims 2 GB/s or
  better on six files and 3 GB/s on gsoc-2018; Table 10 gives twitter
  2.2, citm_catalog 2.5, canada 1.1, marine_ik 0.94 GB/s — a 3.4×
  spread *within one CPU*, driven by structural density.

  It does not predict an M-series Mac because the aarch64 kernel is a
  different program: 64-byte blocks instead of 128, a shift-XOR
  `prefix_xor` instead of CLMUL, an offset-table classifier instead of
  nibble tables, a reverse-bits flatten instead of tzcnt, and an
  8-instruction bitmask fold instead of `PMOVMSKB`. The only honest
  local number is one you measure.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the RESP structural sketch and its bytes-per-structural.

  <details><summary>Answer</summary>

  Self-check. The RESP one has a concrete target: count structural
  characters in a real command frame such as
  `*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\nb\r\n` (34 bytes), decide
  whether `\r\n` counts as one structural position or two, and divide.
  Compare against Step 6's table — a protocol with a marker every few
  bytes sits at the marine_ik end, which is precisely where the paper's
  8-wide unconditional flatten stops helping.

  </details>

## References

**Papers**
- Langdale & Lemire — "Parsing Gigabytes of JSON per Second"
  (VLDB Journal 2019,
  [arXiv:1902.08318](https://arxiv.org/abs/1902.08318)). §3.1.1 escape
  parity and prefix-XOR; §3.1.2 nibble classification; §3.1.4 index
  extraction (Fig. 6, the 8-wide flatten); §4.1 hardware; §4.3 the
  regression model used in Step 8; Table 5 dataset statistics;
  Table 10 the throughput comparison.

**Code**
- [simdjson](https://github.com/simdjson/simdjson) at `c783809` —
  `src/arm64.cpp` and `include/simdjson/arm64/` (simd.h, bitmask.h,
  bitmanipulation.h) are what your machine runs; `src/generic/stage1/`
  holds the ISA-independent algorithms they instantiate. Read the
  arm64 files *first* — several of them contradict the paper, and the
  comments explain why.
