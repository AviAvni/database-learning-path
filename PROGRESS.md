# Progress

Status: `todo` → `in progress` → `done`. Add a one-line takeaway when done.

| # | Topic | Status | Takeaway |
|---|-------|--------|----------|
| 0 | The Performance Toolbox | todo | |
| 1 | Storage Engine Landscape: B-Tree vs LSM | todo | |
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

## Capstone milestones (falkordb-rs-next-gen from scratch)

| Milestone | Depends on topic | Status |
|-----------|------------------|--------|
| M0 workspace + bench harness + reference baselines | 0 | todo |
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

## Session log

<!-- newest first: date — what was done -->
- 2026-07-10 — repo initialized: plan, capstone design, resources.
