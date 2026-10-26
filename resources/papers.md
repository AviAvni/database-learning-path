# Papers, Articles, Books, Courses

Per-topic papers are listed in `PLAN.md`. This file is the consolidated library plus
foundations that span topics.

## Read-first foundations

- **"Architecture of a Database System"** — Hellerstein, Stonebraker, Hamilton (2007). The map of the territory. Read before topic 1.
- **Designing Data-Intensive Applications** — Kleppmann. Best breadth book; ch. 3 (storage), 5, 7–9 are core.
- **Database Internals** — Alex Petrov. The companion book to this whole plan (part I = topics 1–6, part II = topic 15).
- **CMU 15-445** (intro, Pavlo) and **15-721** (advanced) — lectures free on YouTube. 15-721 readings overlap heavily with PLAN.md.
- **The Redbook** (Readings in Database Systems, 5th ed) — redbook.io.

## Classics (by area)

- Storage: O'Neil "LSM-Tree" '96 · Comer "Ubiquitous B-Tree" '79 · Graefe "Modern B-Tree Techniques" · "RUM Conjecture" '16
- Recovery: Mohan "ARIES" '92 · "Aether: Scalable WAL" VLDB'10
- Buffer/memory: "Are You Sure You Want to Use MMAP?" CIDR'22 · "LeanStore" ICDE'18 · "vmcache" SIGMOD'23
- Transactions: Berenson "Critique of ANSI Isolation" '95 · Kung/Robinson "Optimistic Methods for Concurrency Control" TODS'81 · "SSI in PostgreSQL" VLDB'12 · "Hekaton" SIGMOD'13 · Wu/Pavlo "In-Memory MVCC Evaluation" VLDB'17
- Indexing: Leis "ART" ICDE'13 · "Bw-Tree" ICDE'13 + "More Than Buzz Words" SIGMOD'18
- LSM tuning: "Monkey" SIGMOD'17 · "Dostoevsky" SIGMOD'18 · RocksDB FAST'21 (also ACM TOS 17(4)) · "LSM Compaction Design Space" VLDB'21
- Query optimization: Selinger "Access Path Selection" '79 · "How Good Are Query Optimizers, Really?" VLDB'15 · Graefe "Cascades" '95
- Execution: "MonetDB/X100" CIDR'05 · "Compiled vs Vectorized" VLDB'18 · "Morsel-Driven Parallelism" SIGMOD'14 · Neumann "HyPer compilation" VLDB'11
- Columnar: "C-Store" VLDB'05 · "Compression + Execution in Column Stores" SIGMOD'06 · "BtrBlocks" SIGMOD'23 · "FSST" VLDB'20
- Graph: Davis "SuiteSparse:GraphBLAS" TOMS · "Kùzu" CIDR'23 · "EmptyHeaded" · Ngo et al. worst-case optimal joins · LDBC SNB spec
- Vector/ANN: "HNSW" arXiv:1603.09320 · Jégou "Product Quantization" PAMI'11 · "DiskANN" NeurIPS'19
- Distributed: "Raft" ATC'14 · "Viewstamped Replication Revisited" · "Spanner" OSDI'12 · "Percolator" OSDI'10 · "Calvin" SIGMOD'12
- Testing: "SQLancer/PQS" OSDI'20 · "TLP" OOPSLA'20 · Jepsen analyses (jepsen.io/analyses)
- Perspective: "OLTP Through the Looking Glass" SIGMOD'08 · "What's Really New with NewSQL?" '16

## Modern systems & directions (post-2015, dbscholar-audited)

Cross-checked 2026-07-12 against Ryan Marcus's citation-PageRank ranking
(https://rmarcus.info/dbscholar — SIGMOD/VLDB/CIDR/PODS citation graph;
the old blog post is https://rmarcus.info/blog/2023/07/25/papers.html).
Everything below is either linked from a topic README or listed here:

- Query optimization, learned: Kipf "Learned Cardinalities" CIDR'19 · "Neo" VLDB'19 · Marcus "Bao" SIGMOD'21 (→ topic 10)
- Planner/executor as libraries: "Apache Calcite" SIGMOD'18 · "Velox" VLDB'22 (→ topics 10, 11)
- Engines: "Photon" SIGMOD'22 · "Umbra" CIDR'20 · "DuckDB" SIGMOD'19 demo + CIDR'20 (→ topics 6, 11, 19)
- Cloud: "Aurora" SIGMOD'17 · "Snowflake" SIGMOD'16 · "Socrates" SIGMOD'19 · "Lakehouse" CIDR'21 + "Delta Lake" VLDB'20 · "CockroachDB" SIGMOD'20 (→ topics 28, 29)
- Columnar/nested: "Dremel" VLDB'10 (→ topic 12) · "ClickHouse" VLDB'24
- Streaming: "MillWheel" VLDB'13 · "DBSP" VLDB'23 (→ topic 27)
- Time-series: "Gorilla" VLDB'15 · "Monarch" VLDB'20 (→ topic 30)
- HTAP: "TiDB" VLDB'20 · "F1 Lightning" VLDB'20 (→ topic 32)
- Learned indexes: Kraska "The Case for Learned Index Structures" SIGMOD'18 → PGM → ALEX (→ topic 26)
- Graph query languages: "Graph Pattern Matching in GQL and SQL/PGQ" SIGMOD'22 · "G-CORE" SIGMOD'18 (→ topic 13)

## 2025–2026 additions (read in full, 2026-10-26)

Each was downloaded and read before anything was written about it; the topic
README's "Newer work (2025–2026)" section carries the figures with their
section or table. Venue labels follow the PVLDB volume where the paper is
already published there.

- Storage engines (→ 1, 3, 4): "Dynamic read & write optimization with TurtleKV" VLDB'26 — RUM made a runtime knob · "How to Write to SSDs" VLDB'26 — end-to-end write amplification is DB WAF × SSD WAF (→ also 45) · "How Much Can RocksDB Chew? Achieving Near-Zero Write Stalls with Sustainable RocksDB" VLDB'26 — learned admission control instead of stalls · "A Multi-tenant Relational OLTP Database at Salesforce" CIDR'26 — production LSM read paths
- In-memory & concurrency (→ 2, 9): "Arctic: A Practical Lock-Free Adaptive Radix Tree" OSDI'26 — lock-free ART memtable in Rust, hazard-key reclamation
- Durability & buffers (→ 5, 6): "BtrLog: Low-Latency Logging for Cloud Database Systems" VLDB'26 — one-round-trip quorum WAL on remote NVMe · "Sampling-based Predictive Database Buffer Management" PVLDB 18(13) — eviction by forecast next access · "High-Performance DBMSs with io_uring: When and How to Use It" VLDB'26 (→ also 45)
- Networking (→ 7): "Garnet: A Next-Generation Cache-Store" PVLDB 19(2) — RESP server, shared latch-free store, no thread hand-off · "Rearchitecting the Thread Model of In-Memory Key-Value Stores with μTPS" SOSP'25 — thread-per-stage against run-to-completion
- Isolation & testing (→ 8, 16, 21): "Fast Verification of Strong Database Isolation" (VeriStrong) VLDB'26 · "Swan: Hybrid MVCC … in LSM-Tree-Based Key-Value Stores" VLDB'26 · "Dinkel: State-Aware and Granular Framework for Validating Graph Databases" VLDB'26 — 62 RedisGraph bugs · "Testing Graph Databases via Transformations Between Fixed-Length and Variable-Length Queries" (PATHTest) VLDB'26 · "Pisco: An Isolation Bug Case Reduction and Deduplication Framework" VLDB'26
- Planning & execution (→ 10, 11, 19): "Robust Predicate Transfer with Dynamic Execution" VLDB'26 · "I Can't Believe It's Not Yannakakis: Pragmatic Bitmap Filters in Microsoft SQL Server" CIDR'26 · "Survivorship Bias in Industrial Database Workloads" CIDR'26 · "Factorized and Vectorized Execution" SIGMOD'26 · "On the Vexing Difficulty of Evaluating IN Predicates" CIDR'26 · "The Data World Is Not Flat: Efficient Factorized Execution for Relational Systems" VLDB'26
- Columnar & formats (→ 12): "F3: The Open-Source Data File Format for the Future" SIGMOD'26 · "Waiting to Decompress: The Economics of LLM-Based Compression" CIDR'26
- Graph engines & languages (→ 13): "TurboLynx: Schemaless Graph Engine Strikes Back" VLDB'26 · "A Unified Query Planning Framework for Conjunctive Regular Path Queries" VLDB'26 · "Semantic Data Modeling, Graph Query, and SQL, Together at Last?" CIDR'26. Note: SQL/PGQ was committed to PostgreSQL and reverted before 19 (topic 13 README)
- Vector search (→ 14): extended RaBitQ, "Practical and Asymptotically Optimal Quantization of High-Dimensional Vectors for ANN Search" SIGMOD'25 · "Fast Vector Search in PostgreSQL: A Decoupled Approach" CIDR'26 · "Revisiting Filtered ANN Benchmarks: A Hardness-Controlled Benchmark Generator" VLDB'26
- GPU (→ 18): "Rethinking Analytical Processing in the GPU Era" (Sirius) CIDR'26 · "gMatch: Fine-Grained and Hardware-Efficient Subgraph Matching on GPUs" VLDB'26
- Streaming (→ 27): "FlowLog: Efficient and Extensible Datalog via Incrementality" VLDB'26
- Cloud & distributed (→ 28, 29): "Cloudspecs: Cloud Hardware Evolution Through the Looking Glass" CIDR'26 · "Interoperable ACID Transactions for Open Table Formats" (LakeVilla) VLDB'26 · "Tiga: Accelerating Geo-Distributed Transactions with Synchronized Clocks" SOSP'25 · "SunStorm: Geographically distributed transactions over Aurora-style systems" PVLDB 18(13)
- Temporal & dynamic graphs (→ 33): "TVA: A Version-aware Temporal Graph Storage System" VLDB'26 · "Worst-Case Optimal BGPs on Temporal Graphs" VLDB'26 · "RadixGraph" SIGMOD'26

Looked for and not added, because no readable copy was found: cache_ext
(SOSP'25), Fawkes (SOSP'25), ValScope (OSDI'26), Aquila (VLDB'26), CGIF
(VLDB'26).

## arXiv monitoring

Interesting recent arXiv finds go here with a one-line why. Search these when starting a topic:
- cs.DB new submissions: https://arxiv.org/list/cs.DB/recent
- Queries that pay off: "learned index", "LSM compaction", "vector search filtering", "cardinality estimation deep learning", "worst-case optimal join"

<!-- append finds below -->

## Blogs & talks worth following

- Andy Pavlo (databases yearly review) · CedarDB blog · DuckDB blog · turso blog (DST posts)
- Justin Jaffray (query engines) · Phil Eaton (eatonphil.com — builds DBs from scratch)
- Marc Brooker (AWS, distributed systems) · antithesis.com blog (DST)
- Jepsen analyses · valkey engineering blog · qdrant tech blog
