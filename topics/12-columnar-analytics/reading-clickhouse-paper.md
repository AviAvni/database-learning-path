# ClickHouse: the case for brute force

The system paper, fifteen years in: Robert Schulze, Tom Schreiber, Ilya Yatsishin, Ryadh
Dahimene, Alexey Milovidov, *ClickHouse — Lightning Fast Analytics for Everyone*, PVLDB
17(12): 3731–3744, 2024. <https://www.vldb.org/pvldb/vol17/p3731-schulze.pdf>

Read it **after** [reading-clickhouse-mergetree.md](reading-clickhouse-mergetree.md): the
code guide shows you *what* the storage engine does, and this paper supplies the *why*,
plus the parts you did not read code for — pruning beyond the primary key, mutations,
replication, and the benchmark results. Every number below carries the section or figure
it came from.

One vocabulary note, because the paper uses the word without defining it. A **projection**
in ClickHouse is an alternative copy of a table's rows sorted by a *different* key (§3.2) —
the same idea C-Store built its whole design on, which is why
[reading-cstore-compression.md](reading-cstore-compression.md) is worth reading beside
this.

---

## The problem in one sentence

If a vectorized engine can scan compressed columns fast enough that a query over a
100-million-row table is a sub-second *scan*, how much of classical database machinery —
per-row indexes, transactional updates, row-level replication — should you simply refuse to
build?

The paper does not frame itself as "brute force"; §1 lists **five key challenges** it claims
to address: (1) huge data sets with high ingestion rates, (2) many simultaneous queries
expecting low latency, (3) diverse data stores, locations and formats, (4) a convenient
query language with performance introspection, and (5) industry-grade robustness and
versatile deployment. Read the rest of the paper as five answers, and notice how many of
them are the same answer: *make the immutable sorted part the unit of everything*.

---

## The concepts, step by step

### Step 1 — The bet: pruning is a menu, and the headline benchmark barely orders from it

> **In:** a table of 100 million rows and a filter.
> **Out:** the three pruning techniques ClickHouse offers, and the measured fact that its
> best-known benchmark result uses almost none of them.

§3.2 lists exactly three ways to avoid reading rows:

1. **The sparse primary key index.** One entry per granule, locally clustered — the
   structure you read code for. The paper's own sizing: "only 1000 entries are required to
   index 8.1 million rows". Check it: `8,100,000 / 8192` = **988.8** entries. The number is
   arithmetic, not a benchmark.
2. **Projections** — "alternative versions of a table that contain the same rows sorted by
   a different primary key", which speed up queries filtering on a non-key column "at the
   cost of an increased overhead for inserts, merges, and space consumption". Two details
   make them affordable where C-Store's were not: they are "populated lazily only from
   parts newly inserted into the main table" unless you materialize them in full, and "the
   query optimizer chooses between reading from the main table or a projection based on
   estimated I/O costs", falling back to the main table for any part that lacks one.
3. **Skipping indices** — metadata over *multiple consecutive granules*, with a
   configurable number of granules per index block. Three types: **min-max** (a **zone map**:
   the minimum and maximum of an index expression per block, good for "locally clustered
   data with small absolute ranges"), **set** (a bounded number of distinct values per
   block, for "clumped together" values), and **Bloom filter** (row, token or n-gram, with
   a configurable false-positive rate — and, unlike the other two, "cannot be used for
   range or negative predicates").

Now the part that makes this step worth its own place. §6.2.1 states how the ClickBench
results were produced: "The physical database design is tuned only lightly, for example, we
specify primary keys, but **do not** change the compression of individual columns, create
projections, or skipping indexes."

So the benchmark ClickHouse wins is run with the sparse index and the default LZ4, and with
options 2 and 3 switched off. That is the brute-force claim, stated by the authors against
their own interest, and it is the strongest evidence in the paper for the thesis of this
topic: if scanning is cheap enough, most index machinery is optional.

### Step 2 — What "cheap enough" costs, on the paper's own hardware

> **In:** the ClickBench setup in §6.2.1.
> **Out:** the actual bandwidth ceiling those numbers were measured against, and why
> "GB/s" is ambiguous until you say which bytes.

§6.2.1 gives the machine: a single-node AWS EC2 **c6a.4xlarge** — **16 vCPUs, 32 GB RAM,
5000 IOPS / 1000 MiB/s disk** — running **43 queries** against a table of **100 million**
anonymized page hits, with the Linux page cache flushed before each *cold* run.

Read those three numbers together, because they decide everything:

| Resource | Ceiling | Per vCPU |
| --- | --- | --- |
| Disk | 1000 MiB/s ≈ **1.05 GB/s** | 65.5 MB/s |
| RAM | 32 GB — smaller than many ClickBench columns uncompressed | — |
| Cores | 16 | — |

A cold query on that instance cannot exceed **~1 GB/s of compressed bytes off disk**, no
matter how many cores are scanning. `FINDINGS.md` row 12 records this topic's measured
in-memory scan floor at **24–57 GB/s on a machine with roughly 150 GB/s of memory
bandwidth** — between **24× and 57×** the c6a.4xlarge's disk rate. So on a cold run the
scan engine is idle most of the time and compression ratio is the only lever that matters:
at a 5× ratio, 1.05 GB/s of disk feeds 5.25 GB/s of logical rows; at 10×, 10.5 GB/s. On a
hot run the page cache removes the disk entirely and the memory-bandwidth story of
`FINDINGS.md` row 12 takes over. This is why the paper reports **cold and hot geometric
means separately** (Figure 10) — they are measuring two different machines.

The discipline this forces is the one this whole topic is about: **say which bytes you
counted.** "1 GB/s" (compressed, off disk) and "5 GB/s" (logical, after decoding) can
describe the identical query. `FINDINGS.md` row 12 also preserves this topic's own
cautionary figure — a hoisted timing loop once printed **19,047,619 GB/s**, roughly
**127,000×** the machine's peak memory bandwidth. A throughput that exceeds the hardware is
never a discovery; it is a bug in the measurement.

### Step 3 — Not an LSM hierarchy: all parts are equal, and there is no WAL

> **In:** an `INSERT`.
> **Out:** a part on disk, and two deliberate departures from the LSM designs of topic 4.

§3.1 says a part "is created whenever a set of rows is inserted", parts are
"self-contained… include all metadata required to interpret their content without
additional lookups to a central catalog", and merges continue "until a configurable part
size is reached (**150 GB** by default)" — the same number as
`MergeTreeSettings.cpp:475`'s `max_bytes_to_merge_at_max_space_in_pool`.

Two sentences in §3.1 are the ones to underline, because both contradict the LSM mental
model you brought from topic 4:

- **"ClickHouse treats all parts as equal instead of arranging them in a hierarchy. As a
  result, merges are no longer limited to parts in the same level."** No L0/L1/L2. The
  paper immediately names the price: "Since this also forgoes the implicit chronological
  ordering of parts, alternative mechanisms for updates and deletes not based on tombstones
  are required (see Section 3.4)." A tombstone only works if you can tell which record is
  newer, and a flat pile of parts cannot. Step 5 is the consequence.
- **"ClickHouse writes inserts directly to disk while other LSM-tree-based stores typically
  use write-ahead logging."** And §3.7 completes it: ClickHouse does "not forcing a commit
  (`fsync`) of newly inserted parts to disk by default, allowing the kernel to batch writes
  at the cost of forgoing atomicity", justified because "most of ClickHouse's write-heavy
  decision making use cases even tolerate a small risk of losing new data in case of a
  power outage".

Also worth having in hand: clients "are encouraged to insert tuples in bulk, e.g. 20,000
rows at once", and an **asynchronous insert mode** exists that buffers rows from many
`INSERT`s server-side and forms a part on a size or time threshold — the answer to
"thousands of monitoring agents continuously sending small amounts of event data".

§3.5 adds a nice piece of engineering economy: idempotent inserts, implemented by keeping
"hashes of the N last inserted parts (e.g. **N=100**)" and ignoring re-inserts of a known
hash. The paper explicitly contrasts this with per-tuple uniqueness indexes, whose "space
and update overhead becomes prohibitive for large data sets and high ingest rates". A
100-entry hash set replaces a billion-key index because the *part*, not the row, is the
unit of identity.

### Step 4 — Everything happens at merge time

> **In:** a merge that is already streaming every row through memory.
> **Out:** a list of maintenance jobs that ride along for free, and the guarantee they give
> up.

Because parts are immutable and merges already touch every row, ClickHouse routes
maintenance through them (§3.3):

| Merge strategy | What it does |
| --- | --- |
| **Replacing** | keeps only the newest version of a tuple (by containing part's creation timestamp, or an explicit version column); "commonly used as a merge-time update mechanism" |
| **Aggregating** | collapses rows with equal primary key values into a **partial aggregation state** — e.g. a sum and a count for `avg()` — combined pairwise as merges proceed |
| **TTL** | processes one part at a time; actions are: move the part to another volume, **re-compress** it with a heavier codec, delete it, or roll it up by aggregating |

Aggregating merges are the substrate for materialized views, and the paper is precise about
what makes them different from everyone else's: "Unlike other databases, ClickHouse does
not refresh materialized views periodically with the entire content of the source table.
Materialized views are rather updated **incrementally** with the result of the
transformation query when a new part is inserted into the source table." The `-State` /
`-Merge` function suffixes are the user-visible seam: `avgState()` emits a partial
aggregate, `avgMerge()` folds the partials into an answer.

The honest limitation, stated in §3.3: "Merge-time data transformation does not compromise
the performance of `INSERT` statements, **but it cannot guarantee that tables never contain
unwanted (e.g. outdated or non-aggregated) values**. If necessary, all merge-time
transformations can be applied at query time by specifying the keyword `FINAL`." So
`ReplacingMergeTree` does not give you a unique key; it gives you a promise of eventual
deduplication, plus a `FINAL` escape hatch that pays the cost per query instead.

**Why it matters:** work that OLTP systems do per write — and pay for in write latency — is
batched into sequential I/O the system was doing anyway. But it makes **merge bandwidth the
resource everything competes for**, which is the failure mode Step 7 of the code guide put
numbers on (`parts_to_delay_insert` = 1000, `parts_to_throw_insert` = 3000).

### Step 5 — Updates are batch jobs, not transactions

> **In:** an `ALTER TABLE … UPDATE` or `DELETE`.
> **Out:** two mechanisms, and the exact guarantee each provides.

§3.4 opens by conceding the point — "The design of the MergeTree\* table engines favors
append-only workloads, yet some use cases require to modify existing data occasionally,
e.g. for regulatory compliance" — and then offers two mechanisms, "neither of which block
parallel inserts".

**Mutations** "rewrite all parts of a table in-place". Read the guarantee carefully,
because it is easy to overstate in either direction:

- They are **not atomic**: "to prevent a table (delete) or column (update) from doubling
  temporarily in size, this operation is non-atomic, i.e. parallel `SELECT` statements may
  read mutated and non-mutated parts."
- They *are* durable in the end: "Mutations guarantee that the data is physically changed
  at the end of the operation."
- They are expensive in a specific way: "Delete mutations are still expensive as they
  rewrite **all columns in all parts**" — an update touches one column's files, a delete
  touches every column's.

**Lightweight deletes** "only update an internal bitmap column", and ClickHouse "amends
`SELECT` queries with an additional filter on the bitmap column". Physical removal waits for
"regular merges at an unspecified time in future". The trade is stated plainly: "Depending
on the column count, lightweight deletes can be much faster than mutations, at the cost of
slower `SELECT`s."

And the scope statement that should end any argument about using ClickHouse for OLTP:
"Update and delete operations performed on the same table are expected to be **rare and
serialized** to avoid logical conflicts."

§3.7 completes the picture: queries run against a snapshot of all parts taken at query
start, with reference counting to keep them alive — "formally, this corresponds to snapshot
isolation realized by an MVCC variant based on versioned parts" — but "statements are
generally **not ACID-compliant** except for the rare case that concurrent writes at the time
the snapshot is taken each affect only a single part."

**Why it matters:** this is what "giving up OLTP" concretely means. When a vendor benchmark
shows ClickHouse-class scan numbers, this paragraph is the capability that was traded for
them.

### Step 6 — Replication ships state transitions, and sometimes recomputes instead

> **In:** a cluster of nodes and a stream of local operations.
> **Out:** a replicated table, and the granularity choice that made it simple.

§3.6: replication is based on **table states**, "which consist of a set of table parts and
table metadata". Nodes advance a state with exactly three operations — inserts add a part;
merges add one and delete several; mutations and DDL add, delete and/or change metadata.
Each is "performed locally on a single node and recorded as a sequence of state transition
in a global replication log".

The log lives in **ClickHouse Keeper** — "typically three" processes, using the **Raft**
consensus algorithm, described in §2 as "a drop-in replacement for Apache Zookeeper written
in C++" and coordinating a multi-master scheme. Replicas replay the log **asynchronously**,
so "replicated tables are only eventually consistent, i.e. nodes can temporarily read old
table states while converging towards the latest state" — though operations can optionally
run synchronously until a quorum adopts the new state.

Compare with topic 15's menu: Redis ships commands, Postgres ships WAL records, ClickHouse
ships a log of *part-level actions* plus the part files themselves. But there is a nuance
that the one-line version of this story drops. §3.6's three optimizations:

1. New nodes do **not** replay the log from scratch — "they simply copy the state of the
   node which wrote the last replication log entry."
2. "**Merges are replayed by repeating them locally or by fetching the result part from
   another node.** The exact behavior is configurable and allows to balance CPU consumption
   and network I/O. For example, cross-data-center replication typically prefers local
   merges to minimize operating costs." So it is not always file shipping — the log entry
   is a *description* of the transition, and each replica chooses whether to re-derive it
   or download it.
3. Mutually independent log entries are replayed in parallel.

**Why it matters:** replication design is downstream of storage design. Immutability is
what makes a coarse unit safe — a fetched part file is never patched afterwards — and
determinism is what makes "recompute instead of download" a legal substitution. Neither
option would exist if parts were mutable.

### Step 7 — What the benchmarks actually show, including the losses

> **In:** §6.2's three benchmark families.
> **Out:** where ClickHouse wins, where it does not, and what is actually responsible.

**ClickBench** (§6.2.1): 43 queries, 100 million page hits, c6a.4xlarge, lightly tuned as
described in Step 1. Figure 10 compares cold and hot geometric means against MySQL,
PostgreSQL, Druid, Pinot, Umbra, Snowflake (size S) and Redshift (ra3.4xlarge). The result,
in the paper's own words: "**While the research database Umbra achieves the best overall
hot runtime**, ClickHouse outperforms all other production-grade databases for hot and cold
runtimes." Note the shape of that sentence — it is a claim about *production* systems, and
it concedes first place on hot runtime to a research system.

**VersionsBench** (§6.2.1): four benchmarks (ClickBench; 15 MgBench queries; 13 queries on a
600-million-row denormalized Star Schema Benchmark fact table; 4 queries on 3.4 billion NYC
Taxi rides), run monthly across **77 releases from March 2018 to March 2024**. Result:
"The performance of VersionBench improved by **1.72×** over the past six years" — which is
`1.72^(1/6)` = **9.5% per year**, compounding, an honest and unglamorous number. The
biggest single jump, August 2022, "was caused by the column-by-column filter evaluation
technique described in Section 4.4": evaluating filters sequentially in descending estimated
selectivity so each predicate sees fewer rows, applied "only when at least one highly
selective predicate is present; otherwise, the latency of the query would deteriorate".

**TPC-H at scale factor 100** (§6.2.2), on a c6i.16xlarge (64 vCPUs, 128 GB RAM) against
Snowflake at warehouse size L. This is where the losses live, and they are catalogued:

| Outcome | Queries | Count | Reason given |
| --- | --- | --- | --- |
| Excluded | Q2, Q4, Q13, Q17, Q20, Q21, Q22 | 7 | correlated subqueries "which are not supported as of ClickHouse v24.6" |
| Excluded | Q7, Q8, Q9, Q19 | 4 | need join reordering and join predicate pushdown, "both missing as of ClickHouse v24.6" |
| Faster in ClickHouse | — | 5 | — |
| Faster in Snowflake | — | 6 | — |

The arithmetic closes: `7 + 4` = **11 excluded**, `22 − 11` = **11 run**, `5 + 6` = 11. So
**half the benchmark could not be executed at all**, and the half that ran is a near tie.

The important thing is the *cause*. Not the sparse index, not compression, not the scan —
the **query optimizer**: correlated-subquery decorrelation and join reordering, both
acknowledged as missing and both "planned for implementation in 2024". §6.2.2 opens by
saying so itself — "normalized tables are an emerging use case for ClickHouse". A system
built on "make scanning fast" has, predictably, the weaknesses of a system that did not
spend its first decade on join planning.

### Step 8 — "For everyone": four deployment modes, one of them credited to DuckDB

> **In:** the paper's title.
> **Out:** what "for everyone" actually names, and what it concedes.

§2 lists four operating modes: **on-premise** (single server or sharded/replicated
cluster), **cloud** (ClickHouse Cloud, deferred to a follow-up paper), **standalone** —
"turns ClickHouse into a command line utility for analyzing and transforming files, making
it a SQL-based alternative to Unix tools like `cat` and `grep`" — and **in-process**, chDB,
"for interactive data analysis use cases like Jupyter notebooks with Pandas dataframes".

The sentence to notice: "**Inspired by DuckDB**, chDB embeds ClickHouse as a
high-performance OLAP engine into a host process… this allows to pass source and result
data between the database engine and the application efficiently without copying as they
run in the same address space."

A paper claiming a general-purpose analytics engine names a single-file embedded database
as the design it is copying for one whole deployment mode. That is not a threat to DuckDB's
niche; it is a citation of it — and it tells you the niche is real enough that the
big-cluster system had to grow into it.

---

## How to read the paper (with the concepts in hand)

Budget about two hours. Order:

1. **§1** — the five challenges. Two pages, and the frame for everything else.
2. **§3.1–3.2** — skim; you read the code (previous chapter). Confirm the part/granule/mark
   story matches, and note the three numbers that appear in both: 10 MB Compact-part
   threshold, 1 MB block size, 150 GB maximum merged part.
3. **§3.3** — Step 4. List every job routed through merges. Six, if you count re-compression
   and roll-up separately.
4. **§3.4, §3.7** — Steps 5. Read for the *limits*, not the mechanism.
5. **§3.6** — Step 6. Watch for what Keeper stores: a log of part actions, never data.
6. **§4.4** — the densest section in the paper. Its "Primary key index evaluation"
   paragraph is the prose version of the two search algorithms you found in
   `MergeTreeDataSelectExecutor.cpp` — "the range is split into sub-ranges which are
   analyzed recursively" is `merge_tree_coarse_index_granularity` at
   `src/Core/Settings.cpp:1593`. Also note the monotonicity and preimage tricks
   (`toYear(k) = 2024` rewritten as `k >= 2024-01-01 && k < 2025-01-01`) and the "over 30"
   hash table variants.
7. **§6.2** — Step 7. Read the setup paragraphs before the graphs; the tuning disclosure is
   more informative than the bars.
8. **§5** (integration layer) and **§4.5** (workload isolation) — skim unless you have a
   specific interest.

---

## The experiments to run alongside

This topic's "run something real". The point is to replace the paper's numbers with your
own on your own hardware.

```bash
# duckdb + clickbench slice (see ../duckdb-clickbench.md notes file):
# 1. grab hits.parquet sample; run Q0/Q3/Q8/Q13/Q20 in duckdb
# 2. EXPLAIN ANALYZE each: note rows pruned by zone maps
# 3. PRAGMA storage_info('hits'): which compression per hot column?
# record all of it in notes.md
```

Two things to record beyond the runtimes, because they are what makes the numbers
interpretable:

- **Your disk's sequential read rate and your RAM bandwidth.** The paper's instance is
  capped at 1000 MiB/s (§6.2.1); yours is probably very different, and the cold/hot gap you
  measure is a direct function of that ratio.
- **Which GB/s you are quoting.** Compressed bytes read, or logical rows processed? Write
  both into `notes.md`. `FINDINGS.md` row 12 is this topic's reference for what the local
  machine can actually do (24–57 GB/s against ~150 GB/s of memory bandwidth), and any
  figure above that ceiling is a bug, not a result.

---

## Questions for notes.md

1. **Where does ClickHouse barely win — or lose?** Use §6.2.2's TPC-H table: which queries
   were excluded and why, and of those that ran, what is the score against Snowflake? Is
   the sparse index ever the named cause, or is it always something else? Name the two
   optimizer features the paper admits are missing.
2. **Merge starvation.** Merges do TTL, dedup and aggregation (§3.3). What is the failure
   mode when merge bandwidth cannot keep up with ingest? Put the code guide's thresholds on
   it (`parts_to_delay_insert` = 1000, `parts_to_throw_insert` = 3000) and name the topic 4
   stall mechanism this is the analogue of.
3. **Part-shipping replication.** What does it give up versus WAL shipping — think
   replication lag granularity and partial-part visibility — and why is that acceptable for
   analytics? Then account for §3.6's second optimization: when a replica *recomputes* a
   merge instead of fetching it, what property of merges is being relied on, and what would
   break it?
4. **M12 decision.** User-declared codecs (ClickHouse) versus analyze-and-score (DuckDB)
   versus sampling (BtrBlocks): which would you ship for a **graph** database where property
   columns arrive via `MERGE` statements with unknown distributions? Commit to one and note
   why. Consider that §3.1's `LowCardinality(T)` is itself a *declared* dictionary encoding
   — does that change your answer?
5. **The "for everyone" claim.** What did they add to serve small and embedded use
   (`clickhouse-local`, chDB — §2), and does it threaten DuckDB's niche or validate it? The
   paper's own phrasing about chDB is the evidence; quote it.

---

## Takeaway

The two-sentence thesis: immutable sorted parts make ingest sequential and every read-side
structure simple, and merges convert background bandwidth into query speed; indexes are
sparse because a vectorized scan over a granule is cheap enough that finding the exact row
is not worth the machinery. Everything the paper concedes — non-atomic mutations, eventual
consistency, no ACID, a join optimizer that cannot run half of TPC-H — is the bill for that
design, and it is presented as such rather than hidden.

The transferable lesson is not "brute force wins". It is that ClickHouse chose one unit —
the part — and made it the unit of insertion, merging, deduplication, TTL, replication,
snapshot isolation and identity. The simplicity compounds. When you are designing a storage
layer, the question worth asking is not "which index?" but "what is the unit, and how many
jobs can it do?"

---

## Done when

Answer each before unfolding it.

- [ ] State the ClickHouse thesis in two sentences, then name the single design decision
      that the other four sections are consequences of.

<details><summary>Answer</summary>

Two sentences: *Tables are sets of immutable parts sorted by a declared key, so inserts are
sequential file writes and background merges do all maintenance — dedup, aggregation, TTL,
re-compression — while streaming rows they were reading anyway. Indexes are sparse (one key
per 8192-row granule) because a vectorized scan of a granule is cheap enough that locating
the exact row is not worth the space or update cost.*

The decision everything else follows from is **immutability of the part**. Merges can do
work because parts never change (§3.3). Mutations are expensive rewrites for the same
reason (§3.4). Replication can ship whole files, or re-derive them locally, because a part
is deterministic and never patched (§3.6). Snapshot isolation is reference-counting on
versioned parts (§3.7). Idempotent inserts are a 100-entry hash set over parts rather than
an index over rows (§3.5).

</details>

- [ ] ClickBench is the benchmark ClickHouse leads. Which of its own pruning features does
      §6.2.1 say were switched off, and why does that strengthen rather than weaken the
      result?

<details><summary>Answer</summary>

§6.2.1: "The physical database design is tuned only lightly, for example, we specify primary
keys, but do not change the compression of individual columns, create projections, or
skipping indexes." So of §3.2's three pruning techniques, only the first — the sparse
primary key index — is in play; **projections and skipping indices are off**, and the
default LZ4 codec is used everywhere.

It strengthens the result because the claim being tested is precisely that scanning is
cheap enough to make the rest optional. Winning with the optional machinery disabled is
evidence for the thesis; winning with it enabled would only show that indexes work.

The honest caveat in the same section: Umbra, a research system, still posts the best hot
geometric mean. ClickHouse's claim is bounded to production-grade databases.

</details>

- [ ] ClickHouse could not run 11 of the 22 TPC-H queries. What was missing, and what does
      that tell you about where a decade of engineering went?

<details><summary>Answer</summary>

Two gaps, both named in §6.2.2 as of v24.6. **Correlated subqueries** are unsupported,
excluding Q2, Q4, Q13, Q17 and Q20–Q22 — seven queries. **Join reordering and join predicate
pushdown** are missing, so Q7–Q9 and Q19 "depend on extended plan-level optimizations… to
achieve viable runtimes" — four more. That is 11 of 22 excluded. Of the 11 that ran, 5 were
faster in ClickHouse and 6 in Snowflake (warehouse size L) — a near tie, on a normalized
schema.

What it tells you: none of the failures are scan failures. The sparse index, the codecs and
the vectorized engine are not implicated anywhere. The gaps are all **plan-level
optimization** — the part of a database you need when data is normalized and queries have
joins, which is exactly the workload ClickHouse spent fifteen years not having. §6.2.2 says
"automatic subquery decorrelation and better optimizer support for joins are planned for
implementation in 2024".

</details>

- [ ] `ReplacingMergeTree` deduplicates by primary key. Why is that not a unique
      constraint, and what does it cost to get one?

<details><summary>Answer</summary>

Because deduplication happens **when a merge happens to run**, and merges are asynchronous
and unscheduled. §3.3 says it directly: merge-time transformation "cannot guarantee that
tables never contain unwanted (e.g. outdated or non-aggregated) values". Between an insert
and the merge that collapses it, a `SELECT` will see both versions.

The escape hatch is `FINAL` in the `SELECT`, which "applies all merge-time transformations
at query time" (§3.3) — correct results, paid per query instead of once per merge.

There is a structural reason it cannot be cheaper. §3.1 notes that ClickHouse "treats all
parts as equal instead of arranging them in a hierarchy" and thereby "forgoes the implicit
chronological ordering of parts", which is why "alternative mechanisms for updates and
deletes not based on tombstones are required". A tombstone needs to know which record is
newer; a flat set of parts does not carry that. Replacing merges substitute the part's
creation timestamp, or an explicit version column you supply.

</details>

- [ ] A replica needs to catch up on a merge another node performed. Give both ways it can
      do that and the property that makes the choice legal.

<details><summary>Answer</summary>

§3.6, optimization 2: "Merges are replayed by **repeating them locally** or by **fetching
the result part from another node**. The exact behavior is configurable and allows to
balance CPU consumption and network I/O. For example, cross-data-center replication
typically prefers local merges to minimize operating costs."

The property that makes them interchangeable is that a merge is a **deterministic function
of its immutable inputs**: given the same source parts and the same merge strategy, every
replica produces the same output bytes. Immutability guarantees the inputs are identical;
determinism guarantees the outputs are.

This is also the boundary condition worth naming: anything non-deterministic inside a merge
— a wall-clock reference in a TTL expression, a random tie-break in a replacing merge —
would break the substitution and force file shipping. Which is why the replication log
records *state transitions* ("parts Y+Z merged into W"), not the data itself.

</details>

---

## References

**Papers**

- Robert Schulze, Tom Schreiber, Ilya Yatsishin, Ryadh Dahimene, Alexey Milovidov.
  *ClickHouse — Lightning Fast Analytics for Everyone*. PVLDB 17(12): 3731–3744, 2024.
  <https://www.vldb.org/pvldb/vol17/p3731-schulze.pdf>
  Read §1, §3, §4.4 and §6.2 closely; skim the rest.

**Code**

- [ClickHouse](https://github.com/ClickHouse/ClickHouse) — the code side is covered by
  [reading-clickhouse-mergetree.md](reading-clickhouse-mergetree.md), pinned at
  `ClickHouse/ClickHouse@4d598fb2c`. The paper's §3.1 numbers (10 MB Compact threshold,
  1 MB block, 150 GB max merged part) all appear there as settings defaults.
- [ClickBench](https://github.com/ClickHouse/ClickBench) — the 43 queries and the public
  results dashboard for over 45 systems.

**In this topic**

- [reading-cstore-compression.md](reading-cstore-compression.md) — projections, twenty years
  earlier, and why C-Store could not afford them
- [reading-duckdb-compression.md](reading-duckdb-compression.md) and
  [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md) — the other two answers to "who
  picks the encoding", for question 4
- `FINDINGS.md` row 12 — the measured scan floor (24–57 GB/s on a ~150 GB/s machine) and the
  19,047,619 GB/s hoisted-loop bug
