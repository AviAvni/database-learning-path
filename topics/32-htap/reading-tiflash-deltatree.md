# DeltaTree: columnar storage built for writes

Columnar formats hate point-writes (topic 12: rewrite the column or eat
fragmentation), yet TiFlash must apply an OLTP write stream
*continuously* to columnar data. DeltaTree — the engine under
`dbms/src/Storages/DeltaMerge/` in the TiFlash tree — is the answer, and
you already know its shape. Before the code, this chapter builds the
machine step by step — why columns resist writes, the delta+main split,
segmenting, the merge read, the index that keeps it cheap, and the two
sizes of compaction — then hands you the anchors in reading order.

Every code anchor below is verified against the TiFlash clone this repo
pins — `pingcap/tiflash@b5093dd` (2026-07-09), the pin table at the end of
`../../resources/codebases.md` — with the line numbers those files occupy
at that commit. Where a number is a *code constant* (a tunable read out of
the pinned tree) versus a *design figure* (a shape argued in comments or
the TiDB paper), this chapter says which.

## The problem in one sentence

Apply a continuous stream of point writes to columnar data: inserting
one row into a sorted, compressed column file means rewriting the file —
turning a 100-byte logical write into a rewrite of megabytes — so
DeltaTree must make writes appends and defer the rewriting to background
work, without breaking scans.

## The concepts, step by step

### Step 1 — columnar hates point writes

> **In:** the Raft-learner write stream from the previous chapter — thousands
> of committed row-writes per second landing on a columnar replica. **Out:**
> the reason applying them in place is ruinous, which forces the delta+main
> split of Step 2.

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

> **In:** the point-write stream that Step 1 showed cannot be applied in
> place. **Out:** the two-layer structure that absorbs it — an append-only
> **delta** in front of a sorted, scan-friendly **stable** — plus the debt it
> creates: every read must now merge the two (Step 4) and a background job
> must fold delta into stable (Step 6).

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

> **In:** the delta+main pair from Step 2, which — if kept table-wide — folds
> the whole table on every merge. **Out:** the partition that bounds the fold:
> one **Segment** per key range, each with its own delta and stable, so a hot
> range's merge is O(segment) not O(table).

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
and merge as they grow and shrink. The size a Segment targets is a **code
constant**, not a paper figure: `dt_segment_limit_rows = 1000000`
(`dbms/src/Interpreters/Settings.h:156`, "Base rows of segments in
DeltaTree Engine").

### Step 4 — the merge read: delta shadows stable, per key

> **In:** a Segment holding a sorted stable and an append-ordered delta
> (Step 2/3). **Out:** the single sorted stream a scan sees — a two-way merge
> where a delta row shadows the stable row with the same key — which is cheap
> only if the delta is pre-sorted, motivating the DeltaIndex of Step 5.

A scan must see one truth despite two layers, so the read path is a
two-way sorted merge: walk stable and delta in key order; where both
have the key, the delta's (newer) version wins:

```rust
// ILLUSTRATION — not quoted from TiFlash. The real read builds a delta
// stream ordered by the DeltaIndex (Segment.h ensurePlace:705 / placeUpsert:715),
// then merges it against the stable snapshot; your M32 analogue is
// replica.rs:41 (scan_sum_a). One Segment: a delta over a stable, both
// covering one key range.
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

Incoming writes land in the delta via `writeToCache` (`Segment.h:217`),
which appends a block into the Segment's in-memory `MemTableSet` (to be
flushed to disk later); `writeToDisk` (`:214`) attaches an already-persisted
column file. `placeUpsert` (`:715`) runs in the *opposite* direction — it is
part of the read/place path (`ensurePlace`, `:705`) that references the
delta's inserts into stable's sort order, i.e. it builds the DeltaIndex of
Step 5, not the write. The catch is that one line — `iter_sorted()`: the
delta is append-ordered, not key-ordered, so without help every scan would
re-sort the whole delta first. That's Step 5.

### Step 5 — the DeltaIndex: pay the sort once, not per scan

> **In:** the append-ordered delta whose lack of key order made the Step 4
> merge threaten a per-scan re-sort. **Out:** the cached index that removes
> that cost — a delta-row → stable-sort-position map, built once per delta
> change and reused by every scan.

The **DeltaIndex** (`DeltaIndex/DeltaIndex.h:27`) is a long-lived,
incrementally-maintained *in-memory* index — not an on-disk structure —
mapping each delta row to its position in stable's sort order. It holds a
`delta_tree` with `placed_rows` / `placed_deletes` counters (`:33`, `:35`,
`:36`) tracking how far the delta has been placed, and it is keyed in an
`LRUCache` (`:30`) and cheaply cloned as the delta grows (the `Update`
struct, `:41`). Built once when the delta changes, then reused by every
scan, so the merge read of Step 4 becomes a cheap zipper instead of a
per-scan sort. It's the same budget decision an LSM makes with merge
iterators and bloom filters, answered differently: index the small side
once (question 2). This is precisely the piece your `replica.rs::scan_sum_a`
(`replica.rs:41`) deliberately lacks — your scans re-sort the delta every
time, which is honest and slow.

### Step 6 — compaction at two sizes, and the correctness contract

> **In:** a delta that keeps growing as writes arrive and as Step 5's index
> tracks them. **Out:** the two background jobs that bound it — small folds
> *within* the delta (MinorCompaction) and the big fold delta→stable
> (segmentMergeDelta) — plus the invariant both must preserve: scans return
> identical results before and after.

The delta itself is tiered — fresh writes sit in the in-memory
`MemTableSet`, which spills to persisted column files
(`DeltaValueSpace.h:65`): a little LSM inside the delta of the big
two-level LSM. Two background jobs manage it:

- **MinorCompaction** (`Delta/MinorCompaction.h`) — fold small persisted
  column files together *within* the delta, so long-lived deltas don't
  fragment into hundreds of tiny files before the big fold (question 4).
- **segmentMergeDelta** (`DeltaMergeStore.h:668`) — the big fold: rebuild
  the Segment's stable with the delta applied, empty the delta.

When each fires is set by **code constants**, not paper figures — the delta
row-count thresholds in `dbms/src/Interpreters/Settings.h`:

- `dt_segment_delta_limit_rows = 80000` (`:158`) — a background delta-merge
  is triggered once a Segment's delta passes this many rows (the "delta
  layer" size threshold; the byte companion `dt_segment_delta_limit_size`
  follows at `:159`).
- `dt_segment_force_merge_delta_rows = 134217728` (`:162`, 128 M) — the
  delta size that *forces* a merge into stable.
- `dt_segment_stop_write_delta_rows = 268435456` (`:164`, 256 M) — the delta
  size at which new writes are stalled until the fold catches up.
- `dt_segment_delta_cache_limit_rows = 4096` (`:166`) — how many rows the
  in-memory `MemTableSet` cache holds before spilling to a persisted column
  file.

Both jobs must be invisible: scans return identical results before and
after a fold, and stable keeps one version per key in sorted order. Your
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
   the same key range (Step 3). `:217 writeToCache` — where an incoming
   write lands in the delta's in-memory MemTableSet (Step 4); `:715
   placeUpsert` is the opposite direction — placing the delta into stable's
   order for the merge read (Step 5).
3. `Delta/MemTableSet.h`, `Delta/DeltaValueSpace.h:65` — the delta layer
   is itself tiered: in-memory column files, then persisted ones. A little
   LSM inside the delta of the big two-level LSM (Steps 2, 6).
4. `Delta/MinorCompaction.h` — compaction *within* the delta (fold small
   column files together) before the big fold into stable (Step 6).
5. `DeltaIndex/DeltaIndex.h:27` — the trick your `scan_sum_a` lacks: a
   cached, incrementally-maintained in-memory index (keyed in an LRUCache,
   `:30`) mapping delta rows into stable's sort order, so merge reads don't
   re-sort the delta every scan (Step 5).
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

Answer each before unfolding it.

- [ ] You can trace one write from the Raft learner to where it physically lands.
  <details><summary>Answer</summary>
  It appends into the Segment's in-memory `MemTableSet` via `writeToCache`
  (`Segment.h:217`) — not into stable, and *not* via `placeUpsert` (`:715`,
  which is the read/place path). The delta cache holds up to
  `dt_segment_delta_cache_limit_rows = 4096` rows (`Settings.h:166`) before
  spilling to a persisted column file (`DeltaValueSpace.h:65`). Nothing
  rewrites a sorted column file on the write path — that was the whole point
  of Step 1.
  </details>

- [ ] You can trace one scan and say why it does not re-sort the delta.
  <details><summary>Answer</summary>
  A scan is the two-way merge of Step 4: walk stable and delta in key order,
  delta shadows stable on equal keys. It stays cheap because the DeltaIndex
  (`DeltaIndex.h:27`) already maps each delta row to its stable-sort position
  (`placed_rows`, `:35`), built once per delta change and reused — a cached
  in-memory index, not a per-scan sort. Your `replica.rs::scan_sum_a`
  (`replica.rs:41`) omits it deliberately and re-sorts every time.
  </details>

- [ ] You can name the two folds and the sizes that trigger them.
  <details><summary>Answer</summary>
  MinorCompaction (`Delta/MinorCompaction.h`) folds small persisted column
  files *within* the delta; segmentMergeDelta (`DeltaMergeStore.h:668`)
  rebuilds stable with the delta applied and empties the delta. The triggers
  are code constants in `Settings.h`: background merge past
  `dt_segment_delta_limit_rows = 80000` (`:158`), forced merge at
  `dt_segment_force_merge_delta_rows = 134217728` (`:162`), writes stalled at
  `dt_segment_stop_write_delta_rows = 268435456` (`:164`).
  </details>

- [ ] You can state the invariant both folds must preserve, and why a Segment matters.
  <details><summary>Answer</summary>
  A fold must be invisible: scans return identical results before and after,
  and stable holds one version per key in sorted order (modulo MVCC snapshots,
  question 5). Segments (`Segment.h:84`, targeting
  `dt_segment_limit_rows = 1000000`, `Settings.h:156`) make that fold
  O(segment) not O(table), so a hot key range's merges never tax the cold
  99% — the improvement over HANA's whole-table merge (Step 3).
  </details>

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
