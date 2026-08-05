# The SwissTable design walk: how benchmarks kill hash tables

How Google replaced `std::unordered_map` fleet-wide — told as a sequence of
designs, each one killed by a measurement. Watch Kulukundis's CppCon 2017 talk
*after* [`reading-hashbrown.md`](reading-hashbrown.md), because the talk is the
design narrative for the code you just read. This chapter rebuilds that
narrative step by step — each design, the number that killed it, the idea that
replaced it — so you watch for the beats instead of chasing them. Budget ~60
min video + 30 min notes.

**On sourcing.** A talk is not a citable artifact the way a file at a commit
is, so every claim below is grounded in something you can re-check: the
[abseil Swiss Tables design notes](https://abseil.io/about/design/swisstables)
for the design as Google documented it; Google's own
[sparsehash](https://github.com/sparsehash/sparsehash) at `1dffea3d9` for what
`dense_hash_map` really did; hashbrown at `d69025b` for the final shape; and
cppreference for the C++ requirements. Two consequences. First, the
fleet-wide RAM- and CPU-percentage figures this talk is famous for are **not
reproduced here** — they are spoken numbers with no retrievable primary
source, and this repo does not print numbers it has not checked. Note them
yourself as you watch, with the timestamp. Second, where the talk's account
and the code disagree, the code wins; two such places are flagged below.

## The problem in one sentence

`std::unordered_map` costs 2+ dependent cache misses and one malloc'd node per
entry — topic 0's `cache_ladder` priced a dependent DRAM miss at ~100 ns
([FINDINGS.md](../../FINDINGS.md) row 0) — and the three C++ requirements that
force that layout are in the standard, so it cannot be fixed in place.

## The concepts, step by step

The design walk, in one picture — each arrow is a benchmark verdict:

```
std::unordered_map          chaining, per-node malloc, reference stability
        │  "every lookup = 2+ dependent misses"
        ▼
dense_hash_map              open addressing, quadratic probe — but 2 sentinel
        │                   keys taken from the user, and a 50% occupancy cap
        │  "half the array is slack, and the API steals two key values"
        ▼
"store metadata per slot"   1 byte: empty/deleted/full + 7 hash bits (H2)
        │  "but scanning bytes one at a time is a branch per byte"
        ▼
SwissTable                  group the bytes, compare a whole group at once
                            → 87.5% occupancy, ~1 group load per lookup
```

### Step 1 — the incumbent: chaining, mandated by an API contract

> **In:** nothing yet — this step establishes what is being replaced and why
> the replacement could not be done in place.
> **Out:** three named requirements from the C++ standard and the cost they
> impose. Step 2 is the first attempt to escape them.

`std::unordered_map` uses **chaining** (buckets holding malloc'd linked-list
nodes — the family the [redis dict chapter](reading-redis-dict.md) covers) not
because its authors preferred it, but because three published requirements
leave almost nothing else. Naming them precisely matters, because "the
standard mandates chaining" is a summary, not a quotation:

1. **Reference and pointer stability.** "References and pointers to either key
   or data stored in the container are only invalidated by erasing that
   element, even when the corresponding iterator is invalidated"
   (cppreference, `std::unordered_map` → Iterator invalidation → Notes). A
   rehash may invalidate iterators but *not* references — so entries can never
   move, so they cannot live inline in an array that gets reallocated.
2. **The bucket interface.** `bucket_count()`, `bucket_size(n)`, `bucket(key)`
   and `begin(n)`/`end(n)` returning a `local_iterator`, which cppreference
   defines as an iterator that "can be used to iterate through a single bucket
   but not across buckets". Chains are part of the public API.
3. **Node handles** (C++17): `extract()` returns a `node_type`, a handle that
   owns the element's *node* and can be re-inserted into another container
   without copying. That only means anything if a per-element node exists.

The measured cost: a lookup dereferences the bucket array, then a node, then
possibly the next node — each dependent on the last, so the out-of-order
window cannot overlap them — plus a malloc per insert. Lesson zero of the
talk, and the one that generalises past hash tables: **API guarantees are
performance decisions**, made once and un-unmakeable.

### Step 2 — first replacement: `dense_hash_map` and its two warts

> **In:** Step 1's diagnosis — the pointer chase has to go.
> **Out:** open addressing working, with two specific costs (sentinel keys, a
> 50% occupancy cap) and the published probe table that explains the cap.
> Steps 3 and 4 remove one wart each.

Google's earlier answer, `dense_hash_map`, dropped chaining for **open
addressing**: entries live inline in one flat array, collisions are resolved
by probing. Lookups fell to about one miss. Its probe rule is quadratic, and
it is one macro:

```c
// sparsehash@1dffea3d9 — src/sparsehash/internal/densehashtable.h:115-119
   115  // The probing method
   116  // Linear probing
   117  // #define JUMP_(key, num_probes)    ( 1 )
   118  // Quadratic probing
   119  #define JUMP_(key, num_probes)    ( num_probes )
```

Line 119 makes the k-th step jump k slots, so the offsets from the home slot
are 1, 3, 6, 10, … — the triangular numbers, the same sequence hashbrown walks
at `raw.rs:90`, except that hashbrown counts in *groups* and this counts in
slots. The loop it drives is at densehashtable.h:648-653, and it stops at the
first empty slot.

The first wart is in the API. The table has no metadata array, so "empty" and
"deleted" have to be encoded *in key space*: the user must donate two key
values that can never appear in real data.

```c
// sparsehash@1dffea3d9 — densehashtable.h:390, 496 (the two donations)
   390    void set_deleted_key(const key_type &key) {
   // ... 391-395: assert it differs from the empty key ...
   // ... 496: void set_empty_key(const_reference val) {
   497      // Once you set the empty key, you can't change it
   498      assert(!settings.use_empty() && "Calling set_empty_key multiple times");
```

Forget to call them and the table asserts; choose a value that later shows up
in your data and it silently disappears. An API landmine, and one no standard
container could ever ship.

The second wart is memory, and the source states the trade-off outright:

```c
// sparsehash@1dffea3d9 — densehashtable.h:1309-1316
  1309  // How full we let the table get before we resize.  Knuth says .8 is
  1310  // good -- higher causes us to probe too much, though saves memory.
  1311  // However, we go with .5, getting better performance at the cost of
  1312  // more space (a trade-off densehashtable explicitly chooses to make).
  // ... 1313-1315: "feel free to play around", then the template header ...
  1316  const int dense_hashtable<V,K,HF,ExK,SetK,EqK,A>::HT_OCCUPANCY_PCT = 50;
```

Half the array is slack. Why 50 and not Knuth's 80? The file publishes its own
answer — a probe table sitting in the header comment:

```c
// sparsehash@1dffea3d9 — densehashtable.h:77-84 (the file's own numbers)
    77  // NUMBER OF PROBES / LOOKUP       Successful            Unsuccessful
    78  // Quadratic collision resolution   1 - ln(1-L) - L/2    1/(1-L) - L - ln(1-L)
    // ... 79-80: the same for linear probing ...
    81  // -- enlarge_factor --           0.10  0.50  0.60  0.75  0.80  0.90  0.99
    82  // QUADRATIC COLLISION RES.
    83  //    probes/successful lookup    1.05  1.44  1.62  2.01  2.21  2.85  5.11
    84  //    probes/unsuccessful lookup  1.11  2.19  2.82  4.64  5.81  11.4  103.6
```

Read line 84 across: an unsuccessful lookup costs 2.19 slot probes at L = 0.50
and 5.81 at L = 0.80. **2.65× the probes to save 37.5% of the slots** — and
each probe touches a slot, so each is a potential cache miss. Given that
exchange rate, 50% is the right answer. Hold on to the exchange rate; Step 5
changes it.

### Step 3 — the metadata byte: state out of key space, 7 hash bits for free

> **In:** Step 2's two warts — sentinel keys and the occupancy cap.
> **Out:** a dense one-byte-per-slot array that fixes the first wart outright
> and sets up the fix for the second. Step 4 makes reading it cheap.

The idea that removes the sentinels: stop encoding state in key space and keep
**one metadata byte per slot** in a separate dense array. Google's design
notes describe the split of the 64-bit hash:

> H1, a 57 bit hash value, used to identify the element index within the table
> itself … H2, the remaining 7 bits of the hash value, used to store metadata
> for this element. … Each metadata entry consists of one byte, which consists
> of a single control bit and the 7 bit H2 hash.
>
> — [abseil, *Swiss Tables Design Notes*](https://abseil.io/about/design/swisstables)

Two wins at once. No key value is stolen: "empty" and "deleted" are states of
the metadata byte, not of the key. And the 7 H2 bits are a free per-slot
**pre-filter** — a probe compares one byte instead of touching the slot's key,
and is wrong only when 7 bits collide, probability 2⁻⁷ = 1/128 per occupied
slot. This is hashbrown's control byte at `src/control/tag.rs:9-49`, here at
the moment of invention.

**Where the code diverges from the design note.** hashbrown's H2 is the same —
`Tag::full` takes the top 7 bits (`tag.rs:47`) and masks the eighth
(`tag.rs:48`) — but its H1 is not 57 bits:

```rust
// hashbrown@d69025b — src/raw.rs:58-64
    58  /// Primary hash function, used to select the initial bucket to probe from.
    // ... 59-60: #[inline] and a clippy allow ...
    61  fn h1(hash: u64) -> usize {
    62      // On 32-bit platforms we simply ignore the higher hash bits.
    63      hash as usize
    64  }
```

It is the whole hash truncated to `usize` and then masked by `bucket_mask`
(raw.rs:2453), so the index bits and the tag bits *overlap* on a 64-bit
target — harmless, because the index uses low bits and the tag uses high ones,
but it means "H1 is 57 bits" describes abseil, not hashbrown. Also note that
hashbrown has no identifier called `h2` at all; the local is `tag_hash`
(raw.rs:2010). When you hear "H2" in the talk, translate to `Tag::full`.

### Step 4 — group probing: one instruction filters a whole group

> **In:** the dense metadata array from Step 3.
> **Out:** the probe loop as it ships, and the parameter — group width — that
> every remaining number depends on. Step 5 spends what this buys.

The next verdict: scanning metadata bytes one at a time is still a loop with a
branch per byte. But the bytes are dense, so **SIMD** can compare a whole
**group** of adjacent tags against H2 in one instruction. Abseil's design
notes give both the algorithm and the code:

```
1. Use the H1 hash to find the start of the "bucket chain" for that hash.
2. Use the H2 hash to construct a mask.
3. Use SSE instructions and the mask to produce a set of candidate matches.
4. Perform an equality check on each candidate.
5. If no element is found amongst the current candidates, perform probing to
   generate a new set of candidates. Note that a deleted element does not
   cease probing, though an empty element would.

MaskMatch(h2_t hash) const {
  auto match = _mm_set1_epi8(hash);
  return Mask(_mm_movemask_epi8(_mm_cmpeq_epi8(match, metadata)));
}
                    — abseil, Swiss Tables Design Notes
```

`_mm_cmpeq_epi8` compares 16 bytes lane-wise; `_mm_movemask_epi8` collapses
the result to a 16-bit integer whose set bits are the candidate lanes. Step 5
of that list is the tombstone rule, and it is exactly hashbrown's
`match_empty` stopping condition at `raw.rs:2040`.

**Sixteen is not a law.** This is the second place the talk and the code
diverge, and it is the one most retellings get wrong. In hashbrown the group
is whatever the target provides:

| Backend | Type | `Group::WIDTH` | Selected when |
|---|---|---|---|
| SSE2 | `__m128i` (`sse2.rs:20`) | **16** | x86/x86-64 with `sse2` (`mod.rs:17-21`) |
| NEON | `uint8x8_t` (`neon.rs:16`) | **8** | little-endian aarch64 with `neon` (`mod.rs:24-31`) |
| LSX | `m128i` (`lsx.rs:17`) | 16 | nightly + loongarch64 + `lsx` (`mod.rs:34-39`) |
| generic | `u64` (`generic.rs:41`) | **8** on 64-bit | everything else (`mod.rs:42-44`) |

This repo measures on an Apple M3 Pro, so `Group::WIDTH = 8` here and
`match_tag` is `vceq_u8` plus a reinterpret (`neon.rs:68-73`) rather than
`_mm_movemask_epi8` (`sse2.rs:73-86`). A previous version of this chapter
pointed at `neon.rs:78-90` for the group compare; at `d69025b` that range is
`match_empty`, and `match_tag` is 68-73.

The width also sets the false-positive rate, because a group holds
`WIDTH × load` occupied lanes and each collides with probability 1/128:

```
WIDTH = 8  (NEON / generic):  8 × 7/8 = 7  lanes;   7 / 128 = 0.0547  →  5.5%
WIDTH = 16 (SSE2):           16 × 7/8 = 14 lanes;  14 / 128 = 0.109   → 10.9%
```

So the wider group filters more slots per instruction *and* wastes more key
comparisons. Neither number is "the" SwissTable false-positive rate.

### Step 5 — what the combination buys: the exchange rate flips

> **In:** Step 2's published probe table and Step 4's group width.
> **Out:** the 87.5% load factor justified by division rather than assertion,
> plus the deletion rules that come with it. Step 6 generalises the method.

Step 2's exchange rate — more occupancy costs proportionally more probes — was
computed in the currency of *slot probes*. Group probing changes the currency
to *group loads*, and the same arithmetic comes out the other way. Using
densehashtable.h:78's own formula for an unsuccessful lookup,
`1/(1−L) − L − ln(1−L)`, evaluated at SwissTable's load factor:

```
L = 0.500  →  1/0.500 − 0.500 − ln(0.500) = 2.000 − 0.500 + 0.693 =  2.193
L = 0.875  →  1/0.125 − 0.875 − ln(0.125) = 8.000 − 0.875 + 2.079 =  9.204

  (L = 0.500 reproduces the 2.19 printed at densehashtable.h:84, which is how
   we know the formula is being read correctly; 0.875 is not in their table.)

  slot probes:  9.204 / 2.193 = 4.20×  more at 87.5% than at 50%
  group loads:  9.204 / 8  = 1.15   (WIDTH = 8, this machine)
                9.204 / 16 = 0.58   (WIDTH = 16, SSE2)
```

Contiguous slots share a group, so ~9.2 slot probes become **1.15** group
loads at width 8 — barely more than one cache line — or 0.58 at width 16.
(That division is an estimate: the formula assumes each probe lands
independently, whereas a group is a contiguous window. It is the right order
of magnitude, and the direction is not in doubt.) Meanwhile the slot array
shrinks:

```
1,000,000 entries at L = 0.500  →  1,000,000 / 0.500 = 2,000,000 slots
1,000,000 entries at L = 0.875  →  1,000,000 / 0.875 = 1,142,857 slots
                                   2,000,000 / 1,142,857 = 1.75× fewer
```

1.75× fewer slots, plus no per-node malloc, for about one cache line per
lookup. hashbrown encodes the 7/8 in `bucket_mask_to_capacity`
(`raw.rs:182-191`, with a separate case for tables of 8 buckets or fewer —
the earlier version of this chapter cited `raw.rs:152`, which is a different
function at this commit).

Deletion is the bill that comes with open addressing. Because the probe stops
at the first *empty* slot, erasing a slot mid-chain would hide every key
probed past it — hence a **tombstone**, the `DELETED` state, which probes skip
and inserts may reuse. Abseil's list says it in one line: "a deleted element
does not cease probing, though an empty element would." hashbrown then adds
two refinements the talk predates:

- It writes a tombstone only when it must — `erase` checks whether an `EMPTY`
  is already within a group's reach on either side, and if so writes `EMPTY`
  and returns the capacity instead (`raw.rs:3279-3284`).
- When tombstones do choke a table, `reserve_rehash_inner` rewrites it in
  place rather than growing, but only if the live items would fit in *half*
  the current capacity (`raw.rs:2756-2757`); otherwise it really grows
  (`raw.rs:2785-2787`).

Same disease as LSM tombstones from topic 1, same cure: compaction, gated by
a rule about when it pays.

And after all of it, this could still not ship as `std::unordered_map`,
because Step 1's three requirements survive any benchmark. `absl::flat_hash_map`
is a different type with a different contract — which is the point.

### Step 6 — the method is the takeaway

> **In:** all five verdicts above.
> **Out:** the transferable procedure, and the honest limit of what a talk can
> establish.

Every arrow in the design walk is a *measurement*, not a preference:
hypothesise → benchmark → let the number keep or kill the design. That is
topic 0's method applied to data-structure design at fleet scale. Watch the
talk as a methodology demonstration wearing a hash table as a costume.

The limit is worth naming, since this chapter is about believing numbers. The
best-known figures from this talk — what fraction of a fleet's RAM and CPU
goes to hash tables — are spoken claims with no retrievable primary source,
which is why they appear nowhere above. Everything else here survived being
checked: the C++ requirements against cppreference, the 50% cap and its
probe table against sparsehash's own header, H1/H2 against Google's design
notes, and every width and line number against hashbrown at `d69025b`. Write
the spoken numbers into notes.md with a timestamp, mark them unverified, and
treat that as the exercise.

## How to read the talk

Timestamps vary across uploads and this chapter does not assert any, so
navigate by slide content. Each beat below maps to a step above and to
something you can open:

| Watch for | Step | Open alongside |
|---|---|---|
| "the standard basically mandates chaining" | 1 | cppreference `unordered_map` → Iterator invalidation, and `local_iterator` |
| `dense_hash_map`, `set_empty_key`, 50% load | 2 | `densehashtable.h:390`, `:496`, `:1309-1316`, and the probe table at `:77-84` |
| the metadata byte slide (1 control bit + 7 hash bits) | 3 | `tag.rs:9-49`; the abseil design notes' H1/H2 diagram |
| the `_mm_movemask_epi8` slide | 4 | `sse2.rs:73-86` beside `neon.rs:68-73` |
| load factor and tombstone discussion | 5 | `raw.rs:182-191`, `raw.rs:2756-2757`, `raw.rs:3279` |
| any fleet-wide percentage | 6 | your notes — record it with a timestamp, marked unverified |

Suggested route: read the [abseil design notes](https://abseil.io/about/design/swisstables)
first (ten minutes, and it is the written form of Steps 3-4), then watch, then
re-open [`reading-hashbrown.md`](reading-hashbrown.md) Step 3 and check the
talk's account against `find_inner` at `raw.rs:2009-2046`. The talk is
`std::unordered_map` → `dense_hash_map` → metadata → SIMD; the code is that
same walk with eight more years of tombstone bookkeeping bolted on.

**Contrast case.** Watch how differently redis solves the same growth problem.
SwissTable's answer to "the table is full" is to stop the world and rebuild
(`raw.rs:2785`), which this repo measured as a **58.4 ms** worst-case insert
([FINDINGS.md](../../FINDINGS.md) row 2). Redis instead keeps two tables and
migrates one bucket per operation (`dict.c:405-434`), trading a permanently
slower lookup for the absence of that spike — see
[`reading-redis-dict.md`](reading-redis-dict.md). Neither is wrong; they are
answers to different questions about the p99.

## Questions to answer in notes.md

1. Google could not ship this as `std::unordered_map` because of the three
   requirements in Step 1. Which redis `dict` features would SwissTable
   similarly break? (Incremental rehash needs stable *entries*? Check — redis
   moves entries between tables anyway, `dict.c:336-377`; the real conflict is
   `dictScan`'s bucket cursor, `dict.c:1424-1445`.)
2. Estimate bytes per entry for a u64→u64 map: chaining with malloc'd nodes
   versus SwissTable at 7/8. Show the arithmetic — bucket array, node size,
   allocator rounding, control bytes, empty-slot slack. Then compare against
   the 1.75× *slot* ratio derived in Step 5 and explain why the byte ratio is
   larger.
3. Kulukundis argues hash quality matters *more* for open addressing than for
   chaining. Give two distinct mechanisms, using the steps above: one about
   Step 2's probe sequence, one about Step 3's 7-bit tag.
4. Take the exchange rate from Step 2 (2.19 → 5.81 probes for 0.50 → 0.80) and
   redo it in group loads at `WIDTH = 8` and `WIDTH = 16`. At which width does
   Knuth's 0.8 stop looking expensive, and what does that say about who the
   1998 advice was written for?

## Takeaway

The design walk's real lesson is not "use SIMD". It is that a table's maximum
load factor is not a constant of nature but an exchange rate between two
costs — and that changing the *unit* the probe is billed in (slots → groups)
re-prices every design decision downstream of it. Also: check which unit, and
which group width, any quoted SwissTable number was computed in.

## Done when

Answer each before unfolding it.

- [ ] You can retell the rejected-design sequence and give the measured reason for each step.

  <details><summary>Answer</summary>

  `std::unordered_map` → chaining, 2+ *dependent* cache misses per lookup
  (~100 ns each on this machine, [FINDINGS.md](../../FINDINGS.md) row 0) plus a
  malloc per insert; unfixable because of the three requirements in Step 1.

  → `dense_hash_map`: open addressing with quadratic probing
  (`densehashtable.h:119`) cuts it to about one miss, but costs two donated
  sentinel keys (`:390`, `:496`) and caps occupancy at 50%
  (`HT_OCCUPANCY_PCT = 50`, `:1316`), because at 80% an unsuccessful lookup
  costs 5.81 slot probes against 2.19 at 50% (`:84`).

  → metadata byte: one byte per slot holding a control bit and 7 hash bits
  (abseil design notes; `tag.rs:9-49`) — the sentinels are gone and a probe
  can reject a slot without touching it, wrong only 1/128 of the time.

  → SwissTable: compare a whole group of those bytes in one instruction
  (`_mm_cmpeq_epi8` + `_mm_movemask_epi8`, or `vceq_u8` on NEON), which
  re-denominates probe cost in group loads and makes 87.5% occupancy cheaper
  than 50% used to be.

  </details>

- [ ] You can name the three C++ requirements that made `unordered_map` unfixable, not just say "the standard mandates chaining".

  <details><summary>Answer</summary>

  (1) **Reference and pointer stability**: cppreference states that references
  and pointers are invalidated only by erasing that element, even when the
  iterator is invalidated — so a rehash may not move elements, which rules out
  storing them inline in a reallocated array. (2) **The bucket interface**:
  `bucket_count()`, `bucket(key)`, `bucket_size(n)` and `begin(n)` returning a
  `local_iterator` that iterates "a single bucket but not across buckets" —
  chains are public API. (3) **Node handles** since C++17: `extract()` returns
  a `node_type` that owns a per-element node and can be re-inserted elsewhere
  without copying, which presupposes that per-element nodes exist.

  None of the three says "use chaining". Together they leave essentially
  nothing else, which is the more interesting version of the claim.

  </details>

- [ ] You can say what the metadata byte fixed, and what it did *not* fix on its own.

  <details><summary>Answer</summary>

  It fixed the API wart completely: "empty" and "deleted" become states of a
  separate byte rather than reserved key values, so `set_empty_key` /
  `set_deleted_key` (`densehashtable.h:390`, `:496`) disappear and no key
  value is unusable. It also bought a free 7-bit pre-filter, since the byte
  has room for H2 alongside the state bit.

  It did not, by itself, fix the occupancy cap. Reading one byte per slot in a
  loop is still a branch per slot; you have swapped a slot touch for a byte
  touch. Only Step 4's group compare — many tags per instruction — changes the
  unit probe cost is billed in, and only then does raising the load factor
  from 50% to 87.5% become affordable.

  </details>

- [ ] You can show, with the divisions performed, why 87.5% is cheaper for SwissTable than 50% was for `dense_hash_map`.

  <details><summary>Answer</summary>

  Using densehashtable.h:78's own formula for unsuccessful quadratic-probe
  lookups, `1/(1−L) − L − ln(1−L)`: at L = 0.50 it gives
  2.000 − 0.500 + 0.693 = **2.193** (matching the 2.19 printed at `:84`, which
  is how we know we are reading it right), and at L = 0.875 it gives
  8.000 − 0.875 + 2.079 = **9.204**. That is 9.204 / 2.193 = **4.20× more slot
  probes**.

  But those slots are contiguous within a group, and a group is examined in
  one load: 9.204 / 8 = **1.15** group loads at `WIDTH = 8`, or
  9.204 / 16 = **0.58** at `WIDTH = 16`. Roughly one cache line either way,
  against `dense_hash_map`'s ~2.2 independent slot touches. And the array is
  smaller: 1,000,000 entries need 1,000,000/0.875 = 1,142,857 slots instead of
  1,000,000/0.500 = 2,000,000 — **1.75× fewer**. The estimate is rough (the
  formula assumes independent probes, a group is a contiguous window) but the
  direction is not.

  </details>

- [ ] You can state which numbers in this chapter come from a source you can re-open, and which the talk asserts without one.

  <details><summary>Answer</summary>

  Re-openable: the C++ requirements (cppreference); `HT_OCCUPANCY_PCT = 50`,
  the quadratic probe table, `JUMP_`, and both sentinel setters (sparsehash at
  `1dffea3d9`); the H1/57-bit, H2/7-bit split, the one-byte-per-entry
  overhead, the `MaskMatch` code and the "deleted does not cease probing" rule
  (abseil's published design notes); every width, constant and line number
  (hashbrown at `d69025b`); the ~100 ns dependent miss and the 58.4 ms insert
  spike ([FINDINGS.md](../../FINDINGS.md) rows 0 and 2).

  Not re-openable, and therefore absent: the fleet-wide RAM and CPU
  percentages the talk opens with. They may well be right; there is no
  artifact to check them against, so this chapter does not print them. That is
  the same standard [reading-fair-benchmarking.md](../00-performance-toolbox/reading-fair-benchmarking.md)
  applies to published speedups.

  </details>

## References

**Talk**
- Matt Kulukundis — "Designing a Fast, Efficient, Cache-friendly Hash Table,
  Step by Step", CppCon 2017 —
  [video](https://www.youtube.com/watch?v=ncHmEUmJZf4) — ~60 min. Slides were
  not deposited in the [CppCon2017](https://github.com/CppCon/CppCon2017)
  repository, and timestamps differ across re-uploads; navigate by the slide
  content in "How to read the talk" above.

**Primary sources used in place of the talk's audio**
- [abseil, *Swiss Tables Design Notes*](https://abseil.io/about/design/swisstables)
  — the H1/H2 split, the one-byte-per-entry metadata, the five-step lookup,
  `MaskMatch`, and the deleted-does-not-stop-probing rule. Co-authored by
  Kulukundis; the written form of Steps 3-4.
- [cppreference, `std::unordered_map`](https://en.cppreference.com/w/cpp/container/unordered_map)
  — reference/pointer stability, `local_iterator`, `node_type`.

| Source | Lines | What |
|---|---|---|
| sparsehash `1dffea3d9` `src/sparsehash/internal/densehashtable.h` | 77-84 | the published probes-per-lookup table (2.19 at L=0.5, 5.81 at L=0.8) |
| " | 115-119 | `JUMP_` — quadratic probing, one macro |
| " | 390, 496 | `set_deleted_key`, `set_empty_key` — the two donated sentinels |
| " | 648-653 | the probe loop, stopping at the first empty slot |
| " | 1309-1316 | "Knuth says .8 … we go with .5", `HT_OCCUPANCY_PCT = 50` |
| hashbrown `d69025b` `src/control/tag.rs` | 9-49 | the metadata byte, as shipped |
| " `src/control/group/mod.rs` | 8-46 | which group width your target gets |
| " `src/control/group/sse2.rs` | 20, 73-86 | 16-wide, `_mm_movemask_epi8` |
| " `src/control/group/neon.rs` | 16, 68-73 | 8-wide, `vceq_u8` — this repo's machine |
| " `src/raw.rs` | 58-64 | `h1` — the whole hash, not abseil's 57 bits |
| " `src/raw.rs` | 182-191 | `bucket_mask_to_capacity` — the 7/8 rule |
| " `src/raw.rs` | 2009-2046 | `find_inner` — the design walk's destination |
| " `src/raw.rs` | 2756-2757, 3279 | tombstone policy: rehash-in-place, and when not to write one |

**Measured in this repo**
- [FINDINGS.md](../../FINDINGS.md) row 0 — the ~1 / 5 / 100 ns cache ladder
  behind Step 1's "dependent miss".
- [FINDINGS.md](../../FINDINGS.md) row 2 — hashbrown insert p50 42 ns, max
  58.4 ms: the price of Step 5's stop-the-world growth.

**Companion chapters**
- [reading-hashbrown.md](reading-hashbrown.md) — the final design as code.
- [reading-redis-dict.md](reading-redis-dict.md) — the incumbent family, and
  the incremental-rehash answer SwissTable does not give.
