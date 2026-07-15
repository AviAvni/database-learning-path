# Morsel-driven parallelism: workers pull, skew dissolves

Leis et al. (SIGMOD '14, HyPer group) — the scheduling half of the modern
engine: this topic's other papers decide the INNER loop; this one decides
how 8+ cores share it. The idea fits in a sentence — workers pull small
work units instead of receiving static partitions — and everything else
falls out of it. This chapter builds the six concepts behind that
sentence, then routes you through the paper.

## The problem in one sentence

Split one query across 8 cores by statically giving each core 1/8 of the
data, and one skewed partition leaves 7 cores idle while 1 grinds — the
query runs at 1/8 speed exactly when parallelism was supposed to pay.

## The concepts, step by step

### Step 1 — the classical answer: exchange operators and static partitions

Volcano-era parallelism (the "exchange" model): the OPTIMIZER picks a
**degree of parallelism** (DOP — the number of threads working on the
query) at plan time, and inserts **exchange operators** — special plan
nodes that split data into static partitions, run copies of the plan
fragment on each, and merge results. Parallelism lives *in the plan*.
Three costs follow directly: the DOP is frozen at optimize time while
the machine's load changes per second; exchange operators materialize
and copy rows between workers; and the plan itself explodes into
parallel variants the optimizer must now reason about.

### Step 2 — the failure mode: skew strands workers

**Skew** (some partitions having far more work than others — a hot key
range, a filter that passes 90% in one region and 1% elsewhere) breaks
static partitioning: the thread holding the hot partition grinds while
the other N−1 finish and idle. The whole comparison in one table:

```
 exchange model                        morsel model
 ─────────────                         ────────────
 plan fixes DOP at optimize time       DOP changes per SECOND
 static partitions → skew strands      workers PULL 100K-row morsels;
   workers (one hot partition = one    fast workers just pull more
   busy thread, N-1 idle)
 exchange = extra materialization +    same pipeline object shared by
   copying between workers               all workers, zero exchange ops
 plan explosion (parallel variants)    one plan, parallelism is runtime
```

You already measured this without naming it: topic 9's scaling.rs —
static key-range split vs the shootout's shared-queue pulling.

### Step 3 — the morsel: work units small enough to rebalance

The fix inverts control: instead of *assigning* data to workers, workers
**pull**. A **morsel** is a small run of input (~100K tuples); a
dispatcher keeps a queue of them per pipeline (a pipeline being the
chain of operators between materialization points — see the DuckDB
guide); each worker grabs one morsel, runs it through the WHOLE pipeline
(scan → filter → probe → partial aggregate), then grabs the next.
Pipelines with dependencies (build before probe) gate on completion
events. Skew now dissolves by construction — a slow morsel just means
that worker pulls fewer; there is no partition to be stuck with. The
worker loop IS the design:

```rust
fn worker(dispatcher: &Dispatcher, ht: &BuildHt) {
    let mut local_agg = PartialAgg::new();       // thread-local: no contention
    while let Some(m) = dispatcher.pull(my_socket()) { // prefer LOCAL morsels,
        let chunk = scan(m);                     // steal remote when starved
        let sel = filter(&chunk);                // the WHOLE pipeline runs
        let matches = probe(ht, &chunk, &sel);   // here, one thread, so
        local_agg.update(&matches);              // intermediates stay hot
    }                                            // commit unit = one morsel:
    dispatcher.combine(local_agg);               // that's the elasticity
}
```

Morsel size is a trade: too small and per-morsel scheduling overhead
dominates; too big and you're back to coarse partitions that can't
rebalance (question 1 below).

### Step 4 — NUMA awareness: run the pipeline where the data lives

On multi-socket machines, memory is **NUMA** (non-uniform memory access:
each socket has local RAM, and touching another socket's RAM costs ~2×
the latency and shares an interconnect). The morsel design absorbs this
with one preference rule: morsels are *placed* on sockets, and a worker
prefers pulling morsels local to its socket, stealing remote ones only
when starved (`dispatcher.pull(my_socket())` above). Because the same
thread runs the whole pipeline on its morsel, intermediate results stay
socket-local automatically — no exchange operator ever ships them
across the interconnect.

### Step 5 — elasticity: the commit unit is one morsel

Since a worker commits to only one morsel at a time, the engine can
change effective DOP *mid-query*: a new query arrives, and workers
simply finish their current ~100K-row morsel (a millisecond or two) and
switch queues. Compare canceling or rebalancing a static-partition plan
mid-flight — the partition is the commit unit, and it's the whole
input/DOP. "Elasticity" means precisely this: **commit granularity = one
morsel**.

### Step 6 — shared state only at pipeline breakers

Within a morsel, a worker touches only its own data — zero
synchronization. Sharing is confined to pipeline BREAKERS (the
materializing sinks): for aggregation, either thread-local partial hash
tables merged at pipeline end (the `combine` call above), or one shared
global hash table with atomic inserts for the join build — the paper
uses the latter, lock-free (topic 9's toolbox). Which of the two wins
depends on group count: 64 groups fit in every thread's cache and merge
in microseconds; 64M groups make merging cost real (question 2).

## How to read the paper (with the concepts in hand)

~1 h. §2–3 for the design, skim the NUMA eval if you live on a laptop.

- **§1–2** — the case against exchange (Steps 1–2) and the morsel
  design (Step 3). The figures showing per-socket morsel queues are
  Steps 3–4 in one picture.
- **§3 — the core**: dispatcher, pipeline gating, the NUMA preference
  rule (Step 4), elasticity (Step 5), and the lock-free shared build HT
  (Step 6).
- **§4–5 (evaluation)** — skim unless you have sockets; note the skew
  experiments confirming Step 2's failure mode and its dissolution.

Where you've already seen the idea shipped:

- DuckDB: row-group (122880 rows) work units + `MaxThreads` on sources —
  morsels without the NUMA half (laptops don't have sockets).
- polars-stream: `Morsel` + `MorselSeq` + source tokens — morsels with
  explicit ordering and backpressure (reading-rust-execution-stack.md).
- Your topic 9 scaling.rs: you measured the skew-stranding effect
  without naming it.

## Questions for notes.md

1. Morsel size tradeoff: 100K rows vs DuckDB's 122880 vs your topic 7
   batch findings — what bounds it below (scheduling overhead per
   morsel) and above (load-balance granularity + cache)? Same
   amortize-and-batch curve as everywhere else.
2. Two-phase aggregation (thread-local HTs + merge) vs the paper's
   shared lock-free build HT: which wins for 64 groups? For 64M groups?
   (Contention vs merge cost — your exec_bench has 64 dense groups;
   predict.)
3. Ordering: morsel pulling destroys tuple order. What does the paper
   (and polars' MorselSeq) do when ORDER BY needs it back, and what does
   that cost?
4. On a MacBook (no NUMA, but P-cores vs E-cores): does the
   heterogeneous-core problem look MORE like NUMA or more like skew?
   Which mechanism (locality preference vs dynamic pulling) addresses
   it?
5. M11: FalkorDB is single-writer, many-reader (M8/M9 decisions). A
   read query's Expand over a big frontier — morselize the FRONTIER?
   What's the natural morsel for SpMV (row-block of the matrix?). This
   is the M11 parallelism design question — write a paragraph.

## Done when

You can explain skew-stranding with the one-hot-partition picture and
say precisely what "elasticity" means (commit granularity = one morsel).

## References

**Papers**
- Leis, Boncz, Kemper, Neumann — "Morsel-Driven Parallelism: A NUMA-Aware
  Query Evaluation Framework for the Many-Core Age" (SIGMOD 2014) —
  ~1 h; §2–3 for the design, skim the NUMA eval if you live on a laptop
