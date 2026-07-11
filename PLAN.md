# The Plan — Database Internals Curriculum

23 topics, self-paced, deliberately diverse: storage / in-memory / query / graph /
vector / distributed / hardware topics are interleaved so it stays fun. Each topic has: why it
matters, core concepts, reference code to read, key papers, and a build+bench exercise
that also advances the **capstone** (`capstone/README.md`).

Order is a recommendation. Topics 0–6 are the foundation; after that, jump around freely.

---

## 0. The Performance Toolbox

**Why:** You care about performance — so learn to measure before learning to build. Everything after this topic gets benchmarked properly.

- **Concepts:** microbenchmark pitfalls (warmup, variance, coordinated omission), CPU caches & memory hierarchy, branch prediction, TLB, `perf` counters, flamegraphs, latency percentiles vs throughput, roofline thinking.
- **Read code:** `criterion.rs` internals (how it fights noise), RocksDB `db_bench`, redis `redis-benchmark.c`.
- **Papers/reading:** "Systems Performance" (Gregg) ch. 1–2; Andrei Pavlo's benchmarking lecture (CMU 15-721); "How NOT to Measure Latency" (Tene talk).
- **Build & bench:** Rust bench harness comparing `Vec` scan vs `HashMap` lookup vs `BTreeMap` across sizes; produce flamegraphs; observe cache-line effects (seq vs random access).
- **Capstone milestone M0:** scaffold `minidb` workspace + criterion bench harness + YCSB-style workload generator.

## 1. Storage Engine Landscape: B-Tree vs LSM

**Why:** The single most consequential design decision in a database. Frames everything else.

- **Concepts:** read/write/space amplification triangle (RUM conjecture), in-place vs out-of-place updates, page-oriented vs log-structured, where each engine family wins.
- **Read code:** fjall (small, clean Rust LSM), turso (`core/storage/` — SQLite-style B-tree in Rust), tidesdb (C LSM), RocksDB high-level layout.
- **Papers:** "The LSM-Tree" (O'Neil '96), "The Ubiquitous B-Tree" (Comer '79), "Designing Access Methods: The RUM Conjecture" (2016), "Architecture of a Database System" (Hellerstein/Stonebraker).
- **Build & bench:** benchmark fjall vs a raw B-tree (e.g. `redb`/sled) on write-heavy vs read-heavy vs scan workloads; explain results in terms of amplification.
- **Capstone M1:** define `minidb`'s `StorageEngine` trait (get/put/scan/delete) — engines get swapped under it later.

## 2. In-Memory Structures: Hash Tables, Skip Lists, Tries

**Why:** Redis's dict and FalkorDB's core structures — the workhorses of every in-memory DB.

- **Concepts:** open addressing vs chaining, incremental rehashing (redis), SwissTable/SIMD probing (hashbrown), skip lists (why LSM memtables use them), radix trees / ART, cache-conscious layout.
- **Read code:** redis `dict.c` (incremental rehash!) + valkey's changes, redis `t_zset.c` (skiplist), hashbrown, RocksDB `memtable/` (concurrent skiplist), redis `rax.c` (radix tree).
- **Papers:** "The Adaptive Radix Tree" (Leis ICDE'13), Google SwissTable talk (CppCon 2017).
- **Build & bench:** implement a skip list and an incremental-rehash hash table in Rust; bench vs `hashbrown` and `crossbeam-skiplist`; measure rehash latency spikes vs redis-style incremental approach.
- **Capstone M2:** in-memory engine for `minidb` (hash index + ordered skiplist index).

## 3. B-Tree Internals & Paged Storage

**Why:** SQLite/Postgres/LMDB/most-embedded-DBs. Pages are how disks think.

- **Concepts:** slotted pages, node splits/merges, B+tree vs B-tree, prefix compression, copy-on-write B-trees (LMDB), overflow pages, page checksums, varint encoding.
- **Read code:** turso `core/storage/btree.rs` + pager (Rust re-implementation of SQLite — ideal), SQLite `btree.c` (the classic), LMDB `mdb.c` (COW).
- **Papers:** "Modern B-Tree Techniques" (Graefe — the survey), SQLite file-format doc.
- **Build & bench:** implement a slotted-page disk B+tree in Rust (fixed 4KB pages); bench point lookups & range scans vs `redb`; try prefix truncation and measure.
- **Capstone M3:** disk-backed B+tree engine behind the `StorageEngine` trait.

## 4. LSM-Tree Deep Dive

**Why:** RocksDB powers half the industry (including graph DBs like TiKV-based ones). Compaction is a fascinating scheduling problem.

- **Concepts:** memtable→SST lifecycle, leveled vs tiered vs FIFO compaction, bloom filters (and Monkey's optimal allocation), fractional cascading, compaction debt/write stalls, SST formats & block cache.
- **Read code:** fjall (read it ALL — it's small), RocksDB `db/compaction/`, `table/block_based/`.
- **Papers:** "Monkey: Optimal Navigable Key-Value Store" (SIGMOD'17), "Dostoevsky" (SIGMOD'18), RocksDB paper (TODS'21), "Constructing and Analyzing the LSM Compaction Design Space" (VLDB'21).
- **Build & bench:** implement a mini-LSM (memtable + SSTs + leveled compaction + bloom filters) — optionally follow skyzh/mini-lsm course; measure write amp with different compaction strategies.
- **Capstone M4:** LSM engine as third `StorageEngine`; benchmark all three engines against each other.

## 5. Durability: WAL, fsync, Crash Recovery

**Why:** The hardest part to get right. Where correctness meets performance.

- **Concepts:** write-ahead logging, ARIES (redo/undo, LSNs, fuzzy checkpoints), group commit, fsync vs fdatasync vs O_DIRECT, torn pages (full-page writes / double-write buffer), io_uring.
- **Read code:** postgres `xlog.c` (skim, it's huge), turso WAL, redis AOF (`aof.c`) vs RDB, RocksDB WAL.
- **Papers:** "ARIES" (Mohan '92 — read a summary first, then the paper), "Scalability of write-ahead logging on multicore" (Aether, VLDB'10).
- **Build & bench:** add WAL + crash recovery to your B+tree; write a crash-injection test (kill -9 mid-write, verify recovery); bench fsync-per-commit vs group commit vs O_DIRECT.
- **Capstone M5:** WAL + recovery for `minidb`; crash-injection test suite.

## 6. Buffer Pool & Memory Management

**Why:** mmap-vs-buffer-pool is one of the great debates; allocation strategy dominates in-memory DB performance.

- **Concepts:** buffer pool design, eviction (LRU, CLOCK, LRU-K, 2Q), pointer swizzling (LeanStore), why mmap is (usually) wrong for DBs, jemalloc/arena allocation, NUMA.
- **Read code:** postgres `bufmgr.c` + CLOCK sweep, redis `zmalloc.c`, DuckDB buffer manager, LeanStore (C++).
- **Papers:** "Are You Sure You Want to Use MMAP in Your DBMS?" (CIDR'22), "LeanStore" (ICDE'18), "Virtual-Memory Assisted Buffer Management" (vmcache, SIGMOD'23).
- **Build & bench:** build a buffer pool (CLOCK) for the B+tree; bench vs mmap on datasets larger than RAM; reproduce mmap's write-back unpredictability.
- **Capstone M6:** buffer pool under the B+tree engine.

## 7. Networking, Protocols & Event Loops

**Why:** Redis's speed is as much about the event loop and RESP as about data structures. You know the module side of FalkorDB; own the server side.

- **Concepts:** RESP2/RESP3 design (why so parseable), event loops (ae.c) vs thread-per-core vs async, pipelining, io-threads in redis/valkey, pgwire protocol, backpressure.
- **Read code:** redis `ae.c` + `networking.c`, valkey's io-threads rework (great perf PRs to study), `pgwire` (Rust crate), qdrant's gRPC/tonic setup.
- **Papers/reading:** "The C10K problem", valkey blog posts on multithreading perf, Glauber Costa on thread-per-core.
- **Build & bench:** implement a RESP server in Rust (tokio) speaking GET/SET; bench with `redis-benchmark` and `memtier_benchmark` against real redis; find your bottleneck with flamegraphs.
- **Capstone M7:** `minidb` gets a RESP-compatible server front-end.

## 8. Transactions & MVCC

**Why:** The intellectual core of OLTP. Postgres MVCC vs in-memory designs is a masterclass in trade-offs.

- **Concepts:** ACID, isolation levels & anomalies (read this twice), 2PL vs OCC vs MVCC, snapshot isolation & write skew, SSI, postgres tuple versioning + vacuum, HOT updates, timestamp ordering, Hekaton-style MVCC.
- **Read code:** postgres `heapam.c` + visibility rules (`HeapTupleSatisfiesMVCC`), surrealdb transaction layer, RocksDB `utilities/transactions/`.
- **Papers:** "A Critique of ANSI SQL Isolation Levels" (Berenson '95), "Serializable Snapshot Isolation in PostgreSQL" (VLDB'12), "An Empirical Evaluation of In-Memory MVCC" (Wu/Pavlo VLDB'17), "Hekaton" (SIGMOD'13).
- **Build & bench:** implement MVCC with snapshot isolation over your KV engine; write tests that demonstrate (and then prevent) write skew; bench txn throughput vs a single global lock.
- **Capstone M8:** MVCC transactions in `minidb`.

## 9. Concurrency: Latches, Lock-Free & Epochs

**Why:** Scaling a storage engine across cores is where the hardest bugs and biggest wins live.

- **Concepts:** latches vs locks, lock coupling / optimistic lock coupling, lock-free structures & memory reclamation (epochs, hazard pointers), Bw-Tree, atomics & memory ordering in Rust, contention profiling.
- **Read code:** crossbeam-epoch, RocksDB concurrent memtable inserts, memgraph skip-list, postgres lwlock.c.
- **Papers:** "The Bw-Tree" (ICDE'13) + "Building a Bw-Tree Takes More Than Just Buzz Words" (SIGMOD'18 — the reality check), "Optimistic Lock Coupling" (Leis).
- **Build & bench:** make your skip list concurrent (epoch reclamation); bench scaling 1→16 threads; compare mutex-sharded vs lock-free; measure with `perf c2c` for false sharing.
- **Capstone M9:** concurrent access to `minidb` engines; multi-threaded server.

## 10. Query Engines I: Parsing, Planning, Optimization

**Why:** The optimizer is the database's brain. Directly relevant to Cypher planning in FalkorDB.

- **Concepts:** logical vs physical plans, relational algebra rewrites (predicate pushdown, join reordering), cost models & cardinality estimation (where it all goes wrong), dynamic programming join ordering, Cascades framework.
- **Read code:** DuckDB `src/optimizer/` (readable!), postgres `optimizer/` (join search), sqlparser-rs, datafusion optimizer, polars lazy-frame optimizer (`crates/polars-plan/`).
- **Papers:** "Access Path Selection" (Selinger '79 — the founding paper), "How Good Are Query Optimizers, Really?" (VLDB'15 — humbling), "The Cascades Framework" (Graefe '95).
- **Build & bench:** write a mini planner: parse SQL subset → logical plan → apply pushdown + join reordering; verify plans change with table sizes; compare against DuckDB's `EXPLAIN`.
- **Capstone M10:** simple query language + planner for `minidb`.

## 11. Query Engines II: Execution Models

**Why:** Volcano vs vectorized vs compiled — the defining performance battle of modern analytics.

- **Concepts:** iterator (Volcano) model, vectorized execution (X100/DuckDB), query compilation (HyPer), morsel-driven parallelism, hash joins & aggregation internals, SIMD in query processing.
- **Read code:** DuckDB `src/execution/` (vectors, pipelines), polars streaming engine + SIMD compute kernels (`crates/polars-compute/`), datafusion (Arrow-based), postgres `executor/` (classic Volcano).
- **Papers:** "MonetDB/X100: Hyper-Pipelining Query Execution" (CIDR'05), "Everything You Always Wanted to Know About Compiled and Vectorized Queries" (VLDB'18), "Morsel-Driven Parallelism" (SIGMOD'14).
- **Build & bench:** implement the same aggregation query (scan+filter+group-by) three ways: tuple-at-a-time, vectorized (1024-row batches), and with SIMD; bench — the gap is the whole lesson.
- **Capstone M11:** vectorized executor for `minidb` queries.

## 12. Columnar Storage & Analytics

**Why:** DuckDB/ClickHouse-style OLAP. Compression IS performance here.

- **Concepts:** row vs column layout, encodings (RLE, dictionary, bit-packing, delta, FSST for strings), zone maps / min-max pruning, Parquet & Arrow formats, late materialization.
- **Read code:** DuckDB `src/storage/compression/`, polars (Arrow memory layout in practice), arrow-rs, parquet-rs.
- **Papers:** "C-Store" (VLDB'05), "Integrating Compression and Execution in Column-Oriented Database Systems" (SIGMOD'06), "BtrBlocks" (SIGMOD'23), "FSST" (VLDB'20).
- **Build & bench:** implement RLE + dictionary + bit-packing encoders; bench scan speed on encoded vs raw data (decompression can be *faster* than reading raw — verify); run ClickBench queries on DuckDB and profile.
- **Capstone M12:** columnar table format + zone-map pruning in `minidb`.

## 13. Graph Engines (Home Turf, Deeper)

**Why:** Compare FalkorDB's sparse-matrix approach against the alternatives you compete with — with benchmarks.

- **Concepts:** adjacency representations (CSR/CSC, adjacency lists, sparse matrices/GraphBLAS), neo4j's fixed-size record store + pointer chasing, memgraph's in-memory skip-list store, BFS as SpMV, worst-case optimal joins for pattern matching, LDBC benchmarks.
- **Read code:** SuiteSparse:GraphBLAS internals (you know the API — go deeper into masks/complement handling), neo4j record format (`kernel/impl/store/`), memgraph `storage/v2/`, kuzu (WCOJ + columnar graph — very relevant).
- **Papers:** "GraphBLAS: SuiteSparse" (Davis, TOMS), "Kùzu: A Database Management System For 'Beyond Relational' Workloads" (CIDR'23), "EmptyHeaded" (worst-case optimal joins on graphs), LDBC SNB spec.
- **Build & bench:** implement 2-hop neighborhood query over CSR vs adjacency-list vs GrB sparse matrix; bench on LDBC-scale data; compare with FalkorDB and neo4j on the same query.
- **Capstone M13:** property-graph layer on `minidb` (nodes/edges over the KV engine) + basic pattern matching.

## 14. Vector Search

**Why:** qdrant/helix-db territory; every DB is adding this. Beautiful algorithms, very benchmarkable.

- **Concepts:** ANN problem & recall/latency trade-off, HNSW (and its memory hunger), IVF, product quantization, scalar/binary quantization, DiskANN/Vamana for on-disk, filtered search (the hard part — qdrant's specialty).
- **Read code:** qdrant `lib/segment/` (HNSW + filtering + quantization), helix-db vector side, usearch (compact HNSW).
- **Papers:** "HNSW" (arXiv:1603.09320), "Product Quantization" (Jégou PAMI'11), "DiskANN" (NeurIPS'19), qdrant blog on filtered HNSW.
- **Build & bench:** implement HNSW in Rust from the paper; measure recall@10 vs QPS curves against qdrant on ann-benchmarks datasets (sift-1m); add scalar quantization, re-measure.
- **Capstone M14:** vector index type in `minidb` — making it officially multi-model (KV + graph + vector).

## 15. Replication, Consensus & Distribution

**Why:** From single node to system. Raft is table stakes; the interesting part is what each DB does differently.

- **Concepts:** replication topologies (leader/follower, async vs sync), redis/valkey replication + failover, Raft (leader election, log replication, snapshots, membership), consistency models (linearizability → eventual), sharding (hash slots vs ranges).
- **Read code:** valkey `replication.c` + cluster, qdrant raft-based consensus (`consensus/`), openraft or tikv/raft-rs, surrealdb+tikv layering.
- **Papers:** "In Search of an Understandable Consensus Algorithm" (Raft, ATC'14), "ZooKeeper" or "Viewstamped Replication Revisited" (for contrast), Kleppmann DDIA ch. 5, 8, 9 (read thoroughly).
- **Build & bench:** implement Raft leader election + log replication (or work through the raft-rs / talent-plan labs); inject partitions and observe; measure replication-lag impact of fsync policies.
- **Capstone M15:** replicate `minidb`'s WAL to a follower node; then upgrade to Raft.

## 16. Testing & Correctness Engineering

**Why:** The topic that separates hobby DBs from production DBs. Turso and FoundationDB made this their identity.

- **Concepts:** deterministic simulation testing (DST), fault injection, property-based testing (proptest), fuzzing (cargo-fuzz/AFL), metamorphic testing (SQLancer's pivoted queries / TLP), Jepsen & elle (checking linearizability), model checking with TLA+ (taste of), SMT solvers (Z3): proving query rewrites equivalent (Cosette-style), checking optimizer rules and constraint/invariant satisfiability.
- **Read code:** turso's simulator + DST setup (they blog about it), FoundationDB simulation docs, SQLancer, antithesis blog posts, redis `test/` harness, Z3 (`z3.rs` bindings; skim the tactic/solver architecture — treat Z3 itself as a masterclass codebase: it's a high-performance search engine over logic).
- **Papers:** "Testing Database Engines via Pivoted Query Synthesis" (OSDI'20), "Finding Logic Bugs via TLP" (OOPSLA'20), Jepsen analyses (pick redis-raft and a graph DB one), "Z3: An Efficient SMT Solver" (TACAS'08), "Cosette: An Automated Prover for SQL" (CIDR'17).
- **Build & bench:** add proptest model-checking to `minidb` (ops vs an in-memory model oracle); build a mini DST harness (simulated clock + fault-injecting IO layer); fuzz your SST/page parsers; use Z3 to verify two of your topic-10 rewrite rules are equivalent (and to find a counterexample when you break one on purpose).
- **Capstone M16:** full DST + crash-recovery + property test suite over `minidb`. Graduation.

## 17. SIMD & Hardware-Conscious Data Processing

**Why:** The last 10x on a single core. Touched in topic 11 — this is the dedicated deep dive: writing kernels that saturate the CPU.

- **Concepts:** SIMD fundamentals (AVX2/AVX-512 vs ARM NEON/SVE — know both, you're on ARM), autovectorization and why it fails, Rust portable SIMD (`std::simd`) vs intrinsics, branchless selection (masks + compress), SIMD hash probing (SwissTable), SIMD string parsing/comparison, bit-packed decoding at SIMD speed (FastLanes), gather/scatter costs, instruction-level parallelism & dependency chains.
- **Read code:** polars `crates/polars-compute/` kernels, simdjson (the masterclass — read with the paper), hashbrown SIMD group probing, DuckDB compressed-scan kernels, usearch/SimSIMD distance functions, memchr crate.
- **Papers:** "Rethinking SIMD Vectorization for In-Memory Databases" (SIGMOD'15), "Parsing Gigabytes of JSON per Second" (simdjson, VLDB'19), "The FastLanes Compression Layout" (VLDB'23).
- **Build & bench:** write filter-selection and dot-product kernels four ways: naive scalar, autovectorized, `std::simd`, NEON intrinsics; bench with `perf stat` (IPC, vector-lane utilization); then SIMD-ize a bit-packing decoder and compare against topic 12's scalar version.
- **Capstone M17:** SIMD-accelerated kernels in `minidb`'s vectorized executor + vector-index distance functions; keep scalar fallbacks and a bench comparing them.

## 18. GPU Acceleration for Databases

**Why:** GPUs are reshaping analytics, graph algorithms, and vector search — directly relevant to FalkorDB's future (GraphBLAS on GPU exists). Learn when the PCIe tax is worth paying.

- **Concepts:** GPU architecture for DB people (SIMT, warps, occupancy, memory coalescing, shared memory), the data-transfer bottleneck (PCIe vs NVLink vs unified memory on Apple Silicon), GPU hash joins & aggregation, GPU graph processing (Gunrock, cuGraph, GraphBLAST — SpMV on GPU!), GPU vector search (Faiss GPU, cuVS/CAGRA), programming models: CUDA vs Metal vs wgpu/WebGPU (portable, works on your Mac).
- **Read code:** cuVS/RAFT (vector search kernels), libcudf (GPU columnar ops), Gunrock or GraphBLAST (graph frontier expansion), HeavyDB query compilation to GPU, Rust: `wgpu` compute examples, `cudarc`.
- **Papers:** "A Study of the Fundamental Performance Characteristics of GPUs and CPUs for Database Analytics" (Crystal, SIGMOD'20), "Billion-scale similarity search with GPUs" (Faiss, arXiv:1702.08734), "Gunrock" (PPoPP'16), "CAGRA: Highly Parallel Graph Construction for GPU ANN" (ICDE'24).
- **Build & bench:** implement filter+aggregate and batch vector-distance as wgpu compute shaders (runs on Apple Silicon Metal); bench vs your topic-17 SIMD kernels *including transfer time* — find the crossover batch size where GPU wins; run BFS via SpMV on GPU vs SuiteSparse CPU.
- **Capstone M18:** experimental GPU backend for one `minidb` hot path (vector distance scoring or columnar aggregate) behind a feature flag, with CPU-vs-GPU crossover benchmark.

## 19. JIT & Query Compilation

**Why:** The other answer to interpretation overhead (vs vectorization, topic 11). HyPer/Umbra made it famous; SQLite has quietly used a bytecode VM forever; SuiteSparse:GraphBLAS JIT-compiles kernels.

- **Concepts:** interpreter → bytecode VM → native JIT spectrum, SQLite's VDBE, produce/consume compilation model (HyPer), compilation latency vs execution speed (why Umbra built its own IR — "Tidy Tuples"), copy-and-patch compilation, adaptive execution (start interpreting, JIT when hot), LLVM vs cranelift vs hand-rolled backends, expression JIT vs whole-pipeline JIT, postgres's LLVM JIT (and why it's often a regression).
- **Read code:** SQLite `vdbe.c` (bytecode design), postgres `src/backend/jit/llvm/`, cranelift-jit examples, SuiteSparse:GraphBLAS JIT kernel generation (`Source/jit*`), DuckDB's *absence* of a JIT (find the discussions — vectorization as the counter-argument).
- **Papers:** "Efficiently Compiling Efficient Query Plans for Modern Hardware" (Neumann, VLDB'11 — the paper), "Tidy Tuples and Flying Start" (Umbra, VLDBJ'21), "Copy-and-Patch Compilation" (OOPSLA'21), "Adaptive Execution of Compiled Queries" (ICDE'18), "Everything You Always Wanted to Know About Compiled and Vectorized Queries" (VLDB'18 — re-read after topic 11).
- **Build & bench:** JIT-compile filter expressions with cranelift; three-way bench: AST-walking interpreter vs vectorized (topic 11 kernel) vs JIT — including compile time; find the query length/selectivity crossover where each wins.
- **Capstone M19:** cranelift expression JIT in `minidb` with interpreter fallback and a compile-time budget heuristic.

## 20. Sparse Linear Algebra & GraphBLAS Internals (Deep Home Turf)

**Why:** You use the GraphBLAS API daily in FalkorDB — this topic is about owning what's *underneath*: the kernels, formats, and scheduling decisions SuiteSparse makes for you.

- **Concepts:** sparse formats and when SuiteSparse switches between them (CSR/CSC, bitmap, full, hypersparse), SpMV vs SpMSpV, SpGEMM algorithms (Gustavson, hash-based, heap-based), masks/accumulators/semirings as an execution model, push vs pull BFS = SpMV vs masked SpMSpV (direction-optimizing), non-blocking mode & lazy evaluation, FalkorDB's delta-matrix pattern, JIT'd kernels (ties to topic 19), GPU GraphBLAS (ties to topic 18).
- **Read code:** SuiteSparse:GraphBLAS internals — format-switch heuristics, `GB_AxB_*` SpGEMM variants, mask handling; LAGraph algorithm implementations (BFS, triangle counting, PageRank); FalkorDB's own delta-matrix layer with fresh eyes.
- **Papers:** Davis "Algorithm 1000: SuiteSparse:GraphBLAS" (TOMS'19) + the v2 update (TOMS'23), Gustavson '78 (two-pointer SpGEMM), Buluç & Gilbert SpGEMM survey, Beamer "Direction-Optimizing BFS" (SC'12), GraphBLAS C API spec (read cover to cover once).
- **Build & bench:** implement CSR SpMV and Gustavson SpGEMM in Rust; bench vs SuiteSparse on the same matrices (SuiteSparse Matrix Collection); implement direction-optimizing BFS with masks; measure where hypersparse representation pays off.
- **Capstone M20:** replace `minidb`'s M13 adjacency-list graph engine with your own sparse-matrix kernels; benchmark both on LDBC queries — a FalkorDB-vs-neo4j architecture shootout inside your own codebase.

## 21. Formal Methods & Verification

**Why:** Testing (topic 16) finds bugs you imagined; formal methods find the ones you didn't. AWS, MongoDB, and CockroachDB all spec their protocols in TLA+. Also: e-graphs are quietly powering modern query optimizers.

- **Concepts:** SAT → SMT (DPLL(T), theories), Z3's architecture (tactics, e-matching, the congruence closure e-graph), TLA+ & PlusCal (specify, then let TLC model-check), safety vs liveness, refinement, equality saturation with e-graphs (egg) for rewrite-rule optimizers, lightweight formal methods (spec only the scary parts), protocol testing languages (P, Ivy) as a lighter alternative.
- **Read code:** Z3 internals (`src/smt/`, the e-graph — a high-performance search engine over logic), egg (Rust equality saturation — read fully, it's small), published TLA+ specs: Raft (Ongaro's), MongoDB replication, CockroachDB's specs repo.
- **Papers:** "How Amazon Web Services Uses Formal Methods" (CACM'15 — the motivation paper), "egg: Fast and Extensible Equality Saturation" (POPL'21), "Z3: An Efficient SMT Solver" (TACAS'08), Lamport's "Specifying Systems" (part I) + the TLA+ video course, "Cosette" (CIDR'17 — revisit from topic 16).
- **Build & bench:** write a TLA+ spec of `minidb`'s WAL-replication protocol (topic 15) and model-check it — then remove an ack and watch TLC find the data-loss trace; build an expression-rewrite pass with egg and compare plans vs your hand-ordered rules from topic 10.
- **Capstone M21:** TLA+ spec of `minidb` replication (or MVCC visibility) checked by TLC in CI; optional egg-based rewrite stage in the planner.

## 22. Standard Benchmarks: TPC-H, TPC-C, YCSB, LDBC & Friends

**Why:** The industry's shared yardsticks — and their hidden messages. Knowing *what each query actually stresses* turns benchmarks from marketing into engineering tools.

- **Concepts:** OLTP vs OLAP benchmark design, TPC-C (contention, think times, and why nobody runs it honestly), TPC-H choke-point analysis (which of the 22 queries stress joins vs aggregation vs expression eval), TPC-DS, Join Order Benchmark (JOB — real data, real cardinality pain), SSB, YCSB workloads A–F & Zipfian skew, LDBC SNB + Graphalytics (graph), ann-benchmarks (vector), ClickBench, fair-benchmarking methodology & benchmarketing sins, scale factors and data generators.
- **Read code/run:** DuckDB's built-in TPC-H/TPC-DS extensions, BenchBase (CMU), HammerDB, `dbgen`/`dsdgen`, LDBC SNB datagen + driver, go-ycsb/memtier.
- **Papers:** "TPC-H Analyzed: Hidden Messages and Lessons Learned" (Boncz — the choke-point paper, read alongside running it), "Fair Benchmarking Considered Difficult" (DBTest'18), "OLTP-Bench" (VLDB'13), "How Good Are Query Optimizers, Really?" (VLDB'15 — the JOB paper, revisit), LDBC SNB paper.
- **Build & bench:** run TPC-H SF10 on DuckDB and postgres, profile three choke-point queries and explain the gap; run YCSB against redis and your topic-7 RESP server; run LDBC SNB interactive on FalkorDB vs neo4j and analyze where each wins.
- **Capstone M22:** standing benchmark suite for `minidb` — YCSB workloads, a TPC-H query subset, micro-LDBC graph queries, ann-benchmarks recall/QPS — with regression tracking across capstone milestones.

---

## After the plan (ideas backlog)

- Streaming & incremental view maintenance (differential dataflow, Materialize)
- Time-series engines (Gorilla compression, InfluxDB IOx)
- Distributed transactions (Percolator, Calvin, Spanner/TrueTime)
- HTAP architectures (TiDB/TiFlash)
- FPGA / SmartNIC / computational storage offload (beyond GPU)
- CRDTs & local-first sync (interesting contrast to consensus)
