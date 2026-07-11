# Reading guide — usearch (compact HNSW, one header)

Repo: `~/repos/usearch`, all of it in
`include/usearch/index.hpp` (+ `index_dense.hpp` for the
type-erased/quantized wrapper). C++ templates, but the structure is
small enough to hold in your head — read it as the reference
implementation for YOUR hnsw.rs.

## Why this matters

qdrant's HNSW is production plumbing; usearch is the algorithm with
the fat trimmed. Same paper, ~10× less code. The interesting part
is the memory layout: one contiguous "tape" per node.

## 1. The node tape

- `:2242` `class index_gt` — the whole index: a vector of node
  pointers + per-node tapes
- `:2404` `neighbors_ref_t` — a view over raw bytes (`tape_`, :2416):
  each node's storage is `[level | links-L0 | links-L1 | ... ]`,
  counts inline, slots preallocated to connectivity limits

```
 node tape:  ┌───────┬────────────────┬──────────┬─────┐
             │ level │ L0: cnt + M0×id │ L1: cnt+M×id │ ... │
             └───────┴────────────────┴──────────┴─────┘
             one allocation, all levels adjacent
```

Compare qdrant (per-level `Vec<Vec<_>>` in the builder, serialized
compressed later) and neo4j's scattered records (topic 13): usearch
picks "everything about a node in one place" — one pointer chase per
node visit, then streaming.

## 2. Defaults = the paper's advice, frozen

- `:1563` `default_connectivity() = 16` (M)
- `:1591` `connectivity_base = 2 × M` (M0) — computed at :1604
- `:1568` `default_expansion_add() = 128` (ef_construction)
- `:1573` `default_expansion_search() = 64` (ef)

## 3. The three core walks

- `:3234` `search_to_insert_` — Alg 1's per-level beam during insert;
  `:3239` `form_links_to_closest_` (defined :4262) applies the Alg 4
  heuristic and back-links (shrinking overfull neighbors)
- `:3446` `search_to_find_in_base_` — Alg 2 on layer 0 with an
  optional `predicate` — filtering exists here too, but ONLY as
  filter-during-traversal (no cardinality planner, no ACORN: compare
  qdrant's search.rs:55-84 — that gap IS qdrant's moat)
- `:3232`, `:3354` — the greedy descent loops
  (`level >= 0; --level`), including the update path (usearch
  supports in-place vector updates — rare among HNSW libs)

## 4. Concurrency

`:664-717` `striped_locks_gt` — insertions take striped per-node
locks (~threads × connectivity stripes), not one big lock; searches
are lock-free over published tapes. Simpler than qdrant's
RwLock-per-node builder; the cost is update-vs-read races handled by
slot versioning in `index_dense.hpp`.

## Questions (answer in notes.md)

1. Bytes per node for M=16, M0=32, avg 1.06 levels, u32 slots — tape
   vs qdrant-builder Vec-of-Vecs (count headers, capacity slack,
   allocator overhead).
2. Why preallocate link slots to the max instead of growing? What
   does it cost in memory, and what does it buy under concurrent
   insert?
3. Filter-during-traversal with a 1% predicate on usearch: what
   happens, and which qdrant mechanism was built to fix exactly this?
4. usearch templates the metric; qdrant enum-dispatches scorers. Map
   this to topic 11's compiled-vs-vectorized argument — who wins
   where?
5. For YOUR hnsw.rs: steal the tape or use `Vec<Vec<u32>>` per level?
   Decide, justify with expected access pattern, and note what M17's
   SIMD needs.
