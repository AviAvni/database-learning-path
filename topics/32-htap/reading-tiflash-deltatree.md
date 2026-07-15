# DeltaTree: columnar storage built for writes

Columnar formats hate point-writes (topic 12: rewrite the column or eat
fragmentation), yet TiFlash must apply an OLTP write stream
*continuously* to columnar data. DeltaTree — the engine under
`dbms/src/Storages/DeltaMerge/` in the TiFlash tree — is the answer, and
you already know its shape. Before the code, this chapter builds the
machine step by step — why columns resist writes, the delta+main split,
segmenting, the merge read, the index that keeps it cheap, and the two
sizes of compaction — then hands you the anchors in reading order.

## The problem in one sentence

Apply a continuous stream of point writes to columnar data: inserting
one row into a sorted, compressed column file means rewriting the file —
turning a 100-byte logical write into a rewrite of megabytes — so
DeltaTree must make writes appends and defer the rewriting to background
work, without breaking scans.

## The concepts, step by step

### Step 1 — columnar hates point writes

A columnar layout stores each column contiguously, sorted and
compressed, so scans stream at memory bandwidth (topic 12) — and exactly
that contiguity makes a point write ruinous: inserting one row into the
middle of a sorted column file means shifting or rewriting everything
after it, in *every* column of the table. One 100-byte row into a 64 MB
column file = a multi-megabyte rewrite, per write. A Raft learner
(previous chapter) receives thousands of such writes per second; applied
naively, the replica would spend all its IO rewriting files and none
serving scans.

### Step 2 — delta+main: append now, fold later

The fix is the same fold you've now met three times: split the data into
a big, sorted, scan-friendly **stable** layer (one version per key,
column files) and a small, append-friendly **delta** layer that absorbs
all incoming writes; reads merge the two (delta shadowing stable), and a
background job periodically folds the delta into a rebuilt stable.

This is the fourth time you've met this diagram: topic 4's LSM
(memtable/SSTables/compaction), HANA's delta+main
(`reading-hyper-hana.md`), FalkorDB's delta matrices (pending blocks over
stable matrices), and now `replica.rs` — your `delta: Vec<LogRec>` is the
MemTableSet, `main_*` columns are the stable layer, `merge_delta()` is
`segmentMergeDelta`.

One TiFlash-specific choice worth pausing on: even the *delta* stores
column files, not rows — an analytical scan must be able to read
recent-but-unmerged data column-wise too, or every fresh scan would
degrade to row reads (question 1).

### Step 3 — Segments: partition by key range so folds stay small

HANA's version of Step 2 folds the *whole table* per merge — O(table)
every time, however small the delta. DeltaTree instead partitions the
key space into **Segments**, each owning one key range with its *own*
delta and its own stable:

```
   Raft log records
        │ apply
        ▼
   ┌─ Segment (a key range) ── Segment.h:84 ──────────────┐
   │                                                       │
   │  delta layer                 stable layer             │
   │  ┌──────────────────┐       ┌──────────────────────┐ │
   │  │ MemTableSet      │       │ sorted column files  │ │
   │  │  (in-mem column  │ read: │  one version per key │ │
   │  │   files, recent) │ merge │  scan-friendly       │ │
   │  │ persisted CFs    │ ────► │                      │ │
   │  │  DeltaValueSpace │       │                      │ │
   │  │  .h:65           │       └──────────────────────┘ │
   │  └──────────────────┘                ▲                │
   │        │  MinorCompaction.h          │                │
   │        └── segmentMergeDelta ────────┘                │
   │            DeltaMergeStore.h:668                      │
   └───────────────────────────────────────────────────────┘
```

Now a hot key range triggers merges only for *its* Segment — the fold is
O(segment), and skewed write workloads (the common case) stop taxing the
cold 99% of the table. The store (`DeltaMergeStore.h:107`) is a map of
key-range → Segment plus the background merge machinery; Segments split
and merge as they grow and shrink.

### Step 4 — the merge read: delta shadows stable, per key

A scan must see one truth despite two layers, so the read path is a
two-way sorted merge: walk stable and delta in key order; where both
have the key, the delta's (newer) version wins:

```rust
// One Segment: a delta over a stable, both covering one key range.
fn scan(seg: &Segment, out: &mut ColumnBatch) {
    let mut stable = seg.stable.iter().peekable();   // sorted, one version per key
    let mut delta = seg.delta.iter_sorted().peekable(); // sorted via the DeltaIndex —
    loop {                                           // without it, every scan
        match (stable.peek(), delta.peek()) {        // re-sorts the delta
            (Some(s), Some(d)) if d.key <= s.key => {
                if d.key == s.key { stable.next(); } // delta version shadows stable
                out.push(delta.next().unwrap());
            }
            (Some(_), _) => out.push(stable.next().unwrap()),
            (None, Some(_)) => out.push(delta.next().unwrap()),
            (None, None) => return,
        }
    }
}
```

Writes land in the delta via `placeUpsert` (`Segment.h:715`). The catch
is that one line — `iter_sorted()`: the delta is append-ordered, not
key-ordered, so without help every scan would re-sort the whole delta
first. That's Step 5.

### Step 5 — the DeltaIndex: pay the sort once, not per scan

The **DeltaIndex** (`DeltaIndex/DeltaIndex.h:27`) is a persistent
structure mapping each delta row to its position in stable's sort order
— built once when the delta changes, then reused by every scan, so the
merge read of Step 4 becomes a cheap zipper instead of a per-scan sort.
It's the same budget decision an LSM makes with merge iterators and
bloom filters, answered differently: index the small side once
(question 2). This is precisely the piece your `replica.rs::scan_sum_a`
deliberately lacks — your scans re-sort the delta every time, which is
honest and slow.

### Step 6 — compaction at two sizes, and the correctness contract

The delta itself is tiered — fresh writes sit in the in-memory
`MemTableSet`, which spills to persisted column files
(`DeltaValueSpace.h:65`): a little LSM inside the delta of the big
two-level LSM. Two background jobs manage it:

- **MinorCompaction** (`Delta/MinorCompaction.h`) — fold small persisted
  column files together *within* the delta, so long-lived deltas don't
  fragment into hundreds of tiny files before the big fold (question 4).
- **segmentMergeDelta** (`DeltaMergeStore.h:668`) — the big fold: rebuild
  the Segment's stable with the delta applied, empty the delta.

Both must be invisible: scans return identical results before and after
a fold, and stable keeps one version per key in sorted order. Your
`merge_delta()` contract (scans identical before/after, delta emptied)
is exactly `segmentMergeDelta`'s correctness condition — pinned by an
oracle in your tests, assertable only as Segment invariants in TiFlash
(question 3). One wrinkle deferred to question 5: TiFlash also keeps
MVCC versions (topic 5) in both layers, so "one version per key" really
means "one per key per surviving snapshot," and GC needs a horizon.

## Where each step lives in the code

Anchors, in reading order:

1. `DeltaMergeStore.h:107` — the store: a map of key-range → Segment,
   plus the background merge machinery (Step 3).
2. `Segment.h:84` — one Segment = one delta + one stable, both covering
   the same key range (Step 3). `:715 placeUpsert` — where an incoming
   write lands in the delta (Step 4).
3. `Delta/MemTableSet.h`, `Delta/DeltaValueSpace.h:65` — the delta layer
   is itself tiered: in-memory column files, then persisted ones. A little
   LSM inside the delta of the big two-level LSM (Steps 2, 6).
4. `Delta/MinorCompaction.h` — compaction *within* the delta (fold small
   column files together) before the big fold into stable (Step 6).
5. `DeltaIndex/DeltaIndex.h:27` — the trick your `scan_sum_a` lacks: a
   persistent index mapping delta rows into stable's sort order, so merge
   reads don't re-sort the delta every scan (Step 5).
6. `DeltaMergeStore.h:668 segmentMergeDelta` — the fold. Your
   `merge_delta()` contract (scans identical before/after, delta emptied)
   is exactly its correctness condition (Step 6).

## Questions

1. Why does the delta store *column files* rather than rows, when it's
   the write-optimized side? What read would rows in the delta ruin?
2. The DeltaIndex makes delta+stable reads cheap without merging. What
   does it have to be rebuilt/patched on, and what's the topic 4 analogue
   (hint: what does an LSM do instead — bloom filters? merge iterators?)?
3. `merge_delta` must not change scan results. Your test pins this with
   an oracle; how would you check it in TiFlash where there's no oracle?
   (Look at what invariants Segment can assert.)
4. MinorCompaction inside the delta: why compact the delta at all if
   segmentMergeDelta will fold everything anyway? What workload makes
   delta-internal compaction pay?
5. MVCC: TiFlash keeps versions (topic 5) in both layers. What does
   "one entry per key in stable" become when snapshots must still read
   old versions — and what bounds GC (compare: causal stability in
   topic 31's tombstone question)?
6. **M32 mapping**: FalkorDB's delta matrix flush is `segmentMergeDelta`
   for adjacency. What is the delta *index* analogue — what structure
   would let algebraic scans consume stable+pending without materializing
   the merge?

## Done when

You can draw one Segment from memory — delta (memory + persisted tiers)
over stable, DeltaIndex bridging them — and trace one write and one scan
through it, naming which background job would touch each part next.

## References

**Papers**
- None dedicated — the design is described in the storage section of
  Huang et al., "TiDB: A Raft-based HTAP Database" (VLDB 2020); the rest
  lives in code comments

**Code**
- [tiflash](https://github.com/pingcap/tiflash)
  `dbms/src/Storages/DeltaMerge/` — start at `DeltaMergeStore.h` and
  `Segment.h`; the delta layer (`Delta/`) and `DeltaIndex/` are the
  parts your `replica.rs` deliberately lacks
