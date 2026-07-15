# The SwissTable design walk: how benchmarks kill hash tables

How Google replaced `std::unordered_map` fleet-wide — told as a sequence of
designs, each rejected by a measurement. This chapter is a watching guide for
Kulukundis's CppCon talk: watch it *after* reading
[`reading-hashbrown.md`](reading-hashbrown.md), because the talk is the design
narrative for the code you just read. Here the narrative is rebuilt step by
step — each design, the benchmark that killed it, and the idea that replaced
it — so you can watch for the beats instead of chasing them. Budget ~60 min
video + 30 min notes.

## The problem in one sentence

`std::unordered_map` costs 2+ dependent cache misses and a malloc'd node per
entry, across a fleet where hash tables hold ~1% of *all* RAM and serve ~4%
of all CPU cycles — and the C++ standard's own API rules forbid fixing it in
place.

## The concepts, step by step

The design walk, in one picture — each arrow is a benchmark verdict:

```
std::unordered_map          chaining, per-node malloc, iterator stability
        │  "every lookup = 2+ dependent misses"
        ▼
dense_hash_map              open addressing, quadratic probe, but 2 sentinel
        │                   keys stolen from the user + 50% max load
        ▼
"store metadata per slot"   1 byte: empty/deleted/full + 7 hash bits
        │  "but scanning bytes one at a time is slow"
        ▼
SwissTable                  group the bytes, compare 16 at once with SSE2
                            → 87.5% load factor, ~1 miss per lookup
```

### Step 1 — the incumbent: chaining mandated by an API contract

`std::unordered_map` uses chaining (buckets of malloc'd linked-list nodes —
the redis dict chapter's family) not because its authors loved it, but
because the C++ standard's API promises force it: **pointer stability**
(references to elements must survive any rehash — impossible if entries live
inline and move) and a **bucket interface** (`bucket_count()`,
`begin(bucket)`, exposing chains as an API). The measured cost: every lookup
is 2+ dependent cache misses (bucket array, then node, then possibly next
node) plus a malloc per insert. Lesson zero of the talk: API guarantees are
performance decisions.

### Step 2 — first replacement: dense_hash_map and its warts

Google's earlier answer, `dense_hash_map`, switched to open addressing
(entries inline in one flat array, collisions resolved by probing — see the
hashbrown chapter, Step 1) with quadratic probing. Lookups dropped to ~1
miss. The measured warts: the user must *donate two sentinel key values*
(one meaning "empty slot", one "deleted slot" — so those keys become
unusable, an API landmine), and it needs a **50% maximum load factor** —
half the table is empty slack, 2× the memory of the entries themselves.
Fast, but RAM-hungry and awkward. The fleet pays for RAM too.

### Step 3 — the metadata byte: state out of band, 7 hash bits for free

The fix for both warts: stop encoding empty/deleted *in key space* and store
**one metadata byte per slot** in a separate dense array — 1 bit of state
(empty/deleted/full) plus **7 bits of the hash** (h2). No sentinel keys
stolen from the user; and the 7 hash bits act as a per-slot pre-filter, so a
probe compares 1 byte instead of touching the slot's key: a false positive
only 1/128 of the time. This is hashbrown's control byte (`tag.rs:9–49`),
here at the moment of invention.

### Step 4 — group probing: scan 16 metadata bytes in one instruction

The next benchmark verdict: scanning metadata bytes one at a time is still a
loop with a branch per byte. Because the metadata is a dense byte array,
SIMD (16-byte-at-once CPU instructions) can compare a whole **group** of 16
tags against h2 in one SSE2 compare + one `_mm_movemask_epi8` (turn the
16-lane comparison into a 16-bit integer bitmask — then iterate its set
bits). One instruction filters 16 slots; probing moves group by group. This
is hashbrown's `Group::match_tag` — NEON, 8-wide, on your machine.

### Step 5 — what the combination buys: 87.5% load and tombstone rules

With group probing, a probe step examines 16 slots nearly free, so the table
stays fast even when almost full: **load factor rises from 50% to 7/8 =
87.5%** — a fleet-wide RAM cut on its own, on top of removing per-node
mallocs. Deletion uses the DELETED metadata state (a **tombstone**: probes
must skip it, since stopping there would hide keys probed past it; inserts
may reuse it) — the talk's discussion is where hashbrown's
rehash-in-place-when-full-of-tombstones policy (raw.rs:152, 1033) comes
from. The end state: ~1 cache miss per lookup, 1 metadata byte per slot of
overhead, no sentinels — and it still couldn't ship as `unordered_map`,
because Step 1's API contract survives any benchmark.

### Step 6 — the method is the takeaway

Every arrow in the design walk is a *measurement*, not an opinion:
hypothesize → benchmark → let the number kill or keep the design — the
topic-0 method applied to data-structure design at fleet scale. Watch the
talk as a methodology demonstration wearing a hash table as a costume.

## How to watch the talk (with the concepts in hand)

Timestamps are approximate across uploads — navigate by slide titles:

- **"The C++ standard basically mandates chaining"** — Step 1: why
  `unordered_map` can't be fixed in place (pointer stability + bucket API
  promises).
- **The metadata byte slide** — Step 3: the h2/control-byte idea introduced.
- **The SSE2 `_mm_movemask_epi8` slide** — Step 4: the group probe; this is
  hashbrown's `Group::match_tag`, NEON on your machine.
- **Load factor + tombstone discussion** — Step 5: where the 7/8 and
  rehash-in-place decisions come from (hashbrown raw.rs:152, 1033).

Connect each talk moment to the code you already read:

| Talk moment | You saw it in |
|---|---|
| metadata byte = 1 bit state + 7 bits hash | `tag.rs:9–49` |
| group probe, movemask | `group/neon.rs:78–90` (ARM twist: 8-wide) |
| "deleted vs empty" probe-stop rule | `raw.rs` tombstone logic :1033–1043 |
| iterators break on rehash — API cost | Rust never promised stability, so hashbrown got this for free |

## Questions to answer in notes.md

1. Google couldn't ship this as `std::unordered_map` because the standard's API
   promises (pointer stability, bucket interface) mandate chaining. Which redis
   `dict` features would SwissTable similarly break? (Incremental rehash needs
   stable *entries*? Check — redis moves entries between tables anyway; the real
   conflict is `dictScan`'s bucket cursor.)
2. The talk reports big fleet-wide RAM savings from the load-factor jump
   (50% → 87.5%) plus removing per-node mallocs. Estimate the bytes-per-entry
   difference for a u64→u64 map: chaining with malloc'd nodes vs SwissTable at
   7/8 load. Show the arithmetic in notes.
3. Kulukundis says hash quality matters *more* for open addressing than
   chaining — why? (Clustering compounds; a bad h2 also raises false positives.)

## Done when

You can retell the rejected-design sequence (chaining → dense_hash_map →
metadata bytes → SIMD groups) and give the one-line benchmark reason each step
was taken.

## References

**Papers**
- Kulukundis — "Designing a Fast, Efficient, Cache-friendly Hash Table,
  Step by Step" (CppCon 2017 talk) —
  [video](https://www.youtube.com/watch?v=ncHmEUmJZf4) — ~60 min;
  timestamps vary across uploads, navigate by the slide titles listed
  above

**Code**
- [hashbrown](https://github.com/rust-lang/hashbrown) — the Rust
  incarnation of the final design; walked in
  [reading-hashbrown.md](reading-hashbrown.md)
