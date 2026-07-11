# Topic 3 — notes

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
