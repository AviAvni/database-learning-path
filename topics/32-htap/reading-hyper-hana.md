# HyPer & HANA: one copy serves both

Before "ship a columnar replica" (TiDB) there was "make one copy serve
both". This chapter reads the two classic tricks: HyPer, which lets the
OS page table be its MVCC, and HANA, which keeps every table columnar
twice and folds delta into main in the background. Both are still
load-bearing today — and one of them is `replica.rs`.

## HyPer: let the OS be your MVCC

```
   OLTP process (writes)                 fork()
   ┌────────────────────┐                  │
   │ heap pages         │   ──────────────►│  OLAP child process
   │  [A][B][C][D]      │   child shares   │  ┌────────────────────┐
   └────────────────────┘   ALL pages,     │  │  [A][B][C][D]      │
        │ write to B        copy-on-write  │  │   (frozen view)    │
        ▼                                  │  └────────────────────┘
   ┌────────────────────┐                  │       scans see the
   │  [A][B'][C][D]     │  only B copied   │       snapshot at fork
   └────────────────────┘  (page fault →   │       time, forever
                            OS duplicates) │
```

`fork()` gives a *transaction-consistent snapshot* of the whole database
in ~microseconds: the child shares every page; the MMU copies a page only
when the parent writes it. Snapshot cost = pages *actually dirtied*, not
database size. It's MVCC (topic 5) where the version chain is the page
table and GC is `exit()`.

```rust
// HyPer's entire snapshot machinery — the OS does the versioning
fn olap_query(db: &Database, q: Query) -> Answer {
    match unsafe { fork() } {
        0 => {                          // child: shares EVERY page, copy-on-write —
            let a = execute(db, q);     // a transaction-consistent snapshot in ~µs;
            send_to_parent(&a);         // long scans see the fork-time state forever
            process::exit(0);           // snapshot GC = process exit
        }
        _pid => continue_oltp(),        // parent: writes fault + copy only
    }                                   // the pages they actually dirty
}
```

The costs: snapshot ages until you re-fork (freshness = fork interval —
lane 3's apply interval in OS clothing); hot write pages get copied every
epoch; and it only works single-node, in-memory, with cooperative process
layout.

## HANA: delta+main inside one engine

HANA keeps every table columnar, twice: a read-optimized **main**
(dictionary-compressed, sorted) and a write-optimized **delta**
(append-friendly dictionary, unsorted). Reads merge both; a background
**delta merge** rebuilds main with the delta folded in — O(table) per
merge, done in shadow copies so readers/writers barely notice.

You know this diagram — it is `replica.rs`, and it is TiFlash's DeltaTree
(`reading-tiflash-deltatree.md`) minus the segmenting: HANA merges whole
table (columns) at once, DeltaTree merges per-Segment key ranges, your
`merge_delta()` merges everything. Same fold, different granularity.

The difference from TiDB: **no second copy, no freshness gap** — every
query sees delta+main, perfectly fresh. The payment: isolation. Scans and
writes share the node, the cache, the merge CPU. Lane 1's interference is
mitigated (delta absorbs writes, main serves scans) but not eliminated —
the trilemma corner HANA gives up is exactly the one lane 1 measures.

## Questions

1. HyPer's snapshot cost is proportional to dirtied pages. Which lane-1
   workload property (skewed_key's u² skew) makes fork() snapshots cheap,
   and which workload makes them pathological?
2. fork() gives snapshot isolation for free — but *which* anomaly class
   does the OLAP child never see, and why can't it ever be made fresher
   without re-forking? Compare to `read_wait`: what's HyPer's equivalent
   of demanding lsn == now?
3. HANA's delta merge is O(table). DeltaTree segments to make merges
   O(segment). What query pattern punishes whole-table merges most, and
   why does your lane-2 `merge_cost` measurement understate the problem
   at scale?
4. Both designs keep writes append-friendly and reads merge-y. State the
   invariant both merges must preserve, in the vocabulary of your
   `merge_preserves_scans_and_sorts_main` test.
5. Neither HyPer nor HANA helps when OLAP needs more *compute* than one
   node has. Where does each hit the wall, and which architecture from
   the README menu is the escape hatch?
6. **M32 mapping**: FalkorDB is single-node and in-memory — HyPer's
   natural habitat. Would fork()-snapshots beat a delta-matrix replica
   for M32's analytical reads? Name the FalkorDB-specific write pattern
   that decides it (hint: matrix flush dirties how many pages?).

## References

**Papers**
- Kemper & Neumann — "HyPer: A Hybrid OLTP&OLAP Main Memory Database
  System Based on Virtual Memory Snapshots" (ICDE 2011) — §2-3 for the
  fork() mechanism and its costs
- Färber et al. — "The SAP HANA Database — An Architecture Overview"
  (IEEE Data Eng. Bull. / SIGMOD Record 2012) — the delta+main and
  delta-merge sections

**Code**
- Paper-only chapter — the delta+main mechanics live in
  [reading-tiflash-deltatree.md](reading-tiflash-deltatree.md)'s code
  walk and in our `replica.rs`
