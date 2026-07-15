# Redis AOF & RDB: the command stream is the log

Redis logs the *commands themselves* (AOF) and checkpoints by *forking* (RDB)
— and since a graph module's data lives inside redis's keyspace, this is the
durability FalkorDB actually has today. Before the code, this chapter builds
the design step by step: what a command log is, what the fsync policy knob
really promises, why a command log must be rewritten, and how fork+COW turns
the OS into a snapshot engine. Read it as the incumbent your M5 design
competes with.

## The problem in one sentence

Redis serves ~100K+ commands/s from one thread, so it cannot afford either
an fsync per command (~1 ms each would cap it at ~1K/s) or any pause to
write a snapshot — its durability design is entirely shaped by "the main
thread must never wait for the disk," and the price is a stated window of
acknowledged-but-lost writes.

## The concepts, step by step

### Step 1 — the command log: log what was *said*, not what changed

An AOF ("append-only file") is a **command log**: instead of logging page
images (turso) or record deltas (postgres), redis appends the write
commands themselves — literally as RESP protocol text, the same bytes a
client would send. `SET user:42 "avi"` goes into the log as `SET user:42
"avi"`. Recovery is replay: start an empty server, feed it the file as if a
very fast client were retyping history. The trade is volume-vs-CPU flipped
from the other designs: a command is usually tiny (tens of bytes — the
cheapest possible log record), but replay must re-execute *full command
processing* — parsing, dispatch, data-structure updates — so recovery time
scales with total command count, not with final data size. Every write
command is appended to an in-memory buffer (`server.aof_buf`) during
command execution; what happens to that buffer next is the whole durability
story.

### Step 2 — the fsync policy: durability as a config knob

Once per event-loop iteration the buffer is `write()`n to the AOF file —
which only reaches the kernel's page cache, not the disk (topic README §3:
the kernel lies until fsync). *When to fsync* is a user-facing policy, and
this is the design's signature: redis makes the durability window a
**config choice** the other systems don't offer:

- **`always`** — fsync before processing more commands. Durable, and slow:
  the main thread eats ~1 ms per batch.
- **`everysec`** — fsync issued at most once per second, **on a background
  thread** (the "bio" thread): the main thread never touches the disk.
  Window: up to ~2 s of *acknowledged* writes can vanish.
- **`no`** — the kernel flushes when it feels like it. Window: unbounded
  (typically ~30 s).

The three contracts, side by side:

```rust
// flushAppendOnlyFile: the client was ACKED before any of this runs.
fn flush_aof(&mut self, policy: Fsync) {
    self.file.write_all(&self.aof_buf);          // into page cache only
    self.aof_buf.clear();
    match policy {
        Fsync::Always => self.file.fdatasync(),  // durable before next ack: slow
        Fsync::EverySec => {
            if self.last_fsync.elapsed() >= Duration::from_secs(1) {
                self.bio.submit(FsyncJob);       // background thread — main
            }                                    // thread never touches the disk;
        }                                        // window: up to ~2 s of ACKED writes
        Fsync::No => {}                          // kernel decides; window unbounded
    }
}
```

Sharpen the comparison with postgres: group commit *batches the flush but
never acks early* — the client waits until its LSN is durable. Everysec is
group commit with a *time*-based batch and the **ack before the flush** —
a different contract, not just different tuning.

### Step 3 — the rewrite problem: command logs grow with history, not with data

A command log's size is proportional to *everything ever said*, not to the
data: 1M `INCR counter` commands is 1M log records describing one 8-byte
value. So the AOF must periodically be **rewritten** — replaced by the
shortest command sequence that reconstructs the *current* state (one `SET
counter 1000000`). Redis does this without pausing: **fork** the process
(the OS clones it; both copies share memory copy-on-write — see Step 5),
let the child serialize current state into a fresh **BASE** file at its
leisure, while the parent keeps serving and appends new commands to a new
**INCR** file. When the child finishes, BASE + INCR replace the old log.

### Step 4 — multi-part AOF: an LSM in disguise

The modern (7.0+) AOF is not one file but a set — a **manifest** file lists
one BASE file plus one or more INCR files; recovery loads the BASE, then
replays the INCRs in order. Squint and this is topic 4 wholesale: BASE =
the bottom level (a compacted, sorted-out rendering of all history), INCR
files = L0 (recent appends), rewrite = full compaction, manifest = the
MANIFEST. Even the write amplification question transfers: a rewrite's cost
is (entire dataset serialized) per (INCR data absorbed) — exactly a
full-compaction WA. Topic 4's vocabulary was never LSM-specific; it's the
vocabulary of *any* log that must be compacted.

### Step 5 — RDB: checkpoint by fork, priced in COW

An RDB snapshot is durability by checkpoint alone: fork, and let the child
walk the entire keyspace writing a compact binary snapshot (with a CRC64
trailer — a 64-bit checksum over the file), while the parent serves
traffic. Correctness is delegated to the OS: **copy-on-write** (COW) means
parent and child share all memory pages until the parent *writes* one, at
which point the kernel copies that 4 KB page — so the child sees a frozen
instant of the keyspace for free. The price is paid in page copies under
write load: a write-hot parent duplicates its working set, and worst case a
multi-GB dataset approaches 2× RAM during the snapshot. This cost reaches
all the way down into data-structure design: topic 2's dict *disables
rehashing during BGSAVE* (dict.c:1655), because a rehash touches every
bucket and would COW-copy the whole table. Durability window with RDB
alone: everything since the last snapshot — minutes.

### Step 6 — the FalkorDB angle: this is the incumbent

A graph module's data lives inside redis's keyspace, so its durability *is*
this file: RDB serializes the matrices via module callbacks, and AOF logs
the `GRAPH.QUERY` commands themselves. Two consequences to quantify in
notes (they are the M5 comparison baseline): replaying `GRAPH.QUERY`
commands re-executes *parsing and planning* per command — estimate recovery
time for 10M mutations vs replaying logical records; and an RDB snapshot of
a multi-GB graph forks + COWs the whole matrix set under write load —
measure-or-estimate the stall and the memory spike.

## Where each step lives in the code

- **Step 1 — `aof.c:1409–1448`**: `feedAppendOnlyFile` — every write
  command appended (as RESP text!) to `server.aof_buf` (:1444).
- **Step 2 — `aof.c:1147–1355`**: `flushAppendOnlyFile` — buffer → write()
  (:1218), then the policy (:1330–1354): `AOF_FSYNC_ALWAYS` (:1337);
  `AOF_FSYNC_EVERYSEC` (:1350) — fsync on the bio background thread
  (`aof_background_fsync`, :983); `AOF_FSYNC_NO`. The "postpone" logic near
  :1147 delays *writes* when the bio fsync falls behind (question 1).
- **Steps 3–4 — `aof.c`**: `rewriteAppendOnlyFileBackground` :2652–2720 —
  fork at :2689; the child serializes a fresh BASE, the parent accumulates
  a new INCR. Multi-part AOF manifest: aof.c:45–71.
- **Step 5 — `rdb.c`**: `rdbSaveBackground` :1859–1892 — fork (:1868),
  child walks the keyspace; CRC64 trailer rdb.c:1702–1706. The
  rehash-disable during BGSAVE: dict.c:1655.

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

## References

**Code**
- [redis](https://github.com/redis/redis) — `src/aof.c` (feed, flush
  policies, rewrite, multi-part manifest) and `src/rdb.c` (fork + COW
  snapshot, CRC64 trailer). Local clone at `~/repos/redis`.
