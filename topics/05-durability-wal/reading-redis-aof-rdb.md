# Reading redis `aof.c` / `rdb.c` — durability, FalkorDB edition (1.5 h)

Repo: `~/repos/redis`. This is the durability FalkorDB actually has today —
read it as the incumbent your M5 design competes with.

## 1. AOF = the command stream is the log

- `feedAppendOnlyFile` — aof.c:1409–1448: every write command appended (as
  RESP text!) to `server.aof_buf` (:1444).
- `flushAppendOnlyFile` — aof.c:1147–1355: buffer → write() (:1218), then the
  policy (:1330–1354):
  - `AOF_FSYNC_ALWAYS` (:1337) — fsync before ack. Durable, slow.
  - `AOF_FSYNC_EVERYSEC` (:1350) — fsync on the **bio background thread**
    (`aof_background_fsync`, :983): main thread never blocks on the disk;
    window = up to ~2s of acked writes.
  - `AOF_FSYNC_NO` — kernel decides. Window = unbounded.
- Group commit comparison: everysec is group commit with a *time* batch and
  the ack BEFORE the flush — postgres groups the flush but never acks early.
  Different contract, not just different tuning.

## 2. AOF rewrite — compacting a command log

A command log grows without bound (1M INCRs = 1M records for one key).
- `rewriteAppendOnlyFileBackground` — aof.c:2652–2720: **fork** (:2689); the
  child serializes current state as a fresh BASE file; the parent keeps
  serving and accumulates new commands into a new INCR file.
- Multi-part AOF (aof.c:45–71): a **manifest** lists BASE + INCR files —
  recovery = load BASE, replay INCRs. This is an LSM in disguise: BASE = the
  bottom level, INCR = L0, rewrite = full compaction, manifest = MANIFEST.
  (Topic 4's vocabulary transfers wholesale.)

## 3. RDB — checkpoint by fork

- `rdbSaveBackground` — rdb.c:1859–1892: fork (:1868), child walks the
  keyspace and writes the snapshot; parent's writes COW pages away. CRC64
  trailer (rdb.c:1702–1706).
- The COW cost is why topic-2's dict disables rehashing during BGSAVE
  (dict.c:1655) — a rehash would touch every bucket and copy the whole table.
  Durability policy reaching down into data-structure design.

## 4. The FalkorDB angle (write this up in notes)

A graph module's data lives inside redis's keyspace, so its durability *is*
this file: RDB serializes matrices via module callbacks; AOF logs the
GRAPH.QUERY commands. Questions that matter for M5:
- Replaying `GRAPH.QUERY` commands re-executes *parsing and planning* — what's
  recovery time for 10M mutations vs replaying logical records?
- An RDB snapshot of a multi-GB graph forks + COWs the whole matrix set under
  write load — measure-or-estimate the stall.

## Questions to answer in notes.md

1. everysec acks before durability. State the exact loss window and why redis
   considers delaying *writes* (not acks) when the bio fsync falls behind
   (:1147 area — the "postpone" logic).
2. AOF-as-LSM: map BASE/INCR/rewrite/manifest onto topic-4 terms. What's the
   "write amp" of an AOF rewrite?
3. Command-log vs page-image vs logical-record WAL: rank recovery speed and
   log volume for a graph-mutation workload; justify your M5 choice.

## Done when

You can state each appendfsync policy's durability window from memory and
explain AOF rewrite as compaction.
