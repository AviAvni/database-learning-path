# simdjson: parsing without branches

Parsing — the most branchy code imaginable — rebuilt as branch-free
bitmask algebra over 64-byte blocks, at gigabytes per second. Before
the paper and the headers, this chapter builds the tricks one at a
time: why branches kill parsers, how masks replace them, the nibble
lookup, the carry-less multiply, the backslash-parity dance, and the
over-write/under-advance flatten. Read the paper alongside
`include/simdjson/arm64/` (you're on ARM — the NEON implementation
is the one your machine runs). Every trick here transfers to a DB
engine: RESP framing, CSV ingest, LIKE prefilters.

## The problem in one sentence

A conventional JSON parser branches on every input byte, and since
JSON bytes are unpredictable, each mispredicted branch costs
~15 cycles — capping parsers around a few hundred MB/s while the
memory system could deliver tens of GB/s; simdjson closes that ~10×
gap by removing the branches.

## The concepts, step by step

### Step 1 — why parsers are slow: one branch per unpredictable byte

SIMD (single instruction, multiple data — one CPU instruction
operating on a whole vector of values at once; NEON, ARM's SIMD
instruction set, uses 128-bit vectors = 16 bytes per instruction) is
useless for a textbook parser, because a parser is a chain of
data-dependent branches: `if byte == '"' … else if byte == '{' …`.
The CPU predicts every branch and speculates ahead; when input bytes
are effectively random, it mispredicts constantly at ~15 cycles a
miss. That serializes execution around unpredictable data — the same
disease as topic 17 README §2's failure #2, at one-byte granularity.

### Step 2 — the fix: classify 64 bytes into bitmasks, branch once per block

simdjson's core move: process input in 64-byte blocks and convert
every per-byte question into a **bitmask** — a u64 where bit i
answers the question for byte i ("is byte i a quote?" → one bit).
Questions about bytes become bit arithmetic on whole blocks, which
has no branches at all. The architecture splits in two:

```
 stage 1: structural indexing (SIMD, branch-free)
   64 input bytes → classify → bitmasks (one bit per byte):
   quotes, backslashes, whitespace, operators {}[]:,
   → resolve strings (quote parity) → structural positions
   → flatten bit positions into an index array
 stage 2: tape building (branchy, but only touches ~1/8 of bytes)
   walk the structural indexes, parse numbers/strings, emit tape
```

Stage 1 never branches on DATA — the only branches are the loop.
Stage 2 stays branchy but only visits the ~1/8 of bytes stage 1
flagged as structural, so Amdahl's law works *for* you: the branchy
part shrank 8×.

### Step 3 — classification by nibble LUT (`lookup_16`)

The first mask to build: which of these 16 bytes are `{ } [ ] : ,`
or whitespace? The tool is `vqtbl1q_u8` — NEON's table-lookup
instruction, which uses each byte of one vector as an index into a
16-entry table (a LUT — lookup table) held in another vector: 16
parallel table lookups in one instruction. One byte is too big for a
16-entry table, so split it into **nibbles** (4-bit halves, values
0–15): look up the high nibble in one table, the low nibble in
another, and AND the results. Any predicate expressible as
(hi-nibble class) ∧ (lo-nibble class) costs 2 shuffles + 1 AND for
16 bytes — versus 16 branchy comparisons. Question: build the two
tables that classify `{ } [ ] : ,` — why do hi and lo tables
disagree on false positives, and why does ANDing fix it?

### Step 4 — quote parity by carry-less multiply (`prefix_xor`)

Knowing where quotes are isn't enough — a byte is *inside a string*
if it's preceded by an odd number of quotes. That's a running
(prefix) parity over 64 positions: naively a serial loop, the exact
dependency chain SIMD can't do. The trick: `prefix_xor(m)` computes,
for each bit position, the XOR of all lower bits — exactly "odd
quote count so far" — in ONE `PMULL` instruction (carry-less
multiply: binary multiplication where additions are XORs, i.e.
multiplication in GF(2); multiplying by all-ones makes every output
bit the XOR of all inputs below it). One instruction turns the
quote mask into an in-string region mask. Question: why is
escaped-quote handling (backslash runs) done BEFORE this, and why
does odd/even backslash parity need its own trick (the
odd_sequence_starts dance)?

### Step 5 — the escaped-backslash problem

`\\\"` vs `\\\\"` — whether a quote is real depends on the PARITY of
the preceding backslash run: `\"` escapes the quote, `\\"` doesn't
(the backslash escaped itself). Run-length parity is again
inherently sequential-looking — and the scanner solves it with
add-carry propagation on masks: adding `backslash_starts` to the run
mask makes the carry ripple through each run of 1-bits and pop out
at the end, landing on an odd or even position depending on the
run's length. Branch-free parity-of-run-length via the adder's carry
chain. This is the paper's cleverest three lines — work the example
in §3.1.1 by hand.

### Step 6 — flatten_bits: over-write, under-advance (`bit_indexer`)

Stage 2 wants an array of positions, not a mask. Turning a 64-bit
mask into positions: `cnt = popcnt` (population count — how many
bits are set), then repeatedly `trailing_zeros` (index of the lowest
set bit) + clear that bit — unrolled by 8 with all 8 slots written
UNCONDITIONALLY, advancing the output cursor by the real count only:

```rust
// bit_indexer: mask → positions. Over-write, under-advance.
fn flatten(out: &mut [u32], n: usize, start: u32, mut m: u64) -> usize {
    let cnt = m.count_ones() as usize;
    let mut k = 0;
    while k < cnt {                    // ceil(cnt/8) iterations, branch-free body
        for j in 0..8 {                // write 8 UNCONDITIONALLY —
            out[n + k + j] = start + m.trailing_zeros();  // garbage lanes are fine
            m &= m.wrapping_sub(1);    // clear lowest set bit
        }
        k += 8;
    }
    n + cnt                            // advance by the REAL count only
}
```

Writing garbage past the real count costs a few redundant stores;
branching on the exact count would cost a mispredict. Same shape as
our branchless filter append — over-write, under-advance is THE
selection-kernel idiom. Question: why is writing 8 always faster
than writing exactly cnt?

### Step 7 — compress on NEON: the missing instruction, emulated

Sometimes stage 1 needs to *compact* the surviving bytes themselves
(keep the lanes where the mask is set, packed left). AVX-512 has an
instruction for this (`vpcompress`); NEON does not. simdjson's
emulation (arm64/simd.h:267-276): take the mask's 4-bit chunks,
index a precomputed LUT of shuffle patterns, and feed that pattern
to `vqtbl1q` (Step 3's table lookup, now used as a byte-shuffler) —
compaction as a lookup-then-shuffle, 8 lanes per shuffle. This is
NEON's missing vpcompress, and it's exactly what our `filter.rs`
NEON compact stub reimplements for f32.

### Step 8 — what transfers to a DB engine

- RESP protocol framing (M7) = structural indexing over `\r\n$*:+-`
- CSV/JSON bulk ingest = the whole pipeline
- string-escape scanning = LIKE/regex prefilters
- the meta-lesson: turn per-byte branches into per-block masks,
  THEN branch once per block (topic 11's vectorization, byte
  edition)

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| arm64/simd.h:179 | 3 | `repeat_16` — build 16-byte LUTs |
| arm64/simd.h:226-229 | 3 | `lookup_16` = `vqtbl1q_u8` — the classification workhorse |
| arm64/bitmask.h:15-22 | 4 | `prefix_xor` — carry-less multiply (PMULL) turns quote bits into in-string regions |
| src/generic/stage1/json_string_scanner.h:16-30 | 4–5 | the string-state block: escaped/quote/in_string masks |
| src/generic/stage1/json_structural_indexer.h:24-28 | 6 | `bit_indexer` — flatten mask bits to positions |
| src/generic/stage1/json_structural_indexer.h:194 | 2 | the stage-1 driver loop |
| arm64/simd.h:267-276 | 7 | compress via pruned `vqtbl1q` + LUT — NEON's missing vpcompress, emulated |
| src/generic/stage1/utf8_lookup4_algorithm.h | 3 | UTF-8 validation as 3 table lookups (the nibble-LUT trick, applied thrice) |

Reading route: the stage-1 driver loop first (see the block
pipeline whole), then simd.h's LUT machinery, then the string
scanner with the paper's §3.1.1 open beside it. In the paper, §3 is
stage 1 — the rest you can skim once the steps above are solid.

## Questions for notes.md

1. Why 64-byte blocks (one u64 mask = 64 lanes) rather than the
   16-byte NEON width?
2. The compress LUT at simd.h:267: how many entries, indexed by
   what, and why does the same trick cap at 8 lanes per shuffle?
3. Stage 2 is still branchy. Why does Amdahl not kill the speedup
   (what fraction of bytes reach stage 2)?
4. UTF-8 validation in 3 lookups: what property of UTF-8 error
   patterns makes nibble tables sufficient?
5. For M7: sketch stage-1 masks for RESP (`*3\r\n$3\r\nSET...`) —
   which characters are "structural"?

## References

**Papers**
- Langdale & Lemire — "Parsing Gigabytes of JSON per Second"
  (VLDB Journal 2019,
  [arXiv:1902.08318](https://arxiv.org/abs/1902.08318)) — §3 is
  stage 1; work the escaped-backslash example in §3.1.1 by hand

**Code**
- [simdjson](https://github.com/simdjson/simdjson) —
  `include/simdjson/arm64/` (simd.h, bitmask.h) plus
  `src/generic/stage1/` — read the NEON files, they're what your
  machine runs
