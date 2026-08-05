# hashbrown: the probe loop the flamegraph couldn't show

This IS `std::collections::HashMap` — you profiled it in topic 0 (21% SipHash,
rest inlined probe loop), and now you read the probe loop the flamegraph
flattened into "everything else". One idea carries the whole design: keep a
dense array of 1-byte tags beside the slots, so one SIMD load filters a whole
group of candidates before a single key byte is touched. This chapter builds
that idea step by step — open addressing, the control byte, group probing, the
probe sequence, tombstones — then maps each step onto the source.

Every anchor below is hashbrown **0.17.1** (`Cargo.toml:3`), the commit
`d69025b` this repo pins, quoted with the line numbers the code occupies in
that version. One warning before you start, because it changes half the
numbers people quote about SwissTable: **the group width is a property of the
target, not of the design.** `Group` is `__m128i` on x86 with SSE2 —
16 tags — and `uint8x8_t` on aarch64 NEON or a bare `u64` in the portable
fallback — **8 tags** (`src/control/group/sse2.rs:20`,
`src/control/group/neon.rs:16`, `src/control/group/generic.rs:41`, selected by
the `cfg_if` at `src/control/group/mod.rs:8-46`). This repo measures on an
Apple M3 Pro, so every figure below that depends on width is given for
`Group::WIDTH = 8` first, with the SSE2 value alongside.

## The problem in one sentence

A chained hash table pays 2+ dependent cache misses per lookup (the bucket
array, then each malloc'd node), and topic 0's `cache_ladder` priced a
dependent DRAM miss at ~100 ns on this machine
([FINDINGS.md](../../FINDINGS.md) row 0) — when the theoretical minimum is one
miss: the line the entry actually lives on.

## The concepts, step by step

### Step 1 — open addressing: store entries in the array itself

> **In:** nothing yet — this step names the family hashbrown belongs to and
> the one parameter (load factor) that decides what it costs.
> **Out:** a flat array of slots and a probe rule, plus the reason a maximum
> load factor is mandatory. Step 2 makes the probe cheap.

Instead of buckets pointing at malloc'd chain nodes (**chaining** — the family
the [redis dict chapter](reading-redis-dict.md) covers), **open addressing**
stores the key-value pairs directly in one flat array of **slots** (a slot is
one fixed-size home for one entry, occupied or not). On a **collision** — the
slot your hash points at is already taken — you don't follow a pointer, you
**probe**: try other slots in a deterministic sequence until you find the key
or an empty slot. Wins: no per-entry malloc, no pointer chase, and the probe
walks memory a hardware prefetcher can follow.

The cost is that performance collapses as the array fills. Under the standard
uniform-hashing model — every probe lands on an independent uniformly random
slot, which is the textbook idealisation, not literally what Step 4's probe
does — the expected number of *slots examined* by an unsuccessful search at
**load factor** α (occupied slots ÷ total slots) is

```
E[slots examined, miss] = 1 / (1 − α)

  symbols:  α = n/m, load factor      n = live entries    m = slots
  reading:  each probe has probability (1 − α) of hitting an empty slot,
            so the number of tries until the first empty is geometric
```

Worked, with the divisions performed:

```
α = 0.50  →  1 / (1 − 0.50)  = 1 / 0.500  =  2.0 slots examined
α = 0.75  →  1 / (1 − 0.75)  = 1 / 0.250  =  4.0
α = 0.875 →  1 / (1 − 0.875) = 1 / 0.125  =  8.0     ← hashbrown's limit
α = 0.9375→  1 / (1 − 0.9375)= 1 / 0.0625 = 16.0
```

Eight slot examinations at hashbrown's 7/8 limit against two at the classic
50%. That is why every open-addressing table before SwissTable capped α at
about 0.5 — and it is exactly the number Steps 2 and 3 make cheap rather than
smaller. Hold on to the 8.0.

Deletion also gets harder (Step 5), for reasons that fall straight out of the
probe rule.

### Step 2 — the control byte: a dense 1-byte summary of every slot

> **In:** the flat slot array and probe rule from Step 1.
> **Out:** a second array — one byte per slot — that answers "is this slot
> worth touching?" without touching it. Step 3 reads 8 or 16 of these bytes
> at once; Step 5 reuses their spare encoding for deletion.

The naive probe compares full keys slot by slot, touching a cache line of slot
data per step. hashbrown's move is to keep a *separate, dense* array holding
**one control byte per slot** — a **tag**: a one-byte summary that is either
"empty", "deleted", or seven bits of the entry's own hash.

```rust
// src/control/tag.rs — the whole encoding, 9-12 and 35-49
     9      pub(crate) const EMPTY: Tag = Tag(0b1111_1111);
    // ... 10-11: doc comment ...
    12      pub(crate) const DELETED: Tag = Tag(0b1000_0000);
    // ... 13-34: is_full / is_special / special_is_empty, all one-bit tests ...
    35      pub(crate) const fn full(hash: u64) -> Tag {
    // ... 36-46: MIN_HASH_LEN, so a 32-bit usize hash still uses its own top bits ...
    47          let top7 = hash >> (MIN_HASH_LEN * 8 - 7);
    48          Tag((top7 & 0x7f) as u8) // truncation
    49      }
```

Line 47 is the one that matters: the tag of an occupied slot is the **top 7
bits of the hash**, and line 48 masks off the eighth so the high bit stays 0.
That single high bit is the entire state machine — `is_full` is
`self.0 & 0x80 == 0` (tag.rs:17), and both special values have it set, which
is why `EMPTY` is `0xff` and `DELETED` is `0x80`: they differ in the *low* bit
(`special_is_empty` at tag.rs:30 tests `self.0 & 0x01`), so one SIMD sign test
finds "empty or deleted" and one low-bit test separates them.

```
tag values:  EMPTY = 0xff   DELETED = 0x80   FULL = 0b0xxxxxxx (top 7 hash bits)

hash (64 bits): ┌──────── low bits: h1 ───────┬─ top 7 bits ─┐
                └── which slot to probe first ┴─ tag value ──┘

control array:  [23|EMPTY|91|07|DELETED|55|23|EMPTY| ... ]
                 └───── one 8-byte (NEON/generic) load ─────┘
                 └──────────── or 16 bytes on SSE2 ─────────┘
slot array:     [ kv | ___ | kv | kv | ___ | kv | kv | ___ ]  touched only on tag hit
```

Two naming warnings, because the literature and the code disagree. The
abseil/CppCon vocabulary calls the index bits **h1** and the tag bits **h2**;
hashbrown keeps `h1` (`src/raw.rs:61-64`, and it is simply `hash as usize` —
the *whole* hash truncated, then masked by `bucket_mask`, so effectively the
low bits) but has no `h2`: the tag constructor is `Tag::full` and the local is
called `tag_hash` (raw.rs:2010). Second, the two arrays are not two
allocations. `RawTableInner` holds one pointer:

```rust
// src/raw.rs — RawTableInner, 566-580
   566  struct RawTableInner {
   567      // Mask to get an index from a hash value. The value is one less than the
   568      // number of buckets in the table.
   569      bucket_mask: usize,
   570
   571      // [Padding], T_n, ..., T1, T0, C0, C1, ...
   572      //                              ^ points here
   573      ctrl: NonNull<u8>,
   574
   575      // Number of elements that can be inserted before we need to grow the table
   576      growth_left: usize,
   577
   578      // Number of elements in the table, only really used by len()
   579      items: usize,
   580  }
```

The comment on lines 571-572 is the layout: slots grow *downward* from the
`ctrl` pointer (T0 immediately before C0) and control bytes upward, one
allocation, one pointer. So the "dense filter, fat payload" split of README §4
is a split in addressing, not in allocation: the filter array is 1 byte per
slot, so 64 slots' worth of metadata fit in one 64-byte cache line.

### Step 3 — group probing: a whole group of tags in one instruction

> **In:** the dense control array from Step 2 and the probe obligation from
> Step 1.
> **Out:** the real lookup loop, and the cache-line budget of one lookup.
> Step 4 supplies the `probe_seq.move_next` this loop calls.

Because tags are dense bytes, **SIMD** (single instruction, multiple data —
one CPU instruction applied to a vector of lanes) can compare the wanted tag
against a whole **group** of adjacent tags at once, producing a bitmask of
candidate lanes. This is the entire lookup:

```rust
// src/raw.rs — find_inner, 2009-2046 (safety comments elided)
  2009      unsafe fn find_inner(&self, hash: u64, eq: &mut dyn FnMut(usize) -> bool) -> Option<usize> {
  2010          let tag_hash = Tag::full(hash);
  2011          let mut probe_seq = self.probe_seq(hash);
  2012
  2013          loop {
  // ... 2014-2027: SAFETY comment — pos is masked, and the trailing group is
  //                always readable because of Step 6's extra Group::WIDTH bytes ...
  2028              let group = unsafe { Group::load(self.ctrl(probe_seq.pos)) };
  2029
  2030              for bit in group.match_tag(tag_hash) {
  // ... 2031-2032: comment: the & is a modulo, buckets being a power of two ...
  2033                  let index = (probe_seq.pos + bit) & self.bucket_mask;
  2034
  2035                  if likely(eq(index)) {
  2036                      return Some(index);
  2037                  }
  2038              }
  2039
  2040              if likely(group.match_empty().any_bit_set()) {
  2041                  return None;
  2042              }
  2043
  2044              probe_seq.move_next(self.bucket_mask);
  2045          }
  2046      }
```

Four lines carry it. **2028** loads one group of control bytes — 8 bytes on
this machine, 16 under SSE2 — with a single unaligned vector load. **2030**
compares all of them against the wanted tag in one instruction and iterates
only the lanes that matched: on aarch64 that is `vceq_u8` against a splatted
tag plus a reinterpret to a `u64` bitmask (`neon.rs:68-73`); on x86 it is
`_mm_cmpeq_epi8` followed by `_mm_movemask_epi8` (`sse2.rs:73-86`). **2035**
is the only place a real key is compared, and it runs only for lanes that
already matched seven hash bits. **2040** is the stopping rule and the subject
of Step 5: an `EMPTY` anywhere in the group means the key cannot be further
along, so the search ends; a `DELETED` does *not* stop it.

Now the false-positive rate, which is what earns line 2035 its rarity. A tag
collision needs 7 bits to agree, probability 2⁻⁷ = 1/128 per occupied lane, so
per group load, with the table at its 7/8 limit:

```
Group::WIDTH = 8  (NEON / generic — this repo's machine)
    occupied lanes ≈ 8 × 7/8 = 7
    E[wasted key compares per group] = 7 / 128 = 0.0547  →  5.5%

Group::WIDTH = 16 (SSE2)
    occupied lanes ≈ 16 × 7/8 = 14
    E[wasted key compares per group] = 14 / 128 = 0.109  →  10.9%
```

The earlier version of this chapter quoted the 16-wide figure ("~12% of the
time") without saying which backend it belonged to; on aarch64 it is half
that, because the group is half as wide.

Cache-line budget per lookup: one line of control bytes (the group load at
2028) plus one line of slot data (the key compare at 2035) — the theoretical
minimum, plus one dense byte per slot. That is the number the flamegraph could
not show you.

### Step 4 — the probe sequence: triangular stride, guaranteed coverage

> **In:** a group load from Step 3 that contained neither the key nor an
> `EMPTY`.
> **Out:** the next group to load, and the guarantee that repeating this
> visits every group exactly once. Step 6's `bucket_mask` power-of-two
> invariant is what makes the guarantee true.

**Clustering** is the failure mode of linear probing (always try the next
slot): runs of occupied slots grow, adjacent runs merge, and everyone's probe
gets longer — including keys whose own home slot was free. hashbrown avoids it
by growing the stride:

```rust
// src/raw.rs — ProbeSeq and its only method, 76-93
    76  struct ProbeSeq {
    77      pos: usize,
    78      stride: usize,
    79  }
    80
    81  impl ProbeSeq {
    82      #[inline]
    83      fn move_next(&mut self, bucket_mask: usize) {
    // ... 84-88: debug_assert that we have not run past the end of the sequence ...
    90          self.stride = self.stride.wrapping_add(Group::WIDTH);
    91          self.pos = self.pos.wrapping_add(self.stride) & bucket_mask;
    92      }
    93  }
```

Line 90 adds one *group width* to the stride each time, and line 91 adds the
new stride to the position — so after k steps the probe sits at

```
pos_k = h1 + WIDTH × (1 + 2 + … + k)  =  h1 + WIDTH × k(k+1)/2   (mod m)

  symbols:  h1 = the starting slot (raw.rs:2453, h1(hash) & bucket_mask)
            WIDTH = Group::WIDTH (8 here, 16 on SSE2)
            k = number of move_next calls        m = number of slots
```

Those are the **triangular numbers** scaled by the group width, and the
comment at raw.rs:66-74 links Fabian Giesen's proof that triangular numbers
mod 2ⁿ hit every residue exactly once — so with m a power of two the sequence
visits every group exactly once, never cycles early, and never misses a slot.
`probe_seq` starts it at `stride: 0` (raw.rs:2449-2456), so the first
`move_next` jumps a single group and the walk begins as a linear scan.

Now put Step 1's arithmetic together with Step 3's group width, which is the
whole SwissTable argument in one division. Step 1 said an unsuccessful search
at α = 7/8 examines ~8 slots. Those slots are contiguous within a group, so
the number of *group loads* — the thing that costs a cache miss — is:

```
                        slots examined      group loads      group loads
  load factor α         (Step 1: 1/(1−α))   at WIDTH = 8     at WIDTH = 16
  ------------------------------------------------------------------------
  0.50                        2.0            2.0/8  = 0.25    2.0/16 = 0.13
  0.75                        4.0            4.0/8  = 0.50    4.0/16 = 0.25
  0.875 (hashbrown)           8.0            8.0/8  = 1.00    8.0/16 = 0.50
  0.9375                     16.0           16.0/8  = 2.00   16.0/16 = 1.00
```

At the 7/8 limit an 8-wide group resolves an average miss in **one** group
load; a 16-wide group needs one every other lookup. The classic 50% cap bought
2.0 slot examinations where hashbrown buys 8.0 — and then made them free by
looking at eight at a time. Raising the load factor was not a compromise the
SIMD paid for; it is the thing the SIMD *bought*.

### Step 5 — deletion and tombstones: why DELETED ≠ EMPTY

> **In:** Step 3's stopping rule (`match_empty` ends the search) and Step 2's
> spare tag value.
> **Out:** the erase rule, the condition under which a tombstone is *not*
> written, and the cleanup path a churn-heavy table triggers.

Open-addressing deletion cannot simply mark a slot `EMPTY`: line 2040 stops
every probe at an `EMPTY`, so erasing a slot in the middle of a probe chain
would make keys *beyond* it unfindable. The classic fix is a **tombstone** — a
marker meaning "occupied once, empty now; keep probing" — which is what
`DELETED` is.

hashbrown is more careful than the classic fix, and this is the part most
retellings get wrong:

```rust
// src/raw.rs — inside RawTableInner::erase, 3232-3241 and 3279-3289
  3232          let index_before = index.wrapping_sub(Group::WIDTH) & self.bucket_mask;
  // ... 3233-3235: SAFETY ...
  3236          let (empty_before, empty_after) = unsafe {
  3237              (
  3238                  Group::load(self.ctrl(index_before)).match_empty(),
  3239                  Group::load(self.ctrl(index)).match_empty(),
  3240              )
  3241          };
  // ... 3243-3278: the long comment deriving the rule below — read it ...
  3279          let ctrl = if empty_before.leading_zeros() + empty_after.trailing_zeros() >= Group::WIDTH {
  3280              Tag::DELETED
  3281          } else {
  3282              self.growth_left += 1;
  3283              Tag::EMPTY
  3284          };
  // ... 3285-3286: SAFETY ...
  3287          self.set_ctrl(index, ctrl);
  3288          }
  3289          self.items -= 1;
```

Line 3279 is the rule: a tombstone is written **only** when the erased slot
sits inside an unbroken window of `Group::WIDTH` occupied-or-deleted slots. If
any `EMPTY` is within a group's reach on either side, a probe would have
stopped there anyway, so line 3283 writes `EMPTY` instead and line 3282 gives
the capacity back. The consequence the comment spells out at 3273-3275: a
table with fewer buckets than the group width can never contain a tombstone at
all, because `index_before == index` there.

Insertion knows about tombstones too. `find_insert_index` (raw.rs:1952-1984)
takes the first empty-*or*-deleted lane in a group (`match_empty_or_deleted`,
via `find_insert_index_in_group` at raw.rs:1749-1759), and the accounting is
the subtle bit:

```rust
// src/raw.rs — inside RawTable::insert, 1031-1043
  1031              let mut index = self.table.find_insert_index(hash);
  1032
  1033              // We can avoid growing the table once we have reached our load factor if we are replacing
  1034              // a tombstone. This works since the number of EMPTY slots does not change in this case.
  // ... 1035-1036: SAFETY ...
  1037              let old_ctrl = *self.table.ctrl(index);
  1038              if unlikely(self.table.growth_left == 0 && old_ctrl.special_is_empty()) {
  1039                  self.reserve(1, hasher);
  // ... 1040-1041: SAFETY ...
  1042                  index = self.table.find_insert_index(hash);
  1043              }
```

Line 1038 reads: grow only if we are out of headroom **and** the slot we are
about to fill was genuinely `EMPTY`. Overwriting a `DELETED` slot costs no
capacity — `record_item_insert_at` decrements `growth_left` only for a slot
that `special_is_empty()` (raw.rs:2459-2460) — because Step 3's stopping rule
depends on the count of `EMPTY` slots, not on the count of free ones, and that
count is unchanged.

So a churn-heavy table fills with tombstones and hits `growth_left == 0` while
holding far fewer live items than its capacity. The cure is
`reserve_rehash_inner`:

```rust
// src/raw.rs — inside reserve_rehash_inner, 2756-2757 and 2770-2792
  2756          let full_capacity = bucket_mask_to_capacity(self.bucket_mask);
  2757          if new_items <= full_capacity / 2 {
  // ... 2758-2769: comment and SAFETY ...
  2770              unsafe {
  2771                  self.rehash_in_place(hasher, layout.size, drop);
  2772              }
  2773              Ok(())
  2774          } else {
  // ... 2775-2783: "conservatively resize to at least the next size up" ...
  2784              unsafe {
  2785                  self.resize_inner(
  2786                      alloc,
  2787                      usize::max(new_items, full_capacity + 1),
  // ... 2788-2791: hasher, fallibility, layout ...
  2792                  )
```

Line 2757 is the decision, and it is a *half*, not a threshold on tombstone
count: if the live items would still fit in half the current capacity, the
table is rewritten in place (2771) — `rehash_in_place` (raw.rs:2985) first
converts every FULL tag to DELETED and every DELETED to EMPTY
(raw.rs:2048-2054) and then re-seats each live entry — and no memory is
allocated. Otherwise it really grows (2785-2792). Same disease as LSM
tombstones (topic 1), same cure: rewrite/compact, with a rule for when the
rewrite is worth it.

### Step 6 — two closing tricks: the 7/8 rule and the mirrored tail

> **In:** everything above — the probe loop, the stride, the tombstone rules.
> **Out:** the exact capacity function (which is not 7/8 for small tables) and
> the allocation trick that lets Step 3's group load run off the end of the
> array without a branch.

The load factor is one function, and it has two cases:

```rust
// src/raw.rs — bucket_mask_to_capacity, 182-191
   182  fn bucket_mask_to_capacity(bucket_mask: usize) -> usize {
   183      if bucket_mask < 8 {
   184          // For tables with 1/2/4/8 buckets, we always reserve one empty slot.
   185          // Keep in mind that the bucket mask is one less than the bucket count.
   186          bucket_mask
   187      } else {
   188          // For larger tables we reserve 12.5% of the slots as empty.
   189          ((bucket_mask + 1) / 8) * 7
   190      }
   191  }
```

Line 189 is the famous 7/8 = 87.5%; line 186 is the case the slogan omits.
For 1, 2, 4 or 8 buckets the capacity is `bucket_mask` = buckets − 1, so a
4-bucket table holds 3 entries (75%) and an 8-bucket table holds 7 (87.5%,
which happens to agree). One empty slot must always exist or Step 3's `loop`
at 2013 would never terminate — that is also why `RawTable::new` describes
itself as a table with "exactly 1 bucket" whose data pointer may dangle
(raw.rs:585-587). The old anchor for this function in this chapter was
`raw.rs:152-156`; at `d69025b` it is 182-191.

Compare the overheads at 7/8, per *entry* rather than per slot, since 1/8 of
the slots are empty:

```
control bytes per entry   = 1 × 8/7   =  1.143 bytes
u64→u64 slot = 16 bytes   = 16 × 8/7  = 18.286 bytes
                                        ------
                            total       19.43 bytes per entry

chaining, same map:  8 B bucket-array pointer (at α = 1.0)
                  + 24 B node {next, key, value}, which a 16-byte-granular
                    allocator hands out as a 32 B chunk
                                        ------
                                        40.00 bytes per entry

                     40.00 / 19.43 = 2.06× — and one fewer dependent miss
```

The trailing mirror is the other trick, and it is one line:

```rust
// src/raw.rs — inside TableLayout::calculate_layout_for, 216-223
   216      fn calculate_layout_for(self, buckets: usize) -> Option<(Layout, usize)> {
   217          debug_assert!(buckets.is_power_of_two());
   218
   219          let TableLayout { size, ctrl_align } = self;
   // ... 220-222: ctrl_offset — round the slot region up to ctrl_align ...
   223          let len = ctrl_offset.checked_add(buckets + Group::WIDTH)?;
```

Line 223 allocates `buckets + Group::WIDTH` control bytes rather than
`buckets`, and the tail replicates the head, so the group load at raw.rs:2028
starting on the last bucket reads real bytes instead of running off the
allocation. Branchless boundary handling, paid for in 8 bytes here and 16
under SSE2 — and line 217 is the `is_power_of_two` assertion that Step 4's
coverage proof and the `& bucket_mask` at 2033 both rest on.

### Step 7 — naming what stalled in your topic 0 flamegraph

> **In:** the probe loop from Steps 3-4 and the layout from Steps 2 and 6.
> **Out:** an account of topic 0's measured 10 M-key lookup in terms of named
> lines — and the one hash-policy question your capstone still has to answer.

Topic 0's flamegraph showed the probe loop fully inlined into the bench
closure, with **21% of samples inside SipHash**
(`core::hash::sip::Hasher::write`) and the other ~79% attributed to the
inlined loop ([topics/00-performance-toolbox/notes.md](../00-performance-toolbox/notes.md)).
Now you can name the parts:

- The **21%** is the cost of producing the two things Step 2 splits the hash
  into: `h1` (raw.rs:61) for the starting position and `Tag::full`
  (tag.rs:35-49) for the tag. Rust's default hasher is SipHash-1-3, chosen for
  HashDoS resistance, and it is pure overhead on a u64 key you control.
- The first guaranteed miss is the **control-byte load** at raw.rs:2028, on a
  dense array of one byte per slot.
- The second is the **slot touch** inside `eq(index)` at raw.rs:2035, and
  Step 3's tag filtering exists precisely so that a *third* line is rarely
  needed: 5.5% of the time at `WIDTH = 8`.

That the whole thing measures **8.8 ns at 1e6 keys and 9.3 ns at 1e7** in
topic 0's `lookup_shootout` — nearly flat across a 10× size increase, on a
~160 MB table where a random probe "should" cost a ~100 ns DRAM miss — is not
a contradiction: those 1024 probes are *independent*, so the out-of-order
window overlaps many misses. A single dependent lookup would be far slower.
Two cache lines per lookup is what makes that overlap possible at all; a
chained table's second miss cannot start until its first has landed.

## Where each step lives in the code

| What | Where | Step |
|------|-------|------|
| `h1` — the starting position, low bits | `src/raw.rs:58-64` | 2 |
| `ProbeSeq` and `move_next` (triangular) | `src/raw.rs:66-93` | 4 |
| `bucket_mask_to_capacity` — 7/8, and the small-table case | `src/raw.rs:182-191` | 6 |
| Trailing mirror: `buckets + Group::WIDTH` ctrl bytes | `src/raw.rs:216-223` | 6 |
| `RawTable` | `src/raw.rs:556-562` | 1 |
| `RawTableInner` + the one-allocation layout comment | `src/raw.rs:564-580` | 2 |
| `insert` — the tombstone/`growth_left` rule | `src/raw.rs:1031-1043` | 5 |
| `find_insert_index_in_group` — `match_empty_or_deleted` | `src/raw.rs:1749-1759` | 5 |
| `find_insert_index` — the insert-side probe loop | `src/raw.rs:1952-1984` | 5 |
| **`find_inner` — the lookup, all of it** | `src/raw.rs:2009-2046` | 3 |
| `rehash_in_place`'s tag conversion (FULL→DELETED→EMPTY) | `src/raw.rs:2048-2054`, `2985` | 5 |
| `probe_seq` — where `stride` starts at 0 | `src/raw.rs:2449-2456` | 4 |
| `record_item_insert_at` — `growth_left` only for EMPTY | `src/raw.rs:2459-2460` | 5 |
| `reserve_rehash_inner` — in-place if `items ≤ capacity/2` | `src/raw.rs:2740-2793` | 5 |
| `erase` — DELETED only inside a full group window | `src/raw.rs:3225-3290` | 5 |
| Tag constants + top-7-bit extraction | `src/control/tag.rs:9-49` | 2 |
| Group backend selection (SSE2 / NEON / LSX / generic) | `src/control/group/mod.rs:8-46` | 3 |
| SSE2 group: `__m128i`, 16 wide, `_mm_movemask_epi8` | `src/control/group/sse2.rs:20`, `73-86` | 3 |
| **NEON group (this repo's machine): `uint8x8_t`, 8 wide** | `src/control/group/neon.rs:16`, `68-73` | 3 |
| Generic group: `u64`, 8 wide on 64-bit | `src/control/group/generic.rs:8-21`, `41` | 3 |

Read in this order:

1. **`tag.rs:9-49`** (Step 2) — the encoding. Ask why `EMPTY` is `0xff` and
   full tags are `0b0xxxxxxx`; the answer is at tag.rs:17 and tag.rs:30 —
   "special" is one sign test, "empty vs deleted" is one low-bit test.
2. **`group/mod.rs:8-46`** (Step 3) — read the `cfg_if` before either
   implementation, so you know which `Group` your build gets. Then
   `neon.rs:68-73` and `sse2.rs:73-86` side by side: same function, 8 lanes
   against 16. The stale-sounding comment at mod.rs:14-16 says NEON was not
   worth it — yet lines 24-33 select it; both paths are 8 bytes wide, so the
   choice on aarch64 is between two 8-wide implementations.
3. **`raw.rs:2009-2046`** (Step 3) — `find_inner`, the lookup in 12 real
   lines. Trace one hit and one miss by hand.
4. **`raw.rs:66-93`** (Step 4) — `ProbeSeq`; follow the link at line 74 to the
   coverage proof.
5. **`raw.rs:3225-3290`** (Step 5) — `erase`. The long comment at 3243-3278 is
   the best explanation of tombstones anywhere in the crate; line 3279 is the
   rule it derives.
6. **Aha: `raw.rs:223`** (Step 6) — the whole boundary problem, solved by
   allocating `Group::WIDTH` more bytes than there are buckets.

## Questions to answer in notes.md

1. Why 7/8 rather than redis's 1.0 (dict.c:1653)? Use Step 1's 1/(1−α) and
   Step 4's division: what does α = 15/16 cost in group loads at `WIDTH = 8`,
   and what would it cost on an SSE2 build?
2. Rust chose SipHash for `HashMap` (HashDoS resistance). After this reading
   plus topic 0's 21% flamegraph slice, write the one-paragraph hash policy
   for the capstone: where FxHash/ahash, where SipHash stays, and what
   property of the *key source* decides it.
3. What does `DELETED` do to a long-lived table with churn? Trace it through
   raw.rs:1038 (the growth check), raw.rs:2757 (the in-place threshold) and
   raw.rs:3279 (when a tombstone is even written) — then relate it to LSM
   tombstones from topic 1.
4. `erase` writes `EMPTY` rather than `DELETED` whenever an `EMPTY` is within
   a group's reach (raw.rs:3279). Construct a small table where the same
   deletion writes `DELETED` at `WIDTH = 8` and `EMPTY` at `WIDTH = 16`, and
   say which build ends up doing more work later.
5. This repo measured hashbrown's insert path at p50 42 ns, max 58.4 ms
   ([FINDINGS.md](../../FINDINGS.md) row 2). Which lines produce the 42 ns,
   and which produce the 58.4 ms? (Hint: raw.rs:1038 → raw.rs:2785.)

## Takeaway

SwissTable's trick is not "SIMD makes hashing fast" — SipHash is still 21% of
a lookup. It is that a dense one-byte-per-slot filter turns *probe length*
from a memory problem into a register problem, so the table can run at 87.5%
occupancy with about one group load per miss. The group width, and therefore
half the numbers people quote, depends on which backend your target selects.

## Done when

Answer each before unfolding it.

- [ ] You can draw the control-byte array and narrate one lookup from hash to slot, naming both cache lines it touches.

  <details><summary>Answer</summary>

  The hash is split once. Its low bits become the starting slot —
  `h1(hash) & bucket_mask` (raw.rs:61-64, applied at raw.rs:2453) — and its
  top 7 bits become the tag, `Tag::full` at tag.rs:47, with the eighth bit
  masked off at tag.rs:48 so the tag is distinguishable from `EMPTY` (0xff)
  and `DELETED` (0x80).

  The lookup then loads one *group* of control bytes at that position
  (raw.rs:2028) — **first cache line**, 8 bytes on NEON or generic, 16 under
  SSE2 — and compares all of them against the tag in one instruction
  (raw.rs:2030, `vceq_u8` at neon.rs:70 or `_mm_cmpeq_epi8` at sse2.rs:83).
  Only lanes that matched earn a real key comparison at raw.rs:2035 — **second
  cache line**, the slot itself, which lives *before* the control pointer in
  the same allocation (raw.rs:571-572). If the group holds an `EMPTY`
  (raw.rs:2040) the search returns `None`; otherwise `move_next`
  (raw.rs:83-92) jumps one more group width than last time and the loop
  repeats.

  </details>

- [ ] You can state hashbrown's group width without guessing, and say what changes when it is 8 rather than 16.

  <details><summary>Answer</summary>

  It is `Group::WIDTH`, which is `mem::size_of::<Group>()`, and `Group` is
  chosen by the `cfg_if` at `src/control/group/mod.rs:8-46`: `__m128i` = **16
  bytes** on x86/x86-64 with SSE2 (sse2.rs:20), `uint8x8_t` = **8 bytes** on
  little-endian aarch64 with NEON (neon.rs:16), and `u64` = **8 bytes** in the
  portable fallback on any 64-bit target (generic.rs:8-21, 41). This repo's
  Apple M3 Pro gets 8.

  Three things change with the width. The false-positive rate per group scales
  with it: 7/128 = 5.5% of groups cost a wasted key comparison at width 8
  against 14/128 = 10.9% at width 16. The number of group loads per miss
  scales inversely: Step 1's 8.0 slot examinations at α = 7/8 become 8.0/8 =
  1.00 group loads at width 8 and 8.0/16 = 0.50 at width 16. And the probe
  stride grows by `Group::WIDTH` per step (raw.rs:90), so the two builds walk
  physically different sequences over the same table.

  </details>

- [ ] You can explain why 87.5% occupancy is affordable here and was not affordable for `dense_hash_map` at 50%.

  <details><summary>Answer</summary>

  Because the two designs pay for probe length in different units. Under the
  uniform-hashing model an unsuccessful search examines 1/(1−α) slots:
  1/0.5 = 2.0 at α = 0.5 and 1/0.125 = 8.0 at α = 0.875. A table that
  examines slots one at a time genuinely pays four times more at the higher
  load factor, which is why the classic advice caps α near 0.5.

  hashbrown examines slots `Group::WIDTH` at a time, and those slots are
  contiguous, so the cost unit is group loads: 8.0/8 = **1.00** group load per
  miss at α = 7/8 on this machine, 8.0/16 = 0.50 under SSE2. The extra
  occupancy is nearly free in the currency that matters — cache lines touched
  — while saving 1/8 of the slot array plus every per-node malloc. Concretely,
  a u64→u64 map costs 19.43 bytes per entry here (18.286 for slots at 8/7
  plus 1.143 for control bytes) against about 40 bytes for a chained table
  with 24-byte nodes rounded to 32-byte allocator chunks: 2.06×.

  </details>

- [ ] You can say what `DELETED` is for and when hashbrown declines to write one.

  <details><summary>Answer</summary>

  `DELETED` (tag.rs:12, `0x80`) exists because the probe's stopping rule is
  "this group contains an `EMPTY`" (raw.rs:2040). Marking an erased slot
  `EMPTY` in the middle of a probe chain would stop searches early and hide
  every key that had been pushed past it, so a tombstone is written instead:
  probes skip it, inserts may reuse it.

  hashbrown writes one only when it has to. `erase` loads the group ending at
  the erased slot and the group starting there (raw.rs:3236-3241), and line
  3279 writes `DELETED` only if `empty_before.leading_zeros() +
  empty_after.trailing_zeros() >= Group::WIDTH` — that is, only when the slot
  is inside an unbroken window of `Group::WIDTH` non-empty slots. Otherwise a
  probe would have stopped at a nearby `EMPTY` anyway, so line 3283 writes
  `EMPTY` and line 3282 returns the capacity. A consequence spelled out in the
  comment at 3273-3275: tables smaller than the group width never hold a
  tombstone at all.

  </details>

- [ ] You can trace how a churn-heavy table recovers its capacity, and name the threshold.

  <details><summary>Answer</summary>

  Filling a tombstone costs no capacity — `record_item_insert_at` decrements
  `growth_left` only when the old tag `special_is_empty()` (raw.rs:2459-2460),
  and `insert` checks the same thing before deciding to grow (raw.rs:1038),
  because Step 3's stopping rule depends on the number of `EMPTY` slots and
  overwriting a `DELETED` does not change it. So a table that inserts and
  erases repeatedly eventually reaches `growth_left == 0` while holding far
  fewer live items than its capacity.

  `reserve_rehash_inner` then decides at raw.rs:2756-2757: if
  `new_items <= full_capacity / 2` — the live entries would still fit in half
  the current table — it calls `rehash_in_place` (raw.rs:2771), which converts
  every FULL tag to DELETED and every DELETED to EMPTY (raw.rs:2048-2054) and
  re-seats the live entries without allocating. Otherwise it really grows, to
  at least `full_capacity + 1` (raw.rs:2785-2787), with the comment at
  2775-2776 explaining the conservatism: resizing up avoids "churning deletes
  into frequent rehashes".

  </details>

- [ ] You can account for topic 0's measured numbers in terms of specific lines of this crate.

  <details><summary>Answer</summary>

  The 21% SipHash slice is the work that produces the two hash derivatives the
  table needs: `h1` at raw.rs:61-64 and `Tag::full` at tag.rs:35-49. Nothing
  in the probe loop can start until both exist, and on a u64 key you generate
  yourself it buys only HashDoS resistance you do not need — which is the
  capstone's hash-policy question.

  The remaining ~79% is raw.rs:2009-2046 inlined: the control-byte group load
  at 2028 (one cache line), the SIMD compare at 2030, and the key touch at
  2035 (a second cache line). `lookup_shootout` measured 8.8 ns at 1e6 and
  9.3 ns at 1e7 — nearly flat, because the 1024 probes are independent and the
  out-of-order window overlaps their misses, which two-independent-lines-per
  -lookup makes possible and a chain of dependent loads does not. The insert
  side's 58.4 ms max ([FINDINGS.md](../../FINDINGS.md) row 2) comes from the
  other branch entirely: raw.rs:1038 finding no headroom, then raw.rs:2785
  allocating and re-seating the whole table.

  </details>

## References

**Code**
- [hashbrown](https://github.com/rust-lang/hashbrown) — pinned at **0.17.1** /
  `d69025b` (version confirmed in `Cargo.toml:3`). `src/raw.rs` is 4627 lines,
  most of it SAFETY commentary; the load-bearing parts are listed below.

| File | Lines | What |
|------|-------|------|
| `src/raw.rs` | 61-64 | `h1` — the starting position is just the truncated hash |
| `src/raw.rs` | 66-93 | `ProbeSeq`, the triangular stride, and the link to its proof |
| `src/raw.rs` | 182-191 | `bucket_mask_to_capacity` — 7/8, plus the small-table case |
| `src/raw.rs` | 223 | `buckets + Group::WIDTH` control bytes — the trailing mirror |
| `src/raw.rs` | 566-580 | `RawTableInner` and the one-allocation layout comment |
| `src/raw.rs` | 1038 | grow only if out of headroom *and* the slot was truly EMPTY |
| `src/raw.rs` | 1952-1984 | `find_insert_index` — the insert-side probe |
| `src/raw.rs` | 2009-2046 | `find_inner` — the entire lookup |
| `src/raw.rs` | 2459-2460 | `growth_left` accounting, tombstones excluded |
| `src/raw.rs` | 2757 | rehash in place iff the live items fit in half the capacity |
| `src/raw.rs` | 3279 | write DELETED only inside a full group-width window |
| `src/control/tag.rs` | 9-49 | EMPTY / DELETED / top-7-bit tags |
| `src/control/group/mod.rs` | 8-46 | which `Group` your target actually gets |
| `src/control/group/sse2.rs` | 20, 73-86 | 16-wide group, `_mm_movemask_epi8` |
| `src/control/group/neon.rs` | 16, 68-73 | 8-wide group, `vceq_u8` |
| `src/control/group/generic.rs` | 8-21, 41 | 8-wide portable fallback (`u64`) |

**Measured in this repo**
- [FINDINGS.md](../../FINDINGS.md) row 2 — hashbrown insert p50 42 ns, max
  58.4 ms; the max is the resize at raw.rs:2785, not the probe loop.
- [FINDINGS.md](../../FINDINGS.md) row 0 — the ~1 / 5 / 100 ns cache ladder,
  and the 21% SipHash slice of a lookup.
- [topics/00-performance-toolbox/notes.md](../00-performance-toolbox/notes.md)
  — `lookup_shootout`: HashMap 7.4 ns at n=100 rising only to 9.3 ns at n=1e7.

**Companion chapters**
- [reading-swisstable-talk.md](reading-swisstable-talk.md) — the design
  narrative that produced this code, told as a sequence of rejected designs.
- [reading-redis-dict.md](reading-redis-dict.md) — the chaining family, and
  the incremental rehash hashbrown deliberately does not do.
