# Kùzu: DuckDB for graphs

kuzu is "DuckDB for graphs": columnar disk-based storage, vectorized
execution — and two graph-specific ideas worth stealing: CSR that
survives updates via node groups, and a worst-case-optimal Intersect
operator embedded in an otherwise binary-join plan. This chapter walks
the C++ alongside the CIDR '23 system paper.

## 1. Columnar CSR node groups

`src/include/storage/table/csr_node_group.h`:

- `:165-171` — the design comment: **persistent data** (checkpointed,
  CSR format, `persistentChunkGroup`) + **transient data** (in-memory
  chunked groups, append-only, with a `csrIndex` mapping bound node →
  row indices). Reads merge both. Same LSM-shaped answer as
  Delta_Matrix: read-optimal core + mutable overlay.
- `:172` `class CSRNodeGroup final : public NodeGroup` — rel tables
  reuse the node-group machinery (a node group ≈ DuckDB row group,
  topic 12): edges are just columns (`:162-163` — column 0 is neighbor
  id, column 1 is rel id, properties follow) sorted by source node,
  with a CSR header (offsets) per group.
- `InMemChunkedCSRHeader` (`:117`, `:141`) — offsets+lengths being
  built in memory; checkpoint merges transient rows into a rebuilt CSR
  for that node group only (`oldHeader/newHeader`, `:152-153`).
  **Update pain is bounded per node group**, not per graph.

So: adjacency = a columnar table clustered by src with a CSR index on
top. Zone maps, compression (topic 12 encodings!), and vectorized
scans all apply to edges for free.

## 2. Worst-case optimal joins: the Intersect operator

`src/include/processor/operator/intersect/intersect.h:29`
`class Intersect : public PhysicalOperator` (+ `intersect_build.h:35`
building sorted adjacency lists into a hash table keyed by node).

The plan shape for a triangle `(a)->(b), (b)->(c), (a)->(c)`:
binary-join to get (a,b) pairs, then for each pair **intersect N(a) ∩
N(b)** to produce c — never materializing the O(m²) intermediate
(a,c)×(b,c) pairs. This is Generic Join specialized: intersect one
variable at a time, cost bounded by the AGM output bound (m^1.5 for
triangles). See reading-wcoj.md for the theory.

Note it's a hybrid: kuzu's optimizer picks Intersect only where cyclic
patterns make binary joins asymptotically wrong; chains/trees stay
binary hash joins (the topic-10/11 machinery).

## 3. Factorization (CIDR '23 §vectorization)

One-to-many expands multiply rows: `MATCH (a)-[]->(b)-[]->(c)` flat =
Σ deg(a)·deg(b) tuples. kuzu keeps vectors FACTORIZED — an "unflat"
vector holds a group (e.g. all b's for one a) with a multiplicity,
deferring the cross product. A DataChunk carries flat + unflat vectors
(the vector-type flags of topic 11, pushed further). Aggregations like
`count(*)` can multiply group sizes without ever flattening.

```rust
// factorized count(*) for a 2-hop: multiply group SIZES, never
// materialize the Σ deg(a)·deg(b) tuples a flat plan would build
fn two_hop_count(csr: &Csr) -> u64 {
    (0..csr.n)
        .map(|a| {
            csr.neighbors(a).iter()
                .map(|&b| csr.degree(b) as u64)   // multiplicity, not rows
                .sum::<u64>()
        })
        .sum()   // matrix spelling: the grand sum of A²'s path counts
}
```

FalkorDB's matrix spelling of the same fact: A² holds PATH COUNTS as
values — the algebra factorizes for you.

## Questions (answer in notes.md)

1. Rebuild-per-node-group bounds update cost. What's the worst case —
   which insert pattern still hurts? (Hint: supernode crossing groups.)
2. Why must adjacency lists be SORTED for Intersect? What does the
   build side (`intersect_build.h`) have to guarantee?
3. Triangle count on m=16M edges: estimate intermediates for binary
   join vs AGM bound. How many × saved?
4. Factorized `count(*)` for 2-hop = Σ over a of deg-products. Write
   the matrix expression that computes the same number. (This is
   hop_bench's count!)
5. Kuzu compresses neighbor-id columns with topic-12 encodings. Which
   encoding wins for CSR targets sorted by src, and why? (Think about
   what's monotonic within a run and what isn't.)

## References

**Papers**
- Feng, Gupta, Jin, et al. — "KÙZU Graph Database Management System"
  (CIDR 2023) — the §vectorization/factorization discussion is the
  part the code doesn't narrate

**Code**
- [kuzu](https://github.com/kuzudb/kuzu) (shallow clone) —
  `src/include/storage/table/csr_node_group.h` (the design comment at
  :165-171 is the storage story),
  `src/include/processor/operator/intersect/intersect.h` +
  `intersect_build.h` (the WCOJ operator)
