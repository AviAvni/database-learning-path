# ClickHouse: the case for brute force

The system paper, 15 years in — the design rationale behind the
mechanisms you just read in
[reading-clickhouse-mergetree.md](reading-clickhouse-mergetree.md),
plus the parts you didn't read code for (mutations, replication,
scaling). Read it AFTER the code guide, paired with a local
`clickhouse local` session and ClickBench; its two-sentence thesis is
this topic's strongest counterpoint to index-everything instincts.

## Read for these arguments

- **Why brute force wins**: their bet is that with vectorization +
  compression + parallelism, scanning is fast enough that you rarely
  need per-row indexes. The sparse index prunes coarse ranges; from
  there it's bandwidth. (Your scan_bench measures exactly this bet in
  miniature.)
- **Everything happens at merge time**: TTL enforcement, dedup
  (ReplacingMergeTree), pre-aggregation (Summing/AggregatingMergeTree),
  recompression to heavier codecs for cold parts. Merges are the
  system's metabolic cycle — background bandwidth converted into query
  speed. (Topic 4's compaction-as-computation, fully weaponized.)
- **Specialized codecs as a product feature**: per-column
  `CODEC(Delta, ZSTD)` chains, Gorilla/DoubleDelta for time series —
  they let the USER declare what DuckDB's analyze pass discovers.
  Position this against BtrBlocks' sampling: three answers to "who
  chooses the encoding".
- **The updates problem**: mutations (`ALTER TABLE ... UPDATE`) rewrite
  whole parts asynchronously — updates are batch jobs, not
  transactions. Honest scope: this is what giving up OLTP buys.
- **Scaling section**: shared-nothing shards + ReplicatedMergeTree via
  Keeper (their RAFT-ish ZooKeeper replacement) — replication ships
  PARTS, not rows (state-machine replication at part granularity;
  topic 15 contrast: redis ships commands, postgres ships WAL,
  ClickHouse ships files).

## The experiments to run alongside (this topic's "run something real")

```bash
# duckdb + clickbench slice (see ../duckdb-clickbench.md notes file):
# 1. grab hits.parquet sample; run Q0/Q3/Q8/Q13/Q20 in duckdb
# 2. EXPLAIN ANALYZE each: note rows pruned by zone maps
# 3. PRAGMA storage_info('hits'): which compression per hot column?
# record all of it in notes.md
```

## Questions for notes.md

1. The paper's own numbers: where does ClickHouse lose (or barely win)
   on ClickBench-class queries, and is the cause ever the sparse index
   (vs e.g. string handling)?
2. Merges do TTL/dedup/aggregation — what's the failure mode when merge
   bandwidth can't keep up with ingest (too many parts)? Which topic 4
   stall mechanism is the analogue?
3. Part-shipping replication: what does it give up vs WAL shipping
   (replication lag granularity, partial-part visibility) and why is
   that acceptable for analytics?
4. User-declared codecs vs analyze-and-score vs sampling: which would
   you ship for a GRAPH database where property columns arrive via
   MERGE statements with unknown distributions? (M12 decision — commit
   to one and note why.)
5. The "for everyone" claim: what did they add to serve small/embedded
   use (chdb, clickhouse-local), and does it threaten DuckDB's niche or
   validate it?

## Done when

You can give the two-sentence ClickHouse thesis (immutable sorted
parts + merge-time work + brute-force vectorized scans; indexes only
sparse), and you have ClickBench-on-DuckDB numbers recorded in
notes.md.

## References

**Papers**
- Schulze, Schreiber, Yatsishin, Dahimene, Milovidov — "ClickHouse:
  Lightning Fast Analytics for Everyone" (VLDB 2024) — read for the
  arguments above, not the mechanisms; skim the eval against your own
  ClickBench numbers

**Code**
- [ClickHouse](https://github.com/ClickHouse/ClickHouse) — the code
  side is covered by
  [reading-clickhouse-mergetree.md](reading-clickhouse-mergetree.md);
  [ClickBench](https://github.com/ClickHouse/ClickBench) for the
  queries to run alongside
