# Reading guide — Calvin (SIGMOD '12): deterministic databases

Paper: *Calvin: Fast Distributed Transactions for Partitioned Database
Systems*, Thomson et al., SIGMOD 2012. No repo clone — the lineage lives
on in FaunaDB and the deterministic-database literature (Abadi's group).

## The contrarian move

Every other system in this topic agrees on transaction *outcomes* at
runtime (2PC, Paxos-per-commit). Calvin agrees on transaction *inputs*
before execution, then executes deterministically — so every replica and
every shard reaches the same state with **zero runtime coordination about
outcomes**. No 2PC. No commit protocol at all.

```
        conventional                          Calvin
  txns arrive ──> execute ──> agree     txns arrive ──> AGREE ON ORDER
  (locks, 2PC, aborts, retries)         (sequencer: batch + replicate log)
        │                                       │
  nondeterminism everywhere             execute deterministically
  => replicas must ship outcomes        => replicas re-derive outcomes
```

## The three layers (paper §2)

1. **Sequencer** — collects txn requests into 10ms epochs, replicates the
   batch (Paxos) across replicas, hands each shard the global order.
   *This is the only consensus in the system, and it's off the critical
   path of execution.*
2. **Scheduler** — deterministic locking: acquire locks in exactly the
   order txns appear in the log. Deadlock-free by construction (a total
   order over lock acquisition), and every replica makes identical
   grant decisions without talking.
3. **Executor** — runs txn logic. Cross-shard txns exchange *read results*
   (push, not request) — each shard knows from the plan exactly which
   remote reads to expect.

A crashed shard recovers by replaying the input log from a checkpoint —
no undo, no in-doubt txns, no blocking window. Our `tpc.rs` crash matrix
simply *cannot happen* here: there is no coordinator state to lose.

## The catch (why not everyone is Calvin)

- **Read/write sets must be known up front** to lock deterministically.
  Interactive txns (`BEGIN; read; think; write; COMMIT`) don't fit.
  Dependent txns get the OLLP trick: run a *reconnaissance* read-only pass
  to discover the sets, then submit, then re-check and retry if they moved.
- **One slow txn stalls the lock queue behind it** — deterministic order
  means no reordering around stragglers.
- Latency floor = epoch batching + log replication before *any* execution.

Contrast with our lane 2: Percolator aborts under contention (measured vs
θ); Calvin never aborts for conflicts — contention converts to *queueing*
at the scheduler. Same enemy (the Zipf table in README §0), opposite
symptom.

## Questions to answer while reading

1. Calvin still uses locks (§3.2). Why does deterministic lock *ordering*
   eliminate both deadlock and the need for 2PC, when 2PL alone
   eliminates neither?
2. Trace a node failure during a cross-shard txn: how do the other shards
   finish without it, and why can't this deadlock? (Hint: any replica of
   the dead shard can supply the pushed reads.)
3. OLLP's reconnaissance pass is optimistic. Construct the pathological
   workload where it livelocks, and relate it to our θ=1.3 row (99.6%
   collision).
4. Why is a deterministic database's replication cheaper than shipping a
   physical WAL (topic 15), and what does that trade for CPU?
5. Where does Calvin's design reappear in modern systems? (FaunaDB
   directly; but also: FoundationDB's sequencer fixes a global order
   *before* resolution — which half of Calvin is that?)
6. M29 mapping: graph traversals are the ultimate dependent transaction —
   the read set IS the result. Could an M29 FalkorDB use OLLP
   (reconnaissance traversal, then deterministic re-execution), and what
   invalidation check would "did the read set move?" become on a graph?
