# Topic 5 notes — durability, WAL, crash recovery

## Baseline (provided lane, Apple M3 Pro / APFS, measured 2026-07-28)

`cargo run --release --bin fsync_ladder`. macOS, so the rungs are `write()`,
`fsync`, and `F_FULLFSYNC` (there is no `fdatasync`; the bench compiles the
Linux rung out).

| rung | p50 | p99 | p99.9 | implied max commits/s |
|---|---|---|---|---|
| `write()` only | 1.17 µs | 4.54 µs | 14.46 µs | 856 898 |
| `fsync` | 22.67 µs | 56.73 µs | 157.06 µs | 44 109 |
| `F_FULLFSYNC` | **2.97 ms** | 3.61 ms | 9.89 ms | **337** |

**Three rungs, a 2540× spread in the last column, and only the bottom one is
actually durable on this hardware.** The middle rung is the trap: `fsync` on
macOS returns once the data reaches the *drive*, not once the drive has
committed it to stable media — the write can still be sitting in the disk's
volatile cache. `F_FULLFSYNC` is what forces a cache flush, and it costs 131×
more than the `fsync` that most code calls and believes.

337 commits/s is the number to keep. Any single-threaded design that fsyncs per
transaction is capped there regardless of how fast the rest of the engine is,
which is why group commit is not an optimization but a structural requirement —
and why topic 15's follower-fsync table looks the way it does.

## Predictions (fill BEFORE running fsync_ladder)

| Rung | Predicted p50 | Measured p50 | Measured p99 |
|---|---|---|---|
| `write()` only | | | |
| `fdatasync` | | | |
| `fsync` (macOS — weak!) | | | |
| `F_FULLFSYNC` | | | |

Predicted max commits/s at 1 fsync/commit: ______
Predicted group-commit speedup at batch 64: ______

## fsync_ladder results

(paste table from `cargo run --release --bin fsync_ladder`)

Surprises vs predictions:

## WAL design decisions (src/wal.rs)

- Page images vs logical records — chose: ______ because:
- Group-commit trigger (size / time / both): ______
- Why replay needs no LSN-idempotence check here (and when it would):

## crash_test log

- Rounds passed: ___/100
- Failures seen while developing (torn tail? lost ack? partial txn?) and the
  bug behind each:

## commit_throughput results

| Policy | commits/s | durability window |
|---|---|---|
| fsync per commit | | 0 |
| group 8 | | 0 |
| group 64 | | 0 |
| group 512 | | 0 |

## Reading-guide questions

### postgres xlog (reading-postgres-xlog.md)
1. Why xl_prev when reading forward:
2. FPI sawtooth formula in (dirty rate, checkpoint interval):
3. Raising NUM_XLOGINSERT_LOCKS — when, and the flush-time cost:

### turso WAL (reading-turso-wal.md)
1. Page images vs deltas — two buys, one cost:
2. The failure salts catch that checksums alone miss:
3. My experiment's format choice + justification:

### redis AOF/RDB (reading-redis-aof-rdb.md)
1. everysec loss window + the write-postpone logic:
2. AOF-as-LSM mapping + rewrite write amp:
3. Command-log vs page-image vs logical-record ranking for graph mutations:

### ARIES (reading-aries.md)
1. Why CLRs are redo-only (crash-during-undo walkthrough):
2. Nested top action for a B-tree split — why correct AND necessary:
3. My topic-3 B+tree + WAL: steal? force? ⇒ which passes needed:

Steal/force 2×2 (from memory):

| | force | no-force |
|---|---|---|
| **no-steal** | undo: __ redo: __ | undo: __ redo: __ |
| **steal** | undo: __ redo: __ | undo: __ redo: __ |

### Aether (reading-aether.md)
1. Why ELR preserves durability for dependents:
2. The ELR hazard (non-logging escape channel):
3. Consolidation array vs postgres's 8 insert locks:
4. Which bottleneck my M5 design leaves unfixed, and at what commits/s it bites:

## M5 log (capstone)

- [ ] WAL + recovery for graph mutations behind the storage trait
- [ ] crash_test harness pointed at the graph — rounds: ___/100
- [ ] Contrast vs FalkorDB-on-redis: durability window of RDB-only, RDB+AOF
      everysec, AOF always:
