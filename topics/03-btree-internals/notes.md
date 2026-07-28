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

## Predictions (fill BEFORE running)

| Bench | my btree | redb |
|---|---|---|
| point lookup 1M, warm (ns/op) | | |
| range scan 1K rows (µs) | | |
| long-key (32B, shared prefix) height / lookup | | — |
| after suffix truncation: height / fanout / lookup | | — |

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
