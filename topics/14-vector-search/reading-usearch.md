# usearch: HNSW with the fat trimmed

qdrant's HNSW is production plumbing; usearch is the algorithm with
the fat trimmed — same paper, ~10× less code, essentially all of it
in one header. Read it as the reference implementation for YOUR
hnsw.rs. Before the code, this chapter builds the concepts in order:
what an HNSW node must store, why layout decides hop cost, the tape
that answers it, and how a search structure stays correct under
concurrent inserts. This chapter assumes
[reading-hnsw-paper.md](reading-hnsw-paper.md) — the algorithms (Alg
1/2/4, M, M0, ef) are used here by name.

## The problem in one sentence

Every hop in an HNSW search is a random memory access, so if a
node's per-level neighbor lists live in separately allocated
`Vec<Vec<u32>>` structures, each visit costs 2–3 dependent cache
misses (~100 ns apiece) instead of 1 — and at ~300 node visits per
query that's the difference between ~30 µs and ~90 µs before a
single distance is computed.

## The concepts, step by step

### Step 1 — what an HNSW node must store

An HNSW index is, per node: a level (how high the node reaches in
the hierarchy), and one neighbor list per layer from its level down
to 0 — up to M ids per upper layer, M0 = 2M at layer 0 — plus the
vector itself. The natural first implementation is a
`Vec<Vec<u32>>` per node (one inner Vec per level): easy to grow,
but every inner Vec is its own heap allocation somewhere else in
memory. Search touches nodes in data-dependent order (topic 0's
pointer chase), so layout — where those lists physically live — is
the entire performance story of an in-RAM implementation.

### Step 2 — the node tape: one allocation, all levels adjacent

usearch stores everything about a node's graph presence in one
contiguous byte buffer — the "tape": the level first, then each
layer's neighbor slot (a count followed by ids), preallocated to the
connectivity limits so nothing ever moves:

```
 node tape:  ┌───────┬────────────────┬──────────┬─────┐
             │ level │ L0: cnt + M0×id │ L1: cnt+M×id │ ... │
             └───────┴────────────────┴──────────┴─────┘
             one allocation, all levels adjacent
```

Finding layer l's neighbors is pure offset arithmetic — no pointer
hops:

```rust
// the tape: level header, then per-level slots preallocated to the
// connectivity limit — neighbors(l) is offset arithmetic, not Vec hops
struct NodeTape<'a> { bytes: &'a [u8] }   // one allocation per node

impl NodeTape<'_> {
    fn neighbors(&self, l: usize, m: usize, m0: usize) -> &[u32] {
        let slot = |links: usize| (1 + links) * 4;       // count + ids
        let start = 2 + if l == 0 { 0 }                  // 2 = level header
                    else { slot(m0) + (l - 1) * slot(m) };
        let cnt = read_u32(self.bytes, start) as usize;
        cast_u32(&self.bytes[start + 4..start + 4 + cnt * 4])
    }   // one miss to reach the tape; the rest prefetches
}
```

One miss to reach the tape, then the neighbor ids stream in
sequentially — the prefetcher's favorite pattern. Compare qdrant
(per-level `Vec<Vec<_>>` in the builder, serialized compressed
later) and neo4j's scattered records (topic 13): usearch picks
"everything about a node in one place" — one pointer chase per node
visit, then streaming. The cost: slots are preallocated to the max
(M or M0), so a node with 3 links pays for 16 — memory traded for
predictable layout and lock-free growth (Step 5).

### Step 3 — defaults: the paper's advice, frozen into constants

usearch hard-codes the parameter choices the ecosystem converged on
— the same table as the paper chapter, now as source constants:
`default_connectivity() = 16` (M), `connectivity_base = 2 × M = 32`
(M0), `default_expansion_add() = 128` (ef_construction),
`default_expansion_search() = 64` (ef). Worth internalizing: the
tape's size per node is fixed the moment M is chosen — question 1
makes you count the bytes.

### Step 4 — the three walks: the paper's algorithms as three functions

The whole engine is three traversals, each a direct transcription of
a paper algorithm:

- **`search_to_insert_`** — Alg 1's per-level beam during insert;
  `form_links_to_closest_` applies the Alg 4 heuristic and
  back-links (shrinking overfull neighbors back to their slot
  limits).
- **`search_to_find_in_base_`** — Alg 2 on layer 0, with an optional
  `predicate` parameter — filtering exists here too, but ONLY as
  filter-during-traversal: the predicate rejects nodes as they're
  scored. There is no cardinality planner and no ACORN; a selective
  filter simply disconnects the walk (percolation — compare qdrant's
  search.rs:55-84; that gap IS qdrant's moat, walked in
  [reading-qdrant-hnsw.md](reading-qdrant-hnsw.md)).
- **The greedy descent loops** (`level >= 0; --level`) — the ef=1
  upper-layer descent, including the update path: usearch supports
  in-place vector updates, rare among HNSW libraries.

The mapping is the point: one paper algorithm ↔ one function, no
architecture in between. That's what "reference implementation for
your hnsw.rs" means concretely.

### Step 5 — concurrency: striped locks for writers, lock-free readers

Concurrent inserts mutate neighbor lists, so writes need exclusion —
but one global lock would serialize the build. usearch uses
**striped locks** (`striped_locks_gt`): a fixed array of ~threads ×
connectivity mutexes, each node hashing to one stripe — writers take
only their stripe, so unrelated inserts proceed in parallel.
Searches take NO locks: they read published tapes, which never move
(Step 2's preallocation pays off here — growth never reallocates).
Simpler than qdrant's RwLock-per-node builder; the cost is
update-vs-read races, handled by slot versioning in
`index_dense.hpp`.

## Where each step lives in the code

Everything is in `include/usearch/index.hpp` (line numbers from the
walked revision; navigate by symbol name when they drift):

- **Step 1–2 (the tape)**: `:2242` `class index_gt` — the whole
  index: a vector of node pointers + per-node tapes; `:2404`
  `neighbors_ref_t` — the view over raw bytes (`tape_`, :2416) that
  the Rust sketch above transcribes.
- **Step 3 (defaults)**: `:1563` `default_connectivity() = 16` (M);
  `:1591` `connectivity_base = 2 × M` (M0) — computed at :1604;
  `:1568` `default_expansion_add() = 128` (ef_construction); `:1573`
  `default_expansion_search() = 64` (ef).
- **Step 4 (the walks)**: `:3234` `search_to_insert_`; `:3239`
  `form_links_to_closest_` (defined :4262) — Alg 4 + back-links;
  `:3446` `search_to_find_in_base_` — Alg 2 with the `predicate`;
  `:3232`, `:3354` — the greedy descent loops, including the update
  path.
- **Step 5 (concurrency)**: `:664-717` `striped_locks_gt`; the
  type-erased/quantized wrapper and slot versioning live in
  `index_dense.hpp`.

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

## References

**Papers**
- Malkov, Yashunin — the HNSW paper
  ([arXiv:1603.09320](https://arxiv.org/abs/1603.09320)) — gets its
  own chapter: [reading-hnsw-paper.md](reading-hnsw-paper.md)

**Code**
- [usearch](https://github.com/unum-cloud/usearch) — all of it in
  `include/usearch/index.hpp` (+ `index_dense.hpp` for the
  type-erased/quantized wrapper); C++ templates, but small enough to
  hold in your head
