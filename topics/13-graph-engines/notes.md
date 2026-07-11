# Topic 13 notes — graph engines

## Predictions (fill BEFORE implementing csr.rs / matrix.rs)

Baseline (provided, measured): adj_list 3484 ns/query random,
294885 ns/query supernodes (85× tail); graph 1M nodes / 16M directed
edges, max degree 6565, p50 degree 11.

| impl | sources | predicted vs adj_list (×) | actual ns/query |
|---|---|---|---|
| csr | random | | |
| csr | supernodes | | |
| matrix (SpMV) | random | | |
| matrix (SpMV) | supernodes | | |

| question | prediction | actual |
|---|---|---|
| does csr beat adj_list at all? (per-node vecs are already contiguous — where's the win?) | | |
| matrix vs csr: what does the frontier materialization cost? | | |
| supernode ratio: does CSR shrink the 85× tail or just shift it? | | |
| CSR build time vs adj_list build time | | |

## Implementation log

- [ ] csr.rs: counting-sort build + slice two_hop; all 5 tests green
- [ ] matrix.rs: spmv_masked + two-SpMV two_hop; all 4 tests green
- [ ] hop_bench full table recorded above (checksums match:
      random=10220457, supernodes=7890665)
- [ ] external: same 2-hop count on FalkorDB
      (`MATCH (a)-[*1..2]->(b) WHERE id(a)=<src> RETURN count(DISTINCT b)`)
      — ns/query here:
- [ ] optional: neo4j same query — ns/query:

Surprises / dead ends:

## Questions from the reading guides

### GraphBLAS + Delta_Matrix (reading-graphblas-internals.md)

1. Why delta_minus instead of deleting from M (CSR delete cost):
2. dot3 vs saxpy at frontier 10 vs 10⁶:
3. When BITMAP fits a label matrix:
4. Why (M ∪ DP) ∖ DM reads beat flush-per-write:
5. Delta_Matrix → LSM vocabulary map:

### neo4j record store (reading-neo4j-record-store.md)

1. 1000-edge Expand: chain (~110 ns/edge) vs CSR stream — ×:
2. Why 15 B nodes / 34 B rels — field inventory:
3. Delete cost: chain vs CSR vs Delta_Matrix:
4. Property chains vs M12 columns for `WHERE n.age > 65`:
5. Index-free adjacency: what survives DRAM, what died:

### memgraph storage (reading-memgraph-storage.md)

1. Why edges in both endpoints' vectors; FalkorDB's transposed trio:
2. small_vector inline + power-law degrees:
3. Per-object vs per-row delta chains under supernode edge inserts:
4. memgraph vector vs kuzu CSR slice — 16 B triples vs 8 B ids:
5. PageRank on pointer soup vs matrix — where the bus time goes:

### kuzu (reading-kuzu.md)

1. Node-group-bounded rebuilds — which insert pattern still hurts:
2. Why Intersect needs sorted adjacency:
3. Triangle intermediates: binary join vs AGM at m=16M:
4. Factorized count(*) 2-hop as a matrix expression:
5. Best topic-12 encoding for CSR target columns:

### WCOJ (reading-wcoj.md)

1. Star-graph R⋈S intermediates vs triangle output:
2. AGM bound for the 4-cycle:
3. Galloping and power-law skew:
4. Why `C<A>=A²` never materializes A² (dot3):
5. M10 trigger for intersect vs binary join (pattern cyclicity):

### LDBC SNB (reading-ldbc-snb.md)

1. Why timed dependency-tracked inserts:
2. IC5-ish pattern: anchor + expand count + worst representation:
3. Which architecture flatters itself on uniform-degree graphs:
4. Where multi-hop cardinality estimation dies:
5. SF that fits this Mac per representation (bytes/edge each):

## Cross-topic threads

- Pointer chase vs stream = topic 0's cache ladder deciding graph
  architecture: neo4j chains pay ~110 ns/edge, CSR pays bandwidth.
- Delta overlay everywhere (Delta_Matrix, kuzu transient groups,
  GraphBLAS pending tuples) = topic 4's LSM applied to adjacency.
- Masks = predicate pushdown (topic 10) into the kernel; SpMV
  frontier = topic 11's batch, spelled algebraically.
- memgraph = topic 8's N2O deltas + topic 9's skip list, at vertex
  granularity; bit-smuggling ledger: deleted-flag in delta pointer
  low bits (PointerPack), neo4j's 35-bit pointers in the inUse byte.
- Supernodes = the graph-shaped tail: same lesson as Zipfian keys
  (topic 2) and JOB correlations (topic 10) — uniform data lies.

## M13 log (naive adjacency core — the baseline M20 must beat)

- [ ] node/edge store over adjacency lists + label bitmaps
- [ ] scan-anchor-then-expand for `(a:L)-[:R]->(b)`
- [ ] Expand fills M11 batches; bench engine-vs-raw (interpretation
      overhead = the M11 payoff measurement)
- [ ] update-pain notes for the M20 delta overlay design

## Done when

- csr + matrix tests green; hop_bench table filled with matching
  checksums; FalkorDB external comparison recorded.
- Reading-guide questions answered.
- M13 update-pain notes written.
