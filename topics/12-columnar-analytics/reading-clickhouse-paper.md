# ClickHouse: the case for brute force

The system paper, 15 years in — the design rationale behind the
mechanisms you just read in
[reading-clickhouse-mergetree.md](reading-clickhouse-mergetree.md),
plus the parts you didn't read code for (mutations, replication,
scaling). Read it AFTER the code guide, paired with a local
`clickhouse local` session and ClickBench. Before the paper, this
chapter builds the five arguments it makes — one at a time — because
its two-sentence thesis is this topic's strongest counterpoint to
index-everything instincts.

## The problem in one sentence

If a vectorized engine scans compressed columns at multiple GB/s per
core, a query over a 100M-row table is a sub-second *scan* — so how
much of classical database machinery (per-row indexes, transactional
updates, row-level replication) should you simply refuse to build?

## The concepts, step by step

### Step 1 — the brute-force bet: make scanning cheap instead of avoiding it

ClickHouse's founding bet is that with vectorization (topic 11) +
compression (this topic) + parallelism across all cores, scanning is
fast enough that you rarely need per-row indexes at all. Arithmetic:
16 cores × ~2 GB/s of decompressed scan throughput each ≈ 32 GB/s —
a 10-byte-per-row hot column over 1B rows scans in ~0.3 s *with no
index*. The sparse primary index (one key per 8192 rows, previous
chapter) prunes coarse ranges; from there it's bandwidth. Your
scan_bench measures exactly this bet in miniature. Why it matters:
every other argument in the paper is a consequence of refusing
per-row machinery — read them as corollaries, not separate features.

### Step 2 — everything happens at merge time

Because parts are immutable and background merges already stream every
row (previous chapter, Step 6), ClickHouse routes ALL maintenance work
through merges: TTL enforcement (expired rows dropped as merges
rewrite parts), dedup (`ReplacingMergeTree`), pre-aggregation
(`Summing`/`AggregatingMergeTree` — the substrate for materialized
views), recompression of cold parts to heavier codecs. Merges are the
system's metabolic cycle — background bandwidth converted into query
speed. (Topic 4's compaction-as-computation, fully weaponized.) Why it
matters: work that OLTP systems do per-write (and pay for in latency)
is batched into sequential IO the system was doing anyway — but it
makes merge bandwidth the resource everything competes for.

### Step 3 — who picks the codec: the user, explicitly

ClickHouse exposes per-column codec CHAINS — `CODEC(Delta, ZSTD)`,
Gorilla/DoubleDelta for time series — and makes the USER declare what
DuckDB's analyze pass discovers automatically. That completes the
three answers to "who chooses the encoding": user-declared
(ClickHouse — zero ingest cost, assumes the operator knows the data),
full-analyze (DuckDB — pays a pass, needs no knowledge), sampled
(BtrBlocks — the middle). Why it matters: it's the same performance
philosophy as `ORDER BY`-at-creation — ClickHouse consistently trades
operator burden for machine efficiency, and the paper is explicit
that this targets operators who profile.

### Step 4 — updates are batch jobs, not transactions

A **mutation** (`ALTER TABLE ... UPDATE/DELETE`) is executed by
asynchronously rewriting every affected part in the background — a
single-row update can rewrite gigabytes, and there is no
read-your-write guarantee on it. This is the honest scope statement:
immutable parts made ingest and scans fast (Steps 1–2), and this is
the bill — point updates became bulk jobs. Why it matters: this is
what "giving up OLTP" concretely means; when a vendor benchmark shows
ClickHouse-class scan numbers, this is the capability that was traded
for them.

### Step 5 — replication ships parts, not rows

Replicas coordinate through Keeper (their RAFT-ish ZooKeeper
replacement) on a shared log of *actions* — "part X was inserted",
"parts Y+Z merged into W" — and fetch whole part files from peers;
shards are shared-nothing on top. Contrast topic 15's menu: redis
ships commands, postgres ships WAL records, ClickHouse ships FILES —
state-machine replication at part granularity. The granularity is
acceptable precisely because parts are immutable (a fetched file is
never patched) and the workload tolerates second-scale replica lag.
Why it matters: replication design is downstream of the storage
design — immutability made the coarsest, simplest unit the right one.

## How to read the paper (with the concepts in hand)

1. **Architecture/storage sections** — skim; you read the code
   (previous chapter). Confirm the part/granule/mark story matches.
2. **The performance discussion** — read as Step 1's bet: where do
   they credit vectorization vs compression vs pruning? Note where
   the sparse index is *not* the hero.
3. **Merge-time features** (TTL, Replacing/Summing/Aggregating,
   recompression) — Step 2; list every job they route through merges.
4. **Codecs** — Step 3; note the time-series specials (Gorilla,
   DoubleDelta) and what they assume about the data.
5. **Mutations** — Step 4; read for the honest limits, not the
   mechanism.
6. **Replication/scaling** — Step 5; watch for what Keeper stores (log
   of part actions, not data).
7. **Evaluation** — skim against your own ClickBench numbers (below);
   vendor evals are hypotheses, yours are measurements.

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
