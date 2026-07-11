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
- Transactions: Berenson "Critique of ANSI Isolation" '95 · "SSI in PostgreSQL" VLDB'12 · "Hekaton" SIGMOD'13 · Wu/Pavlo "In-Memory MVCC Evaluation" VLDB'17
- Indexing: Leis "ART" ICDE'13 · "Bw-Tree" ICDE'13 + "More Than Buzz Words" SIGMOD'18
- LSM tuning: "Monkey" SIGMOD'17 · "Dostoevsky" SIGMOD'18 · RocksDB TODS'21 · "LSM Compaction Design Space" VLDB'21
- Query optimization: Selinger "Access Path Selection" '79 · "How Good Are Query Optimizers, Really?" VLDB'15 · Graefe "Cascades" '95
- Execution: "MonetDB/X100" CIDR'05 · "Compiled vs Vectorized" VLDB'18 · "Morsel-Driven Parallelism" SIGMOD'14 · Neumann "HyPer compilation" VLDB'11
- Columnar: "C-Store" VLDB'05 · "Compression + Execution in Column Stores" SIGMOD'06 · "BtrBlocks" SIGMOD'23 · "FSST" VLDB'20
- Graph: Davis "SuiteSparse:GraphBLAS" TOMS · "Kùzu" CIDR'23 · "EmptyHeaded" · Ngo et al. worst-case optimal joins · LDBC SNB spec
- Vector/ANN: "HNSW" arXiv:1603.09320 · Jégou "Product Quantization" PAMI'11 · "DiskANN" NeurIPS'19
- Distributed: "Raft" ATC'14 · "Viewstamped Replication Revisited" · "Spanner" OSDI'12 · "Percolator" OSDI'10 · "Calvin" SIGMOD'12
- Testing: "SQLancer/PQS" OSDI'20 · "TLP" OOPSLA'20 · Jepsen analyses (jepsen.io/analyses)
- Perspective: "OLTP Through the Looking Glass" SIGMOD'08 · "What's Really New with NewSQL?" '16

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
