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

Kafka's performance design is to *not have one*: writes are sequential
appends (the fastest thing a disk does — topic 0's ~100× sequential vs
random gap), there is no in-process message cache (the OS page cache
already caches the segment files — topic 6's "don't fight the OS"
lesson, chosen deliberately), and delivery to consumers uses
**sendfile** (a zero-copy syscall that moves bytes from file to socket
without passing through userspace). The consequence that matters
downstream: a log this cheap can retain days of history, which is what
makes Step 2's "replay from anywhere" economical rather than theoretical.

### Step 4 — ordering per partition only

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

With a dumb broker, delivery guarantees degrade to one question: **where
do you store your consumed offset, and is that store transactional with
your output?** Offset stored before processing → at-most-once (crash
loses a message); after → at-least-once (crash duplicates). The only
real "exactly-once" is consumer-side: commit the offset *atomically
with* the derived output — an idempotent or transactional sink.
RisingWave's barrier checkpoint is exactly this recipe (offsets stored
IN the same checkpoint as operator state — question 1); so is every
"exactly-once" system you'll meet.

### Step 6 — log compaction: the log becomes a table changelog

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

- **§3 (architecture + storage)** — Steps 1–3: segment files,
  offset-as-identity, page cache + sendfile. Notice what is *absent*: no
  broker index, no message cache, no ack bookkeeping.
- **§3.2 (consumer)** — Step 2: the pull model and consumer-held
  offsets; §4's delivery-semantics discussion is Step 5 in 2011
  vocabulary (compaction, Step 6, came later — read its design in the
  Kreps blog).
- **§4–5 (coordination + numbers)** — Step 4's per-partition ordering
  and the throughput comparisons; the numbers are dated, the ratios
  (sequential append vs per-message ack) aren't.

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

## References

**Papers**
- Kreps, Narkhede, Rao — "Kafka: a Distributed Messaging System for
  Log Processing" (NetDB 2011) — 7 pages, read whole
- Kreps — "The Log: What every software engineer should know about
  real-time data's unifying abstraction" (2013 blog) — the ideology;
  read after the paper
