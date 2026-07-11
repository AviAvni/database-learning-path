# Reading guide — Aether: A Scalable Approach to Logging (VLDB 2010)

Johnson, Pandis, Stoica, Athanassoulis, Ailamaki. ~12 pages. Budget 2 h.
The question: on a multicore, the log is ONE shared object every transaction
must append to and flush — how does it not become the bottleneck?

## The four bottlenecks (§1–2)

```
 txn commits ──► [A] contend on log-buffer insert   (one mutex around append)
             ──► [B] wait for fsync                  (I/O latency per commit)
             ──► [C] hold locks WHILE waiting for [B] (lock contention amplified)
             ──► [D] context switches around the wait
```

Aether attacks each separately — read the paper as four independent fixes
that compose:

| Bottleneck | Fix | Modern descendant |
|---|---|---|
| B: fsync per commit | group commit | postgres `XLogFlush` recheck |
| C: locks held across flush | **Early Lock Release** (ELR) | controversial; see Q2 |
| D: scheduling | flush pipelining (async commit queues) | redis everysec (cruder) |
| A: buffer insert mutex | **consolidation array** | postgres reserve-then-copy |

## 1. Early Lock Release (§3)

Release locks at commit-record *creation*, not commit-record *durability*.
Dependent transactions may read your uncommitted-but-logged data — safe IF
they can't acknowledge before you do (their commit record follows yours in the
log, so log order enforces the dependency). Serial log = free dependency
tracking.

## 2. Flush pipelining (§4)

Worker threads never block on fsync: they enqueue the commit and *detach*,
picking up new work; a daemon acks completed commits after the flush lands.
Throughput of async commit, durability of sync commit — the cost is latency
and a more complex scheduler, not a loss window.

## 3. Consolidation arrays (§5) — the part that shipped everywhere

The insight: even with group commit, every append still serializes on the
buffer mutex. Fix: threads *combine* their requests before hitting the lock.

```
 naive:      T1 ─lock─ memcpy ─unlock─ T2 ─lock─ memcpy ─unlock─ T3 …
 consolidated:
   T1,T2,T3 meet in a slot array, add up sizes (CAS, no lock),
   ONE of them acquires the lock, reserves sum(bytes) once,
   each thread memcpys into its own slice IN PARALLEL.
```

Decouples *sequencing* (must be serial, make it tiny) from *copying* (can be
parallel, make it so). Postgres's `ReserveXLogInsertLocation` (spinlock for 3
arithmetic ops) + 8 parallel insertion locks is this idea in production —
read reading-postgres-xlog.md §2 side by side with §5.

## Read in this order

1. §1–2 for the bottleneck taxonomy (the table above).
2. §5 consolidation arrays — the durable contribution.
3. §3 ELR — for the *argument* about log order as dependency tracking.
4. Skim §4 + evaluation (§6): note which fix buys what at which core count.

## Questions to answer in notes.md

1. Why does ELR NOT violate durability for the *dependent* transaction?
   (Its commit record is behind yours; a crash that loses yours loses its too.)
2. ELR hazard: what if the dependent txn's result escapes to the user by a
   channel other than its own commit ack (e.g. a read-only txn that never
   logs)? This is why real systems mostly didn't ship it.
3. Consolidation vs postgres's 8 insert locks: both parallelize the copy —
   what's the difference in HOW threads find a slot? (CAS-combining into a
   shared slot vs hashing onto a fixed lock array; contrast under 8 vs 80
   writers.)
4. Which bottleneck does your M5 group-commit design leave unfixed? (Likely A
   — a single mutex around the WAL buffer is fine at graph-workload commit
   rates; say at what commits/s it wouldn't be.)

## Done when

You can name the four bottlenecks from memory, sketch a consolidation array,
and point at the postgres code that embodies it.
