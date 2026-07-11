# Topic 9 — Concurrency: Latches, Lock-Free & Epochs

Scaling across cores is where the hardest bugs and the biggest wins live.
Topic 8 asked "who sees what" for transactions; this topic asks the same
question for NANOSECONDS: two threads, one cache line, who wins?

Budget: ~12 h. Order: §1 vocabulary → §2 memory ordering → §3 latch
protocols → §4 reclamation → §5 code → experiments → M9.

## 1. Latches vs locks (say it right)

| | lock (topic 8) | latch (this topic) |
|---|---|---|
| protects | logical content (rows, predicates) | physical structure (a node, a page) |
| held for | a transaction (seconds) | a critical section (nanoseconds) |
| deadlock | detected/resolved | must be IMPOSSIBLE by ordering |
| implemented by | lock manager table | atomics in the object itself |

Everything in this topic is latches. The three escalation rungs:

```
 mutex/rwlock          →  optimistic (version check)  →  lock-free (CAS)
 block on conflict        restart on conflict            never block; help
 postgres LWLock          LeanStore HybridLatch (T6)     RocksDB memtable,
                          + OLC B-trees                  crossbeam SkipSet
```

## 2. Memory ordering in one table (Rust atomics)

| Ordering | Guarantee | When you reach for it |
|---|---|---|
| `Relaxed` | atomicity only, no ordering | counters, stats |
| `Acquire` (loads) | later reads/writes can't move before it | reading a "ready" flag |
| `Release` (stores) | earlier reads/writes can't move after it | publishing a "ready" flag |
| `AcqRel` | both, for RMW ops | CAS that links a node |
| `SeqCst` | one global order of all SeqCst ops | when you can't prove less is enough |

The publication idiom that everything below builds on:

```
 writer:  node.data = 42;                    (plain writes)
          list.next.store(node, Release);    ← publish
 reader:  let n = list.next.load(Acquire);   ← subscribe
          n.data  // guaranteed to see 42
```

memgraph's `fully_linked.store(true, memory_order_release)` and RocksDB's
CASNext are both exactly this idiom. x86 gives you Acquire/Release for
free (TSO); ARM (this Mac!) does not — wrong orderings that "pass" on
x86 crash on the M-series. Test here.

## 3. Latch protocols for trees & lists

- **Latch coupling** (topic 3's B-trees): hold parent, grab child, release
  parent. Correct, but every traversal WRITES the latch cache line —
  root's line ping-pongs between all cores. Read-scaling: none.
- **Optimistic latch coupling (OLC, Leis)**: version counter per node.
  Readers read version → read node → re-check version; restart if changed.
  Writers latch + bump. Reads write NOTHING shared. This was LeanStore's
  HybridLatch (topic 6) — same trick, now you study it as a protocol.
- **Lock-free**: no latch at all; every mutation is one CAS that either
  lands or retries. The hard part is never the CAS — it's DELETION
  (§4) and multi-pointer updates (the skiplist's towers, Bw-tree's SMOs).

## 4. The reclamation problem (the actual boss fight)

Lock-free reads mean a reader may hold a pointer to a node you just
unlinked. `free()` it and the reader explodes. Options:

```
 epoch-based (crossbeam, this topic's build)
 ┌────────────────────────────────────────────────────┐
 │ global epoch E ────────► 3 garbage bags: E, E-1, E-2│
 │ reader: pin() → local epoch = E                    │
 │ writer: unlink node → defer_destroy into bag E     │
 │ advance: only when ALL pinned locals reached E     │
 │ free bag E-2: nobody can still see its nodes       │
 └────────────────────────────────────────────────────┘
 hazard pointers: per-reader "I'm reading THIS ptr" slots — O(readers)
   scan per free, but bounded garbage (epochs can be wedged by one stall)
 accessor ids (memgraph): each accessor gets a monotonic id; a retired
   node waits until all accessors older than the retire-time are gone —
   epoch flavor with txn-scoped pins
 RCU/QSBR (kernel): quiescent states instead of pins
```

Trade to internalize: epochs make READS free (one pin per operation, no
per-pointer traffic) but garbage unbounded under a stalled reader.
Hazard pointers invert it. Databases almost always pick epochs — readers
outnumber stalls.

## 5. Bw-tree: the cautionary tale

ICDE'13: a fully lock-free B-tree — updates are DELTA RECORDS prepended
by CAS onto a mapping table entry; splits are multi-step state machines.
SIGMOD'18 ("...More Than Just Buzz Words") rebuilt it honestly: delta
chains wreck cache locality, consolidation needs tuning, and a well-built
**OLC B+tree beats it** on almost every workload. Lesson: optimistic
latches + epochs is the pragmatic frontier; fully lock-free indexes are
usually a research flex. (Read both; guide: reading-bwtree.md.)

## 6. False sharing (the silent 10×)

Two ATOMICS in one 64B/128B cache line = every write invalidates the other
core's line even though the data is "independent". redis padded its
per-thread `used_memory` counters (topic 6); you'll measure the effect in
`false_sharing.rs` (M-series lines are 128B — check both alignments).

## 7. Code to read (guides in this dir)

| Guide | What you'll trace |
|---|---|
| reading-postgres-lwlock.md | One word, one CAS, one queue: postgres's production rwlock |
| reading-crossbeam-epoch.md | Epoch reclamation: the GC that makes lock-free reads free |
| reading-concurrent-skiplists.md | Two concurrent skiplists: CAS vs lazy locking |
| reading-bwtree.md | Bw-tree vs OLC: why lock-free lost to optimistic latches |

## 8. Experiments (`experiments/`)

- `src/concurrent_set.rs` — YOU make topic 2's skiplist concurrent:
  lock-free insert/contains/remove over crossbeam-epoch. Tests fix the
  contract (disjoint-key races, same-key races → exactly one winner,
  remove-under-readers doesn't UAF).
- `src/bin/scaling.rs` — provided: 1→16 threads, 90/10 read/write mix:
  `Mutex<BTreeSet>` vs 16-shard mutex vs crossbeam `SkipSet` (reference)
  vs yours. The mutex line runs today; predict the shapes first.
- `src/bin/false_sharing.rs` — provided, runs now: packed vs padded
  atomic counters, 8 threads. Predict the ratio on this M-series Mac.

## 9. M9 checklist (capstone)

- [ ] threadpool.rs: fixed pool, work queue, no per-query spawn. Compare
      against the reference's design (steal or not? — recall the
      Glommio/tokio trade from topic 7)
- [ ] single-writer/multi-reader graph: readers pin an epoch + version
      (M8's snapshot), writer publishes new matrix versions with Release
- [ ] parallel query execution over read snapshots — where does
      GraphBLAS's own parallelism meet the pool? (One pool, not two —
      decide who owns the threads)
- [ ] contention profile: Instruments "System Trace"/cachegrind stand in
      for perf c2c on macOS; find one false-sharing line in your code
