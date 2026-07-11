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
- 2026-07-10 — topic 12 scaffolded: study guide (row-vs-column ASCII, lightweight-encoding zoo table incl. FSST, analyze→score→compress lifecycle, zone-map pruning diagram, Arrow-vs-Parquet boundary, MergeTree/DuckDB/Pinot architecture table), 6 reading guides (DuckDB compression framework + 4-mode bitpacking + fetch_row-shapes-the-menu + CheckZonemap; ClickHouse MergeTree — newly cloned — parts/granules/sparse-index/marks two-offset trick + merge-time work; arrow-rs + parquet-rs — newly cloned — buffer recipes, RLE-hybrid, two compression layers; C-Store + SIGMOD '06 process-compressed thesis; BtrBlocks sampling cascade + FSST symbol tables; ClickHouse VLDB '24 with ClickBench-on-DuckDB exercise), experiments crate compiles: RLE/Dict/BitPacked `todo!()` stubs with exact-size + maximal-runs + FOR-width + O(1) random-access contract tests, scan_bench PROVIDED (100M values × 3 shapes, raw vs encoded scans incl. RLE sum-without-decode and dict codes-only sum — "raw-equiv GB/s > memory bandwidth" is the compression-IS-performance headline to verify).
- 2026-07-10 — topic 11 scaffolded: study guide (Volcano→X100→HyPer mermaid, selection vectors + vector-type flags, morsel-driven parallelism diagram, vectorized hash join/agg internals), 6 reading guides (DuckDB DataChunk/2048 + pipeline executor push-pull hybrid + join-HT salt-in-pointer probe; postgres ExecProcNode self-replacing dispatch + execExprInterp computed-goto; polars-stream Morsel/MorselSeq/SourceToken + float_sum masked-SIMD multi-accumulator + DataFusion ExecutionPlan streams and intern-then-flat-arrays GroupedHashAggregateStream; X100 CIDR'05 U-curve; VLDB'18 compiled-vs-vectorized scorecard — memory-bound probes favor vectorized, the M11 architecture argument; SIGMOD'14 morsels), experiments crate compiles: one query three engines — Volcano PROVIDED and run (180.7 M rows/s; found LLVM DEVIRTUALIZING the statically-known Box<dyn> chain, 202→180 after black_box — a compiler will silently turn your Volcano into a compiled engine), vectorized (batches + selection vectors + flat group array) and fused branchless kernel are `todo!()` stubs with oracle-agreement tests incl. partial-final-batch and mask-sign-extension traps, exec_bench sweeps selectivity 5/50/95.
- 2026-07-10 — topic 10 scaffolded: study guide (parse→bind→logical→rewrite→join-order→physical pipeline mermaid, rewrite-rule menu, Selinger DP vs DuckDB DPccp+greedy-fallback, cardinality three-lies table, Selinger-vs-Cascades memo ASCII), 5 reading guides (DuckDB optimizer.cpp 25-pass pipeline + plan_enumerator DPccp with greedy escape hatch :234 + cost=output-cardinality-only; postgres allpaths.c standard_join_search + geqo threshold 12 + DEFAULT_EQ_SEL 0.005; sqlparser-rs Pratt parse_subexpr + DataFusion fixpoint-of-rules vs DuckDB ordered passes — sqlparser/datafusion/polars newly cloned; Selinger '79 vs Cascades with M10 architecture-choice question; Leis VLDB'15 JOB — cardinality error 10²–10⁴ dwarfs cost model 2× and search 1.2×, graph-JOB design exercise), experiments crate compiles: toy cost-based planner `todo!()` stubs (parse_and_plan naive left-deep → push_down → greedy reorder_joins → estimate with 1/NDV + independence + containment) with contract tests incl. join_order_flips_with_stats, explain binary PROVIDED for side-by-side DuckDB EXPLAIN comparison.
- 2026-07-10 — topic 9 scaffolded: study guide (latch vs lock table, memory-ordering cheat sheet + publication idiom, latch-coupling→OLC→lock-free ladder, epoch reclamation diagram, Bw-tree cautionary arc, false sharing), 4 reading guides (postgres lwlock.c packed u32 + recheck-after-enqueue lost-wakeup dance; crossbeam-epoch pin/defer/try_advance — newly cloned; RocksDB InlineSkipList CAS+splices vs memgraph lazy-locking skiplist with accessor-id GC — memgraph newly cloned; Bw-tree ICDE'13 + SIGMOD'18 reality check + Leis OLC), experiments crate compiles: lock-free ConcurrentSet `todo!()` stub over crossbeam-epoch with 5 contract tests (same-key/remove races exactly-one-winner, reader-survives-removal-churn UAF canary), scaling shootout PROVIDED (global mutex / 16-shard / crossbeam SkipSet / yours, 1→16 threads), false_sharing PROVIDED and run — packed 63 M inc/s vs pad128 3707 M (59×), and pad64 still 2.2× slower than pad128: Apple M-series coherence granularity is 128 B, x86-style 64 B padding only half-fixes it.
- 2026-07-10 — topic 8 scaffolded: study guide (anomaly-per-isolation-level table, doctors write-skew walkthrough, 2PL/OCC/MVCC comparison, postgres tuple-header + visibility flowchart, HOT chain, Hekaton contrast), 6 reading guides (postgres heapam.c/heapam_visibility.c HeapTupleSatisfiesMVCC + HOT + prune/vacuum with line anchors; RocksDB optimistic vs pessimistic txns over one base class — memtable-only OCC validation, point lock manager; surrealdb kvs layer — newly cloned — versioned reads + putc as portable OCC; Berenson '95 history notation + SI dethroned; SSI VLDB'12 dangerous structure + the single-writer M8 shortcut question; Hekaton + Wu/Pavlo 5-axis menu), experiments crate compiles: Mvcc `todo!()` stub with 8 contract tests including write_skew_HAPPENS_under_SI (test passes when the anomaly occurs) and Serializable-mode prevention via read-set validation, txn_bench PROVIDED (global Mutex baseline vs MVCC, 3 mixes incl. 64-key hot set, abort counts).
- 2026-07-10 — topic 7 scaffolded: study guide (RESP wire anatomy, event-loop mermaid beforeSleep→poll→read→execute→buffer, three threading models table, backpressure: querybuf/output-buffer kills vs pgwire portals), 4 reading guides (redis ae.c + networking.c parse/reply path with line anchors; valkey 8 io_threads.c SPSC inboxes + tagged job pointers + memory_prefetch.c batch-MLP; pgwire Parse/Bind/Execute/Sync portals + qdrant dual tonic servers — both newly cloned; C10K → thread-per-core arc with the shared↔sharded plane exercise), experiments crate compiles: RESP2 parse/encode `todo!()` stub with 8 format-fixing tests (incomplete-input-keeps-bytes, binary-safe bulks, pipelining), tokio server PROVIDED (16-shard store, parse-all-then-flush-once pending-writes trick) — benches vs real redis via redis-benchmark -P 1/-P 64 + flamegraph once resp.rs is implemented.
- 2026-07-10 — topic 6 scaffolded: study guide (translation-cost table hash/swizzle/MMU, miss-path mermaid, three shapes of approximate-LRU, swip state diagram, mmap CIDR-'22 checklist), 6 reading guides (postgres bufmgr.c packed-atomic state + CLOCK + buffer rings; DuckDB eviction queue with dead nodes + 4096-insert purge — newly cloned; LeanStore swips/cooling/hybrid latches — newly cloned; redis zmalloc per-thread padded counters + turso CLOCK page cache bonus; mmap paper with LMDB rebuttal; LeanStore+vmcache paper arc), experiments crate compiles: CLOCK BufferPool `todo!()` stub with contract tests (pinned-never-evicted, dirty-writeback, scan-pressure survival), pool_vs_mmap binary (1GiB file, 4× memory budget, Zipf, tail-latency focus), eviction bench PROVIDED and run — CLOCK 67.0% vs strict-LRU 66.3% hit rate at 20× less time per access (32ms vs 678ms per 1M trace): the "nobody ships strict LRU" lesson, measured.
- 2026-07-10 — topic 5 scaffolded: study guide (WAL rule, four-designs axis LMDB→turso→postgres→redis-AOF, fsync ladder table, group-commit mermaid), 5 reading guides (postgres xlog.c — newly cloned — reserve-then-copy/XLogFlush-recheck/FPI with line anchors; turso WAL checksum chain + salts; redis aof.c/rdb.c with the AOF-as-LSM mapping + FalkorDB angle; ARIES three passes/CLRs; Aether four-bottleneck taxonomy), experiments crate compiles: fsync_ladder PROVIDED and run (this Mac: fsync 21µs vs F_FULLFSYNC 3.0ms — 140×, the macOS weak-fsync gap is real), Wal `todo!()` stub with format-fixing tests (torn tail, uncommitted-txn invisibility, commit_many = 1 fsync), crash_test kill-9 harness (100 rounds, acked-key + atomicity checks), commit_throughput bench (per-commit vs group 8/64/512).
- 2026-07-10 — topic 4 scaffolded: study guide (memtable→SST lifecycle mermaid, SST block anatomy, leveled/tiered/lazy RUM table, stall triggers, Monkey intuition), 6 reading guides (lsm-tree crate — newly cloned, fjall delegates to it — + RocksDB compaction/table with line anchors; Monkey, Dostoevsky, RocksDB TODS '21, compaction design-space VLDB '21), experiments crate compiles: mini-LSM with provided Bloom (tests pass) + Memtable, SST writer/reader + Lsm engine `todo!()` stubs with correctness tests (tombstone-across-compaction, WA>1 check), write_amp binary measuring the full RUM position of leveled vs tiered.
- 2026-07-10 — topic 3 scaffolded: study guide (slotted page anatomy, 3-sibling balance mermaid, LMDB double-meta COW commit diagram), 5 reading guides (turso btree deep + SQLite btree.c + LMDB mdb.c with line anchors from fresh clones; Graefe survey selective-read map, SQLite file-format hex-dump exercise), experiments crate compiles: slotted Page + DiskBTree `todo!()` stubs with format-fixing tests, bench vs redb (point/scan) + prefix-truncation stress case (32B keys, 24B shared prefix).
- 2026-07-10 — topic 2 scaffolded: study guide (chaining vs open addressing cache stories, incremental-rehash mermaid, skiplist/rax ASCII, dense-filter/fat-payload pattern table), 7 reading guides (redis dict/zset/rax, hashbrown SwissTable, RocksDB InlineSkipList — line numbers from local clones; ART paper, CppCon SwissTable talk), experiments crate compiles: skiplist + incremental_map `todo!()` stubs with tests (the build work is the learning work), benches vs hashbrown/BTreeMap/crossbeam-skiplist, rehash_spike binary (HdrHistogram per-insert max/p99.9).
- 2026-07-10 — topic 1 started: study guide (two-family write/read paths, amplification vocabulary, RUM triangle), 8 reading guides (fjall/turso/tidesdb/rocksdb code + O'Neil/Comer/RUM/Hellerstein papers, line numbers from fresh shallow clones), engine_shootout scaffold (fjall vs redb behind a common trait, db_bench workload names, durability parity) compiles + smoke-tested (space-amp binary at 20K keys shows fixed-overhead floor, not amplification — re-run at 1M+). Topic 0 plan audit: fixed phantom CMU-lecture reference in PLAN.md, added missing roofline-thinking section to topic 0 README §4.
- 2026-07-10 — topic 0 finished: cache_ladder (after fixing a self-caching bug: restarting the pointer chase at 0 measured an 8MB hot path — fixed by carrying the walker across iterations; true ladder 1.0 ns L1 / 5–9 ns L2 / ~110 ns DRAM+TLB), lookup_shootout (HashMap flat at ~7–9 ns thanks to MLP; binary search wins ≤1e4; linear scan never beats hashing at n≥100 — folklore busted), flamegraph captured (21% SipHash in HashMap lookups), reference baselines recorded in capstone/BASELINES.md. Topic 0 + M0 done.
- 2026-07-10 — topic 0 started: study guide + 3 experiment benches (cache_ladder, lookup_shootout, branch_misprediction); capstone workspace scaffolded with `workload` crate (seeded Zipfian generator, ~11M ops/s). First measured result: branchy filter 8.1x slower on shuffled vs sorted data; branchless flat at 15 Gelem/s. Repo published to github.com/AviAvni/database-learning-path.
- 2026-07-10 — repo initialized: plan, capstone design, resources.
