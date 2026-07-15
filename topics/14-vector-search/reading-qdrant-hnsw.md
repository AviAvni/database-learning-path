# Qdrant's HNSW: filtered search is a planner problem

Production HNSW: the paper plus five years of scar tissue — and
filtering, qdrant's actual specialty. The payoff of this chapter is
watching a query planner appear inside an index; before the code, it
builds the ideas in order — the build/serve split, the pooled search
machinery, why filters shatter graphs (percolation), and the
per-query decision that picks HNSW / brute force / ACORN from an
estimated cardinality. Everything lives under
`lib/segment/src/index/hnsw_index/`; this chapter assumes
[reading-hnsw-paper.md](reading-hnsw-paper.md).

## The problem in one sentence

`WHERE category = X AND vec NEAR q` breaks a graph index: rejecting
non-matching nodes during traversal effectively deletes them, and a
graph with average degree K disconnects once only ~1/K of its nodes
survive — so at 5% selectivity on an M0=32 graph, greedy search
strands in an island and recall falls off a cliff.

## The concepts, step by step

### Step 1 — build structure ≠ serve structure

A graph being *built* needs concurrent mutation (parallel inserts
locking individual nodes); a graph being *queried* needs compact,
immutable, cache-friendly layout. qdrant makes them two types:

- `GraphLayersBuilder` (graph_layers_builder.rs:35) — per-node
  `RwLock`'d link lists so threads insert in parallel; holds the
  paper's build parameters: `ef_construct` (:38),
  `level_factor = 1/ln(M)` (:317 — the paper's mL),
  `get_random_layer` (:385, `-ln(sample) * level_factor` at :391),
  and `link_new_point` (:414) — Alg 1. The Alg 4 heuristic is a
  *flag*: `use_heuristic` (:41-42) — find
  `select_candidates_with_heuristic` below it and match the paper.
- `GraphLayers` (graph_layers.rs:74) — the FROZEN serve-side graph:
  `search_on_level` (:109), `search_entry` (:248 — the ef=1 greedy
  descent).

The same builder/immutable split as CSR (topic 13): pay a
conversion step once, serve reads from the compact form forever.

### Step 2 — the search machinery: two heaps and a pooled visited set

Serving a query needs the paper's Alg 2 state: `SearchContext`
(search_context.rs:8) holds the two bounded heaps (candidates
min-heap, results max-heap of size ef). The hot structure is the
**visited set** — every scored node checks and sets membership.
Allocating and zeroing one per query would dominate small searches,
so qdrant pools them: `VisitedListHandle` (visited_pool.rs:9) hands
out reusable lists, "cleared" by bumping a generation stamp instead
of zeroing (:14's comment says exactly this). Your hop_bench stamp
trick, productionized with a pool because queries are concurrent —
each in-flight query borrows its own list.

### Step 3 — percolation: why filters shatter graphs

Percolation theory studies when a graph falls apart as you randomly
delete nodes: a random graph with average degree K stays connected
while more than ~1/K of nodes survive, and disintegrates into
islands below that. A filter that rejects nodes during traversal IS
node deletion from the walk's point of view. So each filter has a
critical **selectivity** (the fraction of points that pass):

```
 survival fraction p:      1.0 ────────── ~1/K ──────────── 0.0
 filtered graph:           connected      │    islands
 greedy search:            works          │    strands near start,
                                          │    recall cliff
```

qdrant doesn't assume the threshold — it computes it, in a comment
citing the theory (hnsw/build.rs:378-386):

```rust
// According to percolation theory, random graph becomes disconnected
// if 1/K points are left, where K is average number of links per point
let percolation = 1. - 2. / (average_links_per_0_level_int as f32);
```

Then it MEASURES: sample subgraph connectivity at the 2/K survival
point (:390-392, three samples, take max), and if the main graph
would shatter for an indexed payload category, add extra
category-aware links (`payload_m`, hnsw.rs:93) so each category's
subgraph is navigable on its own. The failure mode is measured
during build — topic 0 discipline inside an index builder.

### Step 4 — the per-query decision: a planner inside the index

With the cliff located, each query picks its algorithm from an
estimated filter cardinality (the number of points expected to pass
— topic 10's central quantity), in hnsw/search.rs:55-84:

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
  non-matching points during traversal (the graph stays connected
  well above 1/K, so this is safe)
- selectivity low → `search_plain_batched` (:264) — brute-force the
  filtered id list; below `full_scan_threshold` the graph can't
  help, and scanning 500 survivors exactly beats walking a
  shattered graph approximately
- middle → ACORN, Step 5

The cost of getting this wrong is asymmetric: brute-forcing a 90%
filter scans ~900K points for nothing; HNSW-ing a 1% filter returns
garbage. Hence a planner, not a constant.

### Step 5 — ACORN: traverse through the blocked nodes

For the middle band, ACORN (`search_on_level_acorn`,
graph_layers.rs:155) keeps the walk connected WITHOUT extra links:
when expanding a node, also consider its 2-hop neighborhood —
neighbors-of-neighbors — treating filtered-out nodes as passable
wires rather than walls:

```
 1-hop, filtered:   ● ──✗── ✗ ──✗── ●     walk stops at the wall
 ACORN 2-hop:       ● ──(✗)──(✗)── ●      blocked nodes relay,
                        pass-through        only ● gets scored
```

If a p fraction of nodes pass, 1-hop expansion sees ~K·p useful
edges but 2-hop sees ~K²·p — squaring the degree pushes the
percolation threshold from ~1/K down to ~1/K². The price: more
distance computations per expansion (the 2-hop frontier is bigger),
paid only on queries in the awkward band. Compare `payload_m`'s
extra links (Step 3): RAM paid at build time for known categories vs
CPU paid at query time for arbitrary filters — question 2.

### Step 6 — the scar tissue worth grepping

Production remainders, each a small chapter of its own:

- `hnsw/build.rs:95-109` — `full_scan_threshold` derives the
  "indexing threshold": tiny segments never build a graph at all
  (brute force wins below ~thousands of points — the build cost
  never amortizes).
- `graph_links.rs` — serialized link format: delta-compressed,
  topic 12 encodings applied to graph edges.
- `gpu/` — GPU-built HNSW (topic 18 preview).
- `graph_layers_healer.rs` — repairing the graph around deleted
  points instead of rebuilding: the paper's deletes wart, patched.

## Where each step lives in the code

All paths relative to `lib/segment/src/index/hnsw_index/`:

| step | anchors |
|---|---|
| 1 build side | `graph_layers_builder.rs:35` (builder), `:38` ef_construct, `:41-42` use_heuristic, `:317` level_factor, `:385/:391` get_random_layer, `:414` link_new_point |
| 1 serve side | `graph_layers.rs:74` GraphLayers, `:109` search_on_level, `:248` search_entry |
| 2 machinery | `search_context.rs:8` SearchContext, `visited_pool.rs:9/:14` VisitedListHandle |
| 3 percolation | `hnsw/build.rs:378-386` threshold, `:390-392` connectivity sampling, `hnsw.rs:93` payload_m |
| 4 the planner | `hnsw/search.rs:55-84` algorithm choice, `:74` estimate_cardinality, `:80` selectivity, `:264` search_plain_batched |
| 5 ACORN | `graph_layers.rs:155` search_on_level_acorn |
| 6 scar tissue | `hnsw/build.rs:95-109`, `graph_links.rs`, `gpu/`, `graph_layers_healer.rs` |

Read order: Step 4's `search.rs:55-84` first (it's 30 lines and the
thesis), then chase each branch to its implementation.

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

## References

**Papers**
- Patel, Kraft, Guestrin, Zaharia — "ACORN: Performant and
  Predicate-Agnostic Search Over Vector Embeddings and Structured
  Data" (SIGMOD 2024,
  [arXiv:2403.04871](https://arxiv.org/abs/2403.04871)) — optional;
  the 2-hop-expansion idea qdrant adopted
- The HNSW paper itself is
  [reading-hnsw-paper.md](reading-hnsw-paper.md)

**Code**
- [qdrant](https://github.com/qdrant/qdrant) — everything under
  `lib/segment/src/index/hnsw_index/`: `graph_layers_builder.rs`,
  `graph_layers.rs`, `search_context.rs`, `visited_pool.rs`,
  `hnsw/search.rs` (the per-query algorithm choice), `hnsw/build.rs`
  (the percolation measurement), `graph_links.rs`,
  `graph_layers_healer.rs`
