# Calvin: agree on inputs, not outcomes

Every other protocol in this topic coordinates on transaction *outcomes*
at runtime. Calvin is the counterpoint: fix the input order first, execute
deterministically, and the whole commit-protocol problem disappears —
along with the interactive transactions everyone actually writes. This
chapter builds the idea step by step — why nondeterminism forces
coordination, the sequencer flip, deterministic locking, and the price —
then routes you through the paper. There is no reference repo to read
here; the lineage lives on in FaunaDB and in Abadi's
deterministic-database literature, so this chapter is paper-only.

## The problem in one sentence

2PC, replicated coordinators, and reader-driven resolution all pay per
transaction to agree on *what happened*; Calvin asks why replicas must
agree on outcomes at all, when agreeing once on the *inputs* — and
executing them identically everywhere — costs one consensus round per
10 ms batch instead of per transaction.

## The concepts, step by step

### Step 1 — nondeterminism is why databases coordinate

A replica given the same transactions in the same order can still reach a
different state, because execution is **nondeterministic**: thread
scheduling decides who wins a lock, deadlock detectors pick victims
arbitrarily, aborts depend on timing. That's why conventional systems
must ship *outcomes* — replicas can't re-derive them, and cross-shard
atomicity needs a runtime vote (2PC) because each shard's yes/no isn't
predictable from the input:

```
        conventional                          Calvin
  txns arrive ──> execute ──> agree     txns arrive ──> AGREE ON ORDER
  (locks, 2PC, aborts, retries)         (sequencer: batch + replicate log)
        │                                       │
  nondeterminism everywhere             execute deterministically
  => replicas must ship outcomes        => replicas re-derive outcomes
```

Remove the nondeterminism and the arrow flips: agreement moves *before*
execution, once, on the inputs — and everything downstream becomes pure
recomputation.

### Step 2 — the sequencer: one consensus, off the critical path

Calvin's only consensus is at the front door. The **sequencer** collects
incoming transaction requests into **epochs** (10 ms batches), replicates
each batch with Paxos across replicas, and hands every shard the same
global order. Amortization is the trick: one consensus round covers
every transaction in the batch — at 10 ms epochs and thousands of txns
per epoch, the per-transaction consensus cost rounds to zero — and it
runs *ahead of* execution, so it pipelines instead of blocking. The cost
is a latency floor: no transaction can begin executing until its epoch is
sealed and replicated (~10 ms + a replication round), even at zero load.

### Step 3 — the scheduler: deterministic locking

Each shard's **scheduler** runs two-phase locking (2PL — acquire all
locks before releasing any, topic 9) with one constraint that changes
everything: lock requests are enqueued in **exactly the log order** from
Step 2. That single total order over acquisition makes deadlock
impossible (a deadlock needs a cycle of "A waits for B waits for A";
a total order can't cycle) and makes every replica's lock-grant decisions
*identical without communication* — same queues, same order, same grants:

```rust
fn scheduler(log: &[Txn], lm: &mut LockManager) {
    for txn in log {                          // exactly log order, every replica
        for key in txn.read_write_set() {     // known up front — the Calvin price
            lm.enqueue(key, txn.id);          // FIFO queue per key
        }
    }
    // grant rule: txn runs once it heads every queue it sits in.
    // A total order over acquisition => no deadlock cycle can form,
    // and every replica makes IDENTICAL grant decisions without talking.
}
```

Note the load-bearing comment: the keys must be known *up front* — the
price arrives in Step 6.

### Step 4 — executors: cross-shard reads are pushed, not requested

A cross-shard transaction executes at every shard that holds any of its
keys, and each shard knows — from the fixed order and the declared
read/write sets — exactly which remote values the others will need. So
shards **push** their local read results to the other participants and
block until the pushes they expect arrive; nobody requests, nobody votes.
There is no commit protocol at all: every participant *deterministically
reaches the same commit/abort conclusion* from the same inputs, so
"did it commit?" needs no network round — it's a theorem, not a message.

### Step 5 — recovery for free: replay the inputs

A crashed shard recovers by loading a checkpoint and replaying the input
log through the same deterministic machinery — no undo, no in-doubt
transactions, no blocking window. Our `tpc.rs` crash matrix simply
*cannot happen* here: there is no coordinator state to lose, because
there is no coordinator. Replication gets the same discount: replicas
ship the compact input log (a command log) instead of a physical WAL of
every modified byte, trading network bytes for the CPU of re-executing
every transaction at every replica (Q4).

### Step 6 — the catch: why not everyone is Calvin

Three structural costs, all downstream of "the order is fixed before
execution":

- **Read/write sets must be known up front** to lock deterministically
  (Step 3). Interactive transactions (`BEGIN; read; think; write;
  COMMIT`) don't fit. Dependent transactions get the **OLLP** trick: run
  a *reconnaissance* read-only pass to discover the sets, submit with
  those sets declared, then re-check at execution and retry if they moved
  — optimism that can livelock under fire (Q3).
- **One slow transaction stalls the lock queues behind it** —
  deterministic order means no reordering around stragglers.
- **Latency floor** = epoch batching + log replication before *any*
  execution (Step 2).

Contrast with our lane 2: Percolator aborts under contention (measured vs
θ); Calvin never aborts for conflicts — contention converts to *queueing*
at the scheduler. Same enemy (the Zipf table in README §0), opposite
symptom.

## How to read the paper (with the concepts in hand)

- **§2** — the three layers as Steps 2–4: sequencer (epochs + Paxos),
  scheduler, executors with pushed reads. Verify the claim that
  sequencing is the *only* consensus.
- **§3 (especially §3.2)** — deterministic locking, Step 3 in the
  authors' words. This is where Q1 lives: why pinned lock *ordering*
  kills both deadlock and 2PC when 2PL alone kills neither.
- **§5 — OLLP** — Step 6's answer to dependent transactions:
  reconnaissance, declare, re-check, retry. Read it against the θ=1.3
  row of README §0 (99.6% collision) and construct the livelock (Q3).
- Checkpointing/recovery sections — skim with Step 5 in hand; the point
  is what *isn't* there (no undo, no in-doubt state).

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

## References

**Papers**
- Thomson, Diamond, Weng, Ren, Shao, Abadi — "Calvin: Fast Distributed
  Transactions for Partitioned Database Systems" (SIGMOD 2012) — §2-3
  are the architecture and the deterministic locking; §5's OLLP is the
  answer to dependent transactions

**Code**
- No reference implementation to clone — the lineage lives on in FaunaDB
  and in Abadi's deterministic-database papers
