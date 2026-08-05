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

Every section reference below is to Thomson et al., **"Calvin: Fast
Distributed Transactions for Partitioned Database Systems"**, SIGMOD 2012 —
there is no clone to pin, so the numbers are cited by the paper's own
section, figure and table.

## The problem in one sentence

2PC, replicated coordinators, and reader-driven resolution all pay per
transaction to agree on *what happened*; Calvin asks why replicas must
agree on outcomes at all, when agreeing once on the *inputs* — and
executing them identically everywhere — costs one consensus round per
10 ms batch instead of per transaction.

## The concepts, step by step

### Step 1 — nondeterminism is why databases coordinate

> **In:** nothing yet — this step fixes the two words ("nondeterministic",
> "2PC") every later step argues against.
> **Out:** the reason conventional systems ship *outcomes*, which Step 2
> then removes at the source.

A **replica** is a full copy of a shard's data kept on another machine for
durability and availability. Give two replicas the same transactions in the
same order and they can still reach a *different* state, because execution is
**nondeterministic** — the result depends on things the input does not fix:
thread scheduling decides who wins a lock, deadlock detectors pick their
victim arbitrarily, an abort fires or doesn't depending on timing. That is
why conventional systems must ship *outcomes* (the actual row values a
transaction produced): replicas cannot re-derive them from the input alone.

And cross-shard atomicity needs a runtime vote — **two-phase commit (2PC)**,
the protocol where a coordinator asks every shard to *prepare*, then commits
only if all vote yes (topic 15 and the Percolator guide) — because each
shard's yes/no is not predictable from the input either:

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

> **In:** the nondeterminism problem from Step 1.
> **Out:** a single global transaction order, sealed once per epoch and
> handed identically to every shard — the input Step 3 locks against and
> Step 4 executes.

Calvin's only consensus is at the front door. **Consensus** is the act of
getting a set of replicas to agree on one value despite failures; **Paxos**
is the classic algorithm for it (topic 15). The **sequencer** is the layer
that collects incoming transaction requests into **epochs** — 10-millisecond
batches (§3.1: "Calvin divides time into 10-millisecond epochs") — replicates
each batch with Paxos across replicas, and hands every shard the same global
order.

Amortization is the trick: *one* consensus round covers a whole *batch* of
transactions, and it runs **ahead of** execution, so it pipelines instead of
blocking. Work it on the paper's own headline configuration (§6.1, Figure 4:
100 nodes, 10 warehouses/node, 10% distributed → ~5,000 New Order txns/s
*per node*, "nearly half a million" cluster-wide). Each node's sequencer does
one replication round per epoch, amortized over the requests that node
collected in those 10 ms:

```
epochs per second         = 1000 ms / 10 ms               =   100 epochs/s
per-node txns per epoch    = 5,000 txns/s / 100 epochs/s   =    50 txns/epoch
consensus rounds paid      = 1 replication round / sequencer / epoch
per-transaction consensus  = 1 round / 50 txns             =  0.02 rounds/txn
cluster throughput         = 5,000 txns/s × 100 nodes      ≈ 500,000 txns/s
```

So the consensus a conventional system pays *per distributed transaction*,
Calvin pays once per ~50 — a 50× dilution that keeps shrinking as load per
epoch grows. The cost it buys instead is a **latency floor**: no transaction
can begin executing until its epoch is sealed and replicated (~10 ms of
batching plus one replication round), even at zero load.

### Step 3 — the scheduler: deterministic locking

> **In:** the single global order from Step 2.
> **Out:** a per-key lock-grant schedule that is byte-identical on every
> replica, computed with zero cross-replica communication — the guarantee
> Step 4 leans on to skip the commit vote.

Each shard's **scheduler** is the component that decides which transaction
holds which lock when. It runs **two-phase locking (2PL** — a transaction
acquires all the locks it needs and releases none until it is done, topic 9**)**,
but with two added invariants that the paper states in §3.2 and that change
everything:

1. **Request order follows the serial order.** For any two transactions A
   and B that both want an exclusive lock on record R, if A precedes B in
   the sequencing layer's order, then A must *request* R's lock before B
   does. Calvin gets this by having a single thread scan the global order
   and issue every transaction's lock requests in turn — which forces every
   transaction to **declare its full read/write set in advance** (the Calvin
   price, Step 6).
2. **Grants follow request order.** The lock manager grants each lock
   strictly in the order the requests arrived, so B cannot jump ahead of A
   on R even if A is momentarily stalled.

Those two together make **deadlock impossible** — a deadlock needs a cycle
"A waits for B waits for A", and a single total order over acquisition cannot
contain a cycle — and make every replica's grant decisions *identical without
communication*: same queues, same order, same grants.

```
ILLUSTRATION — the two invariants of Calvin §3.2 as one scan; not quoted
code (Calvin has no public reference implementation).

for txn in global_order:            // §3.2 invariant 1: one thread, log order
    for key in txn.read_write_set(): // declared UP FRONT — the Calvin price
        lock_queue[key].push(txn)    // FIFO per key

// §3.2 invariant 2: grant strictly in request order.
// A txn runs once it is at the head of every queue it sits in.
// A total order over acquisition => no deadlock cycle can form,
// and every replica makes IDENTICAL grant decisions without talking.
```

The load-bearing line is the read/write-set lookup: the keys must be known
*before* the scan, or invariant 1 has nothing to enqueue — the price arrives
in Step 6.

### Step 4 — executors: cross-shard reads are pushed, not requested

> **In:** the identical lock schedule from Step 3.
> **Out:** committed writes at every shard, reached with no commit vote —
> the absence Step 5 turns into free recovery.

A cross-shard transaction executes at every shard that holds any of its
keys. The paper's execution model (§3.1) has five phases; the one that
matters here is the third. Each shard knows — from the fixed order and the
declared read/write sets — exactly which of its local values the other
shards will need, so it **serves remote reads by pushing**: it forwards its
local read results to the counterpart worker threads on the other
participants and blocks until the pushes *it* expects arrive. A shard that
holds only keys in the read set is a **passive participant** (it forwards
reads and is then done); a shard that holds a written key is an **active
participant** (it also runs the transaction body). Nobody *requests* a value
mid-execution, and nobody votes.

There is no commit protocol at all: every participant *deterministically
reaches the same commit/abort conclusion* from the same inputs, so "did it
commit?" needs no network round — it is a theorem the shards each prove
locally, not a message they exchange.

### Step 5 — recovery for free: replay the inputs

> **In:** the deterministic execution of Step 4.
> **Out:** a recovery story with no undo and no in-doubt state — the
> structural win Step 6 has to weigh against its costs.

A crashed shard recovers by loading a checkpoint and replaying the input
log through the same deterministic machinery — no undo, no in-doubt
transactions, no blocking window. Our `tpc.rs` crash matrix simply
*cannot happen* here: there is no coordinator state to lose, because
there is no coordinator. Replication gets the same discount: replicas
ship the compact input log (a **command log** — the transaction requests
themselves) instead of a physical WAL of every modified byte, trading
network bytes for the CPU of re-executing every transaction at every
replica (Q4).

### Step 6 — the catch: why not everyone is Calvin

> **In:** the whole pipeline (Steps 2–5), whose every win depended on the
> order being fixed *before* execution.
> **Out:** the three costs that buys, and the reason interactive and
> graph-shaped workloads (M29) are where the model strains.

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

- **§3 — the three layers as Steps 2–4:** sequencer (§3.1, epochs +
  replication), scheduler (§3.2), and the executor's five phases (§3.2,
  numbered list) with pushed reads. Verify the claim that sequencing is the
  *only* consensus.
- **§3.2 — deterministic locking**, Step 3 in the authors' words, including
  the two locking invariants. This is where Q1 lives: why pinned lock
  *ordering* kills both deadlock and 2PC when 2PL alone kills neither.
- **§3.2.1 — dependent transactions (OLLP)** — Step 6's answer:
  reconnaissance, declare, re-check, retry. Read it against the θ=1.3 row of
  README §0 (99.6% collision) and construct the livelock (Q3).
- **§6.1 — TPC-C** — the ~500,000 New Order txns/s on 100 nodes headline
  behind Step 2's amortization arithmetic; **§6.3 handling high contention**
  is the θ contrast in Step 6.
- **§5 checkpointing / recovery** — skim with Step 5 in hand; the point is
  what *isn't* there (no undo, no in-doubt state).

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

## Done when

Answer each before unfolding it.

- [ ] You can state why nondeterminism is the reason databases coordinate.

  <details><summary>Answer</summary>

  Because execution outcome is not fixed by the input alone: thread
  scheduling, deadlock-victim choice, and timing-dependent aborts all diverge
  run to run (Step 1). So two replicas fed the same ordered log can reach
  different states, which forces conventional systems to ship *outcomes*
  rather than inputs, and forces cross-shard atomicity through a runtime vote
  (2PC) because no shard's yes/no is predictable from the input. Remove the
  nondeterminism and both needs vanish.

  </details>

- [ ] You can explain how the sequencer takes consensus off the critical path.

  <details><summary>Answer</summary>

  The sequencer batches requests into 10-ms epochs (§3.1) and runs *one*
  replication round per sequencer per epoch, *ahead of* execution. On the
  paper's 100-node TPC-C run (§6.1) that is ~50 txns/epoch/node, so the
  per-transaction consensus cost is 1 round / 50 txns ≈ 0.02 rounds/txn and
  keeps shrinking with load — versus one agreement *per distributed
  transaction* in a 2PC system. The price is a latency floor of ~10 ms of
  batching plus one replication round before any execution begins.

  </details>

- [ ] You can explain deterministic locking and why lock *ordering* is what makes it work.

  <details><summary>Answer</summary>

  A single thread scans the global order and requests every transaction's
  locks in that order (§3.2 invariant 1); the lock manager grants strictly in
  request order (invariant 2). That total order over *acquisition* is what
  matters: it cannot contain a wait cycle, so deadlock is impossible, and it
  makes every replica compute identical grants with zero communication. Plain
  2PL has neither property because it never pins the acquisition order — it
  can deadlock and its outcome depends on who races to the lock first.

  </details>

- [ ] You can explain why cross-shard reads are pushed rather than requested.

  <details><summary>Answer</summary>

  Because the fixed order plus the declared read/write sets tell every shard,
  in advance, exactly which of its local values the others will need — so a
  shard *forwards* (pushes) its local reads to the other participants and
  blocks for the pushes it expects (§3.2, phase 3). Nothing is requested
  mid-flight and nothing is voted on: each participant reaches the same
  commit/abort verdict deterministically, so "did it commit?" is a local
  theorem, not a network round.

  </details>

- [ ] You can explain why recovery is replaying inputs, and why replication is cheaper than shipping a write set.

  <details><summary>Answer</summary>

  Recovery is checkpoint + replay of the input log through the same
  deterministic machinery — no undo, no in-doubt transactions, because there
  is no coordinator state to lose (Step 5). Replication ships the compact
  *command log* (the transaction requests) instead of a physical WAL of every
  modified byte, so it trades network bytes for the CPU of re-executing every
  transaction at every replica (Q4).

  </details>

- [ ] You can state the catch — dependent transactions — and connect it to why graph traversals are the hard case for M29.

  <details><summary>Answer</summary>

  Deterministic locking needs the read/write set *before* execution, which
  interactive and *dependent* transactions cannot supply. OLLP (§3.2.1) works
  around it with a reconnaissance read to guess the set, then a re-check and
  retry — optimism that livelocks when the set keeps moving under contention
  (the θ=1.3 / 99.6% row). A graph traversal is the extreme dependent
  transaction: the read set *is* the query result, discovered only by walking,
  so the reconnaissance-vs-execution gap is maximal — that is why M29 is where
  the pattern strains.

  </details>

- [ ] You wrote answers to all six questions in notes.md.

  <details><summary>Answer</summary>

  Self-check: your Q3 answer should name a concrete moving-read-set workload
  and tie it to the θ=1.3 row of README §0; your Q5 answer should identify
  FoundationDB's global-version sequencer as the "fix the order first" half of
  Calvin (the resolution step is the other half); Q6 should propose an
  invalidation predicate for "did the traversal's frontier move?".

  </details>

## References

**Papers**
- Thomson, Diamond, Weng, Ren, Shao, Abadi — "Calvin: Fast Distributed
  Transactions for Partitioned Database Systems" (SIGMOD 2012) — §3 is the
  architecture and the deterministic locking; §3.2.1's OLLP is the answer to
  dependent transactions; §6.1 has the TPC-C throughput numbers

**Code**
- No reference implementation to clone — the lineage lives on in FaunaDB
  and in Abadi's deterministic-database papers
