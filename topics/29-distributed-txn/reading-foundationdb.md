# Reading guide — FoundationDB (SIGMOD '21): the unbundled transaction

Paper: *FoundationDB: A Distributed Unbundled Transactional Key Value
Store*, SIGMOD 2021. Code: [`~/repos/foundationdb`](https://github.com/apple/foundationdb) (C++ with the Flow
actor dialect — read for structure, not style).

## The move: decompose the transaction itself

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

- **OCC, lock-free**: a txn commits iff no key in its *read set* was
  written between its read version and commit version. Resolvers keep a
  ~5s in-memory window of write ranges in a skip list.
- **Failure handling = recovery, not repair**: any role dies → bump the
  epoch, recruit a fresh generation of roles, recover the tail of the
  TLogs. There is no per-txn in-doubt state (contrast `tpc.rs`): in-flight
  txns at recovery simply *abort* (clients see commit_unknown and retry
  idempotently).
- **The 5s window**: a txn older than the resolvers' memory can't be
  checked, so it's rejected — `transaction_too_old` is the protocol
  showing through the API.

## Code walk

1. `fdbserver/resolver/ConflictSet.cpp:947` —
   `ConflictBatch::detectConflicts`: the heart. `:996`
   `checkReadConflictRanges` probes each txn's read ranges against the
   version-annotated **skip list** (`SkipList` at `:224`);
   `addConflictRanges` (`:432`, `:1004`) inserts the batch's write ranges
   for future txns. The whole SI-conflict check is ~a hundred lines over
   one data structure.
2. `fdbserver/commitproxy/CommitProxyServer.cpp:504` —
   `CommitBatchContext`: one *batch* of client txns is the unit of
   sequencing, resolution, and TLog durability. Batching is why one
   sequencer process scales: it stamps batches, not txns.
3. `fdbserver/sequencer/masterserver.cpp` — the sequencer: barely more
   than an atomic counter plus epoch bookkeeping. The lesson: after
   decomposition, the *ordering* role is trivial; the *checking* role
   (resolvers) is where the work went.
4. `fdbserver/resolver/ResolverBug.cpp` — injectable resolver bugs: the
   simulator can be told to *corrupt conflict detection on purpose* to
   prove the tests catch it. This is the DST culture (topic 16) applied
   to the exact component our lane 3 crash-storms — they fault-inject
   correctness itself, not just crashes.

## Two design reads

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
