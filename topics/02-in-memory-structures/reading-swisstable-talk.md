# Watching guide — Matt Kulukundis, "Designing a Fast, Efficient, Cache-friendly Hash Table, Step by Step" (CppCon 2017)

The SwissTable talk — how Google replaced `std::unordered_map` fleet-wide.
~60 min video + 30 min notes. Watch *after* reading hashbrown
([`reading-hashbrown.md`](reading-hashbrown.md)): the talk is the design
narrative for the code you just read.

## Why watch a talk about a table you already read

The hashbrown source shows the *final* design. The talk shows the **sequence of
rejected designs** and the benchmark that killed each one — it's a masterclass
in the topic-0 method (hypothesize → measure → iterate).

## The design walk (watch for these beats)

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

Timestamps are approximate across uploads — navigate by slide titles instead:
- **"The C++ standard basically mandates chaining"** — why `unordered_map`
  can't be fixed in place (pointer stability + bucket API promises).
- **The metadata byte slide** — the h2/control-byte idea introduced.
- **The SSE2 `_mm_movemask_epi8` slide** — the group probe; this is
  hashbrown's `Group::match_tag`, NEON on your machine.
- **Load factor + tombstone discussion** — where the 7/8 and rehash-in-place
  decisions come from (hashbrown raw.rs:152, 1033).

## Connect to what you've read

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
