# Reading guide — hashbrown `Group` + memchr `Vector` (the movemask story)

Clone: [`~/repos/hashbrown`](https://github.com/rust-lang/hashbrown) (`src/control/group/`) and [`~/repos/memchr`](https://github.com/BurntSushi/memchr)
(`src/vector.rs`). Two crates, one question: how do you get an x86
`movemask` (one bit per lane) on ISAs that don't have it — and when
should you not even try?

## Anchor map

| anchor | what it is |
|---|---|
| hashbrown group/mod.rs:8-30 | the `cfg_if!` backend choice + the famous "NEON wasn't worth it" comment |
| hashbrown group/sse2.rs:20 | `Group(__m128i)` — 16 control bytes |
| hashbrown group/sse2.rs:73-84 | `match_tag` = `_mm_cmpeq_epi8` + `_mm_movemask_epi8` → `BitMask(u16)` |
| hashbrown group/neon.rs:16 | `Group(uint8x8_t)` — EIGHT bytes, not 16! |
| hashbrown group/neon.rs:68-75 | `match_tag` = `vceq_u8` + reinterpret as u64 — NO movemask at all |
| hashbrown group/neon.rs:85-99 | `match_empty_or_deleted` via `vcltz_s8` (sign bit test) |
| hashbrown group/generic.rs:41 | `Group(GroupWord)` — SWAR on a plain u64 |
| hashbrown group/generic.rs:105-109 | SWAR match_tag: `x ^ repeat(tag)`, then the zero-byte trick |
| memchr vector.rs:25-64 | the `Vector` trait: splat/load/cmpeq/movemask over 3 ISAs |
| memchr vector.rs:322-328 | NEON movemask: `vshrn_n_u16(_, 4)` → u64 with 4 bits/lane |
| memchr arch/generic/memchr.rs:107 | `LOOP_SIZE = 4 * V::BYTES` — the 4× unroll |
| memchr arch/generic/memchr.rs:171-206 | the unrolled search loop (OR-combine 4 cmpeqs, one movemask check) |

## 1. Three answers to "one bit per lane"

```
 SSE2 (16B group):   vceqq → PMOVMSKB → u16, 1 bit/lane. Native. Done.

 NEON, memchr style (16B): no PMOVMSKB. Idiom:
   vceqq_u8        → 16 lanes of 0xFF/0x00
   vshrn_n_u16(,4) → narrow each u16 pair, keeping 4 bits per byte
   vget_lane_u64   → u64 where each lane owns a NIBBLE
   & 0x8888...     → keep 1 bit per nibble  (vector.rs:322-328)
   position = trailing_zeros() >> 2   ← note the /4!

 NEON, hashbrown style (8B group): don't narrow at all.
   vceq_u8 on uint8x8_t → 8 lanes of 0xFF/0x00 = exactly one u64
   vget_lane_u64 → done. BitMask where each lane owns a BYTE.
   position = trailing_zeros() >> 3
```

hashbrown chose to SHRINK the group to 8 so the comparison result
*is already* the bitmask. memchr keeps 16 lanes and pays one `vshrn`.
Question: why does the right choice differ? (Hint: hash probing
expects to find its match in the first group — mod.rs's comment:
"the probability of finding a match drops off drastically after the
first few buckets" — while memchr scans megabytes and amortizes.)

## 2. The generic SWAR backend (generic.rs:105-109)

No SIMD at all — a u64 *is* an 8-lane vector if you're careful:

```rust
let cmp = self.0 ^ repeat(tag);            // matching byte → 0x00
BitMask((cmp.wrapping_sub(repeat(0x01)) & !cmp & repeat(0x80)).to_le())
```

The classic "detect zero byte" trick: subtracting 1 borrows into
bit 7 only where the byte was 0. Question: this can false-positive
on adjacent-byte borrows — why is that acceptable here (what does
the caller do with a candidate match)? Compare neon.rs which has
no false positives.

## 3. SwissTable probing at lane level

```
 h1(hash) → group index        h2(hash) → 7-bit tag
 ┌────────────────────── one group (8 or 16 control bytes)
 │ 0x51 0x7f EMPTY 0x51 DEL 0x12 ...
 └── match_tag(0x51)  → candidates 0b...01001  → probe those slots
     match_empty()    → can this group absorb an insert?
     match_empty_or_deleted() → insertion slot (vcltz: top bit set)
```

Control bytes encode EMPTY=0xFF, DELETED=0x80, FULL=0..0x7f — all
three predicates are single-instruction because the encoding puts
the discriminator in the SIGN bit (neon.rs:85,94 use `vcltz`/`vcgez`).
Question: this is topic 2's hash table — rewrite your M2 probe loop's
per-slot compare as a per-group `match_tag` and count instructions
per probed slot.

## 4. memchr's 4× unroll (arch/generic/memchr.rs:171-206)

The search loop loads 4 vectors, `cmpeq`s each, ORs the results,
and calls movemask ONCE per 64 bytes; only on a hit does it
re-movemask the individual vectors to localize. Same shape as
polars' one-branch-per-block filter and simdjson's 64-byte stage 1.
Question: why OR-then-locate instead of 4 movemask+test — count the
instructions on the (overwhelmingly common) miss path.

## 5. The portability pattern to copy

`Vector` trait (memchr) / `Group` struct-per-file (hashbrown): the
ALGORITHM is written once against splat/cmpeq/movemask; each ISA
file is ~100 lines of intrinsics implementing the interface, chosen
by `cfg_if!` at COMPILE time (vs polars' runtime dispatch — binding
times again). This is the shape for M17's kernel layer.

## Questions for notes.md

1. hashbrown mod.rs says a 16-byte NEON Group lost to the generic
   u64 SWAR. What cost model explains that (latency of narrow +
   extract vs the SWAR's 4 ALU ops)?
2. The vshrn nibble-mask means `trailing_zeros()>>2`; hashbrown's
   byte-mask means `>>3`. What breaks if you forget the shift?
   (memchr wraps it in `NeonMoveMask` newtype — why?)
3. SWAR match_tag tolerates false positives; match_empty (bit
   pattern 0b1111_1111) doesn't need the subtract trick — why
   (generic.rs:119 uses `self.0 & (self.0<<1)`)?
4. For M2's table: your tags are full hashes. What do you lose by
   truncating to 7 bits + sign-bit encoding, and what do you gain
   per probe?
5. Compile-time cfg (here) vs runtime detect (polars) vs init-time
   fn pointers (SimSIMD): which fits a Cypher engine that ships one
   binary to unknown ARM servers?
