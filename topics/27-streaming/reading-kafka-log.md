# Kafka: the log is the database

Before any view can be maintained incrementally, the changes have to
live somewhere with the right guarantees — and Kafka is the industry's
answer. This chapter reads the 2011 paper's design bets (all still
load-bearing) and Kreps' "the log is the database" ideology as the
substrate every IVM system in this topic tails.

## 1. Why this paper is in the IVM topic

Every system in this topic consumes *changelogs* and maintains *derived
state*. Kafka is the answer to "where does the changelog live, and what
guarantees does it have?" The thesis (from the blog, distilling the
paper): **the log is the database; tables are caches of log prefixes.**
Which is topic 5's WAL rule promoted from implementation detail to
system architecture — postgres logical replication, Debezium CDC,
Materialize sources, RisingWave sources: all tail someone's WAL through
exactly this abstraction.

## 2. The paper's actual design bets (2011, all still load-bearing)

```
  topic ─ partition 0:  [ append-only segment files ]  ← offset = position
        ─ partition 1:  [ ... ]                          (no per-message id,
                                                          no broker index!)
```

- **Dumb broker, smart consumer**: the broker keeps NO per-consumer state;
  a consumer is (partition, offset). Rewind = replay = free. Contrast
  every prior MQ where acking mutated broker state per message.
- **Sequential I/O + page cache + sendfile**: no in-process message cache;
  zero-copy from segment file to socket. Topic 6's lesson (don't fight the
  OS cache) chosen deliberately.
- **Offsets as consumer-owned watermarks**: delivery semantics degrade to
  "where do you store your offset, and is that store transactional with
  your output?" — which is the whole exactly-once question.
- **Ordering per partition only**: total order costs coordination
  (topic 15); per-key order is what state maintenance actually needs
  (updates to the same key must not reorder — Z-set merges for DIFFERENT
  keys commute anyway).

**Q1.** Consumer-side offset + idempotent/transactional sink = the only
real "exactly-once." Map RisingWave's barrier checkpoint (offsets stored
IN the same checkpoint as operator state) onto this recipe. What plays
the role of the transactional sink?

**Q2.** Log compaction (retain latest record per key) turns a topic into
a *table changelog* that new consumers can bootstrap from. Compare to an
arrangement's `advance`/consolidation (differential guide §2) and an LSM's
tombstone GC (topic 4): same operation, three communities. What must a
compacted topic keep that an LSM needn't? (Hint: deletes need tombstones
readable by late-joining consumers for a grace period.)

## 3. The rosetta table

| Kafka | database internals |
|---|---|
| partition | WAL shard / redo stream |
| offset | LSN |
| consumer group rebalance | replica assignment (topic 15) |
| log compaction | checkpoint + WAL truncation, per key |
| retention window | how far behind a replica may fall before full resync (PSYNC backlog, topic 15) |
| topic with schema registry | the WAL made a public, typed API |

**Q3.** "Turning the database inside out": instead of app → DB → CDC →
caches, write to the log first and derive EVERYTHING (DB included). What
classical guarantee gets harder in the inside-out design?
(Read-your-writes: the deriving views lag the log.) Which system in
reading-materialize-risingwave.md solves that with timestamps, and how?

**Q4 (M27).** FalkorDB already has the log (Redis replication / AOF,
topic 5's guide). A standing-query subscriber is a consumer of *view
deltas*. Decide: do subscribers get (a) the raw mutation log (Kafka
style — they rebuild), or (b) per-query result deltas (Materialize
SUBSCRIBE style)? What does (b) require the server to persist if a
subscriber disconnects for an hour — and where's the retention-window
trade from §2 hiding in your answer?

## References

**Papers**
- Kreps, Narkhede, Rao — "Kafka: a Distributed Messaging System for
  Log Processing" (NetDB 2011) — 7 pages, read whole
- Kreps — "The Log: What every software engineer should know about
  real-time data's unifying abstraction" (2013 blog) — the ideology;
  read after the paper
