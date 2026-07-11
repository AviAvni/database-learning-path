# Reading guide — C10K, valkey multithreading posts, thread-per-core (2 h)

Three readings spanning 1999→2024, one thread: *what should a server thread
be responsible for?*

## 1. "The C10K problem" — Dan Kegel (kegel.com/c10k.html)

Read it as history that explains present defaults. 1999's question: how do
you serve 10,000 sockets when a thread per connection costs a stack + a
scheduler slot each?

The menu it catalogs (skim the I/O-strategy section, skip the dated driver
patches):
1. thread per connection — dies at ~10K in 1999 (stacks, context switches)
2. select/poll — O(n) scans of the fd set per wakeup
3. **readiness notification** (epoll/kqueue) — O(ready) not O(registered):
   this line wins; ae.c is exactly this + a dispatch table
4. async I/O (POSIX aio) — stillborn on Linux for sockets; the idea returns
   as io_uring twenty years later

What changed since: threads got cheaper (10K threads is fine now), cores
multiplied (one loop can't fill a 64-core box), and NICs got faster than a
single core's syscall budget. Hence the two modern answers below.

## 2. Valkey's multithreading blog posts (valkey.io/blog — the 8.0
   performance series)

Read after reading-valkey-iothreads.md; the posts give the measured story:
- redis-6-style io-threads: threads spin-wait, main thread coordinates
  every batch — modest gains, high CPU burn.
- valkey 8 rework: SPSC handoff, threads own the whole read→parse and
  write path, main thread prefetches for batches ⇒ ~1M+ ops/s/node claims,
  ~2–3× over redis 7 on the same box.
- The reasoning to internalize: they profiled first — parse+syscall was the
  majority of CPU at high op rates; commands themselves were ~30%. The
  rework parallelizes exactly the majority and nothing else. (Amdahl,
  applied with discipline.)

## 3. Glauber Costa on thread-per-core (the Seastar/ScyllaDB and later
   Glommio essays: "The reactor pattern is dead, long live the reactor",
   shard-per-core posts)

The radical position: don't share ANYTHING between cores.
- One reactor per core, connections pinned, data **sharded by core** —
  a request for shard 7 arriving on core 2 is forwarded as a message
  (cross-core SPSC again), never locked.
- No locks ⇒ no lock contention, but also: no work stealing ⇒ a hot shard
  is a hot core; tail latency now depends on your partitioning function.
- Rust incarnation: Glommio (io_uring + thread-per-core executors) vs
  tokio's work-stealing multi-thread runtime. Tokio moves tasks to idle
  workers (great for uneven load, pays cross-core cache traffic); Glommio
  never moves them (great cache locality, pays imbalance).

```
        shared keyspace ◄──────────────────────► sharded keyspace
 redis/valkey: 1 exec thread     DragonflyDB/Scylla: N exec threads,
 + io threads, zero data locks   keyspace hash-partitioned per core,
                                 cross-shard ops = messages/transactions
```

## Questions to answer in notes.md

1. Which C10K strategy is tokio's multi-thread runtime? (Careful: it's
   readiness-based mio underneath + work-stealing tasks on top — two
   layers, name both.)
2. A graph database's unit of work is a *query* (ms-scale), not a GET
   (µs-scale). Redo valkey's Amdahl analysis for M7: what fraction of a
   GRAPH.QUERY round-trip is parse+I/O, and does ANY threading of the
   network layer matter? Where do the threads belong instead (M9)?
3. Thread-per-core for a graph: matrices don't hash-partition like a
   keyspace. What's the sharding unit — graph? subgraph? matrix tile? What
   does a BFS crossing shards cost in messages?
4. io_uring (the C10K "async I/O" line resurrected): what changes in ae.c's
   design if poll+read+write become submission-queue entries? (Topic 6's
   O_DIRECT thread rejoins here.)

## Done when

You can place redis, valkey 8, tokio, and DragonflyDB in the
shared↔sharded / loop↔threads plane and argue M7's position in it.
