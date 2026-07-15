# hashbrown & memchr: movemask without movemask

Two crates, one question: how do you get an x86 `movemask` (one bit
per lane) on ISAs that don't have it — and when should you not even
try? Before the anchors, this chapter builds the pieces in order:
what movemask is for, the three ways to fake it, the SWAR fallback
that needs no SIMD at all, and the SwissTable probe loop that puts
it all to work. hashbrown answers the title question by shrinking
the group to 8 bytes so the comparison result already *is* the mask;
memchr answers with the `vshrn` nibble-mask idiom. Between them sits
the portability pattern every SIMD kernel layer copies.

## The problem in one sentence

Every SIMD search kernel ends the same way — "compare 16 bytes at
once, then tell me WHICH lanes matched, as an integer I can iterate"
— and ARM NEON simply has no instruction for that second half, so
every fast hash table and substring search on your Mac is built
around a workaround.

## The concepts, step by step

### Step 1 — movemask: from vector comparison to iterable integer

A SIMD comparison (e.g. `vceqq_u8` on NEON, ARM's 128-bit SIMD
instruction set) compares 16 byte lanes at once, producing a vector
where each matching lane is 0xFF and each non-match is 0x00. Useless
by itself — you can't loop over a vector. x86's `PMOVMSKB`
("movemask") fixes that: it extracts the top bit of each lane into a
16-bit integer, one bit per lane. Now ordinary scalar tools finish
the job: `mask != 0` (any match?), `trailing_zeros()` (index of the
first match), clear-lowest-bit (next match). Search = one vector
compare + one movemask + bit iteration. NEON has the compare but not
the movemask — hence this chapter.

### Step 2 — three answers to "one bit per lane"

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

memchr keeps 16 lanes and pays one `vshrn` (shift-right-narrow: an
instruction that halves each 16-bit element to 8 bits, here abused
to fold 16 comparison lanes into a 64-bit mask with 4 bits per
lane). hashbrown instead SHRINKS its unit of work to 8 bytes so the
comparison result, reinterpreted as a u64, already *is* the bitmask
— zero extra instructions. Question: why does the right choice
differ? (Hint: hash probing expects to find its match in the first
group — mod.rs's comment: "the probability of finding a match drops
off drastically after the first few buckets" — while memchr scans
megabytes and amortizes.)

### Step 3 — SWAR: a u64 is an 8-lane vector if you're careful

SWAR (SIMD within a register — doing lane-parallel work with plain
integer instructions on a u64) is the portable fallback when there's
no SIMD at all (hashbrown's generic.rs). Compare 8 bytes against a
tag in four ALU ops:

```rust
let cmp = self.0 ^ repeat(tag);            // matching byte → 0x00
BitMask((cmp.wrapping_sub(repeat(0x01)) & !cmp & repeat(0x80)).to_le())
```

XOR turns matches into zero bytes; then the classic zero-byte
detector — subtracting 1 borrows into bit 7 only where the byte was
0 — leaves bit 7 set per matching lane. The subtraction's borrow can
ripple across lane boundaries, so this can false-positive on
adjacent bytes. Question: why is that acceptable here (what does the
caller do with a candidate match)? Compare neon.rs which has no
false positives. Remarkably, hashbrown's mod.rs comment records that
a 16-byte NEON Group *lost* to this u64 SWAR in benchmarks — the
narrowing overhead wasn't worth it for probes that end early.

### Step 4 — SwissTable: control bytes with the answer in the sign bit

SwissTable (the hash-table design behind hashbrown, hence Rust's
`HashMap`) keeps, alongside the key/value slots, one **control
byte** per slot in a dense array — and probes the control bytes a
**group** (8 or 16) at a time with exactly Step 2's machinery. Each
control byte is either EMPTY=0xFF, DELETED=0x80, or FULL=0..0x7f
(a 7-bit **tag** — the top 7 bits of the hash, stored so most
non-matching slots are rejected without touching the actual key):

```
 h1(hash) → group index        h2(hash) → 7-bit tag
 ┌────────────────────── one group (8 or 16 control bytes)
 │ 0x51 0x7f EMPTY 0x51 DEL 0x12 ...
 └── match_tag(0x51)  → candidates 0b...01001  → probe those slots
     match_empty()    → can this group absorb an insert?
     match_empty_or_deleted() → insertion slot (vcltz: top bit set)
```

The encoding is the trick: EMPTY and DELETED both have the top
(sign) bit set, FULL never does — so all three predicates are
single-instruction (neon.rs:85,94 use `vcltz`/`vcgez`, "compare
less-than/greater-equal zero" on signed bytes).

### Step 5 — the probe loop: ~3 instructions per 8–16 slots

Assemble Steps 1–4 and a lookup probes a whole group per iteration:

```rust
// the probe loop at group granularity: ~3 instructions per 8-16 slots
fn find(&self, hash: u64, key: &K) -> Option<usize> {
    let (mut g, tag) = (h1(hash) & self.mask, h2(hash));  // 7-bit tag
    loop {
        let group = Group::load(&self.ctrl[g]);
        let mut m = group.match_tag(tag);        // vceq + extract → BitMask
        while let Some(i) = m.next_set() {       // trailing_zeros() >> 3
            if self.slot(g + i).key == *key { return Some(g + i); }
        }                                        // false positive? just loop
        if group.match_empty().any() { return None; }  // EMPTY ends the probe
        g = (g + GROUP_SIZE) & self.mask;        // (triangular in real code)
    }
}
```

Tag false positives (two keys sharing a 7-bit tag, or SWAR borrow
noise) just cost one extra key comparison — correctness never
depended on the mask being exact, only on it never missing a real
match. This is topic 2's hash table at lane level. Question: rewrite
your M2 probe loop's per-slot compare as a per-group `match_tag` and
count instructions per probed slot.

### Step 6 — memchr's 4× unroll: one movemask per 64 bytes

memchr (substring/byte search) has the opposite profile from probing
— it expects to scan megabytes of non-matches. Its main loop
(arch/generic/memchr.rs:171-206, `LOOP_SIZE = 4 * V::BYTES`) loads 4
vectors, `cmpeq`s each, ORs the four results together, and pays the
(NEON-expensive) movemask ONCE per 64 bytes; only on a hit does it
re-movemask the individual vectors to localize the match. The miss
path — the overwhelmingly common one — stays minimal. Same shape as
polars' one-branch-per-block filter and simdjson's 64-byte stage 1:
amortize the expensive extraction over a block, localize only on
hit. Question: why OR-then-locate instead of 4 movemask+test — count
the instructions on the miss path.

### Step 7 — the portability pattern to copy

Both crates write the ALGORITHM once against a tiny interface —
memchr's `Vector` trait (splat/load/cmpeq/movemask), hashbrown's
`Group` struct with one file per backend — and each ISA implements
that interface in ~100 lines of intrinsics, selected by `cfg_if!` at
COMPILE time (vs polars' runtime dispatch — binding times again).
The abstraction cost is zero after monomorphization, and the
generic/SWAR backend doubles as the oracle for testing the SIMD
ones. This is the shape for M17's kernel layer.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| hashbrown group/mod.rs:8-30 | 2, 7 | the `cfg_if!` backend choice + the famous "NEON wasn't worth it" comment |
| hashbrown group/sse2.rs:20 | 2 | `Group(__m128i)` — 16 control bytes |
| hashbrown group/sse2.rs:73-84 | 1–2 | `match_tag` = `_mm_cmpeq_epi8` + `_mm_movemask_epi8` → `BitMask(u16)` |
| hashbrown group/neon.rs:16 | 2 | `Group(uint8x8_t)` — EIGHT bytes, not 16! |
| hashbrown group/neon.rs:68-75 | 2 | `match_tag` = `vceq_u8` + reinterpret as u64 — NO movemask at all |
| hashbrown group/neon.rs:85-99 | 4 | `match_empty_or_deleted` via `vcltz_s8` (sign bit test) |
| hashbrown group/generic.rs:41 | 3 | `Group(GroupWord)` — SWAR on a plain u64 |
| hashbrown group/generic.rs:105-109 | 3 | SWAR match_tag: `x ^ repeat(tag)`, then the zero-byte trick |
| memchr vector.rs:25-64 | 7 | the `Vector` trait: splat/load/cmpeq/movemask over 3 ISAs |
| memchr vector.rs:322-328 | 2 | NEON movemask: `vshrn_n_u16(_, 4)` → u64 with 4 bits/lane |
| memchr arch/generic/memchr.rs:107 | 6 | `LOOP_SIZE = 4 * V::BYTES` — the 4× unroll |
| memchr arch/generic/memchr.rs:171-206 | 6 | the unrolled search loop (OR-combine 4 cmpeqs, one movemask check) |

Reading order: hashbrown's mod.rs comment first (the design doc),
then sse2.rs → neon.rs → generic.rs in that order (native → shrunk
→ SWAR), then memchr's vector.rs and the unrolled loop.

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

## References

**Code**
- [hashbrown](https://github.com/rust-lang/hashbrown) —
  `src/control/group/` — one file per backend (sse2/neon/generic);
  the `cfg_if!` block in `mod.rs` and its "NEON wasn't worth it"
  comment are the design doc
- [memchr](https://github.com/BurntSushi/memchr) — `src/vector.rs`
  (the `Vector` trait + NEON movemask idiom) and
  `src/arch/generic/memchr.rs` (the 4× unrolled search loop)
