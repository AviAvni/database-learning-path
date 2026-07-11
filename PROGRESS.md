# Progress

Status: `todo` → `in progress` → `done`. Add a one-line takeaway when done.

| # | Topic | Status | Takeaway |
|---|-------|--------|----------|
| 0 | The Performance Toolbox | done | Benchmarks lie by default: my own cache_ladder measured its own cache footprint until the walker carried state; flamegraph showed 21% of HashMap lookup time is SipHash; DRAM ladder verified at ~1/5/100 ns. |
| 1 | Storage Engine Landscape: B-Tree vs LSM | in progress | |
| 2 | In-Memory Structures: Hash Tables, Skip Lists, Tries | todo | |
| 3 | B-Tree Internals & Paged Storage | todo | |
| 4 | LSM-Tree Deep Dive | todo | |
| 5 | Durability: WAL, fsync, Crash Recovery | todo | |
| 6 | Buffer Pool & Memory Management | todo | |
| 7 | Networking, Protocols & Event Loops | todo | |
| 8 | Transactions & MVCC | todo | |
| 9 | Concurrency: Latches, Lock-Free & Epochs | todo | |
| 10 | Query Engines I: Parsing, Planning, Optimization | todo | |
| 11 | Query Engines II: Execution Models | todo | |
| 12 | Columnar Storage & Analytics | todo | |
| 13 | Graph Engines | todo | |
| 14 | Vector Search | todo | |
| 15 | Replication, Consensus & Distribution | todo | |
| 16 | Testing & Correctness Engineering | todo | |
| 17 | SIMD & Hardware-Conscious Data Processing | todo | |
| 18 | GPU Acceleration for Databases | todo | |
| 19 | JIT & Query Compilation | todo | |
| 20 | Sparse Linear Algebra & GraphBLAS Internals | todo | |
| 21 | Formal Methods & Verification | todo | |
| 22 | Standard Benchmarks: TPC-H, TPC-C, YCSB, LDBC | todo | |
| 23 | Full-Text Search & Inverted Indexes | todo | |
| 24 | Advanced Graph Algorithms & Analytics | todo | |
| 25 | Graph Neural Networks & Graph ML | todo | |
| 26 | Indexing & Probabilistic Data Structures | todo | |
| 27 | Streaming & Incremental View Maintenance | todo | |
| 28 | Cloud-Native & Disaggregated Storage | todo | |
| 29 | Distributed Transactions | todo | |
| 30 | Time-Series Engines | todo | |
| 31 | CRDTs & Multi-Master Replication | todo | |

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

## Session log

<!-- newest first: date — what was done -->
- 2026-07-10 — topic 5 scaffolded: study guide (WAL rule, four-designs axis LMDB→turso→postgres→redis-AOF, fsync ladder table, group-commit mermaid), 5 reading guides (postgres xlog.c — newly cloned — reserve-then-copy/XLogFlush-recheck/FPI with line anchors; turso WAL checksum chain + salts; redis aof.c/rdb.c with the AOF-as-LSM mapping + FalkorDB angle; ARIES three passes/CLRs; Aether four-bottleneck taxonomy), experiments crate compiles: fsync_ladder PROVIDED and run (this Mac: fsync 21µs vs F_FULLFSYNC 3.0ms — 140×, the macOS weak-fsync gap is real), Wal `todo!()` stub with format-fixing tests (torn tail, uncommitted-txn invisibility, commit_many = 1 fsync), crash_test kill-9 harness (100 rounds, acked-key + atomicity checks), commit_throughput bench (per-commit vs group 8/64/512).
- 2026-07-10 — topic 4 scaffolded: study guide (memtable→SST lifecycle mermaid, SST block anatomy, leveled/tiered/lazy RUM table, stall triggers, Monkey intuition), 6 reading guides (lsm-tree crate — newly cloned, fjall delegates to it — + RocksDB compaction/table with line anchors; Monkey, Dostoevsky, RocksDB TODS '21, compaction design-space VLDB '21), experiments crate compiles: mini-LSM with provided Bloom (tests pass) + Memtable, SST writer/reader + Lsm engine `todo!()` stubs with correctness tests (tombstone-across-compaction, WA>1 check), write_amp binary measuring the full RUM position of leveled vs tiered.
- 2026-07-10 — topic 3 scaffolded: study guide (slotted page anatomy, 3-sibling balance mermaid, LMDB double-meta COW commit diagram), 5 reading guides (turso btree deep + SQLite btree.c + LMDB mdb.c with line anchors from fresh clones; Graefe survey selective-read map, SQLite file-format hex-dump exercise), experiments crate compiles: slotted Page + DiskBTree `todo!()` stubs with format-fixing tests, bench vs redb (point/scan) + prefix-truncation stress case (32B keys, 24B shared prefix).
- 2026-07-10 — topic 2 scaffolded: study guide (chaining vs open addressing cache stories, incremental-rehash mermaid, skiplist/rax ASCII, dense-filter/fat-payload pattern table), 7 reading guides (redis dict/zset/rax, hashbrown SwissTable, RocksDB InlineSkipList — line numbers from local clones; ART paper, CppCon SwissTable talk), experiments crate compiles: skiplist + incremental_map `todo!()` stubs with tests (the build work is the learning work), benches vs hashbrown/BTreeMap/crossbeam-skiplist, rehash_spike binary (HdrHistogram per-insert max/p99.9).
- 2026-07-10 — topic 1 started: study guide (two-family write/read paths, amplification vocabulary, RUM triangle), 8 reading guides (fjall/turso/tidesdb/rocksdb code + O'Neil/Comer/RUM/Hellerstein papers, line numbers from fresh shallow clones), engine_shootout scaffold (fjall vs redb behind a common trait, db_bench workload names, durability parity) compiles + smoke-tested (space-amp binary at 20K keys shows fixed-overhead floor, not amplification — re-run at 1M+). Topic 0 plan audit: fixed phantom CMU-lecture reference in PLAN.md, added missing roofline-thinking section to topic 0 README §4.
- 2026-07-10 — topic 0 finished: cache_ladder (after fixing a self-caching bug: restarting the pointer chase at 0 measured an 8MB hot path — fixed by carrying the walker across iterations; true ladder 1.0 ns L1 / 5–9 ns L2 / ~110 ns DRAM+TLB), lookup_shootout (HashMap flat at ~7–9 ns thanks to MLP; binary search wins ≤1e4; linear scan never beats hashing at n≥100 — folklore busted), flamegraph captured (21% SipHash in HashMap lookups), reference baselines recorded in capstone/BASELINES.md. Topic 0 + M0 done.
- 2026-07-10 — topic 0 started: study guide + 3 experiment benches (cache_ladder, lookup_shootout, branch_misprediction); capstone workspace scaffolded with `workload` crate (seeded Zipfian generator, ~11M ops/s). First measured result: branchy filter 8.1x slower on shuffled vs sorted data; branchless flat at 15 Gelem/s. Repo published to github.com/AviAvni/database-learning-path.
- 2026-07-10 — repo initialized: plan, capstone design, resources.
