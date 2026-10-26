# Progress

Two different things get tracked here, and conflating them is misleading:

- **Package** — does `topics/NN-name/` exist and hold up? That means a study
  guide, four to seven reading guides, `notes.md`, and an experiments crate
  whose provided lane runs and whose numbers are recorded. All 47 are built;
  `./verify.sh` re-derives every one of their measured lanes.
- **Studied** — have *I* actually worked through the material and the two
  exercise lanes? That is a much smaller number, and it is the honest one.
  `todo` here does not mean the topic is missing; it means the exercises are
  still exercises.

Status values: `todo` → `in progress` → `done`. A takeaway lands when Studied
is done.

| # | Topic | Package | Studied | Takeaway |
|---|-------|---------|---------|----------|
| 0 | The Performance Toolbox | done | done | Benchmarks lie by default: my own cache_ladder measured its own cache footprint until the walker carried state; flamegraph showed 21% of HashMap lookup time is SipHash; DRAM ladder verified at ~1/5/100 ns. |
| 1 | Storage Engine Landscape: B-Tree vs LSM | done | in progress |  |
| 2 | In-Memory Structures: Hash Tables, Skip Lists, Tries | done | todo |  |
| 3 | B-Tree Internals & Paged Storage | done | todo |  |
| 4 | LSM-Tree Deep Dive | done | todo |  |
| 5 | Durability: WAL, fsync, Crash Recovery | done | todo |  |
| 6 | Buffer Pool & Memory Management | done | todo |  |
| 7 | Networking, Protocols & Event Loops | done | todo |  |
| 8 | Transactions & MVCC | done | todo |  |
| 9 | Concurrency: Latches, Lock-Free & Epochs | done | todo |  |
| 10 | Query Engines I: Parsing, Planning, Optimization | done | todo |  |
| 11 | Query Engines II: Execution Models | done | todo |  |
| 12 | Columnar Storage & Analytics | done | todo |  |
| 13 | Graph Engines | done | todo |  |
| 14 | Vector Search | done | todo |  |
| 15 | Replication, Consensus & Distribution | done | todo |  |
| 16 | Testing & Correctness Engineering | done | todo |  |
| 17 | SIMD & Hardware-Conscious Data Processing | done | todo |  |
| 18 | GPU Acceleration for Databases | done | todo |  |
| 19 | JIT & Query Compilation | done | todo |  |
| 20 | Sparse Linear Algebra & GraphBLAS Internals | done | todo |  |
| 21 | Formal Methods & Verification | done | todo |  |
| 22 | Standard Benchmarks: TPC-H, TPC-C, YCSB, LDBC | done | todo |  |
| 23 | Full-Text Search & Inverted Indexes | done | todo |  |
| 24 | Advanced Graph Algorithms & Analytics | done | todo |  |
| 25 | Graph Neural Networks & Graph ML | done | todo |  |
| 26 | Indexing & Probabilistic Data Structures | done | todo |  |
| 27 | Streaming & Incremental View Maintenance | done | todo |  |
| 28 | Cloud-Native & Disaggregated Storage | done | todo |  |
| 29 | Distributed Transactions | done | todo |  |
| 30 | Time-Series Engines | done | todo |  |
| 31 | CRDTs & Multi-Master Replication | done | todo |  |
| 32 | HTAP Architectures | done | todo |  |
| 33 | Temporal Graphs | done | todo |  |
| 34 | Debugging & Production Diagnosis | done | todo |  |
| 35 | Overload Control & Resource Governance | done | todo |  |
| 36 | Sharding, Partitioning & Rebalancing | done | todo |  |
| 37 | Distributed Query Execution | done | todo |  |
| 38 | GraphRAG & Agent Memory (graph use case 1/6) | done | todo |  |
| 39 | Fraud Rings & Identity Graphs (graph use case 2/6) | done | todo |  |
| 40 | Security & Attack Graphs (graph use case 3/6) | done | todo |  |
| 41 | On-Chain & Crypto Analytics (graph use case 4/6) | done | todo |  |
| 42 | Recommendations & Social Graphs (graph use case 5/6) | done | todo |  |
| 43 | Network & IT-Ops Dependency Graphs (graph use case 6/6) | done | todo |  |
| 44 | E-graphs as a Database: Relational E-matching & egglog | done | todo |  |
| 45 | Async I/O & Modern SSDs: io_uring, Out-of-Place Writes, and When They Help | done | todo |  |
| 46 | Graph Queries on Relational Engines: SQL/PGQ, Factorized Execution & Path-Query Planning | done | todo |  |
| 47 | Semi-join Reduction in Practice: Yannakakis, Predicate Transfer & Bitmap Filters | done | todo |  |
| 48 | Memory Tiering: CXL, Disaggregated Memory & Placement | done | todo |  |
| 49 | Open File & Table Formats: Parquet's Successors, Iceberg/Delta/DuckLake, and Transactions on Object Storage | done | todo |  |
| 50 | LLM-Era Query Processing: Semantic Operators, Text-to-Query, and Their Cost | done | todo |  |

## Capstone milestones (falkordb-rs-next-gen from scratch)

| Milestone | Depends on topic | Accept when | Status |
|-----------|------------------|-------------|--------|
| M0 workspace + bench harness + reference baselines | 0 | `capstone/baselines/run.sh` regenerates both engines' JSON and `summarize.py` reproduces BASELINES.md's tables | done — workspace + workload gen + smoke bench + BASELINES.md (C FalkorDB v4.22.2 @ 7b61f511d + Rust FalkorDB v6.0.3 @ c937aa4fa, median+p99+memory, 2026-10-26) |
| M1 storage-backend abstraction | 1 | one storage test suite passes unchanged against an in-memory and a file backend through the same trait | todo |
| M2 attribute store + string pool + datablocks | 2 | seeded property round-trip of 10K entities is lossless; resting `used_memory` at 1M nodes at or below C's 60.1 MiB (BASELINES.md) | todo |
| M3 B+tree backend (properties + range indexes) | 3 | range scans equal a sorted-`Vec` oracle under seeded inserts and deletes; point-lookup p99 recorded beside topic 3's `btree_baseline` at the same size | todo |
| M4 LSM backend + backend shootout | 4 | M1's suite passes on the LSM backend; space, read and write amplification reported beside topic 1's `engine-shootout` numbers | todo |
| M5 WAL + crash recovery | 5 | every one of 1,000 seeded crash points recovers every acknowledged commit and nothing unacknowledged (topic 16's crash-matrix method) | todo |
| M6 buffer pool | 6 | pool p99 below `mmap` p99 on topic 6's `pool_vs_mmap` workload with eviction active | todo |
| M7 RESP server (GRAPH.QUERY wire-compatible) | 7 | falkordb-py against the capstone server reproduces `test_return`'s 86–109 µs floor (BASELINES.md); then an open-loop HdrHistogram p99 | todo |
| M8 MVCC copy-on-write graph | 8 | a snapshot-isolation checker finds no anomaly over 10K seeded transactions; the read-heavy mix beats topic 8's global-mutex `txn_bench` row | todo |
| M9 threadpool + parallel execution | 9 | throughput does not fall from 1 to all cores on topic 9's `scaling` workload — the global mutex there scales backwards | todo |
| M10 Cypher parser + binder + planner | 10 | openCypher TCK pass rate recorded per commit (M16 runner); every LDBC SNB query (M22) parses and plans | todo |
| M11 vectorized runtime | 11 | results equal the M13 tuple-at-a-time path; rows/s beats topic 11's Volcano lane on the same query | todo |
| M12 columnar attribute storage | 12 | column scan GB/s recorded against topic 12's `scan_bench` memory-bandwidth floor; results equal the row store | todo |
| M13 naive adjacency graph core (baseline) | 13 | engine-side create throughput beats C and delete beats Rust, after the ~0.1 ms RESP floor; 1M-edge load peak under C's 886 MiB `used_memory_peak` (BASELINES.md) | todo |
| M14 vector index + distance kernels | 14 | recall@10 ≥ 0.95 at a QPS above topic 14's brute-force floor on the same vectors; filtered queries equal brute force with the filter applied | todo |
| M15 replication → Raft | 15 | a linearizability checker over seeded partitions and leader kills on 3 nodes finds 0 violations and 0 lost acknowledged writes | todo |
| M16 openCypher TCK runner + DST + fuzzing | 16 | TCK runner in CI with the pass rate tracked; any DST failure reproduces from its seed alone | todo |
| M17 SIMD kernels | 17 | each SIMD kernel beats the autovectorized one at every selectivity of topic 17's `simd_bench`, with identical output | todo |
| M18 GPU backend (experimental) | 18 | GPU path chosen only above the crossover topic 18's `gpu_bench` measures; output equals the CPU path | todo |
| M19 Cypher expression JIT | 19 | JIT output equals the interpreter on every TCK expression scenario; JIT chosen only past the compile break-even topic 19 measures | todo |
| M20 sparse-matrix/delta-matrix core (the heart) | 20 | every query equals M13's output; `match_*` at the better of C and Rust at median and p99 (BASELINES.md); SpMV GB/s recorded beside topic 20's kernel | todo |
| M21 TLA+ spec + Lean invariant proof | 21 | TLC checks the WAL and MVCC spec with no invariant violation; the Lean proof builds with no `sorry` | todo |
| M22 LDBC suite + 3-way FalkorDB shootout | 22 | LDBC SNB Interactive runs on capstone, C and Rust FalkorDB; a per-query median and p99 three-way table | todo |
| M23 full-text index + hybrid search | 23 | BM25 top-k identical to topic 23's exhaustive-scoring lane and faster than it | todo |
| M24 algorithm library as Cypher procedures | 24 | PageRank, WCC and triangle counts equal topic 24's oracles on RMAT and uniform graphs; procedure time recorded against C FalkorDB's | todo |
| M25 GNN embeddings pipeline + GraphRAG queries | 25 | embeddings separate SBM blocks by the cosine margin topic 25's test demands (> 0.2); a hybrid vector + pattern query is one round trip | todo |
| M26 MVCC secondary indexes + bloom + HLL count path | 26 | Bloom false-positive rate within its configured target; HLL count within its stated standard error; an MVCC index never returns an invisible version (property test) | todo |
| M27 standing Cypher queries (incremental results) | 27 | standing-query results equal full recompute after every batch; per-batch cost below topic 27's `ivm_bench` full recompute | todo |
| M28 tiered object storage + graph branching | 28 | a graph restored from object storage equals its source; cold and warm p99 reported beside topic 28's `tier_bench` | todo |
| M29 cross-shard transactions + pattern matching | 29 | cross-shard transactions pass a serializability checker under seeded faults; abort rate reported across topic 29's Zipf sweep | todo |
| M30 temporal graph + time-travel queries | 30 | `AT TIME` queries equal a snapshot-replay oracle; bytes per change recorded beside topic 30's delta+varint baseline | todo |
| M31 active-active graph (CRDT merge) | 31 | replicas reach the same state hash under every seeded delivery order; metadata bytes recorded beside topic 31's `crdt_bench` | todo |
| M32 HTAP: changelog-fed analytical replica + freshness-bound routing | 32 | measured replica lag never exceeds the declared freshness bound; OLTP p99 unchanged within run-to-run noise with the replica attached | todo |
| M33 time-respecting MATCH + AT TIME/BETWEEN views + temporal path functions | 33 | time-respecting paths equal topic 33's brute-force oracle; anchor+delta reads beat snapshot replay | todo |
| M34 GRAPH.SLOWLOG + per-query perf context + latency histograms behind a PerfLevel dial | 34 | slowlog and histograms measured open-loop (no coordinated omission); overhead at the lowest PerfLevel measured and recorded | todo |
| M35 priority admission (DAGOR-style queuing-time cursor) + retry-budget hints + plan-time memory gate | 35 | under topic 35's metastable trigger, goodput returns to its pre-trigger level after the trigger is removed | todo |
| M36 slot-sharded graph (hash tags, MOVED/ASK redirects, throttled live slot migration, greedy placement) | 36 | a slot migration moves only the migrating slots' keys (topic 36's mod-N moves almost all); no write lost during migration | todo |
| M37 distributed queries (scatter-gather over slots, in-engine exchange operator, hedged reads with budget) | 37 | hedged reads cut p99 under topic 37's straggler model within the declared extra-request budget | todo |
| M38 GraphRAG layer (entity ingest + resolution, PPR retrieval procedure over CSR, bi-temporal edge versioning with as-of reads) | 38 | PPR retrieval answers topic 38's multi-hop questions where vector-RAG collapses; as-of reads equal a bi-temporal oracle | todo |
| M39 fraud primitives (dense-block peel procedure over the CSR, write-time identity resolution: blocking indexes + FS match weights + incremental union-find) | 39 | dense-block peeling recovers topic 39's planted camouflaged rings; incremental union-find equals batch clustering | todo |
| M40 attack-path primitives (edge-kind-filtered variable-length reachability, one-pass dominator choke-point procedure over the CSR, Zanzibar-shaped check with a maintained closure index) | 40 | reachability and dominator results equal topic 40's oracles; Zanzibar-shaped check equals evaluation without the closure index | todo |
| M41 provenance & identity (incremental FIFO taint queues in the property layer, maintained union-find cluster index, BlockSci-shaped columnar transaction store for scan queries) | 41 | FIFO taint results equal topic 41's oracle; the maintained cluster index equals batch union-find | todo |
| M42 real-time recommendations (GraphJet-style temporal index segments + doubling edge pools, Pixie random-walk procedure with sub-linear step allocation and early stopping, TAO-shaped association-list API) | 42 | Pixie-walk recommendations measured on topic 42's popularity-trap metric beside the baseline; association-list API p99 recorded | todo |
| M43 observability path (trace ingest as an incrementally-maintained dependency graph with sketched edge weights, walk + Ferret localization procedures over the CSR, happened-before join operator with Pivot Tracing pushdown) | 43 | localization names the single injected gray failure that raises topic 43's thirty-four alerts | todo |
| M44 relational rewrite stage (e-graph planner pass whose patterns compile to conjunctive queries and run through generic join, a timestamped e-node table driving a semi-naive saturation loop, and one cyclic rewrite pattern measured against a binary-join plan) | 44 | relational e-matching returns the same matches as a backtracking matcher; the cyclic pattern measured against a binary-join plan | todo |
| M45 async page-read path for the storage backend (an `IoBackend` trait under the M6 buffer pool — sync `pread`, worker pool, io_uring on Linux — chosen at startup; a read stream whose look-ahead doubles on I/O waits; the two cost models predicted before measuring; out-of-place writes with a total-WAF counter) | 45 | the same reads through every `IoBackend` return identical bytes; the WAF counter equals bytes written over bytes requested | todo |
| M46 factorized intermediates for the pattern engine (`Expand` chains kept as packed `Level { vals, off }` siblings with deferred flattening; `count`/`sum` as products of multiplicities; cascade-correct filters property-tested against flat plans; variable-length relationships planned as closures that can be intersected with other edges, under explicit set or bag semantics) | 46 | factorized `count`/`sum` equal flat plans (property test); intermediate size on topic 46's pattern recorded beside its factorized lane | todo |
| M47 semi-join pass for the Cypher planner (forward/backward semi-joins over label and relationship matrices for acyclic `MATCH` patterns with the ≤ OUT bound asserted; exact bitmap or Bloom filter behind one trait with per-hop false-positive and inherited counters; RPT+-style run-time abandon; null-bearing `IN` kept out of the filter path; both anchors benchmarked against C FalkorDB) | 47 | the ≤ OUT bound asserted on every acyclic pattern; intermediate tuples on topic 47's pattern at or below its reduced lane; p99 against C FalkorDB recorded | todo |
| M48 tier-aware placement for the graph store (fast/slow arenas, `mbind` on Linux; vertex renumbering by access frequency with pages-for-90% reported before and after; the partial-move decision before bulk traversals over a slow-tier adjacency; engine metadata pinned to the fast tier) | 48 | pages-for-90% reported before and after renumbering, measured the way topic 48's `tier_bench` does | todo |
| M49 graph snapshots and branches on object storage (immutable segment files + a manifest with per-segment statistics; commits and branch/tag references by conditional put; an O(1)-metadata planner measured with lane 1a's meter; lane 2's conflict rules for concurrent writers to a branch; manifest-merge interval chosen by lane 3's EOQ rule) | 49 | planning reads O(1) metadata objects per snapshot as lane 1a's meter counts; concurrent conflicting commits rejected by lane 2's rules | todo |
| M50 semantic predicates in the planner (`ExpensivePredicate` with declared cost and a paid selectivity sample, rank ordering, LIMIT pushdown, (row, predicate) answer cache, two-threshold cascade with a guaranteed-threshold learner; call counts match lane 1 against a mock model) | 50 | model call counts match lane 1 against a mock model; cached (row, predicate) answers never re-call the model | todo |

## Session log

Moved to [SESSION-LOG.md](SESSION-LOG.md) — one detailed entry per topic, newest
first, recording every measured number and what was verified against which paper.
