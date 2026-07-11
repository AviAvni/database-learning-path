# DeltaTree: columnar storage built for writes

Columnar formats hate point-writes (topic 12: rewrite the column or eat
fragmentation), yet TiFlash must apply an OLTP write stream
*continuously* to columnar data. DeltaTree — the engine under
`dbms/src/Storages/DeltaMerge/` in the TiFlash tree — is the answer, and
you already know its shape:

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

This is the fourth time you've met this diagram: topic 4's LSM
(memtable/SSTables/compaction), HANA's delta+main
(`reading-hyper-hana.md`), FalkorDB's delta matrices (pending blocks over
stable matrices), and now `replica.rs` — your `delta: Vec<LogRec>` is the
MemTableSet, `main_*` columns are the stable layer, `merge_delta()` is
`segmentMergeDelta`.

The read path is a two-way merge, delta shadowing stable per key:

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

## Anchors, in reading order

1. `DeltaMergeStore.h:107` — the store: a map of key-range → Segment,
   plus the background merge machinery.
2. `Segment.h:84` — one Segment = one delta + one stable, both covering
   the same key range. `:715 placeUpsert` — where an incoming write lands
   in the delta.
3. `Delta/MemTableSet.h`, `Delta/DeltaValueSpace.h:65` — the delta layer
   is itself tiered: in-memory column files, then persisted ones. A little
   LSM inside the delta of the big two-level LSM.
4. `Delta/MinorCompaction.h` — compaction *within* the delta (fold small
   column files together) before the big fold into stable.
5. `DeltaIndex/DeltaIndex.h:27` — the trick your `scan_sum_a` lacks: a
   persistent index mapping delta rows into stable's sort order, so merge
   reads don't re-sort the delta every scan.
6. `DeltaMergeStore.h:668 segmentMergeDelta` — the fold. Your
   `merge_delta()` contract (scans identical before/after, delta emptied)
   is exactly its correctness condition.

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
