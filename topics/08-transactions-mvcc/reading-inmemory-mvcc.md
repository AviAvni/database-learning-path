# In-memory MVCC: timestamps as locks, and the design-space price list

What does MVCC look like when the disk-era assumptions are deleted?
Hekaton (SIGMOD '13) answers with one design — no locks, no latches, no
pages; Wu & Pavlo's VLDB '17 evaluation answers with the whole design
SPACE, benchmark-backed prices attached. This chapter builds Hekaton's
machine one move at a time, then lays out the five-axis menu. Read Hekaton
first (a design), then Wu/Pavlo (the menu).

## The problem in one sentence

When every data access costs ~100 ns instead of ~10 ms, the coordination
machinery built for disks — a central lock manager, page latches, a buffer
pool — costs more than the work it protects: the classic "OLTP through the
looking glass" breakdown found under 15% of instructions doing useful work,
so Hekaton's design rule was absolute — **no locks, no latches, no pages,
anywhere on any hot path**.

## The concepts, step by step

### Step 1 — MVCC recap, minus the disk

MVCC (multi-version concurrency control) means writers never overwrite:
each update creates a **new version** of the record, and every reader is
handed a consistent point-in-time view, so readers and writers never block
each other. Postgres (previous guide) implements this *on disk*: versions
are heap tuples on 8 KB pages, visibility metadata is transaction ids
(xids) plus a commit log to look them up, and cleanup is a background
vacuum process.

Delete the disk and every one of those choices is up for renegotiation:
versions can be plain heap-allocated structs linked by pointers, "who
committed when" can be a single 64-bit timestamp comparison instead of a
log lookup, and any thread can free garbage the moment it's provably
unreachable. Hekaton is what you get when you renegotiate all of them at
once.

### Step 2 — the version record: two timestamps bound a lifetime

Hekaton stamps each version with the time interval during which it was the
truth. A global counter hands out monotonically increasing **timestamps**;
a version created at time 100 and superseded at time 250 is visible to
exactly the transactions reading at times 100–249:

```
 ┌──────────┬─────────┬──────────────┬─────────┐
 │ begin_ts │ end_ts  │ index links  │ payload │
 └──────────┴─────────┴──────────────┴─────────┘
 live version: end_ts = ∞
 during update: end_ts = writer's txn-id (acts as the write lock!)
 visibility: begin_ts ≤ my_read_ts < end_ts
```

The visibility check is one range test — two integer comparisons, no
commit-log probe, no in-progress list scan. Compare postgres, where the
same question costs a clog lookup (cached in hint bits) plus a binary
search over the snapshot's in-progress array. That's the payoff of
timestamps: **the version is self-describing with two u64s**.

### Step 3 — txn-ids double as locks: one CAS does two jobs

There is no lock manager, so where does write-write conflict detection
live? Inside `end_ts` itself. Timestamps and transaction ids share the
field, distinguished by one bit (bit-smuggling again). To update a record,
a writer CASes (compare-and-swap — an atomic "replace this value only if
it still equals what I read") its txn-id into the live version's `end_ts`:

- CAS succeeds → this transaction now "owns" the update; the txn-id sitting
  in `end_ts` *is* the write lock, and the writer links its new version.
- CAS fails, or a txn-id is already there → a second writer has the record;
  abort or wait. First-writer-wins, detected with zero shared tables.

One atomic instruction replaces the entire lock-manager conversation:
acquire lock + install version pointer, fused. The cost: readers who
encounter a txn-id in a timestamp field must go ask the transaction table
what state that writer is in — Step 4's fine print.

### Step 4 — commit processing, not a commit point

Commit is a *pipeline*, not an instant: (1) acquire a **commit_ts** from
the global counter; (2) **validate** — for serializable, re-read your read
set and re-run your scan predicates to confirm nothing changed since your
snapshot (this is OCC, optimistic concurrency control: check at the end
instead of locking up front); (3) write the log record; (4) **fix up** —
walk your versions replacing your txn-id with the real commit_ts in every
`begin_ts`/`end_ts` you touched.

Between (1) and (4), other transactions can meet your txn-id in a version.
Instead of blocking, they take a **commit dependency**: "I'll treat this
version as committed at your commit_ts — but if you abort, I abort too."
Visibility becomes speculative, with the speculation resolved by the
writer's fate:

```rust
fn visible(v: &Version, read_ts: u64, txns: &TxnTable) -> bool {
    let begin = match v.begin_ts {
        Stamp(ts) => ts,
        TxnId(id) => match txns.state(id) {
            Committing { commit_ts } => commit_ts, // take a commit DEPENDENCY:
            _ => return false,                     // I abort if the writer does
        },
    };
    let end = match v.end_ts {
        Stamp(ts) => ts,       // superseded at ts
        TxnId(_) => u64::MAX,  // being updated — still the latest for readers
    };
    begin <= read_ts && read_ts < end
}
```

Why it matters: no reader ever waits on a writer's commit — the cost moved
from blocking (latency) to cascading aborts (wasted work), which is the
right trade when conflicts are rare.

### Step 5 — indexes point at version chains, not rows

With no pages, "the table" is just the set of version chains, and the only
way to find one is through an index. Hekaton's indexes (a lock-free hash
table and the Bw-tree — topic 9's cautionary protagonist) map key → chain
of versions; a lookup walks the chain running Step 4's `visible()` until
it finds its version. Index entries point at *chains*, not individual
versions, so a new version doesn't churn the index. MVCC and lock-free
data structures were co-designed here — the version chain's immutable
"append a new head" discipline is exactly what a CAS-based index can
publish atomically.

### Step 6 — cooperative GC: the workload cleans itself

Old versions pile up (that's MVCC's rent), and there's no vacuum process.
Instead: any thread that *walks past* a version whose `end_ts` is older
than the oldest active transaction's read timestamp unlinks it on the
spot. Cleanup happens in proportion to how much the workload reads, right
where the garbage obstructs traffic, with no separate process to schedule
or throttle. The failure mode to remember: garbage nobody walks past
doesn't get cleaned (Wu/Pavlo call this out — question 4).

Contrast postgres on every axis now visible: timestamps vs
xid+clog+hint-bits (Step 2); CAS-as-lock vs lock manager (Step 3);
commit-time validation vs SIREAD locks (Step 4); new-to-old pointer chains
vs t_ctid old-to-new; cooperative GC vs vacuum (Step 6).

### Step 7 — Wu/Pavlo: the same decisions as a menu with prices

Hekaton is one point in a space. Wu & Pavlo implemented *every* combination
of the design axes in one system (Peloton) and benchmarked them — read
their tables as a price list:

| Axis | Options | Verdict (their workloads) |
|---|---|---|
| concurrency control | MVTO / MVOCC / MV2PL / SI+SSN | MVTO wins TPC-C by 45–120%; MVOCC loses ~50% past contention θ=0.7 (conflicts found only at validation); no protocol helps write-write conflicts |
| version storage | append-only / delta / time-travel | **delta wins for small updates of wide tables** (~2× at 100 attributes) but scan latency grows near-linearly with threads; append-only pays full-tuple copies |
| ordering | newest-to-oldest / oldest-to-newest | N2O wins always — 2.4–3.4× at θ=0.9; O2N walks garbage first, and readers want the newest |
| GC | tuple-level background / cooperative / txn-level / epoch | cooperative: +45% throughput, 30–60% less memory than background vacuum; txn-level epoch: +20% update-intensive, smallest footprint; GC off = throughput *decays over time* |
| index mgmt | logical pointers / physical | logical (indirection) — +25% at high contention, +40% with 20 secondary indexes; physical means every version churns every index |

Terms: *append-only* stores each version as a full copy (postgres);
*delta* stores only the changed columns (like an undo record); N2O/O2N is
which end of the version chain the pointer enters. The meta-lesson (their
words, roughly): everyone argues about CC algorithms, but **version
storage and GC decide throughput**. Storage layer > protocol. (The RUM
triangle strikes again.)

Three findings hide outside the axis table:

- **The allocator is a confounding variable.** Partitioning version
  storage into per-thread memory spaces lifted append-only/time-travel
  throughput **1.6–4×** (Fig 16) — allocator contention masquerades as
  protocol cost. Even on read-only YCSB, scaling flattens past 24 of 40
  cores from cache-coherence traffic on the memory manager's counters,
  not from any protocol.
- **Non-inline attributes** (BLOBs, long strings): reference-count them
  instead of copying per version — +40% read-intensive, >100%
  update-intensive at 50 attributes (Fig 11).
- **The shootout** (§8, Figs 24–25): Peloton configured as each real
  system's Table-1 row, on TPC-C. Oracle/MySQL (MV2PL + delta + vacuum +
  logical ptrs) and NuoDB (MV2PL + append-N2O) come first; **postgres
  comes last** — append-only O2N is the strangler. But the delta winners
  post the *worst* scan latency — no corner of the space dominates. And
  MVTO, the best all-round protocol, ships in none of the nine systems
  surveyed.

## How to read the papers (with the concepts in hand)

**Hekaton first (~1.5 h)** — it's a systems paper; the version format and
commit processing sections carry it:

1. Storage & indexing section — Steps 2 and 5 in the authors' words; check
   the version-record figure against Step 2's diagram.
2. Transaction management — Steps 3–4. Read the commit-processing walk
   slowly; commit dependencies are the subtle part, and `visible()` above
   is your crib.
3. Garbage collection — Step 6; note what triggers cleaning (a scan
   passing by) vs postgres (a scheduled vacuum).
4. Skim the durability/recovery section — checkpointing versions to disk
   is topic-5 material wearing new clothes.

**Then Wu/Pavlo (~1 h)** — read it as a menu with prices: taxonomy
sections §3–§6 map one-to-one onto Step 7's table rows; the §7 graphs are
the message. Landmarks: Fig 1 (the version header — compare Step 2's
diagram), Table 1 (nine real systems placed on the axes), Fig 12 (N2O vs
O2N), Fig 13–15 (storage schemes vs update rate / attribute counts),
Fig 16 (the allocator finding), Fig 18–21 (GC on/off decay over time),
Fig 22–23 (index pointers vs secondary-index count), Fig 24–25 (the
shootout). For each axis, find the crossover workload where the verdict
flips — that's what you're buying.

## Questions for notes.md

1. Hekaton's end_ts-as-lock: write the CAS-based first-writer-wins in
   pseudocode. Your mvcc.rs does the same check where? (Point at the line
   once implemented.)
2. Delta storage wins for writes; append-only N2O for reads. Which is a
   GraphBLAS **delta matrix** (topic 20)? So M8's "copy-on-write + deltas"
   sits where in the Wu/Pavlo taxonomy — and what does their data predict
   about its read path?
3. Logical vs physical index pointers: FalkorDB's node ids ARE logical
   indirection into matrices. What does that make "index management" cost
   for a graph MVCC — which updates still have to touch indexes?
4. Cooperative GC in proportion to reads: what happens to a write-only
   hot key that nobody reads? (Wu/Pavlo call this out — find the fix.)
5. Predict, then check §7 of Wu/Pavlo: at 40 cores, high contention, what
   ruins MVOCC — validation aborts or timestamp allocation?

## Done when

You can fill the 5-axis table from memory and place postgres, Hekaton,
and your M8 design in it — one row each.

## References

**Papers**
- Diaconu, Freedman, Ismert, Larson, Mittal, Stonecipher, Verma, Zwilling
  — "Hekaton: SQL Server's Memory-Optimized OLTP Engine" (SIGMOD 2013) —
  ~1.5 h; the version format and commit processing sections carry it
- Wu, Arulraj, Lin, Xian, Pavlo — "An Empirical Evaluation of In-Memory
  Multi-Version Concurrency Control" (VLDB 2017) —
  [PDF](https://db.cs.cmu.edu/papers/2017/p781-wu.pdf) — ~1 h; read it as
  a menu with prices, Table 1 and the §7 graphs carry the message
