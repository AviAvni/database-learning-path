# Progress

Two different things get tracked here, and conflating them is misleading:

- **Package** — does `topics/NN-name/` exist and hold up? That means a study
  guide, four to seven reading guides, `notes.md`, and an experiments crate
  whose provided lane runs and whose numbers are recorded. All 45 are built;
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

## Capstone milestones (falkordb-rs-next-gen from scratch)

| Milestone | Depends on topic | Status |
|-----------|------------------|--------|
| M0 workspace + bench harness + reference baselines | 0 | done — workspace + workload gen + smoke bench + BASELINES.md (reference @ e8a44d25) |
| M1 storage-backend abstraction | 1 | todo |
| M2 attribute store + string pool + datablocks | 2 | todo |
| M3 B+tree backend (properties + range indexes) | 3 | todo |
| M4 LSM backend + backend shootout | 4 | todo |
| M5 WAL + crash recovery | 5 | todo |
| M6 buffer pool | 6 | todo |
| M7 RESP server (GRAPH.QUERY wire-compatible) | 7 | todo |
| M8 MVCC copy-on-write graph | 8 | todo |
| M9 threadpool + parallel execution | 9 | todo |
| M10 Cypher parser + binder + planner | 10 | todo |
| M11 vectorized runtime | 11 | todo |
| M12 columnar attribute storage | 12 | todo |
| M13 naive adjacency graph core (baseline) | 13 | todo |
| M14 vector index + distance kernels | 14 | todo |
| M15 replication → Raft | 15 | todo |
| M16 openCypher TCK runner + DST + fuzzing | 16 | todo |
| M17 SIMD kernels | 17 | todo |
| M18 GPU backend (experimental) | 18 | todo |
| M19 Cypher expression JIT | 19 | todo |
| M20 sparse-matrix/delta-matrix core (the heart) | 20 | todo |
| M21 TLA+ spec + Lean invariant proof | 21 | todo |
| M22 LDBC suite + 3-way FalkorDB shootout | 22 | todo |
| M23 full-text index + hybrid search | 23 | todo |
| M24 algorithm library as Cypher procedures | 24 | todo |
| M25 GNN embeddings pipeline + GraphRAG queries | 25 | todo |
| M26 MVCC secondary indexes + bloom + HLL count path | 26 | todo |
| M27 standing Cypher queries (incremental results) | 27 | todo |
| M28 tiered object storage + graph branching | 28 | todo |
| M29 cross-shard transactions + pattern matching | 29 | todo |
| M30 temporal graph + time-travel queries | 30 | todo |
| M31 active-active graph (CRDT merge) | 31 | todo |
| M32 HTAP: changelog-fed analytical replica + freshness-bound routing | 32 | todo |
| M33 time-respecting MATCH + AT TIME/BETWEEN views + temporal path functions | 33 | todo |
| M34 GRAPH.SLOWLOG + per-query perf context + latency histograms behind a PerfLevel dial | 34 | todo |
| M35 priority admission (DAGOR-style queuing-time cursor) + retry-budget hints + plan-time memory gate | 35 | todo |
| M36 slot-sharded graph (hash tags, MOVED/ASK redirects, throttled live slot migration, greedy placement) | 36 | todo |
| M37 distributed queries (scatter-gather over slots, in-engine exchange operator, hedged reads with budget) | 37 | todo |
| M38 GraphRAG layer (entity ingest + resolution, PPR retrieval procedure over CSR, bi-temporal edge versioning with as-of reads) | 38 | todo |
| M39 fraud primitives (dense-block peel procedure over the CSR, write-time identity resolution: blocking indexes + FS match weights + incremental union-find) | 39 | todo |
| M40 attack-path primitives (edge-kind-filtered variable-length reachability, one-pass dominator choke-point procedure over the CSR, Zanzibar-shaped check with a maintained closure index) | 40 | todo |
| M41 provenance & identity (incremental FIFO taint queues in the property layer, maintained union-find cluster index, BlockSci-shaped columnar transaction store for scan queries) | 41 | todo |
| M42 real-time recommendations (GraphJet-style temporal index segments + doubling edge pools, Pixie random-walk procedure with sub-linear step allocation and early stopping, TAO-shaped association-list API) | 42 | todo |
| M43 observability path (trace ingest as an incrementally-maintained dependency graph with sketched edge weights, walk + Ferret localization procedures over the CSR, happened-before join operator with Pivot Tracing pushdown) | 43 | todo |
| M44 relational rewrite stage (e-graph planner pass whose patterns compile to conjunctive queries and run through generic join, a timestamped e-node table driving a semi-naive saturation loop, and one cyclic rewrite pattern measured against a binary-join plan) | 44 | todo |

## Session log

Moved to [SESSION-LOG.md](SESSION-LOG.md) — one detailed entry per topic, newest
first, recording every measured number and what was verified against which paper.
