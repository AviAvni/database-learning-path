# Topic 7 notes — networking, protocols & event loops

Predict FIRST, then measure. Numbers without predictions are just trivia.

## Predictions (fill in BEFORE running anything)

| Measurement | Prediction | Actual | Surprised? |
|---|---|---|---|
| redis-benchmark GET -P 1 vs real redis (yours/redis, req/s) | | | |
| redis-benchmark GET -P 64 vs real redis | | | |
| SET -P 64 (write lock on shard shows up?) | | | |
| -P 64 with SHARDS=1 (RwLock contention) | | | |
| Removing "flush only when drained" — -P 64 penalty | | | |
| Flamegraph top-3 under -P 64 | 1. 2. 3. | | |

Reasoning space (why did you predict those numbers?):
- At -P 1 the bottleneck is syscalls + RTT, not parsing — both servers should
  be within ~2× of each other.
- At -P 64 parse+execute per syscall dominates; where does the re-parse-from-
  buffer-start simplification in resp.rs cost show up, if anywhere?

## Bench protocol

```sh
# your server
cd experiments && cargo run --release --bin server
redis-benchmark -p 7379 -t get,set -n 1000000 -P 1
redis-benchmark -p 7379 -t get,set -n 1000000 -P 64

# real redis (brew services or redis-server), port 6379
redis-benchmark -p 6379 -t get,set -n 1000000 -P 1
redis-benchmark -p 6379 -t get,set -n 1000000 -P 64

# flamegraph (cargo install flamegraph; needs sudo for dtrace on macOS)
cargo flamegraph --release --bin server
# then drive load with redis-benchmark -P 64 from another terminal
```

Record: req/s for each cell, and the top-3 flamegraph entries with rough %.

## Questions — reading-redis-ae-networking.md

1. Why does the RESP parser never scan payload bytes? What property of the
   wire format makes that possible, and what does the inline protocol lose?
2. Trace one GET at the function level: aeApiPoll → readQueryFromClient →
   processInputBuffer → processMultibulkBuffer → addReply →
   handleClientsWithPendingWrites. Where is the syscall boundary crossed
   (exactly twice — where)?
3. What do multibulklen/bulklen (server.h:184–185) buy over re-parsing from
   the buffer start (what your resp.rs does)? Estimate the cost difference
   for a 1MB bulk arriving in 16KB reads.
4. PROTO_MBULK_BIG_ARG (:191): what copy does the zero-copy path avoid, and
   why does it only matter above 32KB?

## Questions — reading-valkey-iothreads.md

1. What exactly does an io thread own in valkey 8 (read side, write side),
   and what remains main-thread-only? Why is that split Amdahl-optimal for
   GET-shaped workloads?
2. How does the tagged-pointer inbox (untagJob :333) avoid a second queue —
   and where have you seen bit-smuggling like this before (topics 0, 6)?
3. memory_prefetch.c batches dict lookups to overlap cache misses. What is
   the equivalent MLP opportunity in a GraphBLAS BFS step?
4. Redo the Amdahl analysis for FalkorDB: GRAPH.QUERY spends how much in
   parse+I/O vs execution? Does io-threading the network layer move the
   needle at all for ms-scale queries?

## Questions — reading-pgwire-qdrant.md

1. Extended query protocol: what do Parse/Bind/Execute/Sync + portal
   max_rows give the client that RESP fundamentally cannot? (Hint: who
   controls the flow of a huge result set?)
2. RESP kills clients that exceed output-buffer limits; pgwire suspends
   portals. Which does FalkorDB inherit, and what does that mean for a
   query returning 10M nodes?
3. Why does qdrant run two tonic servers instead of one with auth
   middleware? When would M7 want the same split?

## Questions — reading-c10k-thread-per-core.md

1. Which C10K strategy is tokio's multi-thread runtime? (Two layers —
   name both.)
2. A graph database's unit of work is a query (ms), not a GET (µs). Where
   do threads belong: network layer (M7) or executor (M9)? Argue with the
   valkey numbers.
3. Thread-per-core sharding unit for a graph: graph? subgraph? matrix
   tile? What does a BFS crossing shards cost in messages?
4. io_uring: what changes in ae.c's design if poll+read+write become
   submission-queue entries?

## Design decisions (record as you implement resp.rs)

- Incomplete-input handling: peek-then-commit or re-parse from start? What
  state would you keep to make it O(new bytes) instead of O(buffered)?
- Where does Value allocate? Count allocations for `*2 $3 GET $3 foo` —
  redis does this with zero per-arg heap allocations at steady state (sds
  reuse); how many do you do?
- Error strategy: why kill the connection on protocol error instead of
  resyncing? (What would resync even mean without a framing marker?)

## Threading-model placement (do after all readings)

Place on the shared↔sharded / loop↔threads plane:

```
                 single loop          io threads           thread-per-core
shared keyspace  redis ≤6             valkey 8              (contention!)
sharded keyspace   —                    —                  DragonflyDB/Scylla
```

Where does M7 sit, and why? (Consider: matrices don't hash-partition,
queries are ms-scale, GraphBLAS wants big parallel sections in the
EXECUTOR not the network layer.)

## M7 log (capstone milestone)

- [ ] resp.rs passes all 8 tests
- [ ] server survives redis-benchmark -P 64 for 1M ops
- [ ] benched vs real redis, both pipelining levels, numbers above
- [ ] flamegraph captured; top-3 named and explained
- [ ] GRAPH.QUERY wire-compat: falkordb-py client can connect to the
      M7 server and get a well-formed (stub) reply
- [ ] threading-model decision written down with the Amdahl argument
