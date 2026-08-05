# In-memory MVCC: timestamps as locks, and the design-space price list

What does MVCC look like when the disk-era assumptions are deleted?
Hekaton (SIGMOD '13) answers with one design — no locks, no latches, no
pages; Wu & Pavlo's VLDB '17 evaluation answers with the whole design
SPACE, benchmark-backed prices attached. This chapter builds Hekaton's
machine one move at a time, then walks the four design decisions Wu &
Pavlo isolate and prices each one. Read Hekaton first (a design), then
Wu/Pavlo (the menu).

Two papers, cited throughout by their own section, table and figure
numbers so you can check every claim:

- **Hekaton** — Diaconu, Freedman, Ismert, Larson, Mittal, Stonecipher,
  Verma, Zwilling, *Hekaton: SQL Server's Memory-Optimized OLTP Engine*,
  SIGMOD 2013. Cited as "Hekaton §N".
- **Wu/Pavlo** — Wu, Arulraj, Lin, Xian, Pavlo, *An Empirical Evaluation
  of In-Memory Multi-Version Concurrency Control*, VLDB 2017
  ([PDF](https://db.cs.cmu.edu/papers/2017/p781-wu.pdf)). Cited as
  "Wu §N", with figure and table numbers.

Every number below carries the section, table or figure it came from. If a
claim here has no such tag, it is not from the papers and you should
distrust it.

## The problem in one sentence

Hekaton's authors did the arithmetic before they wrote any code (§2): a
10–100× throughput goal cannot be reached by tuning, because "improving
scalability and CPI can produce only a 3–4× improvement", so "to go 10×
faster, the engine must execute 90% fewer instructions… to go 100× faster,
it must execute 99% fewer instructions" — and the only way to delete 90%
of the instructions in an OLTP path is to delete the machinery itself, so
the design rule became **no latches or spinlocks on any performance-critical
path, no lock manager, no lock table** (§2.1.2).

## The concepts, step by step

### Step 1 — MVCC recap, minus the disk

> **In:** the disk-era MVCC design you met in the previous guide — postgres
> heap tuples, xids, clog, vacuum.
> **Out:** a list of which of those choices are *forced by the disk* and
> therefore up for renegotiation once the database lives in DRAM.

Definitions used from here on:

- A **transaction** is a group of reads and writes that must appear to
  happen all-at-once or not at all.
- **MVCC** (multi-version concurrency control) means writers never
  overwrite: each update creates a **new version** of the record, so
  readers and writers never block each other.
- A **version** is one immutable snapshot of a record's contents plus the
  metadata saying when it was the truth; the set of versions of one record,
  linked, is the **version chain**.
- **Visibility** is the predicate "should transaction T see version V?".
- **Garbage collection** (GC) is reclaiming versions no live transaction can
  still see. Postgres's variant is called **vacuum**.

Postgres implements MVCC *on disk*: versions are heap tuples on 8 KB
pages, the visibility metadata is transaction ids (xids) plus a commit log
(clog) to look those xids up, and cleanup is a background vacuum process.
Every one of those three is a consequence of the page: you need a clog
because a tuple header is too precious to hold a commit timestamp, you need
vacuum because you cannot free half a page.

Delete the disk and all three are renegotiable. Versions can be
heap-allocated structs linked by pointers; "who committed when" can be a
single 64-bit comparison instead of a log probe; and any thread can free
garbage the moment it is provably unreachable. Hekaton is what you get when
you renegotiate all of them at once.

Why it matters: the rest of this guide is a list of *what postgres does
because of the disk*, and what each of those choices becomes without it.

### Step 2 — the version record: two timestamps bound a lifetime

> **In:** the need to answer "was this version the truth when I started?"
> without a commit-log lookup.
> **Out:** the version header — `Begin`, `End`, links, payload — and a
> visibility test that is two integer comparisons.

Hekaton stamps each version with the interval during which it was the
truth. A monotonically increasing counter hands out **timestamps** (§6.1).
Each version carries two: `Begin` is "the commit time of the transaction
that created the version", `End` is "the commit timestamp of the
transaction that deleted the version (and perhaps replaced it with a new
version)" (§6.1). §4 gives the layout:

```
 ┌──────────┬─────────┬──────────────┬─────────┐
 │  Begin   │   End   │ index links  │ payload │
 └──────────┴─────────┴──────────────┴─────────┘
   header                              Name/City/Amount

 live version:  End = ∞
 visibility:    Begin < RT  and  End > RT          (§6.1)
```

where RT is the transaction's **logical read time**, which for every
isolation level Hekaton supports is set to the transaction's start time
(§6.1).

Work it on the paper's own example. Hekaton Figure 2 is a bank-account
table; here are its John rows, with the amounts and timestamps as printed:

| Begin | End | Name | City   | Amount |
|-------|-----|------|--------|--------|
| 10    | 20  | John | London | 100    |
| 20    | 100 | John | London | 110    |
| 100   | ∞   | John | London | 130    |

Three reads, three answers, using `Begin < RT and End > RT`:

- RT = 15 → row 1: `10 < 15` and `20 > 15` → **visible**, Amount 100.
  Row 2: `20 < 15` is false → invisible. Row 3: `100 < 15` false →
  invisible.
- RT = 50 → row 1: `20 > 50` false → invisible. Row 2: `20 < 50` and
  `100 > 50` → **visible**, Amount 110.
- RT = 105 → rows 1 and 2 fail the `End > RT` half; row 3: `100 < 105`
  and `∞ > 105` → **visible**, Amount 130.

Exactly one version is visible at each read time, because "different
versions of a record always have non-overlapping valid times so at most one
version of a record is visible to a read" (§4.1).

Why it matters: the whole check is two integer comparisons on data already
in the cache line you fetched. Compare postgres, where the same question
costs a clog lookup (usually short-circuited by hint bits) plus a search of
the snapshot's in-progress xid array — see
[`heapam_visibility.c:939`](reading-postgres-heapam.md). That is the payoff
of timestamps: **the version is self-describing with two 64-bit fields**.

### Step 3 — txn-ids double as locks: one CAS does two jobs

> **In:** a system with no lock manager and no lock table (§2.1.2), which
> nonetheless has to stop two writers updating one record.
> **Out:** the trick that fits a write lock inside the `End` field, and the
> reader-side cost it creates.

There is no lock manager, so where does write-write conflict detection
live? Inside the timestamp fields themselves. Hekaton §4.2, describing
Figure 2's in-flight transfer:

> Note that transaction 75 has stored its transaction Id in the Begin and
> End fields of the new and old versions, respectively. (One bit in the
> field indicates the field's content type.) A transaction Id stored in the
> End field prevents other transactions from updating the same version and
> it also identifies which transaction is updating the version. A
> transaction Id stored in the Begin field informs readers that the version
> may not yet be committed and identifies which transaction created the
> version.

So one bit distinguishes "this is a timestamp" from "this is a transaction
id" (bit-smuggling again), and installing your txn-id in a live version's
`End` field with an atomic compare-and-swap — **CAS**, "replace this value
only if it still equals what I read" — is simultaneously the lock
acquisition and the conflict test:

- CAS succeeds → this transaction owns the update; the txn-id sitting in
  `End` *is* the write lock, and the writer links its new version in.
- A txn-id is already there → another writer holds the record; this one
  aborts. First-writer-wins, detected with zero shared tables.

Follow Figure 2's transfer of $20 from Larry to John all the way through.
Before commit, four versions are in play (§4.2, Figure 2 as printed):

```
 old John:  Begin=20     End=Tx75    Amount=110
 new John:  Begin=Tx75   End=inf     Amount=130
 old Larry: Begin=30     End=Tx75    Amount=170
 new Larry: Begin=Tx75   End=inf     Amount=150
```

Check the money: John 110 + 20 = 130 ✓, Larry 170 − 20 = 150 ✓, total
before 280, total after 280 ✓ — the transfer conserves the sum, which is
the invariant the transaction exists to protect. Then: "suppose transaction
75 commits with end timestamp 100… transaction 75 returns to the old and
new versions and sets the Begin and End fields, respectively, to 100"
(§4.2). Every `Tx75` above becomes `100`, and old John's valid time becomes
20–100 while new John's becomes 100–∞ — which is exactly the table you
worked in Step 2.

Why it matters: one atomic instruction replaces the entire lock-manager
conversation — acquire lock and publish the version pointer, fused. The
bill arrives on the read side: a reader that meets a txn-id where it
expected a timestamp must go ask the transaction map what that writer is
doing. That is Step 4.

### Step 4 — commit is a pipeline, and readers speculate through it

> **In:** a reader that has just found a txn-id, not a timestamp, in a
> version header.
> **Out:** commit dependencies — the mechanism that lets that reader
> proceed without blocking, and the cascading-abort risk it accepts.

Commit is a *pipeline*, not an instant (§6.2). In order:

1. **Get an end timestamp.** "The validation phase begins with the
   transaction obtaining an end timestamp. This end timestamp determines
   the position of the transaction within the transaction serialization
   history." (§6.2.1)
2. **Validate.** A serializable transaction must show two things (§6):
   **read stability** — "if T reads some version V1 during its processing,
   we must ensure that V1 is still the version visible to T as of the end
   of the transaction" — and **phantom avoidance** — "the transaction's
   scans would not return additional new versions", checked by repeating
   the scans. To make this possible each transaction keeps a *read set*
   (pointers to versions read) and a *scan set* (§6.2.1). Note the price
   list: "repeatable read requires only read validation and snapshot
   isolation and read committed require no validation at all" (§6.2.1).
3. **Log.** "A transaction T is committed as soon as its changes to the
   database have been hardened to the transaction log." (§6.2.2)
4. **Post-process.** Walk the *write set* replacing your txn-id with the
   real end timestamp in every `Begin`/`End` you touched (§6.2.2) — the
   `Tx75 → 100` rewrite from Step 3.

Between steps 1 and 4 your txn-id is visible to everyone. Hekaton's answer
is not to block:

> Any transaction T1 that begins while a transaction T2 is in the validation
> phase becomes dependent on T2 if it attempts to read a version created by
> T2 or ignores a version deleted by T2. In that case T1 has two choices:
> block until T2 either commits or aborts, or proceed and take a commit
> dependency on T2. To preserve the non-blocking nature of Hekaton, we have
> T1 take a commit dependency on T2. This means that T1 is allowed to commit
> only if T2 commits. If T2 aborts, T1 must also abort so cascading aborts
> are possible. (§6.2.1)

Two consequences the paper spells out (§6.2.1): T1 increments a dependency
count and cannot commit until it reaches zero; and because T1 is now
holding uncommitted data, a **read barrier** holds T1's result set back
from the client until the dependencies clear. The non-blocking property is
paid for in latency-at-the-edge, not latency-in-the-engine.

Sketching the reader side makes the two-field decode concrete:

```rust
// ILLUSTRATION — not quoted from any repo; it is Hekaton §6.1's visibility
// rule plus §6.2.1's commit dependency, written in Rust so the two-way
// decode of the Begin/End fields is explicit. Real implementations of the
// same predicate: postgres heapam_visibility.c:939, and this topic's own
// exercise at experiments/src/mvcc.rs:89.
fn visible(v: &Version, read_ts: u64, txns: &TxnTable) -> bool {
    let begin = match v.begin_field {
        Stamp(ts) => ts,
        TxnId(id) => match txns.state(id) {
            Committing { end_ts } => end_ts, // take a commit DEPENDENCY:
            _ => return false,               // I abort if the writer does
        },
    };
    let end = match v.end_field {
        Stamp(ts) => ts,       // superseded at ts
        TxnId(_) => u64::MAX,  // being updated — still the latest for readers
    };
    begin < read_ts && read_ts < end
}
```

Why it matters: no reader ever waits on a writer's commit. The cost moved
from blocking (latency) to cascading aborts (wasted work) — the right trade
when conflicts are rare, and the wrong one when they are not, which is the
finding Step 8 will price.

### Step 5 — indexes point at versions, not at chains

> **In:** a database with no pages, where the only way to reach a record is
> an index probe.
> **Out:** the cost model of Hekaton's choice — every new version is
> inserted into every index — and the correction it forces to a common
> intuition.

With no pages, "the table" is just the set of version records, and
"records are always accessed via an index lookup" (§4). Hekaton has two
index types: "hash indexes which are implemented using lock-free hash
tables and range indexes which are implemented using Bw-trees, a novel
lock-free version of B-trees" (§4) — the Bw-tree is topic 9's cautionary
protagonist.

The layout detail that matters: "Each index requires a link field in the
record… Versions that hash to the same bucket are linked together using the
first link field" (§4). So a hash bucket is a chain of *version records*,
not of *logical rows* — Figure 2's "Hash bucket J contains four records:
three versions for John and one version for Jane" (§4). A lookup scans the
bucket and applies Step 2's test; §4.1's worked case: "A lookup for John
with read time 15… would trigger a scan of bucket J that checks every
record in the bucket but returns only the one with Name equal to John and
valid time 10 to 20."

And crucially, on update: transaction 75 "has created the new versions for
Larry and for John and inserted them into the appropriate buckets in the
index" (§4.2). **New versions go into the index.** Wu §6.2 classifies this
as the *physical pointer* scheme and names Hekaton as a user of it; Wu
Table 1's Hekaton row reads *Physical* under Index Management. Step 11
prices that choice.

> **Correction to a claim this guide used to make.** An earlier version of
> this guide said Hekaton's "index entries point at *chains*, not individual
> versions, so a new version doesn't churn the index." That is the *logical
> pointer* scheme (Wu §6.1), which Hekaton does not use — §4.2 shows new
> versions being inserted into the index buckets, and Wu Table 1 files
> Hekaton under Physical. The churn is real, and Step 11 measures what it
> costs.

Why it matters: this is the axis where "no pages" does *not* buy a free
lunch — deleting the buffer pool made lookups cheap and made updates
touch more index structures, not fewer.

### Step 6 — cooperative GC: the workload cleans itself, mostly

> **In:** an MVCC engine with no vacuum daemon and a bounded memory budget.
> **Out:** the two-part GC design, and the specific hole in it that a
> background process still has to fill.

The correctness rule first (§8.1.1): "the visibility of a version is
determined by its begin and end timestamps. Any version whose end timestamp
is less than the current oldest active transaction in the system is not
visible to any transaction and can be safely discarded." A GC thread
"periodically scans the global transaction map to determine the begin
timestamp of the oldest active transaction" — the **watermark**.

Removal is in two parts (§8.1.2):

1. **Cooperative.** "Since regular index scanners may encounter garbage
   versions as they scan indexes, index operations are empowered to unlink
   garbage versions when they encounter them. If this unlinks a version
   from its last index, the scanner may also reclaim it." The paper gives
   two reasons: it "naturally parallelizes garbage collection", and it
   "ensures that old versions will not slow down future scanners by forcing
   them to skip over old versions encountered, for example, in hash index
   bucket chains."
2. **Background.** Cooperative cleaning is explicitly "insufficient to
   ensure that either (1) 'cold' areas of an index which are not traversed
   by scanners are free of garbage, or that (2) a garbage version is
   removed from other indexes that it might participate in. Versions in
   these 'dusty corners' (infrequently visited index regions) do not need
   to be collected for performance reasons, but they needlessly consume
   memory."

So the answer to "what about garbage nobody walks past?" is in the paper:
it is not a performance problem (nobody is walking past it, so it slows
nobody down) but it is a memory problem, and a background sweep exists
precisely for it.

Contrast postgres on every axis now visible: timestamps vs xid + clog +
hint bits (Step 2); CAS-as-lock vs a lock manager (Step 3); commit-time
validation vs SIREAD locks (Step 4, and see
[`reading-ssi-postgres.md`](reading-ssi-postgres.md)); indexes carrying
every version vs `t_ctid` chains (Step 5); cooperative cleaning vs vacuum
(Step 6). Note the one axis where they *agree*: Wu Table 1 files both
postgres and Hekaton under append-only storage with **O2N** (oldest-to-newest)
version ordering. Step 12 shows that shared choice putting them in the same
place at the bottom of a benchmark.

Why it matters: this is the last of Hekaton's design decisions, and you
now have a complete point in the design space. Wu/Pavlo's contribution is
the space itself.

### Step 7 — what a Wu/Pavlo number is measured on

> **In:** a paper full of percentages you are about to quote.
> **Out:** the machine, system and protocol behind them, so you know what
> each percentage is a percentage *of*.

Every Wu/Pavlo figure quoted below comes from this setup (§7):

| Knob | Setting |
|---|---|
| Machine | 4-socket Intel Xeon E7-4820, ten 1.9 GHz cores per socket (**40 cores**), 25 MB L3 per socket, 128 GB DRAM, Ubuntu 14.04 |
| System | Peloton, one codebase implementing every combination — so protocol differences are not vendor differences |
| Isolation | SERIALIZABLE throughout |
| Workloads | YCSB (Zipfian skew parameter θ) and TPC-C |
| Method | 60 s warm-up, then measure; results averaged over five trials |

Two things follow. First, "θ" is the contention dial: θ=0.2 is near-uniform
access, θ=0.8–0.9 is a hot handful of keys, and *every verdict below is
conditional on θ*. Second, because one system implements all the variants,
a percentage here compares two designs, not two engineering teams — which
is the reason this paper is worth reading at all.

One instructive control, before any protocol appears. Fig 6a runs a
*read-only* YCSB workload at θ=0.2: "all but one of the protocols scales
almost linearly up to 24 threads. The main bottleneck for all of these
protocols is the cache coherence traffic from updating the memory manager's
counters and checking for conflicts when transactions commit (even though
there are no writes)" (§7.2). Read-only, no conflicts, and the ceiling at
24 of 40 cores is *still* coherence traffic. Fig 6b's counterpart: raising
transactions from 10 to 100 operations "reduced by ∼30×" the throughput but
made all protocols "scale linearly up to 40 threads", because longer
transactions mean less pressure on the shared structures.

Why it matters: hold that finding next to this repo's own measured lane in
[`notes.md`](notes.md) — a single global mutex delivers ~600k txn/s
*flat* across read-heavy, write-heavy and hot-key workloads. Both results
say the same thing from opposite ends: on a multi-core machine the shared
coordination structure, not the isolation algorithm, sets the ceiling.

### Step 8 — design decision 1 of 4: the concurrency control protocol

> **In:** four protocols implemented in one system (Wu §3).
> **Out:** which one to reach for, and the specific contention level at
> which the popular answer collapses.

The four (§3):

| Protocol | Mechanism | Extra per-tuple state |
|---|---|---|
| **MVTO** (timestamp ordering) | order by transaction timestamp; abort a writer whose tuple has already been read by a later transaction | `read-ts` (§3.1) |
| **MVOCC** (optimistic) | run, then validate the read set at commit | — (§3.2) |
| **MV2PL** (two-phase locking) | take read/write locks in the tuple header | `read-cnt`, packed with `txn-id` into one 64-bit word (§3.3) |
| **Certifier** (SI+SSN) | snapshot isolation plus a serial-safety-net check | — (§3.4) |

One implementation detail worth carrying away: MV2PL uses a **no-wait**
deadlock policy — a transaction that cannot get a lock aborts immediately
rather than waiting, so there is no deadlock detector to run (§3.3).

The measured verdicts:

- **Contention is what separates them, and only past a threshold.** Fig 7a
  (read-intensive, 40 threads): "When θ is less than 0.7, we see that all
  of the protocols achieve similar throughput. Beyond this contention level,
  the performance of MVOCC is reduced by ∼50%. This is because MVOCC does
  not discover that a transaction will abort due to a conflict until after
  the transaction has already executed its operations. **There is nothing
  about multi-versioning that helps this situation.**" (§7.2)
- **Nothing helps write-write conflicts.** Fig 7b (update-intensive):
  "there is not a great difference among the protocols except MV2PL; they
  handle write-write conflicts in a similar way and again multi-versioning
  does not help reduce this type of conflicts." (§7.2)
- **On TPC-C, MVTO wins.** Fig 10a, 10 warehouses: MVTO achieves 45–120%
  higher throughput than the others (§7.2).
- **And nobody ships it.** §8: "Overall, we found that MVTO works well on a
  variety of workloads. **None of the systems that we list in Table 1 adopt
  this protocol.**" Table 1 lists nine systems.

Why it matters: this is the axis the literature argues about, and its
verdict is the mildest of the four — below θ=0.7 the protocol barely
matters. Keep that in view while reading Steps 9–11.

### Step 9 — design decision 2 of 4: version storage

> **In:** three ways to lay out a version chain (Wu §4).
> **Out:** the axis Wu/Pavlo call the most important one, and the two
> sub-decisions hiding inside it.

The three schemes (§4), defined:

- **Append-only** — every version is a full copy of the tuple, all in the
  same table space (postgres, Hekaton, MemSQL, NuoDB, HYRISE per Table 1).
- **Time-travel** — full copies again, but the old versions live in a
  separate table (SAP HANA).
- **Delta** — the master version is updated in place and only the *changed
  attributes* are copied into a separate delta store, like an undo record
  (Oracle, MySQL-InnoDB).

Append-only carries a sub-decision: which end of the chain the index enters.
**O2N** (oldest-to-newest) means the index points at the oldest version and
readers walk forward; **N2O** (newest-to-oldest) means the index points at
the newest and readers usually stop immediately.

Measured:

- **N2O always wins.** Fig 12: N2O "always performs better than O2N in both
  workloads", and at the highest contention (θ=0.9) "the N2O ordering
  achieves 2.4–3.4× better performance" (§7.3).
- **Delta wins wide tables with narrow updates.** Fig 13b: "when the table
  has 100 attributes, the delta scheme achieves ∼2× better performance than
  append-only and time-travel schemes because it uses less memory" (§7.3).
  Fig 14 and Fig 15 refine it: delta is best when few attributes are
  *modified*, and degrades fastest as the number of attributes *read* rises.
- **…and loses scans, badly.** Fig 17b (TPC-C, 40 warehouses): "With delta
  storage, the latency of the scan queries grows near-linearly with the
  increase of number of threads (which is bad), while the append-only and
  time-travel schemes maintain a latency that is 25–47% lower when using 40
  threads" (§7.3). Fig 17a: append-only wins TPC-C throughput, because
  TPC-C reads many attributes at once.
- **The allocator is a confounding variable.** Fig 16: the delta scheme is
  "stable regardless of the number of memory spaces", while append-only and
  time-travel throughput is "improved by 1.6–4× when increasing the number
  of separate memory spaces from 1 to 20" (§7.3). Allocator contention
  masquerades as storage-scheme cost — if you benchmark append-only against
  delta with a single shared allocator, you are benchmarking the allocator.
- **Non-inlined attributes should be reference-counted, not copied.**
  Fig 11: with the read-intensive workload the DBMS "achieves ∼40% higher
  throughput when the number of non-inlined attributes is increased to 50",
  and for update-intensive "the performance gap reaches over 100%" (§7.3).

Why it matters: §8's headline finding is that "the version storage scheme
is one of the most important components to scaling an in-memory MVCC DBMS
in a multi-core environment. This goes against the conventional wisdom in
database research that has mostly focused on optimizing the concurrency
control protocols."

### Step 10 — design decision 3 of 4: garbage collection

> **In:** Step 6's Hekaton design plus postgres's vacuum, now as two points
> on a third axis (Wu §5).
> **Out:** the measured cost of each, and the reason the paper prefers the
> option neither system uses.

Two granularities, and within tuple-level, two triggers (§5):

- **Tuple-level VAC** — a background vacuum thread scans for expired
  versions. (Postgres, Oracle, MySQL, NuoDB, MemSQL per Table 1.)
- **Tuple-level COOP** — cooperative cleaning by whatever worker walks the
  chain; Step 6's Hekaton design. §5.1 notes COOP "only works for the O2N
  append-only storage" — which is one reason Hekaton's Table 1 row is O2N.
- **Transaction-level** — reclaim in batches, per transaction/epoch, rather
  than per tuple.

Measured, with append-only O2N, MVTO, 40 worker threads and one GC thread
(§7.4):

- **COOP beats VAC.** Fig 18: "COOP achieves 45% higher throughput compared
  to VAC under read-intensive workloads." Fig 19: "COOP has a 30–60% lower
  memory footprint per transaction than VAC", and its performance is more
  stable because it "amortizes the GC overhead across multiple threads."
- **Transaction-level beats both.** Fig 20a shows a slight edge on
  read-intensive, and "the gap increases to 20% in Fig. 20b for the
  update-intensive workload. Transaction-level GC removes expired versions
  in batches, thereby reducing the synchronization overhead." §8's summary:
  "a transaction-level GC provided the best performance with the smallest
  memory footprint."
- **GC off is not a speed-up, it is a slow decay.** Fig 18 again:
  "performance declines over time when GC is disabled because the DBMS
  traverses longer version chains to retrieve the versions. Furthermore,
  because the system never reclaims memory, it allocates new memory for
  every new version." Both mechanisms "improve throughput by 20–30%
  compared to when GC is disabled" (§7.4).

Why it matters: GC is the axis most likely to be omitted from a benchmark
(it costs nothing in the first 20 seconds) and the one whose absence shows
up as a *downward slope* rather than a lower number — which is why Fig 18's
x-axis is elapsed time, not thread count. Your own benchmarks should copy
that choice.

### Step 11 — design decision 4 of 4: index management

> **In:** Step 5's discovery that Hekaton inserts every new version into
> every index.
> **Out:** the measured price of that, and the alternative.

Two schemes (§6):

- **Logical pointers** — the index maps key → an indirection slot (or
  primary key) that holds the head of the version chain. A new version
  updates the slot; the secondary indexes never move.
- **Physical pointers** — the index entry points straight at a version
  record, so "when updating any tuple in a table, the DBMS inserts the newly
  created version into all the secondary indexes" (§6.2). Hekaton and
  MemSQL do this.

Measured on update-intensive YCSB with MVTO, append-only N2O and
transaction-level GC, varying the number of secondary indexes (§7.5):

- Fig 22b, **high contention (θ=0.8)**: "logical pointer achieves 25%
  higher performance compared to physical pointer scheme."
- Fig 22a, **low contention (θ=0.2)**: "the performance gap is enlarged to
  40% with the number of secondary indexes increased to 20."
- Fig 23, **eight secondary indexes, varying threads**: "for the high
  contention workload, the DBMS's throughput when using logical pointers is
  45% higher than the throughput of physical pointers."
- §8's summary: "logical pointer scheme always achieve a higher throughput
  especially when processing update-intensive workloads."

Why it matters: the cost is proportional to *number of secondary indexes*,
a quantity that grows silently over a schema's life. A design that is
free at one index is paying 40% at twenty.

### Step 12 — the shootout, and where Hekaton actually lands

> **In:** the four axes, priced.
> **Out:** the paper's own head-to-head of nine real systems' configurations,
> and the result that should revise your opinion of Step 1–6's design.

Wu/Pavlo's last experiment (§8, Figs 24–25) configures Peloton as each of
Table 1's nine real systems and runs TPC-C — "a good approximation of their
abilities", with the honest caveat that it does not capture "other factors
in the real DBMSs… (e.g., data structures, storage architecture, query
compilation)."

Table 1's rows for the four systems this topic cares about:

| System | Protocol | Version storage | GC | Index pointers |
|---|---|---|---|---|
| Oracle / MySQL-InnoDB | MV2PL | Delta | Tuple-level VAC | Logical |
| Postgres | MV2PL / SSI | Append-only, **O2N** | Tuple-level VAC | Physical |
| Hekaton | MVOCC | Append-only, **O2N** | Tuple-level COOP | Physical |
| NuoDB | MV2PL | Append-only, **N2O** | Tuple-level VAC | Logical |

The result (§8, Fig 24):

> As shown in Fig. 24, the DBMS performs the best on both the low-contention
> and high-contention workloads with the Oracle/MySQL and NuoDB
> configurations… **Postgres and Hekaton's configurations lead to the worst
> performance**, and the major reason is that the use of append-only storage
> with O2N ordering severely restricts the scalability of the system.

Hekaton and postgres finish together, at the bottom, for the same reason —
the version-ordering choice from Step 9, not anything in Steps 3, 4 or 6.
That is the sharpest thing this pair of papers teaches: a beautifully
argued protocol design (no locks, no latches, CAS-as-lock, commit
dependencies) is outranked on this benchmark by one layout decision it
inherited.

And no corner dominates. Fig 25 (scan latency): "the DBMS's performance is
the worst with delta storage. This is because the delta storage has to
spend more time on traversing version chains so as to find the targeted
tuple version attribute." The throughput winners are the latency losers.

Why it matters: read this as the general lesson for design-space papers —
the axis everyone optimizes (protocol, Step 8) had the mildest verdict, and
the axis nobody writes papers about (storage layout, Step 9) decided the
shootout.

## Where each claim lives in the papers

| Step | Source | What |
|---|---|---|
| 1 | — | background from the previous guide |
| 2 | Hekaton §4, §4.1, §6.1, Figure 2 | version header, valid time, visibility rule |
| 3 | Hekaton §4.2, Figure 2 | txn-id in Begin/End, the one type bit, the $20 transfer |
| 4 | Hekaton §6, §6.2.1, §6.2.2, §6.2.3 | read stability, phantom avoidance, commit dependencies, read barriers, post-processing |
| 5 | Hekaton §4, §4.1, §4.2; Wu §6.2, Table 1 | index types, bucket chains of versions, physical pointers |
| 6 | Hekaton §8.1.1, §8.1.2; Wu §5.1, Table 1 | watermark, cooperative unlinking, dusty corners; COOP requires O2N |
| 7 | Wu §7, Fig 6a, Fig 6b | hardware, method, the read-only coherence ceiling |
| 8 | Wu §3.1–§3.4, Fig 7a, Fig 7b, Fig 10a, §8 | the four protocols, θ=0.7 cliff, MVTO's 45–120%, nobody ships MVTO |
| 9 | Wu §4, Fig 11, Fig 12, Fig 13b, Fig 14, Fig 15, Fig 16, Fig 17a, Fig 17b, §8 | storage schemes, N2O 2.4–3.4×, delta ∼2×, scan latency 25–47%, allocator 1.6–4× |
| 10 | Wu §5, §5.1, Fig 18, Fig 19, Fig 20a, Fig 20b, §8 | COOP +45%, 30–60% memory, txn-level +20%, GC-off decay |
| 11 | Wu §6.1, §6.2, Fig 22a, Fig 22b, Fig 23, §8 | logical +25% / +40% / +45% |
| 12 | Wu Table 1, §8, Fig 24, Fig 25 | the shootout; postgres and Hekaton last |

## How to read the papers (with the concepts in hand)

**Hekaton first (~1.5 h)** — it is a systems paper; §4 and §6 carry it:

1. **§4 Storage and Indexing** — Steps 2 and 5 in the authors' words. Check
   Figure 2's six version records against Step 2's table and Step 3's
   transfer; the numbers are all there.
2. **§6 Transaction Management** — Steps 3 and 4. Read §6.2.1 slowly;
   commit dependencies are the subtle part and `visible()` above is your
   crib. §6's two properties (read stability, phantom avoidance) are the
   definition of "serializable" the rest of the paper uses.
3. **§8 Garbage Collection** — Step 6; note the trigger (a scan passing by)
   versus postgres's (a scheduled vacuum), and read §8.1.2's "dusty
   corners" paragraph twice.
4. **§9 Experimental Results** — the numbers to keep: §9.1.1 reports a 20×
   lookup speed-up for 10+ lookups per call and 10.8× for a single lookup;
   §9.1.2 reports "around 30×" for transactions updating 100 or more
   records. Both are single-core CPU-efficiency measurements on a 2.67 GHz
   Xeon W3520 with 1M-row tables (§9.1) — *not* end-to-end system
   throughput. Skim §7 (durability); checkpointing versions to disk is
   topic-5 material in new clothes.

**Then Wu/Pavlo (~1 h)** — read it as a menu with prices. §3–§6 map
one-to-one onto Steps 8–11; §7's graphs are the message. Landmarks: Fig 1
(the version header — compare Step 2), Table 1 (nine systems on the four
axes), Fig 12 (N2O vs O2N), Fig 13–15 (storage vs update rate and attribute
count), Fig 16 (the allocator confound), Fig 18–21 (GC, plotted against
elapsed time), Fig 22–23 (index pointers vs secondary-index count), Fig
24–25 (the shootout). For each axis, find the crossover workload where the
verdict flips — that is what you are buying.

## Questions for notes.md

1. Hekaton's `End`-as-lock: write the CAS-based first-writer-wins in
   pseudocode. Where does your `mvcc.rs` do the same check? (Point at the
   line once `commit()` at `experiments/src/mvcc.rs:105` is implemented and
   `first_committer_wins_on_write_write_conflict` passes.)
2. Delta storage wins narrow updates of wide tables (Fig 13b); append-only
   N2O wins reads and scans (Fig 12, Fig 17b). Which is a GraphBLAS **delta
   matrix** (topic 20)? So M8's "copy-on-write + deltas" sits where in Wu's
   taxonomy — and what do Fig 14/15 predict about its read path?
3. Logical vs physical index pointers: FalkorDB's node ids *are* logical
   indirection into matrices. What does that make "index management" cost
   for a graph MVCC — which updates still have to touch indexes, and does
   Fig 22a's 40%-at-20-indexes result apply at all?
4. Cooperative GC cleans in proportion to reads: what happens to a
   write-only hot key that nobody reads? Hekaton §8.1.2 answers this
   directly — find the answer and say why it is a memory problem and not a
   throughput problem.
5. Predict, then check Wu §7.2: at 40 cores and high contention, what ruins
   MVOCC — validation aborts or timestamp allocation? (Fig 7a's sentence
   settles it in one line.)
6. Step 12: Hekaton and postgres finish last in Fig 24 for the same reason.
   Name it, and name the two Hekaton design choices that the shootout says
   were *not* the problem.

## Takeaway

Hekaton shows what MVCC becomes when the disk is deleted: two timestamps
per version, a CAS into a timestamp field standing in for the entire lock
manager, commit as a validate-log-fixup pipeline with speculative readers,
and cleanup done by whoever walks past. Wu & Pavlo show that this beautiful
protocol design is not where the throughput is: across four design
decisions measured in one system, the concurrency control protocol matters
least below θ=0.7, and the version-storage layout matters most — enough to
put Hekaton's configuration at the bottom of their TPC-C shootout next to
postgres, for the one choice the two systems share.

## Connections to this topic's experiment

This topic's provided benchmark lane measures a deliberately naive
baseline: a single global `Mutex<HashMap>`, 4 threads × 50 000 transactions
× 4 operations each. On an Apple M3 Pro (measured 2026-07-28, recorded in
[`notes.md`](notes.md)):

| Workload | Keys | mutex txn/s |
|---|---|---|
| read-heavy 95/5 | 10 000 | 623 454 |
| write-heavy 50/50 | 10 000 | 594 264 |
| write-heavy 50/50 | 64 (hot) | 676 691 |

**Read that table as a negative result, and be careful what you conclude
from it.** The three numbers are flat — within about 12% of each other —
across workloads that differ enormously in read/write mix and key skew. The
reason is not that the mutex is good; it is that a global mutex has
*already serialized everything*, so the workload's shape cannot influence
the result. That is the finding recorded in
[`FINDINGS.md`](../../FINDINGS.md) row 8.

What this repo has **not** measured is MVCC beating that mutex. The `mvcc
txn/s` and `aborts` columns in `notes.md` are `stub` — they are the
exercise. Nothing in this guide, and nothing in the topic, is evidence that
this repo's MVCC implementation is faster than the mutex; the Wu/Pavlo
numbers quoted above were measured on a 40-core Xeon running Peloton, not
here.

Two connections worth holding onto while you implement:

- Step 7's Fig 6a control (a read-only workload still ceilinged at 24 of 40
  cores by coherence traffic on the *memory manager's* counters) and this
  topic's flat mutex line are the same lesson: the shared structure sets the
  ceiling. When your `mvcc.rs` lane finally produces a number, ask which
  shared structure is setting *its* ceiling before you credit the protocol.
- Step 10's Fig 18 is plotted against elapsed time because GC's absence
  shows up as a slope, not a level. `gc_drops_dead_versions_but_respects_active_snapshots`
  in `experiments/src/mvcc.rs` is the correctness half of that; a run long
  enough to show the slope is the performance half.

## Done when

Answer each before unfolding it.

- [ ] Name the four design decisions Wu & Pavlo isolate, and say which one
      §8 calls the most important for scaling an in-memory MVCC DBMS.

<details><summary>Answer</summary>

Concurrency control protocol (§3), version storage (§4), garbage collection
(§5), index management (§6). §8: "the version storage scheme is one of the
most important components to scaling an in-memory MVCC DBMS in a multi-core
environment. This goes against the conventional wisdom in database research
that has mostly focused on optimizing the concurrency control protocols."

</details>

- [ ] Given the Hekaton Figure 2 John rows — `(Begin 10, End 20, 100)`,
      `(20, 100, 110)`, `(100, ∞, 130)` — say which version a read at
      RT = 20 sees, and why the rule cannot return two.

<details><summary>Answer</summary>

The rule is `Begin < RT and End > RT` (§6.1), strict on both sides. At
RT = 20: row 1 fails `End > RT` (20 > 20 is false); row 2 fails `Begin < RT`
(20 < 20 is false); row 3 fails `Begin < RT` (100 < 20 is false). **No
version is visible at RT = 20** — the boundary is exactly the instant the
updating transaction's commit timestamp falls on, and a logical read time is
"any value between the transaction's begin time and the current time"
(§6.1), assigned to a transaction's start time, which is never equal to
another transaction's already-assigned end timestamp. Two versions can never
both qualify because "different versions of a record always have
non-overlapping valid times" (§4.1) — the End of one is the Begin of the
next, and the two comparisons are strict in opposite directions.

</details>

- [ ] Explain why Hekaton's readers never block on an in-flight writer, and
      name the cost that replaces blocking.

<details><summary>Answer</summary>

A reader that finds a txn-id (not a timestamp) in a version's Begin or End
field takes a **commit dependency** rather than waiting: "To preserve the
non-blocking nature of Hekaton, we have T1 take a commit dependency on T2.
This means that T1 is allowed to commit only if T2 commits. If T2 aborts, T1
must also abort so **cascading aborts are possible**" (§6.2.1). The costs
are cascading aborts (wasted work instead of wasted wall-clock) and the
**read barrier** — T1's results are withheld from the client until its
dependency count reaches zero, so the latency reappears at the API edge
rather than inside the engine.

</details>

- [ ] Hekaton uses physical index pointers. Quote the Wu/Pavlo figure and
      number that prices this, and say which workload property makes the
      price grow.

<details><summary>Answer</summary>

Physical pointers mean "when updating any tuple in a table, the DBMS inserts
the newly created version into all the secondary indexes" (Wu §6.2), and
Table 1 files Hekaton under Physical. Fig 22b: at high contention (θ=0.8)
logical pointers achieve **25% higher** throughput; Fig 22a: at low
contention the gap "is enlarged to **40%** with the number of secondary
indexes increased to 20"; Fig 23: with eight secondary indexes under high
contention, logical is **45% higher**. The property that grows the price is
the **number of secondary indexes** — each new version must be inserted into
every one of them.

</details>

- [ ] In Fig 24's shootout, which two configurations finish last, and what
      single design choice does §8 blame?

<details><summary>Answer</summary>

"Postgres and Hekaton's configurations lead to the worst performance, and
the major reason is that the use of **append-only storage with O2N
ordering** severely restricts the scalability of the system" (§8). The
supporting measurement is Fig 12: N2O "always performs better than O2N",
and at θ=0.9 by **2.4–3.4×**. Note what is *not* blamed: Hekaton's
CAS-as-lock, its commit dependencies, or its cooperative GC — the shootout
indicts a layout decision, not the protocol.

</details>

- [ ] State the measured result from this repo's own lane, and state what it
      does **not** show.

<details><summary>Answer</summary>

The global-mutex baseline delivers 623 454 / 594 264 / 676 691 txn/s on
read-heavy 10K-key, write-heavy 10K-key and write-heavy 64-hot-key
workloads respectively (Apple M3 Pro, 2026-07-28; `notes.md`,
[`FINDINGS.md`](../../FINDINGS.md) row 8). It is **flat** because the mutex
already serialized everything, so the workload's shape cannot reach the
result. It does **not** show MVCC beating a mutex — the `mvcc txn/s` and
`aborts` columns are `stub`, and every comparative number in this guide was
measured by Wu & Pavlo on a 40-core Xeon running Peloton, not in this repo.

</details>

## References

**Papers**

- Diaconu, Freedman, Ismert, Larson, Mittal, Stonecipher, Verma, Zwilling —
  *Hekaton: SQL Server's Memory-Optimized OLTP Engine* (SIGMOD 2013) —
  ~1.5 h; §4 (storage and indexing) and §6 (transaction management) carry
  it.
- Wu, Arulraj, Lin, Xian, Pavlo — *An Empirical Evaluation of In-Memory
  Multi-Version Concurrency Control* (VLDB 2017) —
  [PDF](https://db.cs.cmu.edu/papers/2017/p781-wu.pdf) — ~1 h; Table 1 and
  the §7 graphs carry the message.

**Anchors used in this guide**

| Where | What |
|---|---|
| Hekaton §2, §2.1.2 | the 3–4× / 90% / 99% instruction-count argument; "no latches or spinlocks on any performance-critical path" |
| Hekaton §4, §4.1, §4.2, Figure 2 | version record layout; bucket scan at read time 15; the $20 Larry→John transfer and the `Tx75 → 100` fix-up |
| Hekaton §6, §6.1, §6.2.1–§6.2.3 | read stability and phantom avoidance; `Begin < RT and End > RT`; commit dependencies, read barriers, post-processing, rollback |
| Hekaton §8.1.1, §8.1.2 | oldest-active watermark; cooperative unlinking; "dusty corners" |
| Hekaton §9.1, §9.1.1, §9.1.2 | 2.67 GHz Xeon W3520, 1M rows; 20× / 10.8× lookups; ~30× updates |
| Wu Table 1 | nine systems placed on the four axes; postgres and Hekaton both append-only O2N with physical pointers |
| Wu §3.1–§3.4 | MVTO, MVOCC, MV2PL, SI+SSN; `read-ts`, `read-cnt`, no-wait |
| Wu §4, §5, §5.1, §6.1, §6.2 | version storage schemes; GC granularities; COOP requires O2N; logical vs physical pointers |
| Wu §7, Fig 6–Fig 25 | the whole price list; hardware and method in §7 |
| Wu §8 | the four findings and the Fig 24 shootout verdict |

**In this repo**

| Where | What |
|---|---|
| [`notes.md`](notes.md) | the measured mutex baseline and the `stub` MVCC columns |
| [`FINDINGS.md`](../../FINDINGS.md) row 8 | the headline: flat ~600k txn/s, because the mutex already serialized everything |
| `experiments/src/mvcc.rs:105` | `commit()` — where first-committer-wins goes |
| [`reading-postgres-heapam.md`](reading-postgres-heapam.md) | the disk-era design this one is defined against |
| [`reading-ssi-postgres.md`](reading-ssi-postgres.md) | SIREAD locks, the alternative to Step 4's commit-time validation |
