# FoundationDB: the unbundled transaction

What if the transaction manager weren't a process at all, but a pipeline?
FoundationDB decomposes commit into single-purpose roles — sequencer,
resolvers, proxies, logs — batches everything, and turns failure handling
into wholesale recovery instead of per-transaction repair. This chapter
builds the design step by step — optimistic concurrency control, the
role pipeline, the 5-second window, lazy storage, and epoch recovery —
then reads the SIGMOD '21 paper against the production tree; the code is
C++ in the Flow actor dialect — read it for structure, not style.

Paper references below are to Zhou et al., **"FoundationDB: A Distributed
Unbundled Transactional Key Value Store"** (SIGMOD 2021), cited by section,
figure or algorithm. Code anchors are FoundationDB at the SHA pinned in
`resources/codebases.md`; every file:line here was re-checked at that SHA
with `tools/pinned-source.py`.

## The problem in one sentence

Every protocol so far bundles ordering, conflict checking, durability,
and serving into the same processes and pays per-transaction coordination
for it; FDB bets that if you split those four jobs into separate roles
and push *batches* between them, one commodity cluster can commit
millions of transactions per second — with a sequencer that is a single
process, unreplicated, because losing it costs a recovery, not data.

## The concepts, step by step

### Step 1 — OCC: check conflicts at commit, lock nothing

> **In:** nothing yet — this step fixes what "conflict" means and where the
> check runs, the rule Step 4's resolvers implement.
> **Out:** the read-set-vs-recent-writes test that upgrades snapshot
> isolation to strict serializability, and the cheap-abort economics the
> whole pipeline banks on.

**Optimistic concurrency control (OCC)** inverts locking: a transaction
reads freely at a fixed snapshot, buffers its writes locally, and at
commit time submits its **read set** and **write set** (the key ranges it
read and wrote) to a checker. The rule: commit iff *no key in the read set
was written by anyone between the transaction's read version and its commit
version* — otherwise abort and let the client retry. Checking the *read*
set (not just write-write overlap) is what lifts plain snapshot isolation to
**Serializable Snapshot Isolation (SSI)** — the paper is explicit that "FDB
implements Serializable Snapshot Isolation (SSI) by combining OCC with MVCC…
FDB achieves strict serializability" (§2.4.2), because the commit version
defines a serial history and every read is validated against it. The
economics: aborts are cheap (nothing was written anywhere yet — just a
client retry), no locks are ever held across machine boundaries, but wasted
work grows with contention. Percolator is also optimistic; the difference is
*where* the check runs — Percolator checks key-by-key in the data (locks in
a column family), FDB centralizes the check in memory (Step 4). A separate
hard limit falls out of buffering writes client-side: a transaction is
capped at **10 MB** (all written keys+values plus every key in a declared
conflict range; keys ≤ 10 KB, values ≤ 100 KB — §2.2).

### Step 2 — unbundle: one role per job, batches between them

> **In:** the OCC model from Step 1.
> **Out:** the four-role pipeline (sequencer, proxies, resolvers, logs) that
> Steps 3–5 walk one role at a time.

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

> **In:** the role pipeline from Step 2.
> **Out:** a single monotonic version stream that is both snapshot and
> commit order — the versions Step 4's resolvers compare against.

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

> **In:** the version stream from Step 3 and the OCC rule from Step 1.
> **Out:** a commit/abort verdict per transaction from an in-memory window —
> plus the finite-window and false-conflict costs Steps 6–7 weigh.

**Resolvers** implement Step 1's rule (the paper's **Algorithm 1**, §2.4.2).
Each resolver owns a key range and keeps the last **~5 seconds** of
committed write ranges — the `lastCommit` history, "a version-augmented
probabilistic SkipList" — in memory. Checking a transaction = probe each of
its read ranges for a newer write (Algorithm 1, lines 1–5, which also stops
phantom reads); pass = insert its write ranges to haunt the next 5 seconds
(lines 6–7):

```rust
// ILLUSTRATION — our resolver shape; the real check is
// fdbserver/resolver/ConflictSet.cpp:947 ConflictBatch::detectConflicts
// (read-range probe at :996, write-range insert at :1004).
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

Work Algorithm 1's line 1–5 test on three transactions all reading range
`[k1,k5)`, against a `lastCommit` that records a write to that range at
version 150:

```
txn  read_version   newest write in [k1,k5)   150 > read_version?   verdict
 A       100                150                     yes             ABORT (r-w conflict)
 B       160                150                     no              COMMIT, insert [k1,k5)@cv
 C       150                150                     no (150 ≤ 150)   COMMIT (its own snapshot
                                                                     already saw that write)
```

The check is a single comparison — newest conflicting write vs the reader's
own read version — which is why "one single-threaded Resolver can easily
handle 280K TPS" (§2.4.2). Two consequences show through the API. The window
is finite: a transaction older than the resolvers' memory *can't* be
checked, so it's rejected — `transaction_too_old` at 5 s (§6.4: the window
exists to bound resolver/storage memory). And resolvers are range-sharded
and never talk to each other: a multi-range transaction is checked by every
resolver its ranges touch, any one can abort it, and a resolver that aborted
a txn while another passed it has recorded write ranges for a transaction
that never committed — **false conflicts** for 5 s, the price of not running
resolver-2PC (Q2).

### Step 5 — TLogs and lazy storage: durability is the log, again

> **In:** a resolved (conflict-free) batch from Step 4.
> **Out:** the durability point (TLog fsync) and the lazy-storage read model
> — the invariant Step 6's recovery rebuilds from.

A batch that passes resolution goes to the **TLogs** (transaction logs):
append + fsync, replicated, and *that* is the durability point — the
proxy acknowledges commit once the TLogs have the batch. **Storage
servers** consume the log *asynchronously* and apply writes to their
B-trees lazily; a read at version `v` goes to a storage server and waits
until that server has caught up to `v` (Q1). This is topic 28's Aurora
sentence again — the log is the database; storage materializes it behind
the durability frontier — arrived at independently, one datacenter wide.

### Step 6 — failure = recovery, not repair

> **In:** the log-is-durability model from Step 5.
> **Out:** the epoch-recovery failure model (no per-txn in-doubt state) and
> the MTTR cost it trades for — the design choice Step 7 places on the map.

Any role dies — sequencer, proxy, resolver, TLog — and FDB does not
repair around it: it bumps the **epoch** (a generation number), recruits
a fresh generation of every role, recovers the durable tail from the old
TLogs, and resumes. In-flight transactions at the moment of failure
simply *abort* (clients see `commit_unknown_result` and retry
idempotently). There is no per-transaction in-doubt state — contrast our
`tpc.rs` AfterAllPrepares limbo, which FDB eliminates *by construction*
rather than by decision log (Q3). The trade against Spanner: Spanner
replicated every coordinator so nothing stops on a crash; FDB accepts a
brief full-pipeline stall in exchange for never maintaining per-txn
coordination state at all. The paper reports that stall is short — the
whole detect-shutdown-recover cycle is "usually less than five seconds"
of MTTR in production (§2.1, measured §5.3).

### Step 7 — placing FDB on the map

> **In:** the whole pipeline (Steps 1–6).
> **Out:** the two contrasts (vs Calvin, vs Percolator) that fix where FDB's
> choices sit among this topic's protocols.

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
   for future txns. The whole read/write-conflict check (the SSI upgrade of
   Step 1) is ~a hundred lines over one data structure.
2. `fdbserver/commitproxy/CommitProxyServer.cpp:504` —
   `CommitBatchContext`: one *batch* of client txns is the unit of
   sequencing, resolution, and TLog durability (Steps 2, 5). Batching is
   why one sequencer process scales: it stamps batches, not txns.
3. `fdbserver/sequencer/masterserver.cpp` — the sequencer (Step 3):
   barely more than an atomic counter plus epoch bookkeeping (Step 6).
   The lesson: after decomposition, the *ordering* role is trivial; the
   *checking* role (resolvers) is where the work went.
4. `fdbserver/resolver/include/fdbserver/resolver/ResolverBug.h:28-31` —
   the injectable resolver bug:
   three probabilities (`ignoreTooOldProbability`, `ignoreReadSetProbability`,
   `ignoreWriteSetProbability`) the simulator can set to *make conflict
   detection silently drop read/write ranges on purpose* and prove the tests
   catch it. They are consumed in `ConflictSet.cpp:786-796` (the
   `ignoreTooOld`/`ignoreReadSet`/`ignoreWriteSet` predicates) and acted on in
   `addTransaction` at `:805`, `:814` and `:819` — each guarded by
   `bugs->hit()`. (`ResolverBug.cpp` itself is only a 24-line factory that
   constructs the struct; the fields are the interesting part.) This is the
   DST culture (topic 16) applied to the exact component our lane 3
   crash-storms — they fault-inject correctness itself, not just crashes (Q5).

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

## Done when

Answer each before unfolding it.

- [ ] You can explain OCC: check at commit, lock nothing.

  <details><summary>Answer</summary>

  A transaction reads at a fixed snapshot, buffers writes client-side, and at
  commit submits its read and write sets; the checker admits it iff nothing
  wrote a key in its read set between its read version and commit version,
  else it aborts (Step 1). No locks cross machines; aborts are cheap because
  nothing was written anywhere yet. Validating the *read* set (not just
  write-write overlap) is what makes it SSI / strict serializable, not merely
  snapshot isolation (§2.4.2).

  </details>

- [ ] You can name the unbundled roles and what batching happens between them.

  <details><summary>Answer</summary>

  Sequencer (hands out read/commit versions), CommitProxies (batch client
  txns), Resolvers (range-sharded OCC conflict check), TLogs (durability), and
  StorageServers (serve reads, apply the log lazily) — Step 2. The unit moving
  between roles is a *batch* of transactions (`CommitBatchContext`,
  CommitProxyServer.cpp:504), which is why one unreplicated sequencer scales:
  it stamps batches, not individual transactions.

  </details>

- [ ] You can explain the resolver's in-memory conflict window and why sharded resolvers need not talk to each other.

  <details><summary>Answer</summary>

  Each resolver keeps ~5 s of committed write ranges (`lastCommit`, a
  version-augmented skip list) and checks a txn's read ranges against it
  (Algorithm 1). Sharding by key range works without cross-talk because each
  resolver is authoritative for its own ranges — a multi-range txn is checked
  by each resolver it touches and any one may abort it. The cost: a resolver
  that admitted its slice of a txn another resolver aborted has recorded
  phantom write ranges → *false conflicts* for 5 s (Q2). Accepting that is
  cheaper than running 2PC among resolvers.

  </details>

- [ ] You can explain why storage servers may apply writes lazily after commit.

  <details><summary>Answer</summary>

  Because durability is the TLog fsync, not the storage apply (Step 5): once
  the replicated TLogs hold the batch the proxy acknowledges the commit, so
  storage servers can pull the log and apply to their B-trees asynchronously.
  A read at version `v` routes to a storage server and waits until that server
  has caught up past `v` (Q1) — the log is the database, storage just
  materializes it behind the durability frontier (topic 28's Aurora idea).

  </details>

- [ ] You can explain why "failure equals recovery, not repair" is a design choice and what it buys.

  <details><summary>Answer</summary>

  Any role death bumps the epoch, recruits a fresh generation of every role,
  recovers the durable tail from the old TLogs, and aborts all in-flight txns
  (`commit_unknown_result`, retried idempotently) — Step 6. It buys the
  *elimination of per-transaction in-doubt state* by construction: there is no
  decision log to consult, no AfterAllPrepares limbo. The price is a brief
  full-pipeline stall — MTTR "usually less than five seconds" (§2.1) — where
  Spanner, having replicated every coordinator, never stalls.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including why a read-only transaction still gets a consistent snapshot without contacting resolvers.

  <details><summary>Answer</summary>

  A read-only transaction reads entirely at its read version via MVCC, so it
  observes exactly the writes committed before that version and conflicts with
  nothing — the paper notes it "is both serializable (happens at the read
  version)" and "the client can commit these transactions locally without
  contacting the database" (§2.4.1). No read set can be invalidated because no
  concurrent write is newer than a snapshot fixed in the past, so the
  resolvers have nothing to check (Q4).

  </details>

## References

**Papers**
- Zhou et al. — "FoundationDB: A Distributed Unbundled Transactional Key
  Value Store" (SIGMOD 2021) — §2.2 the 10 MB/10 KB/100 KB limits; §2.4.1
  read-only txns commit without the DB; §2.4.2 SSI + Algorithm 1 + 280K
  TPS/resolver; §4 the deterministic simulation (pairs with topic 16); §6.4
  the 5 s MVCC window

**Code**
- [foundationdb](https://github.com/apple/foundationdb)
  `fdbserver/resolver/ConflictSet.cpp`,
  `fdbserver/commitproxy/CommitProxyServer.cpp`,
  `fdbserver/sequencer/masterserver.cpp`,
  `fdbserver/resolver/include/fdbserver/resolver/ResolverBug.h` — C++ with
  the Flow actor dialect; read for structure, not style
