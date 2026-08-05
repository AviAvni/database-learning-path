# Morsel-driven parallelism: workers pull, skew dissolves

Leis et al. (SIGMOD '14, HyPer group) — the scheduling half of the modern
engine: this topic's other papers decide the INNER loop; this one decides
how 8+ cores share it. The idea fits in a sentence — workers pull small
work units instead of receiving static partitions — and everything else
falls out of it. This chapter builds the six concepts behind that
sentence, then routes you through the paper.

Every figure below is checked against the paper and cited to its section
or table. Two claims this guide used to carry did not survive that check,
and one of them is the most useful thing here: **a morsel is not a
vector, and morsel size is not a cache parameter.** The correction is in
Step 3.

## The problem in one sentence

Split one query across 64 hardware threads by statically giving each
1/64 of the data and the query finishes when the *slowest* thread does —
which §5.4 measures at 36.8% slower than ideal from nothing more exotic
than one unrelated single-threaded process occupying one of the 64
cores.

## The concepts, step by step

### Step 1 — the classical answer: exchange operators and static partitions

> **In:** a query plan and a machine with more than one core.
> **Out:** the industry-standard way to connect them — parallelism baked
> into the plan at compile time — and the three costs that follow from
> *when* the decision is made, before any argument about skew.

Volcano-era parallelism is what §1 calls **plan-driven**: "the optimizer
statically determines at query compile-time how many threads should run,
instantiates one query operator plan for each thread, and connects these
with exchange operators". The **degree of parallelism** (DOP — how many
threads work on this query) is a plan property. An **exchange operator**
is a plan node that routes tuple streams between threads, splitting the
input into partitions and merging results, so that "operators are kept
largely unaware of parallelism".

That is a real virtue — §1 notes the approach "allows to implement
parallelism without affecting existing query operators", which is why
Oracle, SQL Server and Vectorwise all use it. Three costs come with it,
and all three follow from the decision being made *early*:

- the DOP is frozen at optimize time, while the machine's load changes
  by the second;
- exchange operators materialize and copy rows between workers, and §1
  argues the on-the-fly partitioning they perform "does not always lead
  to the optimal plan (as partitioning effort does not always pay off)";
- the plan explodes into parallel variants the optimizer must reason
  about.

### Step 2 — the failure mode: the slowest thread sets the runtime

> **In:** a static split of the input across N threads.
> **Out:** the actual failure mode, which is *not* the one this guide
> used to name — plus the paper's measurement of it on a workload with
> no skew in it at all.

The obvious story is **skew**: some partitions carry far more work than
others — a hot key range, a filter that passes 90% in one region and 1%
elsewhere — so the thread holding the hot partition grinds while the
rest idle. That story is true, and it is not the whole failure.

**Correction:** this guide previously attributed stranding to skew
alone. TPC-H is, in the paper's words, "fully uniform" — there is no
data skew to blame — and exchange-based Vectorwise still strands
workers on it. §5.2, on the *trivially* parallelizable scan-only Q6:
"the slowest thread often finishes work 50% before the last. While in
real-world scenarios it is usually data skew that challenges load
balancing, this is not the case in the fully uniform TPC-H."

§1 names the real enemy, and it is broader: perfect load balancing must
survive "uncertain size distributions of intermediate results, as well as
the hard-to-predict performance of modern CPU cores that varies even if
the amount of work they get is the same". Equal work is not equal time.
Frequency scaling, an SMT sibling, a noisy neighbour, or a P-core versus
an E-core all break a static split, and none of them is visible to an
optimizer.

§5.4 puts a number on it by emulating the exchange model inside the
morsel engine — setting morsel size to `n/t`, one chunk per thread:

```
 §5.4, TPC-H on 4-socket Nehalem EX (32 cores, 64 hardware threads):

   single query at a time, uniform data, nothing else running
     static (morsel = n/t)     no significant difference
     -> a static split is fine exactly when nothing perturbs it

   same runs, with one unrelated single-threaded process on one core
     static (morsel = n/t)     36.8% slower
     dynamic (morsel = 100K)    4.7% slower

 one core out of 64 disturbed. 1/64 = 1.6% of the machine,
 and the static plan loses 36.8% of the query.
```

The whole comparison in one table:

```
 exchange model                        morsel model
 ─────────────                         ────────────
 plan fixes DOP at optimize time       DOP changes per SECOND
 static partitions -> the slowest      workers PULL 100K-row morsels;
   thread sets the runtime, from         fast workers just pull more
   skew OR from core-speed variance
 exchange = extra materialization +    same pipeline object shared by
   copying between workers               all workers, zero exchange ops
 plan explosion (parallel variants)    one plan, parallelism is runtime
```

And the end-to-end scoreboard, §5.2 (both systems on the same machine,
scalability = speedup from 1 thread to 64):

```
 system                              geo. mean    sum     scalability
 HyPer (morsel-driven)                 0.45 s   15.3 s      28.1x
 Vectorwise 2.5 (exchange)             2.84 s   93.4 s       9.3x
 Vectorwise, full-disclosure settings  1.19 s   41.2 s       8.4x
```

Note what this is and is not evidence for. Both systems have "similar
single-threaded performance" (§5.2); the 6.3× gap in geometric mean is
almost entirely the 28.1× versus 9.3× in the last column. It is a
comparison of *parallelization frameworks*, not of execution models —
Vectorwise is vectorized and HyPer is compiled, but four years later the
same group built both models on top of morsel parallelism and found them
tied ([reading-compiled-vs-vectorized.md](reading-compiled-vs-vectorized.md)
§6). Morsel parallelism is orthogonal to Steps 2-3 of that guide, which
is exactly why it won.

You already measured the underlying effect without naming it: topic 9's
`scaling.rs` — static key-range split vs the shootout's shared-queue
pulling.

### Step 3 — the morsel: work units small enough to rebalance

> **In:** the need to decide *who does what* later than plan time.
> **Out:** the unit of that decision, the loop that consumes it, and —
> the part most summaries get wrong — the fact that its size is a
> scheduling parameter with a floor, not a cache parameter with an
> optimum.

The fix inverts control: instead of *assigning* data to workers, workers
**pull**. A **morsel** is a small run of input tuples — §3: "We
experimentally determined that a morsel size of about 100,000 tuples
yields good tradeoff between instant elasticity adjustment, load
balancing and low maintenance overhead." A **pipeline** is the chain of
operators between materialization points (see
[reading-duckdb-execution.md](reading-duckdb-execution.md)); a
**pipeline breaker** is the operator that ends one by having to consume
its whole input first. A worker grabs one morsel, runs it through the
*whole* pipeline, materializes into the next pipeline breaker, and grabs
the next.

Three structural details that summaries drop, all from §3 and §3.2:

- **The threads are fixed and pinned.** One worker per hardware thread,
  pre-created and permanently bound, so "the level of parallelism of a
  particular query is not controlled by creating or terminating threads,
  but rather by assigning them particular tasks of possibly different
  queries" — and "no unexpected loss of NUMA locality can occur due to
  the OS moving a thread to a different core" (§1).
- **The dispatcher is not a thread.** It "is implemented as a lock-free
  data structure only. The dispatcher's code is then executed by the
  work-requesting query evaluation thread itself", because a real
  dispatcher thread "would need a core to run on" and "could become a
  source of contention, in particular if the morsel size was configured
  quite small". The `QEPobject` that gates pipelines on their
  dependencies is likewise "a passive state machine", run on the worker
  that just found the queue empty.
- **Morsels are cut on demand.** The per-core lists in Figure 5 are
  illustrative; the implementation keeps "storage area boundaries for
  each core/socket and segment[s] these large storage areas into morsels
  on demand".

The worker loop is the design:

```rust
// ILLUSTRATION — not quoted from any repo. The paper gives no worker
// loop in code; this is Figure 5 plus §3-§4.4 written out. The shipped
// shapes are polars' work-stealing executor
// (crates/polars-async/src/executor/mod.rs:236 try_steal_task) and its
// Morsel type (crates/polars-stream/src/morsel.rs:82) — see
// reading-rust-execution-stack.md.
fn worker(dispatcher: &Dispatcher, ht: &BuildHt) {
    let mut local_agg = PartialAgg::new();       // §4.4 phase 1: fixed-size,
    while let Some(m) = dispatcher.pull(my_core()) { // spills when full
        let chunk = scan(m);                     // per-CORE list of morsels
        let sel = filter(&chunk);                // allocated on this socket;
        let matches = probe(ht, &chunk, &sel);   // steal remote when starved
        local_agg.update(&matches);              // whole pipeline, one thread
    }                                            // preemption happens HERE,
    dispatcher.flush_partitions(local_agg);      // at morsel boundaries only
}
```

Now the part to unlearn. **Correction:** this guide previously said
morsel size is "a trade: too small and per-morsel scheduling overhead
dominates; too big and you're back to coarse partitions that can't
rebalance", and question 1 below asked for the bound "above" in terms of
cache. The cache half is contradicted by §3.3, which opens by
distinguishing morsels from vectors precisely on this point:

```
 §3.3, in full on the point that matters:
   "In contrast to systems like Vectorwise and IBM's BLU, which use
    vectors/strides to pass data between operators, there is no
    performance penalty if a morsel does not fit into cache. Morsels
    are used to break a large task into small, constant-sized work
    units to facilitate work-stealing and preemption. Consequently,
    the morsel size is not very critical for performance, it only
    needs to be large enough to amortize scheduling overhead while
    providing good response times."

 Figure 6 (select min(a) from R, 64 threads, Nehalem EX, morsel
 size swept 100 .. 10M) is therefore a FLOOR, not a U:
   below ~10,000    work-stealing structure overhead shows
   above ~10,000    flat — "the morsel size should be set to the
                    smallest possible value where the overhead is
                    negligible, in this case to a value above 10,000"
   far too large    "results in underutilized threads but does not
                    affect throughput of the system if enough
                    concurrent queries are being executed"
```

A vector's size is set by the cache
([reading-x100.md](reading-x100.md) derives 8K × 40 B = the machine's
320 KB); a morsel's is set by scheduling overhead below and response
time above. They are different parameters answering different questions,
which is why DuckDB carries both — a 2048-value `DataChunk` *and* a
122,880-row row group as its work unit
([reading-duckdb-execution.md](reading-duckdb-execution.md)) — and why
120× separates them.

§3.3 also explains why the shared work-stealing structure does not
become the bottleneck: work is initially split so each thread "temporarily
owns a local range", each range is **cache-line aligned** so "conflicts
at the cache line level are unlikely", and stealing only starts when a
local range is exhausted.

One more consequence, cheaply won: **query cancellation**. A cancelled
query is marked in the dispatcher and "the marker is checked whenever a
morsel of that query is finished", so workers stop within a morsel and —
unlike killing threads — "this approach allows each thread to clean up".

### Step 4 — NUMA awareness: run the pipeline where the data lives

> **In:** a machine where "the computer has become a network in itself"
> (§1) — four sockets, four memory controllers, an interconnect between
> them.
> **Out:** the single preference rule that makes the morsel design
> NUMA-aware, and the measurement showing it worked.

**NUMA** (non-uniform memory access) means each socket has local RAM;
reaching another socket's RAM costs more latency and consumes
interconnect bandwidth that other threads are also using. On the paper's
Sandy Bridge EP some pairs are not directly connected at all, so "some
memory accesses (e.g., from socket 0 to socket 2) require two hops"
(§5.1).

The design absorbs this with a preference, not a partitioning. §3.1: "For
each core a separate list exists to ensure that a work request of, say,
Core 0 returns a morsel that is allocated on the same socket as Core 0."
**Work stealing** is the escape hatch: "If, for some reason, a core
finishes processing all morsels on its particular socket, the dispatcher
will 'steal work' from another core… On some NUMA systems, not all
sockets are directly connected with each other; here it pays off to steal
from closer sockets first. Under normal circumstances, work-stealing from
remote sockets happens very infrequently; nevertheless it is necessary to
avoid idle threads."

Because one thread runs the whole pipeline on its morsel, intermediates
never cross the interconnect — and outputs follow the *worker*, not the
input: "a red morsel turns blue if it was processed by a blue core in the
process of stealing work from the core(s) on the red socket" (§3.2).

The evidence is bandwidth, §5.3 (Table 1, Nehalem EX):

```
 TPC-H Q1 (aggregates the largest relation), HyPer:
   read bandwidth achieved                 82.6 GB/s
   theoretical maximum of the machine       100  GB/s     = 83% of peak
   remote accesses                          low across most queries
   most heavily used QPI link               not saturated

 Vectorwise on the same query:
   remote accesses                          75%
   -> "shows that its buffer manager is not NUMA-aware"
```

**Correction:** this guide previously priced a remote access at "~2× the
latency". The paper gives no such ratio; what it measures is the
*fraction* of accesses that go remote and the QPI saturation that
results. Quote those instead.

### Step 5 — elasticity: the commit unit is one morsel

> **In:** a running query holding all 64 threads, and a new query
> arriving.
> **Out:** the property that makes reassignment cheap, expressed as a
> time bound you can compute.

Since a worker commits to only one morsel at a time, "preemption of a
task occurs at morsel boundaries – thereby eliminating potentially costly
interrupt mechanisms" (§3). §3.1: the engine can "gracefully decrease the
degree of parallelism of, say a long-running query `Ql` at any stage of
processing in order to prioritize a possibly more important interactive
query `Q+`", and when `Q+` finishes "the pendulum swings back". Figure 13
is the profiler trace: four workers on TPC-H Q13, Q14 arrives, workers 2
and 3 finish their current morsels and switch, then return to Q13.

The load-balancing consequence is a *bound*, and this is what makes it
different in kind from "the fast workers do more". §3.2: all threads on
one pipeline job "run to completion in a 'photo finish': they are
guaranteed to reach the finish line within the time period it takes to
process a single morsel."

Size that bound with this repo's own per-row cost as a stand-in:

```
 FINDINGS.md row 11 — 9.68 ns per scanned row through
 scan -> filter -> group-by-sum at 50% selectivity (M3 Pro).

 one morsel  = 100,000 rows x 9.68 ns              = 0.97 ms
   <- the maximum any thread can be left waiting

 a 600M-row scan (TPC-H SF-100 lineitem) on 64 threads:
   ideal per-thread work = 600e6 / 64 x 9.68 ns    = 90.8 ms
   photo-finish window   = 0.97 / 90.8             = 1.1%

 the same query, static split, with one of the 64 cores
 running at half speed:
   that thread's share takes 2 x 90.8              = 181.5 ms
   the query waits for it                          = +100%
   morsel version: the other 63 absorb its work    = +0.8%

 the paper's measured version of that last pair, §5.4:
   static 36.8% slower vs dynamic 4.7% slower
```

"Elasticity" therefore means precisely this: **commit granularity = one
morsel**, so both the reassignment latency and the load-imbalance
penalty are bounded by one morsel's processing time rather than by the
input size.

### Step 6 — shared state only at pipeline breakers

> **In:** morsels flowing through pipelines independently.
> **Out:** the two places threads must actually meet — and the fact that
> the paper solves them with *different* mechanisms, chosen for a reason
> worth stealing.

Within a morsel a worker touches only its own data: zero
synchronization. Sharing is confined to pipeline breakers, and the paper
builds two of them.

**Hash join build — one shared table, lock-free (§4.1, §4.2).** Two
phases. First, build-side tuples are materialized into a *thread-local*
storage area, "this requires no synchronization". Then, since the input
size is now known exactly, "an empty hash table is created with the
perfect size… much more efficient than dynamically growing hash tables,
which incur a high overhead in a parallel setting". Second, each thread
scans its own area and inserts pointers with atomic compare-and-swap:

```
 Figure 7 — lock-free insertion into the tagged hash table:

   insert(entry) {
     slot = entry->hash >> hashTableShift
     do {
       old = hashTable[slot]
       entry->next = removeTag(old)                    // chain
       new = entry | (old&tagMask) | tag(entry->hash)  // OR in the tag
     } while (!CAS(hashTable[slot], old, new))
   }

 the pointer layout that makes one CAS enough:
   16 bit tag  |  48 bit pointer   = 64 bits, one atomic word
```

The tag is an early filter — every element of a bucket list sets its bit
in it — so "for selective probes… the filter usually reduces the number
of cache misses to 1 by skipping the list traversal". Encoding it inside
the pointer "saves space and, more importantly, allows to update both the
pointer and the tag using a single atomic compare-and-swap operation".

The 16/48 split is worth remembering because you have already seen it:
DuckDB's `ht_entry` uses the same 16-bit-plus-48-bit word
([reading-duckdb-execution.md](reading-duckdb-execution.md)). The
mechanisms differ — the paper's table is *chained*, and the tag is a
per-bucket-list filter; DuckDB's is open-addressed with linear probing,
and its 16 bits are a *salt* compared against the probe key's own salt.
Same word layout, same motivation (one cache line, one atomic), different
collision strategy.

**Aggregation — partitioning, not a shared table (§4.4).** Also two
phases, but built the other way round. Phase 1 is a thread-local
*fixed-size* hash table that "efficiently aggregates heavy hitters", and
"when this small pre-aggregation table becomes full, it is flushed to
overflow partitions". After all input is partitioned, partitions are
exchanged between threads; phase 2 has each thread aggregate a whole
partition into a thread-local table, repeating since "there are more
partitions than worker threads", and pushing each finished partition
downstream immediately so "the aggregated tuples are likely still in
cache".

The design note is the transferable part: "the aggregation operator is
fundamentally different from join in that the results are only produced
after all the input has been read. Since pipelining is not possible
anyway, we use partitioning – not a single hash table as in our join
operator." And the whole shape is chosen "without relying on query
optimizer estimates" — few groups are absorbed by the fixed-size local
table and never spill; many groups spill and get partitioned. The
structure handles both cases rather than picking one from a cardinality
estimate.

**What the paper deliberately does not do.** Bushy parallelism — running
two independent pipelines of the same query at once — is available and
declined (§3.2): "the number of independent pipelines is usually much
smaller than the number of cores, and the amount of work in each
pipeline generally differs. Furthermore, bushy parallelism can decrease
performance by reducing cache locality. Therefore, we currently avoid to
execute multiple pipelines from one query in parallel." Intra-pipeline
parallelism at morsel granularity is enough.

## How to read the paper (with the concepts in hand)

~1 h. §3 and §3.3 are the two sections to read carefully; the NUMA
evaluation is skimmable on a laptop, but §5.4's static-vs-dynamic
experiment is not — it is the paper's cleanest single result.

| Section | What is there | Step |
|---|---|---|
| §1 | the case against plan-driven parallelism, and the sentence about cores "that varies even if the amount of work they get is the same" — the real enemy | 1, 2 |
| §2 + Figures 1-4 | the three-pipeline example query; how a plan becomes pipeline jobs; the QEPobject observing dependencies | 3 |
| §3 + Figure 5 | **the core.** Pinned workers, tasks = (pipeline job, morsel), preemption at morsel boundaries, the 100,000-tuple figure, the three scheduling goals | 3, 4, 5 |
| §3.1-3.2 | elasticity; per-core morsel lists; the lock-free dispatcher with no thread of its own; the "photo finish" bound; work stealing; why bushy parallelism is declined; query cancellation | 3, 4, 5 |
| §3.3 + Figure 6 | **read carefully.** Why a morsel is not a vector, and why its size is a floor rather than an optimum | 3 |
| §4.1-4.2 + Figure 7 | two-phase build, exact-size table, the 16-bit tag inside the 48-bit pointer, CAS insertion | 6 |
| §4.4 + Figure 8 | two-phase aggregation by partitioning, and the sentence explaining why it differs from the join | 6 |
| §5.1-5.2 | the 28.1× vs 9.3× scalability table, and Q6's load-imbalance result on uniform data | 2 |
| §5.3 + Table 1 | NUMA: 82.6 GB/s of a 100 GB/s peak, remote-access percentages, QPI saturation | 4 |
| §5.4 + Figure 13 | **don't skip.** The 36.8% vs 4.7% emulation of static assignment, and the profiler trace of a query yielding cores mid-flight | 2, 5 |

Where you have already seen the idea shipped:

- **DuckDB**: row-group (122,880 rows) work units + `MaxThreads` on
  sources — morsels without the NUMA half (laptops do not have sockets).
- **polars-stream**: `Morsel` + `MorselSeq` + source tokens, over its own
  work-stealing executor — morsels with explicit ordering and
  backpressure ([reading-rust-execution-stack.md](reading-rust-execution-stack.md)).
  Its `DEFAULT_IDEAL_MORSEL_SIZE` is 100,000, the paper's number
  unchanged after a decade.
- **Your topic 9 `scaling.rs`**: you measured the stranding effect
  without naming it.

## Takeaway

The paper's contribution is not "pull instead of push" — it is moving
one decision from compile time to run time and discovering how much falls
out. Once the unit of assignment is a morsel rather than a partition, load
balancing becomes a bound rather than a hope ("photo finish", one morsel
wide), elasticity is free (preemption at morsel boundaries, no interrupt
mechanism), NUMA locality is a preference on a queue rather than a
partitioning pass, and cancellation is a flag checked at the same
boundary. Four features, one decision.

Two things to carry into your own scheduler. First, size the work unit
for *scheduling*, not for cache — §3.3 is explicit that a morsel
overflowing cache costs nothing, and Figure 6 shows a floor around
10,000 with a flat plateau above it. Copying a vector-size intuition here
is the classic error. Second, the thing that breaks static splits is not
mainly skewed data; it is that equal work is not equal time. §5.4's one
busy core costing 36.8% is a laptop-scale result: a P-core and an E-core
handed identical partitions will finish at different moments, and only
the pulling design absorbs it.

## Questions for notes.md

1. Morsel size: DuckDB's 122,880-row row group and the paper's 100,000
   tuples land in the same place, while X100's vector is 1,024. §3.3
   says the morsel bound below is scheduling overhead (Figure 6: above
   ~10,000) and that cache does *not* bound it above — so what does?
   State the bound above in units of *time*, then check it against your
   topic 7 batch findings and say which of those bounds are really
   vector bounds.
2. The paper uses a shared lock-free table for the join build (§4.2) and
   partitioning for aggregation (§4.4). Write the sentence from §4.4
   that explains the difference, then predict: for `exec_bench`'s 64
   dense groups, does phase 1's fixed-size local table ever spill? For
   64M groups? Which phase dominates in each case?
3. Ordering: morsel pulling destroys tuple order. What does the paper
   (and polars' `MorselSeq`) do when ORDER BY needs it back, and what
   does that cost? (§4.5 for the parallel merge sort; note the paper
   sorts *only* for `order by` / top-k.)
4. On a MacBook (no NUMA, but P-cores vs E-cores): does the
   heterogeneous-core problem look MORE like NUMA or more like skew?
   §1's "hard-to-predict performance of modern CPU cores that varies
   even if the amount of work they get is the same" and §5.4's 36.8%
   are the paper's own answer — which mechanism (locality preference or
   dynamic pulling) addresses it, and which is dead weight on a laptop?
5. M11: FalkorDB is single-writer, many-reader (M8/M9 decisions). A
   read query's Expand over a big frontier — morselize the FRONTIER?
   What's the natural morsel for SpMV (row-block of the matrix?). Note
   §3.2's reason for declining bushy parallelism before deciding. This
   is the M11 parallelism design question — write a paragraph.

## Done when

Answer each before unfolding it.

- [ ] You can state what actually strands workers under static partitioning, with the paper's measurement on a workload that has no skew in it.

  <details><summary>Answer</summary>

  Not skew — or not only skew. §1: load balancing must survive "uncertain
  size distributions of intermediate results, as well as the
  hard-to-predict performance of modern CPU cores that varies even if the
  amount of work they get is the same". Equal work is not equal time.

  The proof is on uniform data. §5.2, on the trivially parallel scan-only
  TPC-H Q6 under Vectorwise: "the slowest thread often finishes work 50%
  before the last. While in real-world scenarios it is usually data skew
  that challenges load balancing, this is not the case in the fully
  uniform TPC-H." And §5.4 isolates it: emulate static assignment by
  setting morsel size to `n/t` and TPC-H barely changes — until one
  unrelated single-threaded process occupies one of 64 cores, at which
  point static loses 36.8% and morsel-driven loses 4.7%. One core in 64
  is 1.6% of the machine.

  </details>

- [ ] You can say why morsel size is not the same kind of parameter as vector size, and give the paper's rule for setting it.

  <details><summary>Answer</summary>

  §3.3 draws the distinction itself: "In contrast to systems like
  Vectorwise and IBM's BLU, which use vectors/strides to pass data
  between operators, there is no performance penalty if a morsel does not
  fit into cache. Morsels are used to break a large task into small,
  constant-sized work units to facilitate work-stealing and preemption.
  Consequently, the morsel size is not very critical for performance."

  So the rule is a floor, not an optimum: "the morsel size should be set
  to the smallest possible value where the overhead is negligible, in
  this case to a value above 10,000" (Figure 6, `select min(a) from R`
  on 64 threads — chosen because it "stresses the work-stealing data
  structure as much as possible"). Above the floor the curve is flat, and
  an over-large morsel "results in underutilized threads but does not
  affect throughput of the system if enough concurrent queries are being
  executed".

  A vector size, by contrast, is derived from cache — X100's 8K × 40
  bytes = the machine's 320 KB. Different question, different answer;
  DuckDB carries both constants at once.

  </details>

- [ ] You can state the load-balancing guarantee as a bound and compute it for a concrete query.

  <details><summary>Answer</summary>

  §3.2: all threads working on one pipeline job "run to completion in a
  'photo finish': they are guaranteed to reach the finish line within the
  time period it takes to process a single morsel". The worst-case
  imbalance is one morsel, independent of input size — which is exactly
  what a static split cannot promise, since there the bound is one
  partition.

  Using this repo's measured 9.68 ns per scanned row (FINDINGS row 11) as
  a stand-in: one 100,000-row morsel is 0.97 ms. A 600M-row scan on 64
  threads gives each thread 90.8 ms of ideal work, so the photo-finish
  window is 0.97/90.8 = 1.1% of the runtime. Under a static split, one
  core running at half speed doubles the query's runtime; under morsels
  the other 63 threads absorb its share for about +0.8%. §5.4's measured
  36.8% vs 4.7% is the same experiment on real hardware.

  </details>

- [ ] You can explain why the dispatcher has no thread of its own, and what it costs to preempt a worker.

  <details><summary>Answer</summary>

  §3.2 rejects a dispatcher thread on two grounds: "(1) the dispatcher
  itself would need a core to run on or might preempt query evaluation
  threads and (2) it could become a source of contention, in particular
  if the morsel size was configured quite small". So it is "implemented
  as a lock-free data structure only" and "the dispatcher's code is then
  executed by the work-requesting query evaluation thread itself", on the
  core that is momentarily between morsels. The `QEPobject` gating
  pipelines on their dependencies is likewise a passive state machine run
  by whichever worker discovers the queue empty.

  Preemption costs nothing beyond finishing the current morsel: §3 says
  it "occurs at morsel boundaries – thereby eliminating potentially
  costly interrupt mechanisms", and Figure 13 shows workers 2 and 3
  leaving TPC-H Q13 for Q14 and returning. Query cancellation rides the
  same boundary — a marker "checked whenever a morsel of that query is
  finished", which unlike killing a thread "allows each thread to clean
  up".

  </details>

- [ ] You can say why the join build and the aggregation use different shared-state strategies, in the paper's own terms.

  <details><summary>Answer</summary>

  §4.4: "the aggregation operator is fundamentally different from join in
  that the results are only produced after all the input has been read.
  Since pipelining is not possible anyway, we use partitioning – not a
  single hash table as in our join operator."

  The join build is a shared, lock-free, *tagged* table: materialize
  build tuples thread-locally with no synchronization, size the table
  exactly (input size is now known — "much more efficient than
  dynamically growing hash tables"), then CAS pointers in, with a 16-bit
  tag packed into the same 64-bit word as the 48-bit pointer so one
  atomic updates both. The tag is an early filter that "usually reduces
  the number of cache misses to 1" on selective probes.

  Aggregation instead pre-aggregates into a fixed-size thread-local table
  that spills to overflow partitions when full, exchanges partitions
  between threads, then aggregates each partition thread-locally and
  pushes it downstream immediately while it is still cache-hot. The point
  of the shape is robustness "without relying on query optimizer
  estimates": few groups never spill, many groups partition.

  </details>

## References

**Papers**
- Leis, Boncz, Kemper, Neumann — "Morsel-Driven Parallelism: A NUMA-Aware
  Query Evaluation Framework for the Many-Core Age" (SIGMOD 2014) —
  ~1 h. §3 and §3.3 for the design and the morsel-vs-vector distinction,
  §4.2 and §4.4 for the two shared-state strategies, §5.4 for the single
  cleanest experiment in the paper

**In this repo**
- [reading-rust-execution-stack.md](reading-rust-execution-stack.md) —
  polars-stream's `Morsel`, `MorselSeq` and work-stealing executor, with
  the paper's 100,000 still the default
- [reading-duckdb-execution.md](reading-duckdb-execution.md) — row groups
  as morsels, and the 16/48 hash-table word in a different collision
  scheme
- [reading-compiled-vs-vectorized.md](reading-compiled-vs-vectorized.md)
  — §6 there gives morsel parallelism to both execution models, which is
  the evidence that this framework is orthogonal to that argument
- [FINDINGS.md](../../FINDINGS.md) row 11 — the per-row cost used to size
  the photo-finish bound in Step 5
