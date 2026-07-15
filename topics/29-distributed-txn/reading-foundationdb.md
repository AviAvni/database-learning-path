# FoundationDB: the unbundled transaction

What if the transaction manager weren't a process at all, but a pipeline?
FoundationDB decomposes commit into single-purpose roles — sequencer,
resolvers, proxies, logs — batches everything, and turns failure handling
into wholesale recovery instead of per-transaction repair. This chapter
builds the design step by step — optimistic concurrency control, the
role pipeline, the 5-second window, lazy storage, and epoch recovery —
then reads the SIGMOD '21 paper against the production tree; the code is
C++ in the Flow actor dialect — read it for structure, not style.

## The problem in one sentence

Every protocol so far bundles ordering, conflict checking, durability,
and serving into the same processes and pays per-transaction coordination
for it; FDB bets that if you split those four jobs into separate roles
and push *batches* between them, one commodity cluster can commit
millions of transactions per second — with a sequencer that is a single
process, unreplicated, because losing it costs a recovery, not data.

## The concepts, step by step

### Step 1 — OCC: check conflicts at commit, lock nothing

**Optimistic concurrency control (OCC)** inverts locking: a transaction
reads freely at a fixed snapshot, buffers its writes locally, and at
commit time submits its **read set** and **write set** (the key ranges it
read and wrote) to a checker. The rule for **snapshot-isolation**
conflicts: commit iff *no key in the read set was written by anyone
between the transaction's read version and its commit version* —
otherwise abort and let the client retry. The economics: aborts are cheap
(nothing was written anywhere yet — just a client retry), no locks are
ever held across machine boundaries, but wasted work grows with
contention. Percolator is also optimistic; the difference is *where* the
check runs — Percolator checks key-by-key in the data (locks in a column
family), FDB centralizes the check in memory (Step 4).

### Step 2 — unbundle: one role per job, batches between them

Percolator erased the coordinator; Spanner replicated it. FoundationDB
*shreds* it into single-purpose roles connected by batches:

```
 client
   │ get_read_version / commit(read set, write set)
   ▼
 ┌─────────────┐   read version /   ┌────────────┐
 │ CommitProxy │◄──commit version───│ Sequencer  │  one process: hands out
 │ batches txns│                    │ (master)   │  monotonic versions
 └─────┬───────┘                    └────────────┘
       │ txn batch + versions
       ▼
 ┌────────────┐  key-range sharded; checks each txn's READ set
 │ Resolvers  │  against recent WRITES (OCC): conflict => abort
 └─────┬──────┘
       ▼
 ┌────────────┐  make the batch durable (log first, storage async)
 │ TLogs      │──► storage servers apply lazily; reads served at version
 └────────────┘
```

A client transaction touches the pipeline twice: once to get a read
version, once to commit. Everything between roles moves as a **batch** of
transactions — batching is what lets each role be simple and few.

### Step 3 — the sequencer: the global order is a counter

The **sequencer** hands out monotonically increasing **versions** — one
number stream that serves as both read versions (your snapshot) and
commit versions (your position in history). It is *one process*, and
after decomposition that's affordable: it stamps *batches*, not
transactions, so a single atomic counter orders millions of txns/s. Note
the Calvin rhyme: a global order fixed by a central sequencer — but FDB
orders *versions* and still checks conflicts at runtime (Step 4), so
interactive transactions work; Calvin's reconnaissance problem never
arises. The sequencer is deliberately *not* replicated: its loss triggers
recovery (Step 6), not data loss.

### Step 4 — resolvers: the conflict check as an in-memory window

**Resolvers** implement Step 1's rule. Each resolver owns a key range and
keeps the last **~5 seconds** of committed write ranges in a
version-annotated in-memory skip list. Checking a transaction = probe
each of its read ranges for a newer write; pass = insert its write ranges
to haunt the next 5 seconds of transactions:

```rust
fn resolve(batch: &[Txn], commit_v: Version, writes: &mut VersionedRanges) -> Vec<bool> {
    batch.iter().map(|txn| {
        let ok = txn.read_ranges.iter()          // did anyone write what I read,
            .all(|r| writes.newest_write_in(r) <= txn.read_version); // after I read it?
        if ok {
            writes.insert(&txn.write_ranges, commit_v); // haunt later txns for ~5s
        }
        ok    // false => abort: cheap, because nothing was written anywhere yet
    }).collect()
}
```

Two consequences show through the API. The window is finite: a
transaction older than the resolvers' memory *can't* be checked, so it's
rejected — `transaction_too_old` at 5 s. And resolvers are range-sharded
and never talk to each other: a multi-range transaction is checked by
every resolver its ranges touch, any one can abort it, and a resolver
that aborted a txn while another passed it has recorded write ranges for
a transaction that never committed — **false conflicts** for 5 s, the
price of not running resolver-2PC (Q2).

### Step 5 — TLogs and lazy storage: durability is the log, again

A batch that passes resolution goes to the **TLogs** (transaction logs):
append + fsync, replicated, and *that* is the durability point — the
proxy acknowledges commit once the TLogs have the batch. **Storage
servers** consume the log *asynchronously* and apply writes to their
B-trees lazily; a read at version `v` goes to a storage server and waits
until that server has caught up to `v` (Q1). This is topic 28's Aurora
sentence again — the log is the database; storage materializes it behind
the durability frontier — arrived at independently, one datacenter wide.

### Step 6 — failure = recovery, not repair

Any role dies — sequencer, proxy, resolver, TLog — and FDB does not
repair around it: it bumps the **epoch** (a generation number), recruits
a fresh generation of every role, recovers the durable tail from the old
TLogs, and resumes. In-flight transactions at the moment of failure
simply *abort* (clients see `commit_unknown_result` and retry
idempotently). There is no per-transaction in-doubt state — contrast our
`tpc.rs` AfterAllPrepares limbo, which FDB eliminates *by construction*
rather than by decision log (Q3). The trade against Spanner: Spanner
replicated every coordinator so nothing stops on a crash; FDB accepts a
brief full-pipeline stall (a recovery takes ~seconds) in exchange for
never maintaining per-txn coordination state at all.

### Step 7 — placing FDB on the map

Two design reads to carry out of the topic:

- **vs Calvin**: both fix a global order via a sequencer, but FDB orders
  *versions* and still checks conflicts at runtime (OCC), so interactive
  txns work — the reconnaissance problem never arises. Calvin's
  determinism removed aborts; FDB kept aborts and removed blocking.
- **vs Percolator**: both are optimistic. Percolator's conflict check is
  *distributed in the data* (locks in the lock CF, checked key-by-key at
  prewrite); FDB's is *centralized in memory* (resolvers), which makes
  aborts cheap (nothing was written) but adds the 5s window and the
  false-conflict cost of range-sharded resolvers (a txn is checked by
  every resolver its ranges touch; any one can abort it).

## Where each step lives in the code

1. `fdbserver/resolver/ConflictSet.cpp:947` —
   `ConflictBatch::detectConflicts`: the heart (Step 4). `:996`
   `checkReadConflictRanges` probes each txn's read ranges against the
   version-annotated **skip list** (`SkipList` at `:224`);
   `addConflictRanges` (`:432`, `:1004`) inserts the batch's write ranges
   for future txns. The whole SI-conflict check is ~a hundred lines over
   one data structure.
2. `fdbserver/commitproxy/CommitProxyServer.cpp:504` —
   `CommitBatchContext`: one *batch* of client txns is the unit of
   sequencing, resolution, and TLog durability (Steps 2, 5). Batching is
   why one sequencer process scales: it stamps batches, not txns.
3. `fdbserver/sequencer/masterserver.cpp` — the sequencer (Step 3):
   barely more than an atomic counter plus epoch bookkeeping (Step 6).
   The lesson: after decomposition, the *ordering* role is trivial; the
   *checking* role (resolvers) is where the work went.
4. `fdbserver/resolver/ResolverBug.cpp` — injectable resolver bugs: the
   simulator can be told to *corrupt conflict detection on purpose* to
   prove the tests catch it. This is the DST culture (topic 16) applied
   to the exact component our lane 3 crash-storms — they fault-inject
   correctness itself, not just crashes (Q5).

## Questions to answer while reading

1. Why is it safe for storage servers to apply writes *lazily* after the
   TLog fsync — what exactly is the durability point, and what do reads
   at version `v` wait on?
2. Resolvers are sharded by key range and don't talk to each other. Show
   how this yields *false aborts* that a single resolver wouldn't, and
   why FDB accepts that instead of running resolver-2PC.
3. Recovery aborts all in-flight txns by construction. Why does this
   eliminate `tpc.rs`'s AfterAllPrepares limbo without a decision log —
   and what did FDB pay for it that Spanner didn't?
4. A read-only txn in FDB never contacts the resolvers. Why is it still
   serializable (not just SI), given reads happen at a single version?
5. ResolverBug.cpp ships in the production tree. Argue why "the fault
   injector can break conflict detection" is a *stronger* test than our
   lane 3 (which only crashes at protocol steps) — what class of bug does
   each catch?
6. M29 mapping: FalkorDB could unbundle too — a resolver checking
   read/write sets of *graph elements* (nodes, edges, adjacency ranges).
   What is the graph analogue of a range conflict, and does a 2-hop
   traversal's read set even fit in a resolver's memory window?

## References

**Papers**
- Zhou et al. — "FoundationDB: A Distributed Unbundled Transactional Key
  Value Store" (SIGMOD 2021) — §2-4 for the architecture and recovery;
  §5's simulation section pairs with topic 16

**Code**
- [foundationdb](https://github.com/apple/foundationdb)
  `fdbserver/resolver/ConflictSet.cpp`,
  `fdbserver/commitproxy/CommitProxyServer.cpp`,
  `fdbserver/sequencer/masterserver.cpp`,
  `fdbserver/resolver/ResolverBug.cpp` — C++ with the Flow actor
  dialect; read for structure, not style
