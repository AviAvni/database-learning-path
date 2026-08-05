# HyPer & HANA: one copy serves both

Before "ship a columnar replica" (TiDB) there was "make one copy serve
both". This chapter reads the two classic tricks: HyPer, which lets the
OS page table be its MVCC, and HANA, which keeps every table columnar
twice and folds delta into main in the background. Both are still
load-bearing today — and one of them is `replica.rs`. Before the papers,
this chapter builds each trick step by step, from the interference
problem both are answering.

The two papers this chapter reads are Kemper & Neumann, *HyPer: A Hybrid
OLTP&OLAP Main Memory Database System Based on Virtual Memory Snapshots*
(ICDE 2011), and Färber et al., *The SAP HANA Database — An Architecture
Overview* (IEEE Data Eng. Bull. 2012); section and line references below
point into those editions. Note that HyPer's *MVCC* (the row-version design
of Neumann, Mühe & Kemper, SIGMOD 2015) is a **later** system — the ICDE
2011 paper this chapter cites uses `fork()` and hardware copy-on-write, not
version chains, and this chapter is careful to keep them apart.

## The problem in one sentence

Run analytical full scans and transactional point writes against the
*same copy* of the data without one starving the other — bench lane 1
measured the naive version: adding a free-running scanner collapsed
write throughput from **11,438,647 writes per 2 s to 69**, with p99
write latency going from 333 ns to 7.49 s (this topic's README provided
lane, echoed in `notes.md`).

## The concepts, step by step

### Step 1 — the fight: scans and writes contend for one copy

> **In:** one copy of the data and two workloads — million-row analytical
> scans and single-row transactional writes. **Out:** the contention that
> forces every design in this chapter — the shared resource (lock, cache,
> bus, MVCC GC) that Steps 2 and 4 each try to *fake* apart without a second
> copy.

An analytical scan reads millions of rows while a transactional write
touches one — put them on the same copy and they collide on *something*:
a lock (lane 1's coarse one), cache lines, the buffer pool, MVCC
(multi-version concurrency control, topic 5 — keeping old row versions so
readers don't block writers) garbage collection. Lane 1's numbers above
are the extreme, but the shape survives every mitigation short of
separation. The two designs in this chapter are the two classic ways to
*fake* separation while physically keeping one copy: separate the
**views** (HyPer, Step 2) or separate the **formats** (HANA, Step 4).

### Step 2 — HyPer: fork() makes the OS page table your snapshot

> **In:** the shared-copy fight from Step 1, on a single-node in-memory
> database. **Out:** HyPer's separation of *views* — an OLAP child process
> whose page table, frozen by copy-on-write at `fork()` time, is a
> transaction-consistent snapshot the writers cannot disturb.

`fork()` is the Unix call that clones a process — and the OS implements
it lazily with **copy-on-write** (CoW): the child shares every physical
memory page with the parent, and a page is physically copied only when
one of them writes to it. HyPer's insight: for an in-memory database,
that lazy clone *is* a transaction-consistent snapshot, and the paper
reports it is created "in subseconds" (ICDE 2011, §III; "creation of a
fresh, transaction-consistent snapshot can be achieved in subseconds").
The number that makes it cheap is the per-page one: duplicating a single
dirtied page costs "only 2 µs … instead of 10 ms to handle a page fault"
on disk (§III-B) — so the *marginal* cost is proportional to the pages
actually dirtied, not to the database size:

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

```rust
// ILLUSTRATION — HyPer's mechanism (ICDE 2011 §III-B), not code in this
// repo: there is no fork() analogue here. This repo's one-copy design is
// the HANA-style delta+main path instead — apply (replica.rs:36), scan
// (replica.rs:41), merge_delta (replica.rs:46). The OS does the versioning.
fn olap_query(db: &Database, q: Query) -> Answer {
    match unsafe { fork() } {
        0 => {                          // child: shares EVERY page, copy-on-write —
            let a = execute(db, q);     // a transaction-consistent snapshot (subseconds);
            send_to_parent(&a);         // long scans see the fork-time state forever
            process::exit(0);           // snapshot GC = process exit
        }
        _pid => continue_oltp(),        // parent: writes fault + copy only
    }                                   // the pages they actually dirty (2 µs/page)
}
```

It's snapshot isolation where the version store is the page table and GC is
`exit()` — the OLAP child scans a frozen view at full speed, the OLTP
parent keeps writing, and the isolation is total: separate processes,
separate page mappings. (This is *not* HyPer's later MVCC design — Neumann,
Mühe & Kemper, SIGMOD 2015 — which adds in-place version chains; the ICDE
2011 system here versions purely through the OS page table.)

### Step 3 — what the fork trick costs

> **In:** the fork-time snapshot from Step 2. **Out:** its three bills —
> staleness between forks, copy cost that scales with *dirtied* pages, and a
> single-node/in-memory reach limit — which together explain why the later
> chapters move to a separate copy.

Three bills come due. **Freshness**: the snapshot ages until you re-fork
— freshness = fork interval, which is lane 3's apply interval in OS
clothing; the child can never be made fresher, only replaced.
**Write amplification on hot pages**: snapshot cost is proportional to
pages *actually dirtied*, not database size — a skewed write workload
dirtying 1,000 pages per epoch pays only ≈ 4 MB of copying per fork
(1,000 × 4 KB pages, at the paper's 2 µs/page), but a uniform scatter over
a 100 GB heap re-copies a huge fraction of it every epoch (lane 1's
`skewed_key` u² skew is exactly the friendly case; question 1). **Reach**:
it only works single-node, in-memory, with a cooperative process layout —
no distributed version, and every OLAP query family costs a process.

### Step 4 — HANA: one engine, two formats — delta and main

> **In:** the same one-copy fight (Step 1), on a columnar engine. **Out:**
> HANA's separation of *formats* — a read-optimized **main** and a
> write-optimized **delta**, both columnar, merged on every read — which
> damps the fight but leaves the merge cost of Step 5.

HANA's alternative separation is by *format inside one engine*: every
table is columnar twice. The **main** is read-optimized: every column is
compressed "with the help of a sorted dictionary" — each value mapped to
an integer `valueID`, and those valueIDs "further bit-packed and
compressed" (Färber 2012, §column store) — with rows re-sorted for the
best scheme (RLE, sparse, cluster coding), so scans stream compressed
columns at memory bandwidth. The **delta** is write-optimized: it uses
dictionary compression too, "but the dictionary is stored in a Cache
Sensitive B+-Tree (CSB+-Tree)" (Färber 2012, §delta) — a tree, not a
sorted array, precisely so a point write is an insert into the tree, not
a re-sort of a packed column. Every read merges both: scan main, patch
with delta, newest version wins. Writes never touch main; scans mostly
stream main plus a small delta — the interference of Step 1 is damped
because each side mostly works its own structure.

### Step 5 — the delta merge: the background fold that keeps delta small

> **In:** the write-absorbing delta from Step 4, which stays cheap only
> while small. **Out:** the background **delta merge** that folds it into a
> rebuilt main — and its bill: a CPU- and memory-intensive whole-table
> rebuild during which old and new state coexist.

The delta only stays cheap while it's small, so a background **delta
merge** periodically rebuilds main with the delta folded in: rewrite the
table's columns (re-encode dictionaries, re-sort), swap in the result,
empty the delta. Färber calls it "a CPU and memory intensive operation"
(§challenges). It is done without stalling the table: when the merge
starts, writes are redirected to a *new* delta, and "until it is finished,
read operations access new and old delta storage as well as the old main
storage" (§delta) — so the new main is built beside the old one and
atomically switched in. Because old and new main coexist for the duration,
the memory cost is roughly a second copy of the table's main (a reasoned
inference from that coexistence, not a figure the paper quotes), on top of
the CPU burn of the rebuild.

You know this diagram — it is `replica.rs`, and it is TiFlash's DeltaTree
(`reading-tiflash-deltatree.md`) minus the segmenting: HANA merges whole
table (columns) at once, DeltaTree merges per-Segment key ranges, your
`merge_delta()` merges everything. Same fold, different granularity. And
it's topic 4's LSM with exactly two levels: delta = memtable, main = the
one SSTable, delta merge = compaction.

### Step 6 — the trilemma placement: what each design gives up

> **In:** both designs — HyPer's separated views (Steps 2–3) and HANA's
> separated formats (Steps 4–5). **Out:** where each sits on the topic's
> freshness / isolation / cost triangle, and the shared resource each still
> leaves contended.

Put both on the topic README's freshness / isolation / cost triangle.
HANA: **no second copy, no freshness gap** — every query sees delta+main,
perfectly fresh, and the extra cost is just the delta and the merge CPU.
The payment is isolation: scans, writes, and merges share the node, the
cache, the memory bus — lane 1's interference is mitigated (delta absorbs
writes, main serves scans) but not eliminated. HyPer: strong isolation
(separate processes) at near-zero copy cost, paying with snapshot
staleness between forks — and both hit a hard wall when OLAP needs more
*compute* than one node has (question 5); the escape hatches are the
separated architectures of the other two chapters (TiFlash learner
replicas, Lightning's CDC-fed system).

## How to read the papers (with the concepts in hand)

- **HyPer (ICDE 2011)**: read §2–3 for the fork() mechanism (Step 2) and
  its costs (Step 3) — check the paper's own numbers for snapshot
  creation time and page-copy overhead against Step 3's reasoning. Skim
  the rest; the query-compilation material belongs to topic 19.
- **HANA overview (2012)**: read the delta+main and delta-merge sections
  (Steps 4–5) — extract exactly what is rebuilt during a merge and what
  the shadow-copy switch does to concurrent readers. Skim the
  distributed and application-server sections.
- As you read both, keep one question live: *which resource is still
  shared?* That resource is where lane 1's fight resumes.

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

## Done when

Answer each before unfolding it.

- [ ] You can place both designs on the trilemma and name the resource each still shares.
  <details><summary>Answer</summary>
  HyPer buys strong isolation (separate processes, separate page mappings)
  at near-zero copy cost, paying in *freshness* — the child snapshot ages
  until the next `fork()` (Step 3). HANA keeps perfect freshness and no
  second copy, paying in *isolation* — scans, writes, and the delta merge
  share one node's CPU, cache, and memory bus (Step 6). HyPer's still-shared
  resource is time (staleness between forks); HANA's is the node itself,
  where lane 1's fight resumes during a merge.
  </details>

- [ ] You can state HyPer's snapshot mechanism and its true cost, per the ICDE 2011 paper.
  <details><summary>Answer</summary>
  `fork()` + hardware copy-on-write: the OLAP child shares every page and a
  page is duplicated only when written. Creation is "in subseconds" (§III),
  and a dirtied page costs "only 2 µs … instead of 10 ms" for a disk page
  fault (§III-B). The marginal cost scales with pages *dirtied*, not database
  size — so "microseconds regardless of size" is wrong (it was in an earlier
  draft of this guide). This is *not* MVCC; HyPer's version-chain MVCC is the
  later SIGMOD 2015 system.
  </details>

- [ ] You can describe HANA's two formats with the right dictionary structures.
  <details><summary>Answer</summary>
  Main: columns compressed with a **sorted dictionary**, valueIDs bit-packed,
  rows re-sorted for best encoding (Färber 2012, §column store). Delta: also
  dictionary-compressed, but its dictionary lives in a **Cache-Sensitive
  B+-Tree (CSB+-Tree)** so a point write is a tree insert, not a re-sort of a
  packed column (§delta) — *not* an "unsorted dictionary," which an earlier
  draft claimed.
  </details>

- [ ] You can state what the delta merge rebuilds and what coexists during it.
  <details><summary>Answer</summary>
  It rebuilds main with the delta folded in (re-encode dictionaries, re-sort),
  then atomically swaps. During the merge, writes go to a new delta and reads
  "access new and old delta storage as well as the old main storage" (Färber
  §delta); old and new main coexist, which is why the merge is "CPU and memory
  intensive" (§challenges) and why memory roughly doubles for the table's main
  for the duration.
  </details>

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
