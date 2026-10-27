# Topic 3 — notes

## Baseline (provided lane, Apple M3 Pro, measured 2026-07-28)

`cargo run --release --bin btree_baseline`. Two provided things: the page
arithmetic from the format documented in `src/page.rs`, and redb measured on
the workloads this topic's table asks about. Everything is **warm** — the file
sits in the page cache — so this is in-page search plus pointer chasing, not
disk I/O.

### Fanout arithmetic (computed, not measured)

| key shape | leaf cells | fanout | height @ 1e6 | height @ 1e9 |
|---|---|---|---|---|
| 8 B key, 8 B value | 185 | 255 | 3 | 4 |
| 32 B key, 8 B value | 88 | 102 | 4 | 5 |
| 8 B key, 100 B value | 35 | 255 | 3 | 5 |

A 32 B key costs **2.5×** the interior slots of an 8 B key. That ratio — not
the byte count — is what suffix truncation buys back.

### The height ladder (redb, warm)

| keys | ns/lookup | file MB | height (our fmt) |
|---|---|---|---|
| 10 000 | 367 | 1.6 | 2 |
| 100 000 | 423 | 9.0 | 3 |
| 1 000 000 | 862 | 67.9 | 3 |
| 4 000 000 | 1101 | 270.0 | 3 |

**This is the topic's tidy story failing, and it is the most useful number
here.** "Height is the metric" predicts a step function: flat while height is
constant, jumping when it grows. Instead cost climbs 862 → 1101 ns from 1e6 to
4e6 keys with height pinned at 3. Height sets how many pages a lookup *touches*;
what a touch *costs* is set by whether that page is in CPU cache, and at 270 MB
it is not. Two levers, and the second one is why topic 6 exists.

### The long-key case (1e6 keys)

| keys | ns/lookup | file MB |
|---|---|---|
| 8 B | 733 | 67.9 |
| 32 B, 24 B shared prefix | 882 | 135.3 |
| ratio | 1.20× | 1.99× |

The file doubles and lookups slow 20%. The arithmetic above predicted a 2.5×
fanout loss; redb absorbs most of it, which is itself the finding — a
production B-tree already does some of what you are about to implement by hand.

### Lane 1b — bbtree vs std, in memory (provided, Apple M3 Pro, measured 2026-10-27)

`cargo run --release --bin bbtree_vs_std`: `bbtree` 0.1.5 (`unsync::BBTreeMap`,
system allocator) against `std::collections::BTreeMap`, seeded keys, probes in a
different random order than the inserts, `get` best of 3, `get_many` 16 keys per
call. Two consecutive runs (the machine was loaded, load average ≈ 10; the
checksums are identical, only the timings move):

| keys | n | insert ns std → bb | get ns std → bb | get_many16 ns (bb) |
|---|---|---|---|---|
| u64 | 100K | 95.1 → 74.3 / 128.0 → 84.7 | 81.5 → 34.1 / 76.9 → 30.2 | 32.4 / 27.9 |
| u64 | 1M | 154.0 → 121.9 / 121.2 → 87.2 | 203.8 → 71.8 / 138.5 → 58.8 | 41.0 / 40.9 |
| u64 | 10M | 294.2 → 215.1 / 294.2 → 190.6 | 352.5 → 151.1 / 568.1 → 175.6 | 73.0 / 65.8 |
| String | 1M | 457.5 → 245.3 / 413.7 → 226.3 | 700.1 → 270.0 / 676.3 → 271.8 | — |

(run 1 / run 2 in each cell.) Single `get` is 2.3–3.2× faster than std at 10M;
`get_many16` is 2.1–2.7× faster than bbtree's own `get` at 10M and gives almost nothing
at 100K, where the tree is cache-resident. The crate README's "5× faster than
std at single thread" does not reproduce for single gets; the author's own
one-thread tables on a Ryzen 9 9900X show 1.7–4.1× for gets and 2.4–6.6×
batched ([`reading-bbtree.md`](reading-bbtree.md), Step 9). The `String` keys
here (`user:` + 16 hex digits) almost never tie in the 8-byte inline key, the
favourable case; the exercise is to construct the tied one.

## Predictions (fill BEFORE running)

| Bench | my btree | redb |
|---|---|---|
| point lookup 1M, warm (ns/op) | | |
| range scan 1K rows (µs) | | |
| long-key (32B, shared prefix) height / lookup | | — |
| after suffix truncation: height / fanout / lookup | | — |

| Lane 1b (`bbtree_vs_std`) | my prediction | measured |
|---|---|---|
| u64 10M single `get`, bbtree vs std (ratio) | | |
| u64 10M `get_many16` vs bbtree `get` (ratio) | | |
| u64 100K `get_many16` vs bbtree `get` (ratio) | | |
| `String` 1M `get` ratio when keys share a 17-byte prefix | | (exercise) |

Fanout arithmetic check (before measuring): 4KB page, 8B key + 2B ptr + 4B lens
⇒ leaf holds ~N cells; interior fanout ~M ⇒ predicted height at 1e6 keys = ___.

## Reading answers

### turso deep (reading-turso-btree-deep.md)
1. Table vs index interior cell contents / fanout:
2. Why defrag is needed despite freeblocks:
3. Yield-point invariant in async balance:

### SQLite btree.c (reading-sqlite-btree.md)
1. fillInCell overflow-first ordering safety:
2. balance_quick savings for fillseq:
3. Trust-vs-verify position:

### LMDB (reading-lmdb.md)
1. Why no sibling redistribution on split:
2. Which fsync could go, on what hardware:
3. 1-key commit cost LMDB vs WAL engine; where LMDB still wins:

### bbtree (reading-bbtree.md)
1. Depth and bytes/entry at 100M `u64` entries from the cost model:
2. Why a count over every slot and a binary search can both be right (1M vs 10M):
3. Tied-key `String` lane — predicted vs measured get ratio; which step does the work:
4. Why a no-op waker is correct for the batch executor:
5. One property per verification tool that it does not establish:

### Graefe survey
1. Suffix (interior) vs prefix (leaf) truncation asymmetry:
2. Is SQLite right to skip both?
3. The one-sentence dense-filter principle:

### File format doc
- Annotated hex dump (paste here):

## Experiment findings

- Warm-cache caveat: at 1M keys everything fits in the OS page cache — this
  benches CPU + page format, not IO. (Buffer pool + cold runs = topic 6.)
- redb comparison, explained in fanout/height terms:
- Truncation result — fanout before/after, height change, lookup delta:

## M3 log

- [ ] Page format designed before peeking; diffs vs reference cow_btree noted:
- [ ] Disk vs Arc-COW writeup (free-space mgmt, splits, checksums vs refcounts):
- [ ] Range-index smoke bench in workload generator:
