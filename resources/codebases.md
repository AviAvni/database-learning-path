# Reference Codebases

The user-provided list plus additions, annotated with *what each is best for studying*.
Clone the ones in active use to `~/repos/`.

## Your codebases (baseline)

| Repo | Best for |
|------|----------|
| [FalkorDB/FalkorDB](https://github.com/FalkorDB/FalkorDB) | Sparse-matrix graph engine, redis module architecture |
| [FalkorDB/falkordb-rs-next-gen](https://github.com/FalkorDB/falkordb-rs-next-gen) | Rust graph engine rewrite |

## From your list

| Repo | Lang | Best for |
|------|------|----------|
| [redis/redis](https://github.com/redis/redis) | C | dict incremental rehash, skiplists, event loop, RESP, AOF/RDB, rax |
| [valkey-io/valkey](https://github.com/valkey-io/valkey) | C | io-threads/multithreading evolution vs redis — diff the two! |
| [qdrant/qdrant](https://github.com/qdrant/qdrant) | Rust | HNSW, filtered ANN, quantization, raft consensus |
| [surrealdb/surrealdb](https://github.com/surrealdb/surrealdb) | Rust | multi-model design, transaction layer over pluggable KV |
| [facebook/rocksdb](https://github.com/facebook/rocksdb) | C++ | LSM at industrial scale: compaction, block cache, txn utilities |
| [tursodatabase/turso](https://github.com/tursodatabase/turso) | Rust | SQLite rewrite: B-tree, pager, WAL, io_uring, **DST** |
| [neo4j/neo4j](https://github.com/neo4j/neo4j) | Java | record-store graph layout, Cypher planner |
| [HelixDB/helix-db](https://github.com/HelixDB/helix-db) | Rust | graph+vector combined engine (young codebase, easy to read) |
| [memgraph/memgraph](https://github.com/memgraph/memgraph) | C++ | in-memory graph, skip-list storage, MVCC |
| [ravendb/ravendb](https://github.com/ravendb/ravendb) | C# | Voron storage engine (COW B+tree), document DB design |
| [fjall-rs/fjall](https://github.com/fjall-rs/fjall) | Rust | **the** readable Rust LSM — small enough to read fully |
| [tidesdb/tidesdb](https://github.com/tidesdb/tidesdb) | C | compact C LSM, easy first read |
| [duckdb/duckdb](https://github.com/duckdb/duckdb) | C++ | vectorized execution, optimizer, columnar compression — very readable |
| [postgres/postgres](https://github.com/postgres/postgres) | C | MVCC, WAL, buffer manager, planner — the canon |

## Suggested additions

| Repo | Lang | Best for |
|------|------|----------|
| [sqlite/sqlite](https://github.com/sqlite/sqlite) | C | btree.c, pager, VDBE — most-deployed DB on earth |
| [LMDB (openldap/mdb)](https://github.com/LMDB/lmdb) | C | copy-on-write B+tree, single-file mmap design |
| [skyzh/mini-lsm](https://github.com/skyzh/mini-lsm) | Rust | *guided course* — build an LSM step by step (use in topic 4) |
| [cmu-db/bustub](https://github.com/cmu-db/bustub) | C++ | CMU 15-445 teaching DB: buffer pool, B+tree, txn labs |
| [erikgrinaker/toydb](https://github.com/erikgrinaker/toydb) | Rust | reference for the capstone: raft + MVCC + SQL, written to teach |
| [apache/datafusion](https://github.com/apache/datafusion) | Rust | Arrow-native query engine — planner + vectorized exec in Rust |
| [pola-rs/polars](https://github.com/pola-rs/polars) | Rust | vectorized columnar engine: lazy optimizer, streaming exec, SIMD kernels — DuckDB's Rust rival |
| [kuzudb/kuzu](https://github.com/kuzudb/kuzu) | C++ | columnar graph storage, worst-case optimal joins (topic 13) |
| [cberner/redb](https://github.com/cberner/redb) | Rust | clean embedded COW B-tree in Rust |
| [spacejam/sled](https://github.com/spacejam/sled) | Rust | Bw-tree-inspired engine; read its post-mortems too |
| [tikv/tikv](https://github.com/tikv/tikv) | Rust | raft-rs, distributed txn (Percolator) — topic 15 |
| [apple/foundationdb](https://github.com/apple/foundationdb) | C++ | deterministic simulation testing gold standard — topic 16 |
| [ClickHouse/ClickHouse](https://github.com/ClickHouse/ClickHouse) | C++ | columnar OLAP at the extreme; read specific MergeTree parts only |
| [unum-cloud/usearch](https://github.com/unum-cloud/usearch) | C++ | compact single-header HNSW — topic 14 |
| [DrTimothyAldenDavis/GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS) | C | SuiteSparse internals — go deeper than the API you already use |
| [GraphBLAS/LAGraph](https://github.com/GraphBLAS/LAGraph) | C | graph algorithms as linear algebra — the reference library over GraphBLAS (topics 20, 24) |
| [Z3Prover/z3](https://github.com/Z3Prover/z3) | C++ | SMT solver: query-equivalence proving, invariant checking (topic 16); also a perf-engineering masterclass |
| [quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) | Rust | inverted index / full-text engine — the readable Lucene (topic 23) |
| [apache/lucene](https://github.com/apache/lucene) | Java | the canon of search: codecs, FSTs, segment merging (topic 23) |
| [elastic/elasticsearch](https://github.com/elastic/elasticsearch) | Java | distributed search architecture over Lucene: shards, scatter-gather (topic 23) |
| [RediSearch/RediSearch](https://github.com/RediSearch/RediSearch) | C | search as a redis module — your ecosystem's approach (topic 23) |
| [leanprover/lean4](https://github.com/leanprover/lean4) | C++/Lean | theorem proving + Perceus RC runtime (topic 21) |
| [modular/mojo](https://github.com/modular/modular) | Mojo | SIMD-first language on MLIR, CPU+GPU kernels (topics 17, 18) |
