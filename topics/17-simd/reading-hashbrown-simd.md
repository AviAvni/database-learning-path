# hashbrown & memchr: movemask without movemask

Two crates, one question: how do you get an x86 `movemask` (one bit
per lane) on an ISA that does not have it — and when should you not
even try? This chapter builds the pieces in order: what movemask is
for, the three ways to fake it, the SWAR fallback that needs no SIMD
at all, and the SwissTable probe loop that puts it to work. hashbrown
answers the title question by shrinking its group to 8 bytes so the
comparison result already *is* the mask; memchr answers with the
`vshrn` nibble idiom, and then avoids paying for it on the miss path.
Between them sits the portability pattern every SIMD kernel layer
copies.

Every anchor below is `rust-lang/hashbrown@d69025b` or
`BurntSushi/memchr@5fdb40c` (`resources/codebases.md`), quoted with the
line numbers the code occupies in those revisions. Both crates pick
their backend with `cfg!`, so the code your Mac compiles is the
aarch64 branch — and for hashbrown that branch is **not** the one the
crate's own design comment describes.

## The problem in one sentence

Every SIMD search kernel ends the same way — "compare 16 bytes at
once, then tell me *which* lanes matched, as an integer I can iterate"
— and ARM NEON has no single instruction for that second half, so
every fast hash table and substring search on your Mac is built around
a workaround.

## The concepts, step by step

### Step 1 — movemask: from vector comparison to iterable integer

> **In:** a vector of 16 comparison results, each lane `0xFF` or
> `0x00`.
> **Out:** an integer whose set bits name the matching lanes, so
> scalar bit tricks can finish the job.

A SIMD comparison such as NEON's `vceqq_u8` compares 16 byte lanes at
once and writes `0xFF` into every matching lane and `0x00` into every
other. That is useless on its own, because you cannot loop over a
vector: to *use* the result you need to know which lanes matched.

x86 has one instruction for the conversion, `PMOVMSKB`: take the top
bit of each of the 16 lanes and pack them into a 16-bit integer. After
that, ordinary scalar tools finish the search — `mask != 0` answers
"any match?", `trailing_zeros()` gives the first matching lane,
`mask & (mask - 1)` clears it and moves to the next.

NEON has the compare and not the pack. Everything below is a
consequence.

### Step 2 — three answers to "one bit per lane"

> **In:** the comparison vector from Step 1, on a machine with no
> `PMOVMSKB`.
> **Out:** three different integers, with three different bits-per-lane
> conventions — and therefore three different index arithmetics.

```
 (a) SSE2, 16-byte group        native, 1 bit per lane
     _mm_cmpeq_epi8   -> 16 lanes of 0xFF/0x00
     _mm_movemask_epi8-> u16, bit i = lane i
     BITMASK_STRIDE = 1                   (sse2.rs:12)

 (b) NEON, memchr style, 16-byte vector   4 bits per lane
     vceqq_u8         -> 16 lanes of 0xFF/0x00
     vshrn_n_u16(_,4) -> narrow each u16 pair to 8 bits, keeping
                         the top nibble of each half
     vget_lane_u64    -> u64, each lane owning a NIBBLE
     & 0x8888...      -> one bit per nibble  (vector.rs:325-328)
     lane = trailing_zeros() >> 2          (vector.rs:455)

 (c) NEON, hashbrown style, 8-byte group  8 bits per lane
     vceq_u8 on uint8x8_t -> 8 lanes of 0xFF/0x00 = one u64 exactly
     vget_lane_u64        -> done; no narrowing instruction at all
     BITMASK_STRIDE = 8                   (neon.rs:8)
     lane = trailing_zeros() / 8          (bitmask.rs:58)
```

memchr keeps 16 lanes and pays one `vshrn_n_u16` (shift-right-narrow —
halve each 16-bit element to 8 bits; here abused so that each pair of
byte lanes contributes one nibble). hashbrown instead **shrinks its
unit of work to 8 bytes**, so the comparison result reinterpreted as a
`u64` already *is* the bitmask, at the cost of scanning half as many
slots per instruction.

Verify which branch your machine takes, because it decides every
number in Steps 4 and 5:

```rust
// hashbrown src/control/group/mod.rs:8-33 — backend selection
     8  cfg_if! {
// ... 9-12: SSE2 preferred; no AVX because the probability of finding a
//           match drops off drastically after the first few buckets ...
    14      // I attempted an implementation on ARM using NEON instructions, but it
    15      // turns out that most NEON instructions have multi-cycle latency, which in
    16      // the end outweighs any gains over the generic implementation.
    17      if #[cfg(all(
    18          target_feature = "sse2",
// ... 19-21: x86 / x86_64, not miri ...
    22          mod sse2;
    23          use sse2 as imp;
    24      } else if #[cfg(all(
    25          target_arch = "aarch64",
    26          target_feature = "neon",
// ... 27-30: little-endian only, not miri ...
    32          mod neon;
    33          use neon as imp;
```

Read that carefully. The comment at lines 14-16 is **stale**: it
records a past experiment, but a NEON backend ships now and lines 24-33
select it on your machine. It also does not say what the failed
experiment's group width was, does not cite a benchmark, and does not
mention narrowing — it blames "multi-cycle latency" generally. Any
claim that "a 16-byte NEON group lost to the u64 SWAR in benchmarks"
is not supported by anything in this repository; what the source
supports is only that an earlier attempt lost, and that the attempt
which finally shipped **shrank the group to 8 bytes so that no
narrowing instruction is needed at all**:

```rust
// hashbrown src/control/group/neon.rs:6-21 — the shape of the group
     6  pub(crate) type BitMaskWord = u64;
     8  pub(crate) const BITMASK_STRIDE: usize = 8;
     9  pub(crate) const BITMASK_ITER_MASK: BitMaskWord = 0x8080_8080_8080_8080;
// ... 11-15: doc comment — "uses a 64-bit NEON value" ...
    16  pub(crate) struct Group(neon::uint8x8_t);
// ... 18-20 ...
    21      pub(crate) const WIDTH: usize = mem::size_of::<Self>();
```

`uint8x8_t` is 8 bytes, so `WIDTH` is **8**. The SSE2 sibling
(`sse2.rs:20`) is `__m128i`, so its `WIDTH` is **16**. Same crate,
same algorithm, half the group.

And the match itself is two instructions:

```rust
// hashbrown src/control/group/neon.rs:68-73 — match_tag on aarch64
    68      pub(crate) fn match_tag(self, tag: Tag) -> BitMask {
    69          unsafe {
    70              let cmp = neon::vceq_u8(self.0, neon::vdup_n_u8(tag.0));
    71              BitMask(neon::vget_lane_u64(neon::vreinterpret_u64_u8(cmp), 0))
    72          }
    73      }
```

Line 70 compares, line 71 moves the 64-bit result to a general-purpose
register. `vreinterpret_u64_u8` is a type pun, not an instruction.
There is no movemask because there is nothing left to pack.

Two details make the byte-per-lane convention usable. First, the
divide: `bitmask.rs:58` computes `self.0.trailing_zeros() /
BITMASK_STRIDE`, and `BITMASK_STRIDE` is 8 here and 1 on SSE2 — so the
*same* iterator code yields lane indices on both. Second, the mask at
`bitmask.rs:89`:

```rust
// hashbrown src/control/bitmask.rs:86-90 — why iteration needs a second mask
    86      fn into_iter(self) -> BitMaskIter {
    87          // A BitMask only requires each element (group of bits) to be non-zero.
    88          // However for iteration we need each element to only contain 1 bit.
    89          BitMaskIter(BitMask(self.0 & BITMASK_ITER_MASK))
    90      }
```

On NEON a matching lane is `0xFF` — eight set bits, so
`trailing_zeros` would land correctly but `mask & (mask-1)` would step
*within* a lane. ANDing with `0x8080_8080_8080_8080` (`neon.rs:9`)
keeps exactly the top bit of each lane. On SSE2 `BITMASK_ITER_MASK` is
`!0` (`sse2.rs:13`) — a no-op, because `PMOVMSKB` already produced one
bit per lane.

### Step 3 — SWAR: a u64 is an 8-lane vector if you are careful

> **In:** eight control bytes packed in a plain `u64`, and a tag byte.
> **Out:** the same `BitMask` the SIMD backends produce, using four
> integer instructions and no vector unit at all.

SWAR — SIMD Within A Register — does lane-parallel work with ordinary
integer instructions. It is hashbrown's fallback when no supported
vector ISA is present (`mod.rs:42-44`), and it is the reference
implementation the SIMD backends are checked against:

```rust
// hashbrown src/control/group/generic.rs:97-109 — SWAR match_tag
    97      /// This function may return a false positive in certain cases where
    98      /// the tag in the group differs from the searched value only in its
    99      /// lowest bit. This is fine because:
   100      /// - This never happens for `EMPTY` and `DELETED`, only full entries.
   101      /// - The check for key equality will catch these.
   102      /// - This only happens if there is at least 1 true match.
   103      /// - The chance of this happening is very low (< 1% chance per tag).
// ... 104-107: attribute and the bithacks citation ...
   108          let cmp = self.0 ^ repeat(tag);
   109          BitMask((cmp.wrapping_sub(repeat(Tag(0x01))) & !cmp & repeat(Tag::DELETED)).to_le())
```

Line 108 turns every matching byte into `0x00`. Line 109 is the classic
zero-byte detector: subtracting `0x01` from each byte borrows into
bit 7 only where the byte was zero; `& !cmp` cancels bytes that already
had bit 7 set; `& 0x8080…` keeps one bit per lane. Four ALU ops for
eight lanes.

Get the failure mode right, because it is narrower than "adjacent
bytes can false-positive". The doc comment at lines 97-103 says the
false positive happens when a byte differs from the searched tag
**only in its lowest bit**, that it happens **only when there is at
least one true match** (the borrow has to come from somewhere), and
that its probability is **under 1 % per tag**. Work an example:

```
 tag = 0x51, group byte = 0x50  (differs only in bit 0)
 cmp        = 0x50 ^ 0x51 = 0x01
 cmp - 0x01 = 0x00                       <- borrow did NOT leave the byte
 & !cmp     = 0x00 & 0xFE = 0x00         <- no false positive on its own
 now put a true match (0x51) in the byte BELOW it:
 that byte's cmp = 0x00, so its subtraction borrows out of the byte,
 and the borrow lands in the 0x01 byte, turning it into 0xFF
 -> bit 7 set -> a false positive, exactly as documented
```

`match_empty` needs no such trick, because `EMPTY` (`0b1111_1111`) and
`DELETED` (`0b1000_0000`) are the only tags with the top bit set
(`tag.rs:9` and `:12`), and only `EMPTY` also has bit 6 set:

```rust
// hashbrown src/control/group/generic.rs:115-119 — no subtraction needed
   115      pub(crate) fn match_empty(self) -> BitMask {
// ... 116-118: comment — top two bits set means EMPTY ...
   119          BitMask((self.0 & (self.0 << 1) & repeat(Tag::DELETED)).to_le())
```

Three ops, no borrow, no false positive. The encoding was designed to
make this possible.

### Step 4 — SwissTable: control bytes with the answer in the sign bit

> **In:** a 64-bit hash and a table of control bytes.
> **Out:** a group index, a 7-bit tag, and three single-instruction
> predicates over a whole group.

SwissTable — the design behind hashbrown, and therefore behind Rust's
`HashMap` — stores one **control byte** per slot in a dense array
beside the key/value slots, and probes the control array a **group**
at a time using exactly Step 2's machinery. Each control byte is
`EMPTY = 0b1111_1111`, `DELETED = 0b1000_0000`, or a 7-bit **tag** in
`0x00..=0x7f` taken from the top of the hash:

```rust
// hashbrown src/control/tag.rs:35-48 — h2, the 7-bit tag
    35      pub(crate) const fn full(hash: u64) -> Tag {
// ... 36-46: MIN_HASH_LEN handles hashers that only fill a usize ...
    47          let top7 = hash >> (MIN_HASH_LEN * 8 - 7);
    48          Tag((top7 & 0x7f) as u8) // truncation
```

On a 64-bit target line 47 is `hash >> 57`. Note that the *top* bits go
into the tag while the *bottom* bits pick the group — two disjoint
slices of the same hash, so a slot's tag carries information the group
index does not.

The encoding is the trick: `EMPTY` and `DELETED` both have the sign bit
set and a full tag never does, so each predicate is one comparison:

```rust
// hashbrown src/control/group/neon.rs:85-97 — the sign-bit predicates
    85      pub(crate) fn match_empty_or_deleted(self) -> BitMask {
    87              let cmp = neon::vcltz_s8(neon::vreinterpret_s8_u8(self.0));
    88              BitMask(neon::vget_lane_u64(neon::vreinterpret_u64_u8(cmp), 0))
// ... 92-93: doc comment for match_full ...
    94      pub(crate) fn match_full(self) -> BitMask {
    96              let cmp = neon::vcgez_s8(neon::vreinterpret_s8_u8(self.0));
    97              BitMask(neon::vget_lane_u64(neon::vreinterpret_u64_u8(cmp), 0))
```

`vcltz_s8` is "lanes less than zero as signed bytes" — that is the sign
bit, and it answers "empty or deleted" with no constant to load and no
comparison operand. `vcgez_s8` at line 96 is its complement, "full".

### Step 5 — the probe loop, and what the group width costs

> **In:** a hash, a table, and an equality closure.
> **Out:** the index of the matching slot, or `None` — visiting one
> whole group per iteration.

The real loop is short enough to read whole:

```rust
// hashbrown src/raw.rs:2009-2045 — RawTableInner::find_inner
  2009      unsafe fn find_inner(&self, hash: u64, eq: &mut dyn FnMut(usize) -> bool) -> Option<usize> {
  2010          let tag_hash = Tag::full(hash);
  2011          let mut probe_seq = self.probe_seq(hash);
  2013          loop {
// ... 2014-2027: safety argument for the unaligned load ...
  2028              let group = unsafe { Group::load(self.ctrl(probe_seq.pos)) };
  2030              for bit in group.match_tag(tag_hash) {
// ... 2031-2032: the mask is cheaper than a modulo ...
  2033                  let index = (probe_seq.pos + bit) & self.bucket_mask;
  2035                  if likely(eq(index)) {
  2036                      return Some(index);
  2037                  }
  2038              }
  2040              if likely(group.match_empty().any_bit_set()) {
  2041                  return None;
  2042              }
  2044              probe_seq.move_next(self.bucket_mask);
  2045          }
```

Line 2030 iterates only the candidate lanes; line 2035 is the only
place a real key is touched. Line 2040 is the termination rule: an
`EMPTY` slot in the group proves the key is absent, because insertion
would have used it. A tag false positive — two keys sharing 7 bits, or
Step 3's borrow noise — costs exactly one extra `eq` call. Correctness
never depended on the mask being exact, only on it never *missing* a
real match.

Line 2044 is not a linear scan:

```rust
// hashbrown src/raw.rs:83-92 — triangular probing
    83      fn move_next(&mut self, bucket_mask: usize) {
// ... 84-89: debug assertion that the sequence has not run off the end ...
    90          self.stride = self.stride.wrapping_add(Group::WIDTH);
    91          self.pos = self.pos.wrapping_add(self.stride) & bucket_mask;
```

The stride grows by one group each time, so the visited positions are
the triangular numbers times `WIDTH` — which, for a power-of-two table,
visits every group exactly once (the proof is linked at line 74).

Now cost it, because this is where the group width shows up.
hashbrown's maximum load factor is 7/8: `raw.rs:182-190` reserves
"12.5 % of the slots as empty". So, ignoring `DELETED` and treating
slots as independent, the probability that a group contains **no**
empty slot — i.e. that the probe must continue — is `(7/8)^W`:

```
 W = 8  (NEON, your machine):  (7/8)^8  = 0.3436
 W = 16 (SSE2):                (7/8)^16 = 0.1181

 expected groups scanned = 1 / (1 - p)
   W = 8 : 1 / 0.6564 = 1.524 groups -> 1.524 * 8  = 12.2 control bytes
   W = 16: 1 / 0.8819 = 1.134 groups -> 1.134 * 16 = 18.1 control bytes

 instructions per group (Step 2): load + dup + compare + extract = 4
   W = 8 : 4 / 8  = 0.50 instructions per slot scanned
   W = 16: 4 / 16 = 0.25
 a scalar per-slot probe: load + compare + branch >= 3 per slot
```

Two conclusions the width hazard is designed to hide. The 8-wide group
takes **more iterations** (1.52 vs 1.13) but touches **fewer control
bytes** (12.2 vs 18.1), because a wide group scans slots it did not
need. And even the "worse" NEON path is about 6× fewer instructions per
slot than a scalar probe. That is why halving the group is survivable;
it is also why `mod.rs:9-12` refuses to go *wider* than 16 with AVX —
"the probability of finding a match drops off drastically after the
first few buckets", so extra width buys slots you were never going to
look at.

(Two caveats, stated because the model is a model: `DELETED` bytes do
not terminate a probe, which pushes both numbers up in a table that has
seen erases; and after the first iteration the triangular stride jumps
to an uncorrelated region, so the independence assumption is better
after the first group than within it.)

### Step 6 — memchr's 4× unroll, and the movemask it does *not* pay

> **In:** a haystack of megabytes and one needle byte.
> **Out:** the position of the first match, having executed zero
> `vshrn` sequences on any block that does not contain one.

memchr has the opposite profile from hash probing: it expects to scan
enormous runs of non-matches, so the miss path is the one to optimise.
`arch/generic/memchr.rs:107` sets `LOOP_SIZE = 4 * V::BYTES` — 64 bytes
on NEON — and the loop loads four vectors, compares each, and combines:

```rust
// memchr src/arch/generic/memchr.rs:172-206 — the unrolled search loop
   172              while cur <= end.sub(Self::LOOP_SIZE) {
   175                  let a = V::load_aligned(cur);
   176                  let b = V::load_aligned(cur.add(1 * V::BYTES));
   177                  let c = V::load_aligned(cur.add(2 * V::BYTES));
   178                  let d = V::load_aligned(cur.add(3 * V::BYTES));
   179                  let eqa = self.v1.cmpeq(a);
// ... 180-182: eqb, eqc, eqd ...
   183                  let or1 = eqa.or(eqb);
   184                  let or2 = eqc.or(eqd);
   185                  let or3 = or1.or(or2);
   186                  if or3.movemask_will_have_non_zero() {
   187                      let mask = eqa.movemask();
   188                      if mask.has_non_zero() {
   189                          return Some(cur.add(topos(mask)));
   190                      }
// ... 192-204: the same for eqb, eqc, eqd; the last needs no test ...
   205                  }
   206                  cur = cur.add(Self::LOOP_SIZE);
```

The OR tree at 183-185 collapses four comparison vectors into one, so
the loop asks a single question per 64 bytes. That much is the same
idea as simdjson's block pipeline and polars' one-branch-per-block
filter: amortise the expensive extraction, then localise only on a hit.

But look at line 186, and at what NEON does with it:

```rust
// memchr src/vector.rs:358-368 — the NEON override
   358          /// This is the only interesting implementation of this routine.
   359          /// Basically, instead of doing the "shift right narrow" dance, we use
   360          /// adjacent folding max to determine whether there are any non-zero
   361          /// bytes in our mask. If there are, *then* we'll do the "shift right
   362          /// narrow" dance. In benchmarks, this does lead to slightly better
   363          /// throughput, but the win doesn't appear huge.
   365          unsafe fn movemask_will_have_non_zero(self) -> bool {
   366              let low = vreinterpretq_u64_u8(vpmaxq_u8(self, self));
   367              vgetq_lane_u64(low, 0) != 0
   368          }
```

`vpmaxq_u8` is a pairwise maximum: it folds the 16 lanes down so that
any non-zero byte survives into the low half, and one `vgetq_lane_u64`
plus a compare answers "is anything set?". So on the miss path — the
one that runs for essentially the whole haystack — memchr executes
**no** `vshrn`, no `& 0x8888…`, and no `movemask` at all. The nibble
dance at `vector.rs:323-329` runs only inside the `if` at line 186,
i.e. only in the 64-byte block that actually contains the match.

That is a strictly better claim than "one movemask per 64 bytes", and
it is the shape to copy: the *cheapest possible* any-match test on the
hot path, the *precise* extraction only where the answer matters.

Finally, note the index arithmetic that goes with convention (b).
Because each lane owns a nibble, `topos` must divide by 4:

```rust
// memchr src/vector.rs:453-456 — first_offset for the nibble mask
   453          #[inline(always)]
   454          fn first_offset(self) -> usize {
   455              (self.0.trailing_zeros() >> 2) as usize
   456          }
```

Forgetting the `>> 2` gives an offset 4× too large — which is exactly
why memchr wraps the value in a `NeonMoveMask` newtype
(`vector.rs:387`) instead of passing a bare `u64` around: the type is
what stops a nibble-mask being used where a bit-mask is expected.
hashbrown makes the same move with `BITMASK_STRIDE` (8 on NEON,
1 on SSE2), so its shared `BitMask` code divides by the right constant
without knowing which backend it is on.

### Step 7 — the portability pattern to copy

> **In:** one algorithm you want on three ISAs.
> **Out:** one algorithm, three ~100-line backends, and zero
> abstraction cost after monomorphisation.

Both crates write the algorithm once against a tiny interface and
implement that interface per ISA:

- memchr defines `trait Vector` (`vector.rs:17`) with `BYTES`
  (`:20`), `splat`, `load`, `cmpeq`, `movemask` (`:54`) and the
  optional `movemask_will_have_non_zero` from Step 6; a second trait,
  `MoveMask` (`vector.rs:82`), owns the bits-per-lane convention.
- hashbrown defines `struct Group` with `WIDTH`, `load`, `match_tag`,
  `match_empty`, `match_empty_or_deleted`, `match_full` — one file per
  backend, chosen by the `cfg_if!` of Step 2.

Both bind at **compile** time. That is the cheapest of the three
binding times this topic shows you: hashbrown and memchr bind with
`cfg`, polars binds at **call** time with a runtime feature test
(`filter/primitive.rs:33`), and SimSIMD binds at **init** time with a
function-pointer table filled by a library constructor
(`c/numkong.c:917`). Compile-time binding is right here because a
`HashMap` probe is a handful of instructions — an indirect call would
cost more than the work — and because Rust ships source, so the user's
own `cargo build` is the dispatch.

The generic/SWAR backend earns its keep twice: it is the portability
floor, and it is the oracle. Any new backend must agree with it on
every input, which is exactly the testing strategy for `filter.rs`'s
NEON compaction — write the scalar version first, then diff.

## Where each step lives in the code

hashbrown at `d69025b`, memchr at `5fdb40c`.

| anchor | step | what it is |
|---|---|---|
| `hashbrown src/control/group/mod.rs:8-45` | 2, 7 | the `cfg_if!` backend choice; the stale "NEON wasn't worth it" comment at 14-16 |
| `hashbrown src/control/group/sse2.rs:12-20` | 2 | `BITMASK_STRIDE = 1`, `Group(__m128i)` — 16 control bytes |
| `hashbrown src/control/group/sse2.rs:73-86` | 1-2 | `match_tag` = `_mm_cmpeq_epi8` + `_mm_movemask_epi8` |
| `hashbrown src/control/group/neon.rs:6-21` | 2 | `BitMaskWord = u64`, `BITMASK_STRIDE = 8`, `Group(uint8x8_t)` — EIGHT bytes |
| `hashbrown src/control/group/neon.rs:68-73` | 2 | `match_tag` = `vceq_u8` + `vget_lane_u64` — no narrowing at all |
| `hashbrown src/control/group/neon.rs:85-99` | 4 | `match_empty_or_deleted` via `vcltz_s8`, `match_full` via `vcgez_s8` |
| `hashbrown src/control/bitmask.rs:55-59` | 2 | the divide by `BITMASK_STRIDE` that makes both conventions share code |
| `hashbrown src/control/bitmask.rs:86-90` | 2 | `BITMASK_ITER_MASK` — one bit per lane before iterating |
| `hashbrown src/control/group/generic.rs:97-109` | 3 | SWAR `match_tag` and the exact false-positive condition |
| `hashbrown src/control/group/generic.rs:115-119` | 3 | `match_empty` with no subtraction — why the encoding matters |
| `hashbrown src/control/tag.rs:9,12,35-48` | 4 | `EMPTY`, `DELETED`, and the top-7-bits tag |
| `hashbrown src/raw.rs:2009-2045` | 5 | `find_inner` — the real probe loop |
| `hashbrown src/raw.rs:83-92` | 5 | `ProbeSeq::move_next` — triangular stride |
| `hashbrown src/raw.rs:182-190` | 5 | the 7/8 maximum load factor used in Step 5's arithmetic |
| `memchr src/vector.rs:17-82` | 7 | `trait Vector` and `trait MoveMask` |
| `memchr src/vector.rs:321-329` | 2 | NEON movemask: `vshrn_n_u16(_, 4)` then `& 0x8888…` |
| `memchr src/vector.rs:358-368` | 6 | `movemask_will_have_non_zero` via `vpmaxq_u8` — the miss path |
| `memchr src/vector.rs:453-456` | 2 | `first_offset` divides by 4 |
| `memchr src/arch/generic/memchr.rs:107` | 6 | `LOOP_SIZE = 4 * V::BYTES` |
| `memchr src/arch/generic/memchr.rs:172-206` | 6 | the unrolled loop: OR tree, one test, localise on hit |

Reading order: `mod.rs` first (48 lines, and it tells you which file
matters), then `neon.rs` → `sse2.rs` → `generic.rs` in that order
(shrunk → native → SWAR), then `bitmask.rs` to see how the three
conventions share an iterator, then `raw.rs:2009` for the loop that
uses them. Then memchr's `vector.rs` NEON block and the unrolled loop.

## Questions for notes.md

1. Redo Step 5's probe arithmetic for a table at 50 % load instead of
   87.5 %. At what load factor does the expected number of control
   bytes scanned by `W = 8` exceed that of `W = 16`, if ever?
2. Convention (b) gives 4 bits per lane and convention (c) gives 8.
   Write the two `trailing_zeros` expressions, then say what goes wrong
   if you swap them: which one silently returns a *valid but wrong*
   index, and which one returns an out-of-range one?
3. `generic.rs:97-103` says the SWAR false positive needs at least one
   true match. Construct an 8-byte group and a tag where it fires, and
   one where a naive reading ("adjacent bytes false-positive") would
   predict a hit but the code gives none.
4. For M2's table: your tags are currently full hashes. Compute what
   truncating to 7 bits costs you — the probability that a
   non-matching slot survives `match_tag` — and what it buys per probe
   in bytes touched.
5. Compile-time `cfg` (here), runtime detect (polars
   `filter/primitive.rs:33`), init-time function pointers (SimSIMD
   `c/numkong.c:917`): which fits a Cypher engine shipping one binary
   to unknown ARM servers, and what does that choice cost in the
   `HashMap` probe specifically?

## Done when

Answer each before unfolding it.

- [ ] You can explain what movemask is for, and give the three different answers to "one bit per lane" with their index arithmetic.

  <details><summary>Answer</summary>

  A vector compare produces lanes of `0xFF`/`0x00`, which you cannot
  loop over; movemask converts that to an integer whose bits name the
  matching lanes, after which `trailing_zeros` and `mask & (mask-1)`
  finish the search.

  (a) SSE2: `_mm_movemask_epi8` gives 1 bit per lane, stride 1
  (`sse2.rs:12`). (b) memchr on NEON: `vshrn_n_u16(_,4)` plus
  `& 0x8888…` gives 4 bits per lane, so the index is
  `trailing_zeros() >> 2` (`vector.rs:325-328`, `:455`). (c) hashbrown
  on NEON: an 8-byte group means the compare result *is* the mask,
  8 bits per lane, index `trailing_zeros() / 8`
  (`neon.rs:8`, `bitmask.rs:58`).

  </details>

- [ ] You can state hashbrown's group width on your machine, prove it from the source, and say what the crate's design comment does and does not claim.

  <details><summary>Answer</summary>

  **8 bytes.** `mod.rs:24-33` selects `mod neon` when
  `target_arch = "aarch64"`, `target_feature = "neon"` and
  `target_endian = "little"`; `neon.rs:16` is
  `struct Group(neon::uint8x8_t)` and `neon.rs:21` defines `WIDTH` as
  its size — 8. SSE2's is `__m128i`, so 16 (`sse2.rs:20`).

  The comment at `mod.rs:14-16` says only that an earlier NEON attempt
  lost to the generic implementation because "most NEON instructions
  have multi-cycle latency". It names no width, cites no benchmark, and
  is now stale, since a NEON backend ships. The design that *did* win
  is visible in the code: shrink the group to 8 so that
  `vceq_u8` + `vget_lane_u64` needs no narrowing step at all.

  </details>

- [ ] You can explain SWAR, and state the SWAR false positive's exact condition rather than a vague one.

  <details><summary>Answer</summary>

  SWAR treats a `u64` as eight byte lanes and uses integer
  instructions: `cmp = word ^ repeat(tag)` makes matching bytes zero,
  then `(cmp - repeat(0x01)) & !cmp & repeat(0x80)` sets bit 7 in
  exactly the zero bytes (`generic.rs:108-109`). Four ALU ops for eight
  lanes.

  The documented failure (`generic.rs:97-103`) is narrower than "the
  borrow can cross lanes": it fires only for a byte differing from the
  tag **in its lowest bit only**, only when there is **at least one
  true match** to originate the borrow, never for `EMPTY` or `DELETED`,
  and with probability under 1 % per tag. It is safe because the key
  comparison at `raw.rs:2035` rejects it.

  </details>

- [ ] You can narrate the probe loop and compute what the group width costs, rather than asserting that wider is better.

  <details><summary>Answer</summary>

  `find_inner` (`raw.rs:2009-2045`): compute the 7-bit tag, load a
  group, iterate `match_tag`'s candidate lanes calling `eq` on each,
  stop with `None` if the group holds an `EMPTY`, else advance by the
  triangular stride (`raw.rs:90-91`).

  At hashbrown's 7/8 load factor (`raw.rs:188-189`), a group continues
  the probe with probability `(7/8)^W`: 0.3436 at W = 8 and 0.1181 at
  W = 16, so the expected groups scanned are 1.52 and 1.13 — but the
  expected *control bytes* scanned are 12.2 and 18.1. The narrow group
  loops more and reads less. Per slot the group probe costs
  4/8 = 0.5 instructions (NEON) or 4/16 = 0.25 (SSE2), against ≥ 3 for
  a scalar per-slot probe.

  </details>

- [ ] You can say what memchr pays on the miss path, and why "one movemask per 64 bytes" is not quite right.

  <details><summary>Answer</summary>

  Zero movemasks. The loop ORs four comparison vectors together
  (`arch/generic/memchr.rs:183-185`) and tests the result with
  `movemask_will_have_non_zero`, which NEON overrides
  (`vector.rs:365-368`) to be a single `vpmaxq_u8` plus a lane read —
  the comment at 358-363 says so explicitly. The `vshrn` /
  `& 0x8888…` sequence runs only inside the `if` at line 186, i.e. only
  in a 64-byte block that actually contains the needle.

  So the miss path costs 4 loads, 4 compares, 3 ORs, 1 fold and 1 test
  per 64 bytes, and no bitmask is ever materialised.

  </details>

- [ ] You can state the portability pattern and place all three binding times in this topic.

  <details><summary>Answer</summary>

  Write the algorithm once against a minimal interface — memchr's
  `Vector`/`MoveMask` traits (`vector.rs:17`, `:82`), hashbrown's
  `Group` struct — and give each ISA a ~100-line implementation. The
  generic backend is both the portability floor and the test oracle.

  Binding times: **compile** (hashbrown/memchr `cfg_if!`), **init**
  (SimSIMD's `__attribute__((constructor))` at `c/numkong.c:917`
  filling a function-pointer table), **call** (polars'
  `is_avx512_enabled()` test in `filter/primitive.rs:33`). Compile-time
  wins here because the dispatched unit — one group probe — is smaller
  than an indirect call.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the load-factor crossover and the 7-bit-tag cost for M2.

  <details><summary>Answer</summary>

  Self-check. Question 1 has a checkable form: solve
  `(1/(1-α^8))·8 > (1/(1-α^16))·16` for α, and notice that the left
  side is smaller for every α in (0,1) — a narrow group never reads
  *more* control bytes, it only loops more often. Question 4 has a
  one-line answer: a 7-bit tag lets 1/128 of non-matching slots through
  to the key comparison.

  </details>

## References

**Code**
- [hashbrown](https://github.com/rust-lang/hashbrown) at `d69025b` —
  `src/control/group/` (one file per backend), `src/control/bitmask.rs`
  and `src/control/tag.rs` for the shared conventions, and
  `src/raw.rs:2009` for the probe loop that uses them. Read `neon.rs`
  before `sse2.rs`: it is the one your machine compiles.
- [memchr](https://github.com/BurntSushi/memchr) at `5fdb40c` —
  `src/vector.rs` (the `Vector` trait, the NEON movemask idiom, and the
  `vpmaxq_u8` override that avoids it) and
  `src/arch/generic/memchr.rs` (the 4× unrolled search loop).
