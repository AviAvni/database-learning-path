# Reading guide — "Morsel-Driven Parallelism" (SIGMOD '14) (~1 h)

Leis et al. (HyPer group). The scheduling half of the modern engine:
topic 11's other papers decide the INNER loop; this one decides how 8+
cores share it.

## The problem with plan-driven parallelism

The classical approach (Volcano "exchange" operators): the OPTIMIZER
picks a degree of parallelism, inserts exchange operators that
partition data between static worker sets.

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

## The design

- **Morsel** = ~100K tuples. Workers grab one, run the WHOLE pipeline on
  it (scan → filter → probe → partial-agg), grab the next.
- **Dispatcher**: a queue of morsels per pipeline; pipelines with
  dependencies (build before probe) gate on completion events.
- **NUMA awareness**: morsels are placed on sockets; a worker prefers
  local morsels, steals remote ones only when starved. Intermediate
  results stay socket-local because the same thread runs all operators.
- **Elasticity**: since workers commit only to one morsel at a time, the
  engine can change effective DOP mid-query (new query arrives → workers
  finish their morsel and switch). Compare: canceling a static-partition
  plan mid-flight.
- Shared state is confined to pipeline BREAKERS: thread-local partial
  hash tables merged at pipeline end (or a shared global HT with atomic
  inserts for the build — they use the latter, lock-free, topic 9's
  toolbox).

## Where you've already seen it

- DuckDB: row-group (122880) work units + `MaxThreads` on sources —
  morsels without the NUMA half (laptops don't have sockets).
- polars-stream: `Morsel` + `MorselSeq` + source tokens — morsels with
  explicit ordering and backpressure.
- Your topic 9 scaling.rs: static key-range split vs the shootout's
  shared-queue pulling — you measured the skew-stranding effect without
  naming it.

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
