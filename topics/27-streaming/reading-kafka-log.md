# Kafka: the log is the database

Before any view can be maintained incrementally, the changes have to
live somewhere with the right guarantees — and Kafka is the industry's
answer. This chapter builds the log abstraction step by step — what a
log is, why the broker keeps no consumer state, why per-partition
ordering suffices, how compaction turns a log into a table — then hands
you the 2011 paper (whose design bets are all still load-bearing) and
Kreps' "the log is the database" ideology, the substrate every IVM
system in this topic tails.

## The problem in one sentence

Every system in this topic consumes *changelogs* and maintains *derived
state* — so somewhere a changelog must live that many independent
consumers can read at their own pace, replay from any point, and trust
the ordering of; Kafka's 2011 answer was an append-only file per
partition and **zero broker-side per-consumer state**, and it hasn't
changed since.

## The concepts, step by step

### Step 1 — the log: an append-only sequence where position is identity

> **In:** a stream of records to persist for many readers. **Out:** an
> append-only partition file in which each record's identity is its
> **offset** (its position) — no per-message id, no broker-side index,
> no mutation.

A log is a file (conceptually) that is only ever appended to, where each
record's identity is simply its **offset** — its position in the
sequence. No per-message IDs, no broker-side index, no mutation:

```
  topic ─ partition 0:  [ append-only segment files ]  ← offset = position
        ─ partition 1:  [ ... ]                          (no per-message id,
                                                          no broker index!)
```

A **topic** is a named stream; each topic is split into **partitions**,
and each partition is one such log (stored as a chain of segment files).
This is topic 5's WAL — the append-only record of changes every database
already keeps — promoted from implementation detail to the *product*.
The payoff of position-as-identity: "where was I?" is a single integer,
which Step 2 turns into the whole consumer model.

### Step 2 — dumb broker, smart consumer

> **In:** many independent consumers reading the same partition at
> different rates. **Out:** a broker that stores no per-consumer state — a
> consumer *is* a `(partition, offset)` pair it holds itself; rewind and
> replay are just resetting that integer.

The broker keeps NO per-consumer state: a consumer *is* a
(partition, offset) pair, stored by the consumer itself, and "consume"
means "read forward from my offset." Rewind = set the integer back;
replay = free; a new consumer bootstrapping a fresh derived view = read
from offset 0. Contrast every prior message queue, where acking each
message *mutated broker state* — per-message bookkeeping that made
replay impossible and the broker the bottleneck. This one decision is
what makes the log a substrate for IVM: Materialize sources, RisingWave
sources, Debezium CDC — all are just consumers with offsets, and the
broker doesn't know or care how many there are.

### Step 3 — the mechanical bet: sequential IO and the OS page cache

> **In:** the need to serve high-volume reads and writes off disk
> cheaply. **Out:** sequential appends, no in-process message cache (lean
> on the OS page cache), and `sendfile` zero-copy delivery — cheap enough
> to retain days of history so replay stays economical.

Kafka's performance design is to *not have one*: writes are sequential
appends (the fastest thing a disk does — topic 0's ~100× sequential vs
random gap), there is no in-process message cache (the OS page cache
already caches the segment files — topic 6's "don't fight the OS"
lesson, chosen deliberately), and delivery to consumers uses
**sendfile** (a zero-copy syscall that, the paper notes in §3.1, "avoids
2 of the copies and 1 system call" of the four copies and two syscalls a
naive send would make). The consequence that matters downstream: a log
this cheap can retain days of history — the paper's retention SLA is
"typically 7 days" (§3.1) — which is what makes Step 2's "replay from
anywhere" economical rather than theoretical.

### Step 4 — ordering per partition only

> **In:** the question of how much ordering a maintained view needs.
> **Out:** order guaranteed *within* a partition and nothing across
> partitions — route each key to a fixed partition, so per-partition
> order is per-key order, which is all correctness requires.

Kafka guarantees order *within* a partition and nothing across
partitions — because a total order across partitions would cost
coordination (topic 15), and state maintenance doesn't need it: what
must not reorder is updates to the *same key* (apply delete-then-insert
backwards and the key resurrects), so route each key to a fixed
partition and per-partition order is per-key order. The Z-set view makes
the sufficiency precise: merges of deltas for *different* keys commute
anyway. This is the same "how much ordering do you actually need?"
question topic 15 asks of replication, answered minimally.

### Step 5 — delivery semantics: it's all about where the offset lives

> **In:** a consumer that can crash mid-processing. **Out:** the delivery
> guarantee — at-most-once, at-least-once, or exactly-once — determined
> solely by where the consumed offset is stored and whether that store
> commits atomically with the output.

With a dumb broker, delivery guarantees degrade to one question: **where
do you store your consumed offset, and is that store transactional with
your output?** Offset stored before processing → at-most-once (crash
loses a message); after → at-least-once (crash duplicates). Kafka's own
default is the middle one — the paper states plainly that "Kafka only
guarantees at-least-once delivery," since "exactly-once delivery
typically requires two-phase commits" (§3.3). The only real
"exactly-once" is consumer-side: commit the offset *atomically with* the
derived output — an idempotent or transactional sink. RisingWave's
barrier checkpoint is exactly this recipe (offsets stored IN the same
checkpoint as operator state — question 1); so is every "exactly-once"
system you'll meet.

### Step 6 — log compaction: the log becomes a table changelog

> **In:** a topic whose time-based retention would discard history a new
> consumer still needs. **Out:** compaction that keeps the *latest record
> per key* (plus tombstones for deletes), turning the topic into a table
> changelog a late-joining consumer can bootstrap a full table from.

Retention by time throws away history a new consumer needs; **log
compaction** instead retains *the latest record per key*, turning a
topic into a table changelog that a late-joining consumer can bootstrap
a full table from — read compacted-prefix, then follow the live tail.
The same operation appears in three communities: an arrangement's
`advance`/consolidation (differential guide, Step 2 there), an LSM's
tombstone GC (topic 4), and this — keep enough per key to reconstruct
the present, discard superseded history. One extra obligation the others
don't have: deletes must remain visible as **tombstones** (a retained
"key X was deleted" record) for a grace period, so late consumers learn
about the deletion at all (question 2).

### Step 7 — the ideology: turn the database inside out

> **In:** the classic stack, app → DB → CDC → caches. **Out:** the log is
> the database and tables are caches of log prefixes — write to the log
> first and derive *everything*, the DB included, as consumers.

Kreps' thesis, distilling the paper: **the log is the database; tables
are caches of log prefixes.** Instead of app → DB → CDC → caches, write
to the log first and derive *everything* — the DB included — as
consumers. Every IVM system in this topic assumes this architecture; the
rosetta makes the claim concrete:

| Kafka | database internals |
|---|---|
| partition | WAL shard / redo stream |
| offset | LSN |
| consumer group rebalance | replica assignment (topic 15) |
| log compaction | checkpoint + WAL truncation, per key |
| retention window | how far behind a replica may fall before full resync (PSYNC backlog, topic 15) |
| topic with schema registry | the WAL made a public, typed API |

The classical guarantee that gets harder inside-out: read-your-writes —
the deriving views lag the log, and a client that just wrote may query a
view that hasn't caught up (question 3 asks which system in
reading-materialize-risingwave.md fixes that with timestamps).

## How to read the paper (with the concepts in hand)

The paper is 7 pages — read the whole thing, watching for the four bets:

- **§3.1 (efficiency on a single partition)** — Steps 1, 3: segment
  files, offset-as-identity, page cache + `sendfile`, the "stateless
  broker" decision (the offset is held by the consumer, not the broker),
  and the "typically 7 days" retention SLA. Notice what is *absent*: no
  broker index, no message cache, no ack bookkeeping.
- **§3.2 (distributed coordination)** — Step 2/Step 4: consumer groups,
  ZooKeeper-mediated offset ownership across many consumers, and why
  ordering is per-partition.
- **§3.3 (delivery guarantees)** — Step 5: the paper commits only to
  at-least-once and explains why exactly-once is left to the consumer
  side. (Compaction, Step 6, is *not* in this paper — it came later;
  read its design in the Kreps blog.)
- **§5 (experimental results)** — the throughput comparison (e.g. a
  producer sustaining ~50,000 msg/s at batch size 1 and ~400,000 msg/s
  at batch size 50 against ActiveMQ); the numbers are dated, the ratios
  (sequential append vs per-message ack) aren't. §4 is LinkedIn
  deployment context, not the mechanics.

Then the Kreps blog ("The Log", 2013) — the ideology of Step 7, read
after the paper so the architecture claims have mechanics under them.

## Questions to answer in notes.md

1. Consumer-side offset + idempotent/transactional sink = the only real
   "exactly-once." Map RisingWave's barrier checkpoint (offsets stored IN
   the same checkpoint as operator state) onto this recipe. What plays
   the role of the transactional sink?
2. Log compaction (retain latest record per key) turns a topic into a
   *table changelog* that new consumers can bootstrap from. Compare to an
   arrangement's `advance`/consolidation (differential guide Step 2) and an
   LSM's tombstone GC (topic 4): same operation, three communities. What
   must a compacted topic keep that an LSM needn't? (Hint: deletes need
   tombstones readable by late-joining consumers for a grace period.)
3. "Turning the database inside out": instead of app → DB → CDC → caches,
   write to the log first and derive EVERYTHING (DB included). What
   classical guarantee gets harder in the inside-out design?
   (Read-your-writes: the deriving views lag the log.) Which system in
   reading-materialize-risingwave.md solves that with timestamps, and how?
4. **(M27)** FalkorDB already has the log (Redis replication / AOF,
   topic 5's guide). A standing-query subscriber is a consumer of *view
   deltas*. Decide: do subscribers get (a) the raw mutation log (Kafka
   style — they rebuild), or (b) per-query result deltas (Materialize
   SUBSCRIBE style)? What does (b) require the server to persist if a
   subscriber disconnects for an hour — and where's the retention-window
   trade from Step 6 hiding in your answer?

## Done when

Answer each before unfolding it.

- [ ] Why is position identity in an append-only log?
  <details><summary>answer</summary>
  Records are only ever appended, so a record's offset (its position in
  the partition) never changes and uniquely names it. No separate
  per-message id or broker-side index is needed — "where was I?" is one
  integer.
  </details>
- [ ] Explain the dumb-broker/smart-consumer split and what it moves to the client.
  <details><summary>answer</summary>
  The broker stores no per-consumer state (§3.1 "stateless broker"); the
  consumer holds its own `(partition, offset)`. This moves progress
  tracking, rewind, and replay to the client and lets any number of
  independent consumers read the same log without broker bookkeeping.
  </details>
- [ ] State the mechanical bet: sequential IO plus the OS page cache.
  <details><summary>answer</summary>
  Writes are sequential appends; there is no in-process message cache
  (the OS page cache serves segment files); delivery uses `sendfile`,
  which the paper says avoids 2 of 4 copies and 1 of 2 syscalls (§3.1).
  Cheap enough to retain ~7 days of history.
  </details>
- [ ] Why is ordering per partition only, and what does that forbid?
  <details><summary>answer</summary>
  A total order across partitions would need coordination and isn't
  required: correctness only needs same-key updates not to reorder. Route
  each key to a fixed partition and per-partition order is per-key order.
  It forbids relying on a global order across keys/partitions.
  </details>
- [ ] Where does the offset live for each delivery semantic?
  <details><summary>answer</summary>
  Store the offset before processing → at-most-once; after → at-least-once
  (Kafka's own guarantee, §3.3). Exactly-once requires committing the
  offset atomically with the output (idempotent/transactional sink) — a
  consumer-side property, not a broker one.
  </details>
- [ ] Explain log compaction as turning a topic into a table changelog.
  <details><summary>answer</summary>
  Compaction retains the latest record per key (with tombstones for
  deletes for a grace period), so a late consumer can read the compacted
  prefix to reconstruct the current table, then follow the live tail.
  (This is a post-2011 feature — see the Kreps blog, not the paper.)
  </details>
- [ ] You wrote answers to all questions in notes.md, including what FalkorDB's existing log already gives M27.
  <details><summary>answer</summary>
  FalkorDB already has a log (Redis replication / AOF, topic 5). The open
  choice for M27 is whether standing-query subscribers consume the raw
  mutation log (Kafka-style rebuild) or per-query result deltas
  (Materialize SUBSCRIBE-style), and what the server must persist for a
  disconnected subscriber — the Step 6 retention-window trade.
  </details>

## References

**Papers**
- Kreps, Narkhede, Rao — "Kafka: a Distributed Messaging System for
  Log Processing" (NetDB 2011) — 7 pages, read whole
- Kreps — "The Log: What every software engineer should know about
  real-time data's unifying abstraction" (2013 blog) — the ideology;
  read after the paper
