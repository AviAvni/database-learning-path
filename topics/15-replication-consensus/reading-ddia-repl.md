# Lag, lies, and linearizability

The concepts layer over this topic's code — Kleppmann's three
chapters give the vocabulary for everything valkey and Raft do. This
chapter builds that vocabulary step by step: what lag does to
readers (ch. 5), why partial failure and lying clocks make
distribution hard (ch. 8), and what linearizability and consensus
actually promise (ch. 9). Read ch. 5 alongside valkey's
`replication.c` and ch. 9 alongside the Raft paper; ch. 8 is the
connective tissue.

## The problem in one sentence

An async replica is always some milliseconds (or, during a
compaction stall, some *minutes*) behind its leader, and a client
whose reads land on different replicas can watch its own write
vanish, see time run backwards, or read an answer before its
question — unless the system promises one of a small set of
precisely-named guarantees, each with a price.

## The concepts, step by step

### Step 1 — replication lag: the gap between ack and everywhere

Replication lag is the delay between a write committing on the
leader and becoming visible on a given replica. Under async
replication (valkey's design, previous chapter) lag is unbounded by
construction — the ack never waited. Normal lag is milliseconds; the
tail is the problem: a replica doing a full resync, hitting disk, or
GC-pausing can lag minutes. Lag is invisible to anyone who only
talks to the leader; it becomes real the moment reads are scaled out
to replicas — which is the entire reason to have replicas. So the
question "what does a reader see?" needs a taxonomy — Step 2.

### Step 2 — the anomaly catalog: three ways lag bites readers

Each anomaly is a specific reader experience, and each has a
specific, priced fix:

```
 anomaly                fix
 ──────────────────────────────────────────────────────────
 read-your-writes       session stickiness, or read-after
   (I posted, refresh,    -my-offset (track repl offset per
    it's gone)            session — valkey WAIT-ish)
 monotonic reads        pin session to one replica
   (time goes backward
    across refreshes)
 consistent prefix      causally-ordered delivery (or
   (answer before         single-partition ordering)
    question)
```

Read them as contracts weaker than "no lag visible at all"
(linearizability, Step 5) but individually purchasable:
read-your-writes costs offset bookkeeping per session; monotonic
reads costs load-balancing freedom; consistent prefix costs ordering
machinery. Question per anomaly: which does our M15 stage-1 follower
exhibit, and what does the fix cost?

### Step 3 — what actually ships: statements, WAL bytes, or rows

Chapter 5's other half is the replication-log format menu, and this
topic implements two of the three:

- **Statement-based** — ship the commands; compact, but
  nondeterminism must be rewritten first. This is valkey
  (post-`propagateNow` — previous chapter).
- **Physical WAL** — ship the storage engine's own log bytes;
  deterministic by construction, but coupled to the engine version
  and page layout. This is our M15 stage 1.
- **Logical (row-based)** — ship "row X became Y"; decoupled from
  the engine, the format change-data-capture wants.

The tradeoff table maps onto topic 5's logging choices one-to-one.
The chapter's multi-leader and leaderless sections preview topic 31
(CRDTs) — skim them on this pass.

### Step 4 — partial failure: timeouts guess, clocks lie, tokens fence

Chapter 8 is one argument: in a distributed system you cannot
distinguish {slow node, dead node, slow network, lost packet} — all
four look like silence. Three consequences to extract:

- **Timeouts are the only failure detector**, and every timeout is a
  guess (our sim.rs makes this concrete: `election_timeout` ticks).
  Guess short and you declare live nodes dead; guess long and real
  failures stall the system.
- **Process pauses**: a GC or VM pause makes a live leader
  dead-then-alive — it wakes *believing it still leads*. The defense
  is a **fencing token**: a monotonically increasing number issued
  with each grant of authority, checked by everyone downstream, so
  the stale leader's older token is rejected. Raft terms ARE fencing
  tokens (question: walk how). valkey has nothing in this slot —
  hence split-brain during failover.
- **Clock skew**: wall clocks drift and jump, so "leader for the
  next 5 seconds" (a lease) requires bounded clock error, while
  ReadIndex (Step 5) needs no clock at all — it uses a message round
  instead of time.

### Step 5 — linearizability: the single-copy illusion, defined

Linearizability is the strongest single-object guarantee: the system
behaves as if there were exactly ONE copy of the data, with every
operation taking effect atomically at some instant between its start
and its ack. The test-worthy form: there exists a single total order
of operations, consistent with real time — once any read returns a
value, all later reads return it or newer.

The trap this topic keeps stepping on: Raft gives linearizable
WRITES, but reading from the leader without care is NOT linearizable
— a deposed leader partitioned from the majority can serve stale
reads while a new leader commits fresh writes (walk the timeline —
it's question territory). The fixes, priced per read: **ReadIndex**
(confirm leadership with a heartbeat round before serving: one
network round, no clock assumptions) or **leader leases** (serve
freely within a time window: free reads, but correctness now rests
on bounded clock error — Step 4's problem). Async replicas serve
stale reads by design; that's not a bug, it's the A in Step 6.

### Step 6 — CAP, consensus equivalence, and the FLP dodge

The closing vocabulary, three items:

- **CAP, properly**: during a network Partition, choose
  Available-but-stale or Consistent-but-unavailable on the minority
  side. valkey chose A; Raft chose C. Our
  `minority_partition_cannot_commit` test IS the C choice, executed
  — three nodes keep committing, two freeze.
- **Consensus ≡ atomic broadcast ≡ CAS**: ch. 9's equivalence
  proofs. Solve any one and you've solved the others — which is why
  "just use a CAS register" is not an escape from consensus.
- **FLP**: in a fully asynchronous system (no timing assumptions at
  all), no deterministic consensus protocol can be *guaranteed* to
  terminate. Raft's randomized timeouts are the practical dodge —
  termination with probability 1, not certainty — not a refutation.
  One-sentence version for question 4.

## How to read the chapters (with the concepts in hand)

- **Ch. 5 (Replication)** — Steps 1–3. Read the anomaly catalog
  slowly and the log-format section with valkey's `propagateNow`
  open ([reading-valkey-replication.md](reading-valkey-replication.md)
  Step 2 is the same fork). Skim multi-leader/leaderless — they
  return in topic 31.
- **Ch. 8 (The Trouble with Distributed Systems)** — Step 4. The
  chapter is long; extract exactly three things — timeouts as
  guesses, pauses + fencing tokens, clock skew vs leases — and move
  on.
- **Ch. 9 (Consistency and Consensus)** — Steps 5–6, with the Raft
  paper ([reading-raft-paper.md](reading-raft-paper.md)) beside it.
  The linearizability definition deserves a re-read until the
  deposed-leader timeline is obvious; the equivalence section can be
  read for the statements alone, proofs skimmed.

## Questions for notes.md

1. Build the 2×3 matrix: {async, semi-sync, raft} × {read-your-
   writes, monotonic reads, consistent prefix} — which combos hold?
2. A client's WAIT 1 returns success, then the primary dies and a
   NON-acked replica is promoted. Which ch. 5 guarantee broke, and
   which ch. 9 property would have prevented it?
3. Fencing tokens: sketch how M15's follower rejects a stale
   leader's WAL stream using terms.
4. Why does FLP not doom Raft in practice? One sentence.
5. Linearizable-read options: leader lease vs ReadIndex vs quorum
   read — cost per read of each, and which M22 (the capstone's
   read-path milestone) should pick.

## References

**Papers / Books**
- Kleppmann — "Designing Data-Intensive Applications" (O'Reilly
  2017) — ch. 5 (Replication), ch. 8 (The Trouble with Distributed
  Systems), ch. 9 (Consistency and Consensus); pair ch. 5 with
  [reading-valkey-replication.md](reading-valkey-replication.md) and
  ch. 9 with [reading-raft-paper.md](reading-raft-paper.md)
