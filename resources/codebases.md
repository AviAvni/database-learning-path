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
| [TimelyDataflow/differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow) | Rust | incremental computation — the real thing (topic 27) |
| [neondatabase/neon](https://github.com/neondatabase/neon) | Rust | disaggregated postgres: pageserver, safekeepers, branching (topic 28) |
| [slatedb/slatedb](https://github.com/slatedb/slatedb) | Rust | LSM directly on object storage — small, modern (topic 28) |
| [prometheus/prometheus](https://github.com/prometheus/prometheus) | Go | `tsdb/`: readable time-series storage (topic 30) |
| [influxdata/influxdb](https://github.com/influxdata/influxdb) | Rust | IOx: DataFusion+Parquet+object storage combined (topics 11/12/28/30) |
| [RoaringBitmap/CRoaring](https://github.com/RoaringBitmap/CRoaring) | C | compressed bitmaps: container switching, SIMD set ops (topics 23, 26) |
| [automerge/automerge](https://github.com/automerge/automerge) | Rust | CRDT engine — state/op-based, columnar op storage (topic 31) |
| [loro-dev/loro](https://github.com/loro-dev/loro) | Rust | fast modern CRDT engine, great perf blog posts (topic 31) |
| [graphistry/pygraphistry](https://github.com/graphistry/pygraphistry) | Python | GPU graph ETL/analytics on RAPIDS cuDF/cuGraph, GFQL query layer — production analog to Gunrock's research code (topics 18, 24) |

## What the `file:line` anchors were read against

The reading guides quote a few thousand `file:line` anchors into the codebases
above. A line number only means something next to the commit it was read at, so
every clone the guides reference is pinned here — once, rather than in each of
the 230 guides, so the record stays consistent instead of drifting per file.

Regenerate with `python3 tools/pin-table.py`, or `--check` to see whether your
clones have moved since the table was written. Both read `~/repos` (override
with `DLP_CLONES`), so this is a local command — CI has no clones and does not
check it.

Clone to `~/repos/<name>` and `git checkout` the listed commit if you want the
anchors to land exactly; on a newer commit expect the structure to match and the
line numbers to have moved. The mention count is there to tell you which clones
are worth fetching first.

<!-- BEGIN PIN TABLE (generated by tools/pin-table.py) -->

| clone | read at | dated | mentions | origin |
|---|---|---|---|---|
| `postgres` | `701f021` | 2026-07-10 | 511 | [https://github.com/postgres/postgres](https://github.com/postgres/postgres) |
| `redis` | `a176d1225` | 2026-03-24 | 495 | [https://github.com/redis/redis](https://github.com/redis/redis) |
| `FalkorDB` | `aa75821ab` | 2026-08-25 | 347 | [https://github.com/FalkorDB/FalkorDB](https://github.com/FalkorDB/FalkorDB) |
| `duckdb` | `6c0c1a68` | 2026-07-10 | 334 | [https://github.com/duckdb/duckdb](https://github.com/duckdb/duckdb) |
| `rocksdb` | `7c80a5a` | 2026-07-09 | 299 | [https://github.com/facebook/rocksdb](https://github.com/facebook/rocksdb) |
| `neon` | `8f60b04` | 2026-05-25 | 207 | [https://github.com/neondatabase/neon](https://github.com/neondatabase/neon) |
| `sqlite` | `951de30` | 2026-07-09 | 200 | [https://github.com/sqlite/sqlite](https://github.com/sqlite/sqlite) |
| `GraphBLAS` | `1fd54756ca` | 2026-02-05 | 187 | [https://github.com/DrTimothyAldenDavis/GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS) |
| `qdrant` | `44ad62f` | 2026-06-03 | 171 | [https://github.com/qdrant/qdrant](https://github.com/qdrant/qdrant) |
| `turso` | `dd775bc` | 2026-07-10 | 170 | [https://github.com/tursodatabase/turso](https://github.com/tursodatabase/turso) |
| `valkey` | `8891441ab` | 2026-05-03 | 155 | [https://github.com/valkey-io/valkey](https://github.com/valkey-io/valkey) |
| `datafusion` | `1e77af8` | 2026-07-10 | 145 | [https://github.com/apache/datafusion](https://github.com/apache/datafusion) |
| `fjall` | `80cf6bc` | 2026-07-05 | 143 | [https://github.com/fjall-rs/fjall](https://github.com/fjall-rs/fjall) |
| `egg` | `f94c346` | 2026-04-14 | 135 | [https://github.com/egraphs-good/egg](https://github.com/egraphs-good/egg) |
| `hashbrown` | `d69025b` | 2026-07-06 | 126 | [https://github.com/rust-lang/hashbrown](https://github.com/rust-lang/hashbrown) |
| `polars` | `f8bcc3d` | 2026-07-10 | 100 | [https://github.com/pola-rs/polars](https://github.com/pola-rs/polars) |
| `leanstore` | `90fcf18` | 2025-09-11 | 96 | [https://github.com/leanstore/leanstore](https://github.com/leanstore/leanstore) |
| `z3` | `1d425e5` | 2026-07-09 | 96 | [https://github.com/Z3Prover/z3](https://github.com/Z3Prover/z3) |
| `memgraph` | `8f87f6a` | 2026-07-09 | 93 | [https://github.com/memgraph/memgraph](https://github.com/memgraph/memgraph) |
| `clickhouse` | `4d598fb2c` | 2026-07-10 | 92 | [https://github.com/ClickHouse/ClickHouse](https://github.com/ClickHouse/ClickHouse) |
| `LAGraph` | `e2539e2` | 2025-09-08 | 91 | [https://github.com/GraphBLAS/LAGraph](https://github.com/GraphBLAS/LAGraph) |
| `lsm-tree` | `8526dd3` | 2026-07-05 | 85 | [https://github.com/fjall-rs/lsm-tree](https://github.com/fjall-rs/lsm-tree) |
| `egglog` | `e264c37a` | 2026-08-25 | 76 | [https://github.com/egraphs-good/egglog](https://github.com/egraphs-good/egglog) |
| `lmdb` | `704dc70` | 2026-06-24 | 71 | [https://github.com/LMDB/lmdb](https://github.com/LMDB/lmdb) |
| `raft-rs` | `ad13f3d` | 2026-05-13 | 69 | [https://github.com/tikv/raft-rs](https://github.com/tikv/raft-rs) |
| `tiflash` | `b5093dd` | 2026-07-09 | 62 | [https://github.com/pingcap/tiflash](https://github.com/pingcap/tiflash) |
| `prometheus` | `f282b5c` | 2026-07-10 | 61 | [https://github.com/prometheus/prometheus](https://github.com/prometheus/prometheus) |
| `materialize` | `b06b3d6` | 2026-07-10 | 56 | [https://github.com/MaterializeInc/materialize](https://github.com/MaterializeInc/materialize) |
| `kuzu` | `89f0263` | 2025-10-10 | 55 | [https://github.com/kuzudb/kuzu](https://github.com/kuzudb/kuzu) |
| `tikv` | `eb8dd65` | 2026-07-09 | 53 | [https://github.com/tikv/tikv](https://github.com/tikv/tikv) |
| `hypothesis` | `49a797bdf` | 2026-08-25 | 50 | [https://github.com/HypothesisWorks/hypothesis](https://github.com/HypothesisWorks/hypothesis) |
| `neo4j` | `eccd584a` | 2026-07-02 | 50 | [https://github.com/neo4j/neo4j](https://github.com/neo4j/neo4j) |
| `go-ycsb` | `f030f99` | 2025-12-31 | 49 | [https://github.com/pingcap/go-ycsb](https://github.com/pingcap/go-ycsb) |
| `tantivy` | `7152d53` | 2026-07-10 | 49 | [https://github.com/quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) |
| `BlockSci` | `14ccc93` | 2020-11-13 | 48 | [https://github.com/citp/BlockSci](https://github.com/citp/BlockSci) |
| `ligra` | `8763202` | 2024-02-18 | 46 | [https://github.com/jshun/ligra](https://github.com/jshun/ligra) |
| `pgwire` | `6bb6299` | 2026-06-29 | 46 | [https://github.com/sunng87/pgwire](https://github.com/sunng87/pgwire) |
| `cockroach` | `a7e11788` | 2026-07-06 | 45 | [https://github.com/cockroachdb/cockroach](https://github.com/cockroachdb/cockroach) |
| `usearch` | `9fd6b01` | 2026-05-24 | 45 | [https://github.com/unum-cloud/usearch](https://github.com/unum-cloud/usearch) |
| `benchbase` | `33c0047` | 2025-12-13 | 40 | [https://github.com/cmu-db/benchbase](https://github.com/cmu-db/benchbase) |
| `rayon` | `6d9e94b` | 2026-06-27 | 40 | [https://github.com/rayon-rs/rayon](https://github.com/rayon-rs/rayon) |
| `ALEX` | `4370da6` | 2024-03-12 | 39 | [https://github.com/microsoft/ALEX](https://github.com/microsoft/ALEX) |
| `tidesdb` | `810507a` | 2026-07-10 | 39 | [https://github.com/tidesdb/tidesdb](https://github.com/tidesdb/tidesdb) |
| `slatedb` | `323ed1b` | 2026-07-10 | 38 | [https://github.com/slatedb/slatedb](https://github.com/slatedb/slatedb) |
| `cudf` | `2f082a7` | 2026-07-10 | 37 | [https://github.com/rapidsai/cudf](https://github.com/rapidsai/cudf) |
| `gunrock` | `748f79e` | 2026-02-09 | 35 | [https://github.com/gunrock/gunrock](https://github.com/gunrock/gunrock) |
| `memchr` | `5fdb40c` | 2026-07-07 | 35 | [https://github.com/BurntSushi/memchr](https://github.com/BurntSushi/memchr) |
| `SimSIMD` | `63a254f` | 2026-05-23 | 35 | [https://github.com/ashvardanian/SimSIMD](https://github.com/ashvardanian/SimSIMD) |
| `wgpu` | `f945c78` | 2026-07-10 | 34 | [https://github.com/gfx-rs/wgpu](https://github.com/gfx-rs/wgpu) |
| `sqlancer` | `af6ae85` | 2026-06-21 | 33 | [https://github.com/sqlancer/sqlancer](https://github.com/sqlancer/sqlancer) |
| `gapbs` | `b5e3e19` | 2024-05-11 | 32 | [https://github.com/sbeamer/gapbs](https://github.com/sbeamer/gapbs) |
| `raft.tla` | `6ecbdbc` | 2025-02-18 | 30 | [https://github.com/ongardie/raft.tla](https://github.com/ongardie/raft.tla) |
| `RediSearch` | `87276ca` | 2026-07-09 | 30 | [https://github.com/RediSearch/RediSearch](https://github.com/RediSearch/RediSearch) |
| `raphtory` | `5d0d286` | 2026-07-21 | 29 | [https://github.com/Pometry/Raphtory](https://github.com/Pometry/Raphtory) |
| `foundationdb` | `4c775a9` | 2026-07-10 | 28 | [https://github.com/apple/foundationdb](https://github.com/apple/foundationdb) |
| `simdjson` | `c783809` | 2026-07-10 | 28 | [https://github.com/simdjson/simdjson](https://github.com/simdjson/simdjson) |
| `tidb` | `b94006d` | 2026-07-10 | 28 | [https://github.com/pingcap/tidb](https://github.com/pingcap/tidb) |
| `splink` | `04189f5` | 2026-07-23 | 27 | [https://github.com/moj-analytical-services/splink](https://github.com/moj-analytical-services/splink) |
| `quickwit` | `a5ad540` | 2026-07-08 | 26 | [https://github.com/quickwit-oss/quickwit](https://github.com/quickwit-oss/quickwit) |
| `risingwave` | `119de0a` | 2026-07-10 | 25 | [https://github.com/risingwavelabs/risingwave](https://github.com/risingwavelabs/risingwave) |
| `surrealdb` | `9d9a5b0` | 2026-07-02 | 25 | [https://github.com/surrealdb/surrealdb](https://github.com/surrealdb/surrealdb) |
| `crossbeam` | `6b7458d` | 2026-07-10 | 23 | [https://github.com/crossbeam-rs/crossbeam](https://github.com/crossbeam-rs/crossbeam) |
| `cr-sqlite` | `891fe9e` | 2024-10-25 | 21 | [https://github.com/vlcn-io/cr-sqlite](https://github.com/vlcn-io/cr-sqlite) |
| `loro` | `b81abfc` | 2026-07-07 | 20 | [https://github.com/loro-dev/loro](https://github.com/loro-dev/loro) |
| `diamond-types` | `ad48b9c` | 2026-05-29 | 19 | [https://github.com/josephg/diamond-types](https://github.com/josephg/diamond-types) |
| `sqlparser-rs` | `aeb616f` | 2026-07-03 | 19 | [https://github.com/apache/datafusion-sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs) |
| `influxdb` | `d783411` | 2026-06-17 | 18 | [https://github.com/influxdata/influxdb](https://github.com/influxdata/influxdb) |
| `roaring-rs` | `83caaca` | 2026-04-24 | 18 | [https://github.com/RoaringBitmap/roaring-rs](https://github.com/RoaringBitmap/roaring-rs) |
| `spicedb` | `8422483` | 2026-07-24 | 16 | [https://github.com/authzed/spicedb](https://github.com/authzed/spicedb) |
| `cranelift-jit-demo` | `3e5e9b6` | 2025-11-07 | 15 | [https://github.com/bytecodealliance/cranelift-jit-demo](https://github.com/bytecodealliance/cranelift-jit-demo) |
| `VictoriaMetrics` | `c1e39b2` | 2026-07-10 | 15 | [https://github.com/VictoriaMetrics/VictoriaMetrics](https://github.com/VictoriaMetrics/VictoriaMetrics) |
| `automerge` | `c39339d` | 2026-07-10 | 14 | [https://github.com/automerge/automerge](https://github.com/automerge/automerge) |
| `GraphRAG-SDK` | `f42ab3d` | 2026-04-12 | 14 | [https://github.com/FalkorDB/GraphRAG-SDK](https://github.com/FalkorDB/GraphRAG-SDK) |
| `arrow-rs` | `fed7862` | 2026-07-10 | 13 | [https://github.com/apache/arrow-rs](https://github.com/apache/arrow-rs) |
| `bloodhound` | `1968388` | 2026-07-24 | 13 | [https://github.com/SpecterOps/BloodHound](https://github.com/SpecterOps/BloodHound) |
| `feldera` | `bb49055` | 2026-07-10 | 13 | [https://github.com/feldera/feldera](https://github.com/feldera/feldera) |
| `falkordb-rs-next-gen` | `9d28bdc6` | 2026-07-29 | 10 | [https://github.com/FalkorDB/falkordb-rs-next-gen](https://github.com/FalkorDB/falkordb-rs-next-gen) |
| `RedisBloom` | `ab734fa` | 2026-07-05 | 9 | [https://github.com/RedisBloom/RedisBloom](https://github.com/RedisBloom/RedisBloom) |
| `cuvs` | `8b97b61` | 2026-07-10 | 7 | [https://github.com/rapidsai/cuvs](https://github.com/rapidsai/cuvs) |
| `differential-dataflow` | `3f279da` | 2026-05-29 | 5 | [https://github.com/TimelyDataflow/differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow) |
| `falkordb-py` | `122df79` | 2026-07-19 | 5 | [https://github.com/FalkorDB/falkordb-py](https://github.com/FalkorDB/falkordb-py) |
| `PGM-index` | `c6fcf3d` | 2024-11-28 | 5 | [https://github.com/gvinciguerra/PGM-index](https://github.com/gvinciguerra/PGM-index) |
| `pytorch_geometric` | `1f0661c` | 2026-06-19 | 5 | [https://github.com/pyg-team/pytorch_geometric](https://github.com/pyg-team/pytorch_geometric) |
| `RustyTaintChain` | `4e12fd0` | 2021-03-05 | 5 | [https://github.com/TaintChain/RustyTaintChain](https://github.com/TaintChain/RustyTaintChain) |
| `y-crdt` | `03e14a0` | 2026-06-12 | 3 | [https://github.com/y-crdt/y-crdt](https://github.com/y-crdt/y-crdt) |
| `antithesis-sdk-rust` | `78c9db5` | 2026-06-12 | 2 | [https://github.com/antithesishq/antithesis-sdk-rust](https://github.com/antithesishq/antithesis-sdk-rust) |
| `helix-db` | `47191c6` | 2026-07-05 | 2 | [https://github.com/HelixDB/helix-db](https://github.com/HelixDB/helix-db) |
| `timely-dataflow` | `15fc7c9` | 2026-06-12 | 2 | [https://github.com/TimelyDataflow/timely-dataflow](https://github.com/TimelyDataflow/timely-dataflow) |

<!-- END PIN TABLE -->
