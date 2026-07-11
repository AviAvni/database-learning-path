# Topic 5 notes — durability, WAL, crash recovery

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
