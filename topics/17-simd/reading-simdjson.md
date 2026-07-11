# Reading guide — simdjson ("Parsing Gigabytes of JSON per Second", VLDB '19)

Clone: `~/repos/simdjson`. Read the paper alongside
`include/simdjson/arm64/` (you're on ARM — the NEON implementation
is the one your machine runs). The big idea: parsing — the most
branchy code imaginable — rebuilt as branch-free bitmask algebra
over 64-byte blocks.

## Two-stage architecture

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
All data-dependence becomes bit arithmetic.

## Anchor map (arm64)

| anchor | what it is |
|---|---|
| arm64/simd.h:179 | `repeat_16` — build 16-byte LUTs |
| arm64/simd.h:226-229 | `lookup_16` = `vqtbl1q_u8` — the classification workhorse |
| arm64/simd.h:267-276 | compress via pruned `vqtbl1q` + LUT — NEON's missing vpcompress, emulated |
| arm64/bitmask.h:15-22 | `prefix_xor` — carry-less multiply (PMULL) turns quote bits into in-string regions |
| src/generic/stage1/json_string_scanner.h:16-30 | the string-state block: escaped/quote/in_string masks |
| src/generic/stage1/json_structural_indexer.h:24-28 | `bit_indexer` — flatten mask bits to positions |
| src/generic/stage1/json_structural_indexer.h:194 | the stage-1 driver loop |
| src/generic/stage1/utf8_lookup4_algorithm.h | UTF-8 validation as 3 table lookups |

## 1. Classification by nibble LUT (`lookup_16`)

To classify 16 bytes at once: split each byte into hi/lo nibbles,
look each up in a 16-entry table (`vqtbl1q_u8`), AND the results.
Any predicate expressible as (hi-nibble class) ∧ (lo-nibble class)
costs 2 shuffles + 1 AND for 16 bytes. Question: build the two
tables that classify `{ } [ ] : ,` — why do hi and lo tables
disagree on false positives, and why does ANDing fix it?

## 2. Quote parity by carry-less multiply (`prefix_xor`)

In-string = bytes after an odd number of quotes. `prefix_xor(m)`
computes for each bit position the XOR of all lower bits — exactly
"odd quote count so far" — in ONE `PMULL` instruction (multiply by
all-ones in GF(2)). Question: why is escaped-quote handling
(backslash runs) done BEFORE this, and why does odd/even backslash
parity need its own trick (the odd_sequence_starts dance)?

## 3. The escaped-backslash problem

`\\\"` vs `\\\\"` — whether a quote is real depends on the PARITY of
the preceding backslash run. The scanner solves it with add-carry
propagation on masks: adding `backslash_starts` to the run mask
carries out at run ends where the run length is odd. Branch-free.
This is the paper's cleverest three lines — work the example in
§3.1.1 by hand.

## 4. flatten_bits (`bit_indexer`)

Turning a 64-bit mask into an array of positions: `cnt = popcnt`,
then repeatedly `trailing_zeros` + `clear lowest bit`, unrolled by
8 with the count written UNCONDITIONALLY (write 8, advance by
popcount — over-write, under-advance). Same shape as our
branchless filter append. Question: why is writing 8 always faster
than writing exactly cnt?

## 5. What transfers to a DB engine

- RESP protocol framing (M7) = structural indexing over `\r\n$*:+-`
- CSV/JSON bulk ingest = the whole pipeline
- string-escape scanning = LIKE/regex prefilters
- the meta-lesson: turn per-byte branches into per-block masks,
  THEN branch once per block (topic 11's vectorization, byte
  edition)

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
