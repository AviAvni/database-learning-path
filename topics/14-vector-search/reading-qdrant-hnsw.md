# Reading guide — qdrant's HNSW + filtered search

Repo: `~/repos/qdrant`, everything under
`lib/segment/src/index/hnsw_index/`. This is production HNSW: the
paper plus five years of scar tissue — and filtering, qdrant's
actual specialty.

## 1. The graph, split into build and serve shapes

- `graph_layers_builder.rs:35` `GraphLayersBuilder` — per-node
  `RwLock`'d link lists (parallel build), `ef_construct` (:38),
  `level_factor = 1/ln(M)` (:317 — the paper's mL),
  `get_random_layer` (:385, `-ln(sample) * level_factor` at :391),
  `link_new_point` (:414) — Alg 1.
- `:41-42` `use_heuristic` — Alg 4 as a flag; find
  `select_candidates_with_heuristic` below it and match the paper.
- `graph_layers.rs:74` `GraphLayers` — the FROZEN serve-side graph:
  `search_on_level` (:109), `search_entry` (:248 — the ef=1 greedy
  descent). Build structure ≠ serve structure — the same
  builder/immutable split as CSR (topic 13).
- `search_context.rs:8` `SearchContext` — the two bounded heaps of
  Alg 2. `visited_pool.rs:9` `VisitedListHandle` — pooled visited
  sets reused across queries (:14 comment says exactly this): your
  hop_bench stamp trick, productionized with a pool because queries
  are concurrent.

## 2. The filtered-search decision (the good part)

`hnsw/search.rs:55-84` — per-query algorithm choice:

```rust
let mut algorithm = SearchAlgorithm::Hnsw;
if acorn_enabled && let Some(filter) = filter {
    let query_point_cardinality =
        payload_index.with_view(|v| v.estimate_cardinality(filter, ...))?;  // :74
    let selectivity = cardinality / available_vector_count;                  // :80
    if selectivity <= acorn_max_selectivity { algorithm = Acorn; }
}
```

Topic 10 inside the vector index: **estimate cardinality, then pick
the plan**. The full menu:

- selectivity high → normal HNSW, `FilteredScorer` rejects
  non-matching points during traversal
- selectivity low → `search_plain_batched` (:264) — brute-force the
  filtered id list; below `full_scan_threshold` the graph can't help
- middle → ACORN (`search_on_level_acorn`, graph_layers.rs:155):
  traverse through blocked nodes by expanding to 2-hop neighbors, so
  the filtered subgraph stays connected without extra links

## 3. Percolation, measured not assumed

`hnsw/build.rs:378-386`:

```rust
// According to percolation theory, random graph becomes disconnected
// if 1/K points are left, where K is average number of links per point
let percolation = 1. - 2. / (average_links_per_0_level_int as f32);
```

Build-time: sample subgraph connectivity at the 2/K survival point
(:390-392, three samples, take max) and add extra links
(`payload_m`, hnsw.rs:93) for indexed payload categories if the
main graph would shatter under filtering. The failure mode is
MEASURED during build — topic 0 discipline inside an index builder.

## 4. Odds and ends worth grepping

- `hnsw/build.rs:95-109` — `full_scan_threshold` derives the
  "indexing threshold": tiny segments never build a graph at all
- `graph_links.rs` — serialized link format: delta-compressed,
  topic 12 encodings applied to graph edges
- `gpu/` — GPU-built HNSW (topic 18 preview)
- `graph_layers_healer.rs` — repairing the graph around deleted
  points instead of rebuilding: the deletes wart, patched

## Questions (answer in notes.md)

1. Why does the visited pool matter more here than in hop_bench?
   (Concurrency + allocation, name both.)
2. ACORN's 2-hop expansion: what does it cost in scoring work vs
   payload_m's extra links in RAM? When is each the right buy?
3. `estimate_cardinality` comes from the payload index. What's the
   M14 equivalent — which structure estimates label selectivity?
   (M13's label bitmaps.)
4. Why is `full_scan_threshold` in BYTES-ish terms (kB) rather than
   a point count? (Think d and the real cost unit.)
5. The build/serve split (Builder with RwLocks → frozen GraphLayers):
   map it onto topic 13's transient/persistent kuzu split and
   Delta_Matrix. What's the graph-index "flush"?
