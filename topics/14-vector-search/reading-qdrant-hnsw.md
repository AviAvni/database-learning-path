# Qdrant's HNSW: filtered search is a planner problem

Production HNSW: the paper plus five years of scar tissue — and
filtering, qdrant's actual specialty. The payoff of this chapter is
watching a query planner appear inside an index; before the code, it
builds the ideas in order — the build/serve split, the pooled search
machinery, why filters shatter graphs (percolation), and the
per-query decision that picks brute force / HNSW / ACORN from an
estimated cardinality. This chapter assumes
[reading-hnsw-paper.md](reading-hnsw-paper.md).

Every `file:line` below was read at **qdrant v1.20.0,
`qdrant/qdrant@222b2e44a`**, the revision pinned in
`resources/codebases.md`; re-verify any of them with
`python3 tools/pinned-source.py show qdrant <path> -r A:B`. Most
paths are under `lib/segment/src/index/hnsw_index/`, but not all —
`visited_pool.rs` sits one directory up, in
`lib/segment/src/index/`, and the quantization pieces are in a
separate crate ([reading-qdrant-quantization.md](reading-qdrant-quantization.md)).

## The problem in one sentence

`WHERE category = X AND vec NEAR q` breaks a graph index: rejecting
non-matching nodes during traversal effectively deletes them from the
walk, and a random graph with average degree K disintegrates once
only ~1/K of its nodes survive — so on qdrant's default `m0 = 32`
graph the cliff sits near 3% selectivity, and a query below it
strands in an island while the brute-force alternative it is trying
to beat still runs at this topic's measured **117 QPS**.

Two definitions used throughout. **Selectivity** is the fraction of
indexed vectors that pass the filter (qdrant computes it as
`query_cardinality.exp / available_vector_count`,
`hnsw/read_view/dispatch.rs:238`) — so *low* selectivity means a
*restrictive* filter. **Percolation threshold** is the survival
fraction below which a random graph stops having one giant connected
component.

## The concepts, step by step

### Step 1 — build structure ≠ serve structure

> **In:** the paper's single conceptual graph. **Out:** two Rust
> types with different layouts, and the reason a builder and a served
> index cannot be the same object.

A graph being *built* needs concurrent mutation — parallel inserts
each locking individual nodes. A graph being *queried* needs a
compact, immutable, cache-friendly layout. qdrant makes them two
types:

- **`GraphLayersBuilder`** (`graph_layers_builder.rs:35`) — its link
  storage is `links_layers: Vec<LockedLayersContainer>` (:43), where
  the two aliases at :30-31 unfold to `Vec<Vec<RwLock<LinksContainer>>>`:
  one lock per node per level, so threads insert in parallel. It
  holds the paper's build parameters: `ef_construct` (:38),
  `level_factor` (:40), `use_heuristic` (:42), and `link_new_point`
  (:468), which is Algorithm 1.
- **`GraphLayers`** (`graph_layers.rs:69`) — the frozen serve-side
  graph: its links are a serialised `GraphLinks` (:71) and it owns a
  `VisitedPool` (:73). The search functions are trait methods on
  `GraphLayersBase`: `search_on_level` (:114) is Algorithm 2, and
  `search_entry` (:276) is Algorithm 5's ef=1 descent — its doc
  comment at :275 says *"Beam size is 1"*.

The same builder/immutable split as CSR in topic 13: pay a
conversion step once, serve reads from the compact form forever.

Two details in the builder are worth checking against the paper
before you trust your mental model. The first is the level draw.

```rust
// graph_layers_builder.rs — the level constant (322, inside
// new_with_params) and the level draw's mapping (433-447).
   322              level_factor: 1.0 / (max(hnsw_m.m, 2) as f64).ln(),
// ... 323-432: the rest of the constructor, merge logic, get_random_layer ...
   433      /// Map a uniform `[0, 1)` sample to a geometric level.
   434      ///
   435      /// With `level_factor = 1 / ln(m)`, `floor` gives `P(level >= l) = m^-l`,
   436      /// the distribution from the HNSW paper: 1 in `m` points reaches level 1.
   437      /// Rounding instead would give `m^-(l - 0.5)`, putting 4x as many points on
   438      /// level 1 at `m = 16` and growing the upper layers with no search benefit.
   439      ///
   440      /// `Uniform::new(0.0, 1.0)` is half-open, so `sample` can be exactly `0.0`.
   441      /// `ln(0.0)` is `-inf`, and `(-(-inf) * factor).floor() as usize` saturates
   442      /// to `usize::MAX`, which then makes `set_levels` allocate unboundedly.
   443      /// Clamp to the smallest positive `f64` so the level stays bounded.
   444      fn level_from_sample(sample: f64, level_factor: f64) -> usize {
   445          let sample = sample.max(f64::MIN_POSITIVE);
   446          (-sample.ln() * level_factor).floor() as usize
   447      }
```

`level_factor` at :322 is the paper's `mL = 1/ln(M)` exactly (§4.1),
with a floor of `ln 2` so M=1 cannot divide by zero. The line to
focus on is 446: **`.floor()`**, Algorithm 1 line 4 verbatim. The
doc comment at :435-438 does the arithmetic for you, and it is worth
redoing at the default M = 16, because the choice of floor versus
round is not cosmetic:

```
  draw  level = f(-ln(U) · mL),  U uniform on [0, 1),  mL = 1/ln 16

  floor:  level ≥ 1  ⇔  -ln(U)·mL ≥ 1    ⇔  U ≤ 16^(-1)   = 1/16 = 6.25%
  round:  level ≥ 1  ⇔  -ln(U)·mL ≥ 0.5  ⇔  U ≤ 16^(-1/2) = 1/4  = 25%
```

A rounded draw would put four times as many points above layer 0 —
four times the upper-layer link memory — for no search benefit. The
other guard is the edge case at :440-445. `U = 0.0` is a legal draw,
`-ln(0)` is infinite, and the saturating cast would turn that into a
level of `usize::MAX`. Clamping to `f64::MIN_POSITIVE`
(2.225 × 10⁻³⁰⁸) caps the worst draw at
`floor(708.40 × 0.36067) = floor(255.5) = 255` levels.

The second detail is the Algorithm 4 heuristic. It is a *flag*, not
a given: `use_heuristic` (:42) selects between `link_with_heuristic`
(:601) and `link_without_heuristic` (:627) at :592-596. The heuristic
itself lives one file over, in `links_container.rs`, and its
load-bearing line is a single `continue`:

```rust
// links_container.rs — is_redundant (20-31) and the selection loop of
// fill_from_sorted_with_heuristic (84-100). This is the paper's Algorithm 4.
    20  #[inline]
    21  fn is_redundant(
    22      candidate_to_existing: ScoreType,
    23      candidate_score: ScoreType,
    24      existing_score: ScoreType,
    25  ) -> bool {
    26      if candidate_score == existing_score {
    27          candidate_to_existing >= candidate_score
    28      } else {
    29          candidate_to_existing > candidate_score
    30      }
    31  }
// ... 32-83: LinksContainer, its accessors, the signature and the level_m == 0 guard ...
    84          let mut selected_scores: Vec<ScoreType> = Vec::with_capacity(level_m);
    85          'outer: for candidate in candidates {
    86              for (&existing, &existing_score) in self.links.iter().zip(&selected_scores) {
    87                  if is_redundant(
    88                      score(candidate.idx, existing),
    89                      candidate.score,
    90                      existing_score,
    91                  ) {
    92                      continue 'outer;
    93                  }
    94              }
    95              self.links.push(candidate.idx);
    96              selected_scores.push(candidate.score);
    97              if self.links.len() >= level_m {
    98                  break;
    99              }
   100          }
```

The line to focus on is 29: Algorithm 4 line 11 (*"if e is closer
to q compared to any element from R"*) with the inequality flipped,
because qdrant's `score` is a **similarity** — higher means closer —
so "closer to an already-kept neighbour than to the query" reads as
`score(candidate, existing) > candidate.score`. Line 27 is the one
place qdrant departs from the paper's rule. When the candidate and
the already-kept link are *exactly* as close to the query
(:26), the test becomes `>=`. The doc comment at :9-19 gives the
reason: a group of byte-identical vectors scores equal to everything
in the group, so under a strict `>` none of them prunes another, the
group fills every link slot of each member, and the group becomes a
sink with no edges leaving it. For ordinary vectors exact equality
essentially never happens, so the rule is unchanged for them. Note
also what is *not* here: neither of the paper's `extendCandidates`
nor `keepPrunedConnections` flags exists in this implementation.

### Step 2 — the search machinery: two heaps and a pooled visited set

> **In:** a frozen `GraphLayers` (Step 1) and a query. **Out:** the
> two structures Algorithm 2 needs, and why one of them is pooled
> rather than allocated.

`SearchContext` (`search_context.rs:10`) is Algorithm 2's state in
one struct: `nearest`, a `FixedLengthPriorityQueue` of size ef (:12 —
the paper's W), and `candidates`, a `BinaryHeap` (:14 — the paper's
C). `new(ef)` is :18-23, `with_entries` (:26-38) seeds both heaps and
marks the entry points visited, `lower_bound` (:40-45) is the stop
test's input, and `process_candidate` (:49-57) is the line-13
admission test.

The hot structure is the **visited set**: every scored node checks
and sets membership, so it is touched more often than anything else
in the query. Allocating and zeroing one per query would dominate
small searches, so qdrant pools them. `VisitedListHandle`
(`lib/segment/src/index/visited_pool.rs:9`) hands out reusable lists,
and the "clearing" is a generation stamp:

```rust
// visited_pool.rs — VisitedList (18-22) and next_iteration (86-92),
// with the handle's check/check_and_update_visited/unvisit elided.
    18  #[derive(Debug)]
    19  struct VisitedList {
    20      current_iter: u8,
    21      visit_counters: Vec<u8>,
    22  }
// ... 23-85: Default, new(), the handle, check(), check_and_update_visited(), unvisit() ...
    86      pub fn next_iteration(&mut self) {
    87          self.visited_list.current_iter = self.visited_list.current_iter.wrapping_add(1);
    88          if self.visited_list.current_iter == 0 {
    89              self.visited_list.current_iter = 1;
    90              self.visited_list.visit_counters.fill(0);
    91          }
    92      }
```

The counters are `u8`, one byte per point, and a query is "cleared"
by incrementing `current_iter` (:87); only when the stamp wraps to 0
does :90 really zero the array. Work the cost:

```
  points in the segment          n = 100 000
  bytes per point                1 (u8 counter)
  ------------------------------------------------
  visited list size              100 000 B = 100 kB
  stamps before a real wipe      255
  amortised zeroing per query    100 000 / 255 = 392 bytes
```

392 bytes of memset per query instead of 100 kB — a 255× reduction,
and that is only the *amortised* case. `VisitedPool`
(`visited_pool.rs:105-107`) wraps a `RwLock<Vec<VisitedList>>`, `get`
(:116-128) pops one and calls `next_iteration` on it (:124), and
`return_back` (:130-135) puts it back while the pool holds fewer than
`POOL_KEEP_LIMIT` lists — the number of CPUs clamped to 16..128
(`lib/common/common/src/defaults.rs:50`). That last part is the
difference from topic 13's single-threaded stamp trick: queries are
concurrent, so each in-flight query needs its *own* list, and the
pool bounds how many the process keeps alive.

### Step 3 — percolation: why filters shatter graphs

> **In:** an HNSW graph (Step 1) with average level-0 degree K, and a
> filter passing a fraction p of points. **Out:** the critical p below
> which greedy search cannot work at all, and the measurement qdrant
> takes at build time instead of assuming it.

Percolation theory studies when a graph falls apart as you randomly
delete parts of it. Deleting *nodes* is **site percolation**;
deleting *edges* is **bond percolation**. For a random graph with
average degree K both have the same threshold: the graph keeps one
giant connected component while each node still has, on average,
more than one surviving neighbour — that is, while more than ~1/K of
the nodes (or edges) survive — and disintegrates into islands below
that. A filter that rejects nodes during traversal *is* site
percolation from the walk's point of view, and in qdrant it literally
is:

```rust
// point_scorer.rs — FilteredScorer::score_points, 331-346.
   331      pub fn score_points(
   332          &mut self,
   333          point_ids: &mut Vec<PointOffsetType>,
   334          limit: usize,
   335      ) -> OperationResult<impl Iterator<Item = ScoredPointOffset>> {
   336          let mut n = self
   337              .filters
   338              .check_batched(point_ids, Select::Matches, Rest::Discard)
   339              .unwrap_or(0 /* TODO(uio): propagate error */);
   340          if limit != 0 {
   341              n = n.min(limit);
   342          }
   343          point_ids.truncate(n);
   344
   345          self.score_points_unfiltered(point_ids)
   346      }
```

The line to focus on is 338. `check_batched` partitions the
neighbour list in place, matches to the left, and returns the
partition point (its contract is drawn in
`lib/common/common/src/condition_checker.rs:28-38`); :343 truncates
everything else away *before* :345 computes a single distance.
`search_on_level` feeds every unvisited neighbour of the current
candidate through this call (`graph_layers.rs:136-147`), so a
rejected neighbour never enters the candidate heap and the edges
through it do not exist.

```
 survival fraction p:      1.0 ────────── ~1/K ──────────── 0.0
 filtered graph:           connected      │    islands
 greedy search:            works          │    strands near start,
                                          │    recall cliff
```

qdrant does not assume the threshold — it computes one and then
measures around it:

```rust
// hnsw/build/additional_links.rs — the percolation sampling point and
// the main graph's connectivity measurement, 123-145.
   123      // According to percolation theory, random graph becomes disconnected
   124      // if 1/K points are left, where K is average number of links per point
   125      // So we need to sample connectivity relative to this bifurcation point, but
   126      // not exactly at 1/K, as at this point graph is very sensitive to noise.
   127      //
   128      // Instead, we choose sampling point at 2/K, which expects graph to still be
   129      // mostly connected, but still have some measurable disconnected components.
   130
   131      let percolation = 1. - 2. / (average_links_per_0_level_int as f32);
   132
   133      let required_connectivity = if average_links_per_0_level_int >= 4 {
   134          let global_graph_connectivity = [
   135              graph_layers_builder.subgraph_connectivity(rng, &all_points, percolation),
   136              graph_layers_builder.subgraph_connectivity(rng, &all_points, percolation),
   137              graph_layers_builder.subgraph_connectivity(rng, &all_points, percolation),
   138          ];
   139
   140          debug!("graph connectivity: {global_graph_connectivity:?} @ {percolation}");
   141
   142          global_graph_connectivity
   143              .iter()
   144              .copied()
   145              .max_by(|a, b| a.partial_cmp(b).unwrap())
```

Read :131 carefully, because the variable name inverts the meaning:
`percolation` is the **drop** probability. `subgraph_connectivity`
(`graph_layers_builder.rs:110-211`) runs a breadth-first search from
an entry point and flips a coin on every edge it crosses, skipping
the edge when the coin lands below `q` (:177-181) — bond percolation
— then returns the fraction of points reached (:210). So the edge
*survival* probability it samples at is `2/K`, exactly what the
comment says. `K` itself is measured, not assumed:
`get_average_connectivity_on_level(0)` at `additional_links.rs:114`.
Put qdrant's own default in:

```
  K = average links per point on level 0 ≈ m0 = 2·m = 32
      (config.rs:47 sets m0 = m*2; types.rs:1683 sets m = 16;
       additional_links.rs:114 measures the real average)

  percolation threshold  1/K = 1/32 = 3.1%   ← the cliff
  sampling point         2/K = 2/32 = 6.3%   ← where qdrant looks
  `percolation` variable 1 − 2/32 = 0.9375   ← the DROP probability
  surviving degree       32 × 2/32 = 2       ← vs 1 at the cliff
```

So the code drops 93.75% of edges three times (:135-137, different
RNG draws), measures the fraction of points still reachable each
time, and keeps the best of the three (:142-145) as
`required_connectivity`. The guard at :133 skips this entirely for
graphs with average degree below 4, where the arithmetic is
meaningless.

That number is a yardstick for the next stage. If the main graph
would shatter for an indexed payload value, the build adds extra
links *inside each payload block* — the set of points sharing one
value of one indexed field — so the block's subgraph is navigable on
its own. The link budget is `payload_m`: the config doc at
`types.rs:875-877` says that if it is unset, `m` is used, which
`payload_hnsw_m` (`hnsw/old_index.rs:256-261`) implements with
`payload_m0 = payload_m * 2` from `config.rs:53`; so by default every
indexed field whose schema enables HNSW
(`additional_links.rs:57`) gets them. Each block is built as its own
small graph and appended to its points' existing link lists,
*"skipping links it already has"* (`graph_layers_builder.rs:370`).
Three rules decide which blocks get links:

```rust
// hnsw/build/additional_links.rs — the block-size cap (187-192) and the
// connectivity test against the main graph (222-235), inside build_additional_links.
   187          const PERCOLATION_MULTIPLIER: usize = 4;
   188          let max_block_size = if config.m > 0 {
   189              total_vector_count / average_links_per_0_level_int * PERCOLATION_MULTIPLIER
   190          } else {
   191              usize::MAX
   192          };
// ... 193-221: process_block's head — skip blocks above max_block_size (199-201),
//              collect the block's points, skip blocks at or below
//              full_scan_threshold / 4 (216-220) ...
   222              if !is_tenant
   223                  && index_pos > 0
   224                  && let Some(required_connectivity) = required_connectivity
   225              {
   226                  // Always build for tenants
   227                  let graph_connectivity =
   228                      graph_layers_builder.subgraph_connectivity(rng, &points_to_index, percolation);
   229
   230                  if graph_connectivity >= required_connectivity {
   231                      trace!(
   232                          "skip building additional HNSW links for {field}, connectivity {graph_connectivity:.4} >= {required_connectivity:.4}"
   233                      );
   234                      return Ok(());
   235                  }
```

First, a block larger than `4/K` of the collection (:189) is skipped:
a filter that selective-but-not-very passes far more than the 1/K
cliff, so the main graph already serves it. At 100 000 vectors and
K = 32 that cap is `100 000 / 32 × 4 = 3 125 × 4 = 12 500` points.
Second, a block small enough that a filtered query on it would go to
brute force (Step 4) is skipped at :216-220. Third — the line to
focus on is 230 — from the second indexed field on (`index_pos > 0`,
:223), a block whose own subgraph, sampled at the same 93.75% edge
drop, is already at least as connected as the main graph was gets no
links: the links built so far already hold it together. Tenant
fields always get them (:226). The failure mode is *measured during
build*: topic 0's discipline, inside an index builder.

### Step 4 — the per-query decision: a planner inside the index

> **In:** a query with an optional filter, and the graph Step 3 built.
> **Out:** one of three algorithms, chosen from an estimated
> cardinality — topic 10's optimiser, living inside a vector index.

With the cliff located, each query picks its algorithm, in two
stages. The first stage, in `HNSWIndexReadView::search`
(`hnsw/read_view/dispatch.rs:28-197`), decides between the graph and
a brute-force scan of the matching ids:

```rust
// hnsw/read_view/dispatch.rs — the filtered branch of search, 99-156,
// with the exact-search and timer plumbing elided.
    99                  let available_vector_count = self.vector_storage.available_vector_count();
// ... 100-103: comment — the estimate is made once and reused by every strategy ...
   104                  let query_point_cardinality =
   105                      self.payload_index.estimate_cardinality(query_filter)?;
   106                  let query_cardinality = adjust_to_available_vectors(
   107                      query_point_cardinality,
   108                      available_vector_count,
   109                      self.id_tracker.available_point_count(),
   110                  );
// ... 111-131: exact search, or a graph with no links at all, goes plain ...
   132                  if query_cardinality.max < self.config.full_scan_threshold {
   133                      // if cardinality is small - use plain index
// ... 134-143: search_vectors_plain over the matching ids ...
   144                  }
   145
   146                  if query_cardinality.min > self.config.full_scan_threshold {
   147                      // if cardinality is high enough - use HNSW index
   148                      return self.search_vectors_with_graph_filtered(
// ... 149-155: arguments ...
   156                  }
```

Line 105 is topic 10's cardinality estimator, called from inside a
vector index. It returns a **`CardinalityEstimation`**: a range
`min..max` the true count is guaranteed to lie in, plus `exp`, a
point estimate inside it. :106-110 rescales that estimate — the
payload index counts *points* — to the number of *vectors* actually
present in this segment. Then the range is compared with
`full_scan_threshold` (a point count, Step 6). If even the upper
bound is below it (:132), brute force over the matching ids wins; if
even the lower bound is above it (:146), the graph wins; only when
the threshold falls *inside* the range does :161-170 pay for a
sampling estimate (`sample_check_cardinality`) to settle it. The
line to focus on is 132: the decision is made on the estimate's
*bounds*, not its point value, so a confident estimate never pays
for sampling.

Once the graph is chosen, the second stage picks the walk:

```rust
// hnsw/read_view/dispatch.rs — filtered_graph_algorithm, 218-249.
   218      fn filtered_graph_algorithm(
   219          &self,
   220          query_cardinality: &CardinalityEstimation,
   221          params: Option<&SearchParams>,
   222      ) -> SearchAlgorithm {
   223          let Some(acorn) = params.and_then(|params| params.acorn) else {
   224              return SearchAlgorithm::Hnsw;
   225          };
   226          if !acorn.enable {
   227              return SearchAlgorithm::Hnsw;
   228          }
// ... 229-233: a NOTE about unfiltered searches on heavily-deleted segments ...
   234          let available_vector_count = self.vector_storage.available_vector_count();
   235          let selectivity = if available_vector_count == 0 {
   236              1.0
   237          } else {
   238              query_cardinality.exp as f64 / available_vector_count as f64
   239          };
   240
   241          let max_selectivity = acorn
   242              .max_selectivity
   243              .map_or(ACORN_MAX_SELECTIVITY_DEFAULT, |v| *v);
   244          if selectivity <= max_selectivity {
   245              SearchAlgorithm::Acorn
   246          } else {
   247              SearchAlgorithm::Hnsw
   248          }
   249      }
```

ACORN is opt-in per query: without `acorn.enable` in the search
params, :223-228 return plain HNSW. When it is on, line 238 divides
the estimate's *expected* value by the live vector count to get
selectivity, and line 244 — the one to focus on — compares it with
`max_selectivity`, which defaults to
`ACORN_MAX_SELECTIVITY_DEFAULT = 0.4` (`lib/segment/src/types.rs:649`).
Nothing here inspects the graph's shape: a collection built with
`m = 0` has no main graph, only Step 3's payload-block links, and
ACORN walks it with unlimited hop limits (Step 5).

The full menu:

| estimate | plan | why |
|---|---|---|
| `max` < `full_scan_threshold` | `search_vectors_plain` (`hnsw/read_view/search.rs:285`) → `search_plain_batched` (:200) | the surviving id list is small enough to score exactly, and exact beats an approximate walk over a shattered graph |
| graph chosen, ACORN off or selectivity > 0.4 | plain HNSW, `FilteredScorer` rejecting during traversal | the graph stays connected well above 1/K, so deleting nodes from the walk is safe |
| graph chosen, ACORN on, selectivity ≤ 0.4 | ACORN (Step 5) | connected enough to walk, too sparse to walk naively |

The cost of getting this wrong is asymmetric, which is the argument
for a planner rather than a constant: brute-forcing a 90% filter
scans ~900k points for nothing, while HNSW-ing a 1% filter returns
garbage — and unlike the first mistake, the second one is silent. The survey ladder and the measured over-fetch table are in [reading-filtered-ann-survey.md](reading-filtered-ann-survey.md).

One more place the filter reaches into the walk: where it starts.
`HnswGraph::get_entry_point` (`graph.rs:219-273`) first looks for a
stored entry point that passes the filter (:246-251); if none does,
it asks the payload index for the first `SEEDS_PER_EF × max(ef, top)`
matching points (`SEEDS_PER_EF = 2`, :67; the count is computed at
:178) and starts the level-0 search from all of them at once
(`SearchEntry::Seeds`, :253-269). A restrictive filter therefore
never strands the walk at a non-matching entry point.

One anchor to keep straight: `get_oversampled_top` appears at
`hnsw/read_view/search.rs:56` (and again at :249 for the plain
path), but those are only call sites. Its definition is in a
different module, `lib/segment/src/index/vector_index_search_common.rs:26-44`,
and it belongs to quantization rather than to the planner — see
[reading-qdrant-quantization.md](reading-qdrant-quantization.md).

### Step 5 — ACORN: traverse through the blocked nodes

> **In:** a query Step 4 routed to ACORN — restrictive enough to
> shatter the graph, broad enough that scanning is wasteful. **Out:**
> a walk that stays connected without any extra links, and the
> scoring bill it runs up.

For the middle band, qdrant implements **ACORN-1** — the paper's
cheap variant; the module doc at `graph_layers.rs:11-13` and
`search_on_level_acorn`'s own doc at :153-154 name it that
specifically, so do not read the generic ACORN paper and expect a
match. The idea: when expanding a node, treat filtered-out
neighbours as passable wires rather than walls, and reach their
neighbours instead.

```
 1-hop, filtered:   ● ──✗── ✗ ──✗── ●     walk stops at the wall
 ACORN 2-hop:       ● ──(✗)──(✗)── ●      blocked nodes relay,
                        pass-through        only ● gets scored
```

The implementation is precise about who pays. First the budgets:

```rust
// graph_layers.rs — search_on_level_acorn's limits, 179-197.
   179          // Limits are per every explored 1-hop or 2-hop neighbors, not total.
   180          // This is necessary to avoid over-scoring when there are many
   181          // additional graph links.
   182          let m = self.get_m(level);
   183          let (hop1_limit, hop2_limit, capacity);
   184          if m != 0 {
   185              hop1_limit = m;
   186              hop2_limit = m;
   187              capacity = hop1_limit * hop2_limit.min(16);
   188          } else {
   189              // `m = 0` graphs only have payload links: take all, as `search_on_level` does.
   190              hop1_limit = usize::MAX;
   191              hop2_limit = usize::MAX;
   192              capacity = 16 * 16; // Guess: payload_m = 16
   193          }
   194
   195          // Non-matches past the first `hop1_limit` links (the payload-block
   196          // tail) share this budget, evenly spaced.
   197          let hop1_tail_limit = hop1_limit;
```

`get_m(level)` (:500-502) is `m0 = 32` on level 0 and `m = 16` above
it, so on level 0 M = 32 for every limit below. Then the expansion of
one candidate:

```rust
// graph_layers.rs — search_on_level_acorn's expansion, 214-268.
   214              // Collect 1-hop neighbors (direct neighbors)
   215              let mut rank = 0;
   216              _ = self.try_for_each_link(candidate.idx, level, |hop1| {
   217                  let is_head = rank < hop1_limit;
   218                  rank += 1;
   219
   220                  if hop1_visited_list.check(hop1) {
   221                      return ControlFlow::Continue(());
   222                  }
   223
   224                  if points_scorer.filters().check_vector(hop1) {
   225                      hop1_visited_list.check_and_update_visited(hop1);
   226                      to_score.push(hop1);
   227                      if to_score.len() >= hop1_limit {
   228                          return ControlFlow::Break(());
   229                      }
   230                  } else if is_head {
   231                      hop1_visited_list.check_and_update_visited(hop1);
   232                      to_explore.push(hop1);
   233                  } else {
   234                      tail_bridges.push(hop1);
   235                  }
   236                  ControlFlow::Continue(())
   237              });
   238              for hop1 in evenly_spaced(&tail_bridges, hop1_tail_limit) {
   239                  hop1_visited_list.check_and_update_visited(hop1);
   240                  to_explore.push(hop1);
   241              }
// ... 242-246: the 2-hop loop header, a stop check ...
   247                  let total_limit = to_score.len().saturating_add(hop2_limit);
   248                  _ = self.try_for_each_link(hop1, level, |hop2| {
// ... 249-254: skip anything either visited list has already seen ...
   255                      if points_scorer.filters().check_vector(hop2) {
   256                          hop1_visited_list.check_and_update_visited(hop2);
   257                          to_score.push(hop2);
   258                          if to_score.len() >= total_limit {
   259                              return ControlFlow::Break(());
   260                          }
   261                      }
   262                      ControlFlow::Continue(())
   263                  });
   264              }
   265
   266              points_scorer
   267                  .score_points_unfiltered(&to_score)?
   268                  .for_each(|score_point| search_context.process_candidate(score_point));
```

The `else if` at :230-232 is the whole design: **only neighbours that
FAIL the filter become bridges in `to_explore`**. A neighbourhood
where everything passes produces an empty `to_explore`, the 2-hop
loop at :244 never runs, and ACORN costs exactly what plain HNSW
costs. The expansion is bought only where the filter actually removed
something.

Which failing neighbours become bridges depends on where they sit in
the link list. Step 3 appended the payload blocks' links after each
point's main links, so a level-0 list is up to `m0` main links (the
*head*, `rank < hop1_limit`, :217) followed by a payload-block
*tail*. Every failing head neighbour is a bridge (:230-232). Failing
tail neighbours are only collected (:233-234), and then at most
`hop1_tail_limit = M` of them, picked by `evenly_spaced`
(`graph_layers.rs:470-474`: element `j × len / picks` for
`j < picks`), become bridges (:238-241). A neighbour is marked visited
only when it is actually queued (:225, :231, :239), so a tail link the
cap skips stays reachable from another candidate.

The limits are **per neighbour, not per expansion** — the comment at
:179-181 says so in as many words. The 1-hop pass stops once
`hop1_limit` matches are queued (:227-228). The 2-hop pass then
recomputes `total_limit = to_score.len() + hop2_limit` (:247)
*inside* the `for &hop1 in &to_explore` loop (:244), so each bridge
may add up to `hop2_limit` more points. With M = `get_m(level)`, h
matches and f bridges, one expansion queues at most h + f·M points
for scoring. Work it on level 0, M = 32:

```
  no payload links: list ≤ m0 = 32, all head
    worst case: all 32 fail → h = 0, f = 32
    queued ≤ 0 + 32 × 32 = 1 024                 = M², not 2M

  a point in three payload blocks (payload_m0 = 32 each):
    list ≤ 32 + 3 × 32 = 128 links
    every non-match a bridge would be  f = 128 → 128 × 32 = 4 096
    with the tail cap:                 f ≤ 32 + 32 = 64
                                       h ≤ 32 (1-hop stops at M matches)
    queued ≤ 32 + 64 × 32              = 2 080
```

What the per-bridge cap prevents is one bridge with a long adjacency
list flooding the batch, and the tail cap stops a long payload tail
from multiplying the bridges; neither bounds the batch by a constant
multiple of M. The two pooled visited lists (:169, :174) can only
lower the count — a point already queued from either frontier is
skipped (:249-254) — but they do not change the bound. A graph built
with `m = 0` has only payload links; :188-193 lift every limit, so
ACORN takes every link there, as `search_on_level` does.

Why it works: if a fraction p of nodes pass, 1-hop expansion sees
~K·p useful edges but 2-hop reaches ~K²·p candidates, pushing the
percolation threshold from ~1/K down toward ~1/K². Put in the
defaults:

```
  K = m0 = 32
  1-hop cliff   1/K   = 1/32    = 3.1%   selectivity
  2-hop cliff   1/K²  = 1/1024  = 0.098% selectivity
```

Roughly a 32× extension of the usable band — which is why the
threshold at `dispatch.rs:244` can be as generous as 0.4 without the
walk falling apart. The price is more distance computations per
expansion, paid only on queries in the awkward band. Compare
`payload_m`'s extra links from Step 3: RAM paid at build time for
*known* payload fields versus CPU paid at query time for *arbitrary*
filters — question 2.

### Step 6 — the scar tissue worth grepping

> **In:** the working filtered index of Steps 1-5. **Out:** the
> remainders that production forces on you, each a small chapter of
> its own.

**`full_scan_threshold`** decides when a filtered query skips the
graph (Step 4, `dispatch.rs:132`) and when an unfiltered one does
(:66-69). The user-facing knob is in KiB — `types.rs:852` says
*"Minimal size threshold (in KiloBytes)"*, with the aside at :857
*"Note: 1Kb = 1 vector of size 256"* — while the graph config's
field is a point count, documented at `config.rs:21-23` as *"Note:
this is number of vectors, not KiloBytes"*. `derive_config` converts
one into the other when the index is opened or built:

```rust
// hnsw.rs — derive_config, KiB of vectors to a point count, 194-203.
   194      let full_scan_threshold = vector_storage
   195          .size_of_available_vectors_in_bytes()
   196          .checked_div(vector_count)
   197          .and_then(|avg_vector_size| {
   198              hnsw_config
   199                  .full_scan_threshold
   200                  .saturating_mul(BYTES_IN_KB)
   201                  .checked_div(avg_vector_size)
   202          })
   203          .unwrap_or(1);
```

:196 computes the average vector size in bytes, and the line to focus
on, :201, divides the configured KiB budget by it. With
`DEFAULT_FULL_SCAN_THRESHOLD = 10_000` (`types.rs:2407`) and
`BYTES_IN_KB = 1024` (`lib/segment/src/common/mod.rs:243`):

```
  budget = 10 000 KiB × 1024 = 10 240 000 bytes

  d = 128  f32:  avg_vector_size =  512 B → 10 240 000 /  512 = 20 000 points
  d = 1536 f32:  avg_vector_size = 6144 B → 10 240 000 / 6144 =  1 666 points
```

That is why the knob is in bytes rather than points: the thing a
brute-force scan is actually limited by is bytes streamed (topic
12), and a 1536-dimensional collection hits the same memory-bandwidth
cost at one twelfth the point count. Note `BYTES_IN_KB` is 1024, so
the doc comment's "KiloBytes" means KiB.

The others:

- **`graph_links/`** — the serialised link format, a module whose
  map is the doc comment at `graph_links/mod.rs:1-43`. Three formats
  (`graph_links/format.rs`): `Plain`, `Compressed` — each list
  bit-packed by `pack_links`, which may reorder the first `m` (or
  `m0`) links for a better packing
  (`lib/common/common/src/bitpacking_links.rs:12-23`) — and
  `CompressedWithVectors`, which stores quantized vectors inline
  beside the links so a hop reads one region instead of two (the
  `GraphLayersWithVectors` search at `graph_layers.rs:354`). Topic
  12's encodings applied to graph edges.
- **`graph_layers_batched.rs`** — the same search issuing link and
  vector reads in batches, used when the links live behind io_uring,
  mmap or a network store; `HnswGraph::search` picks batch sizes of
  4 and 16 per backend (`graph.rs:189-201`).
- **`gpu/`** — GPU-built HNSW (topic 18 preview).
- **`graph_layers_healer.rs`** — repairing the graph around deleted
  points instead of rebuilding: the paper's deletes wart, patched.

## Where each step lives in the code

Paths are relative to `lib/segment/src/index/hnsw_index/` **except**
where marked. All verified at qdrant v1.20.0,
`qdrant/qdrant@222b2e44a`.

| step | anchors |
|---|---|
| 1 build side | `graph_layers_builder.rs:30-31` the lock aliases, `:35` builder, `:38` ef_construct, `:42` use_heuristic, `:43` the RwLock'd link layers, `:322` level_factor, `:424-431` get_random_layer, `:433-447` level_from_sample (`.floor()` at :446), `:468` link_new_point, `:592-596` the heuristic switch, `:601`/`:627` the two link paths |
| 1 heuristic | `links_container.rs:9-31` is_redundant and its tie rule, `:71-102` fill_from_sorted_with_heuristic — Algorithm 4; `:87-93` is the test |
| 1 serve side | `graph_layers.rs:69` GraphLayers, `:114` search_on_level, `:275-276` search_entry, `:533` search |
| 2 machinery | `search_context.rs:10` SearchContext, `:12`/`:14` the two heaps, `:26-38` with_entries, `:49-57` process_candidate; **`../visited_pool.rs`** (i.e. `lib/segment/src/index/visited_pool.rs`) `:9` handle, `:19-22` the u8 counters, `:86-92` next_iteration, `:105-135` the pool |
| 3 filtering | **`point_scorer.rs:331-346`** — partition, truncate, then score; this is why the graph disconnects; `graph_layers.rs:136-147` the call |
| 3 percolation | `hnsw/build/additional_links.rs:114` measured average degree, `:123-131` the 2/K sampling point, `:133-145` three samples and the max, `:187-192` max_block_size, `:222-235` the per-block skip; `graph_layers_builder.rs:110-211` subgraph_connectivity (coin flip at :177-181), `:370` merge appends; `hnsw/old_index.rs:256-261` payload_hnsw_m; `../../types.rs:875-879` payload_m |
| 4 the planner | `hnsw/read_view/dispatch.rs:99-110` the estimate, `:132-156` plain vs graph, `:161-170` sampling, `:218-249` filtered_graph_algorithm (`:238` selectivity, `:244` the test); `../../types.rs:649` ACORN_MAX_SELECTIVITY_DEFAULT = 0.4; `graph.rs:219-273` entry points and seeds; **`../vector_index_search_common.rs:26-44`** get_oversampled_top (the calls at `hnsw/read_view/search.rs:56,249` are not the definition) |
| 5 ACORN-1 | `graph_layers.rs:153-157` search_on_level_acorn, `:169`/`:174` two pooled visited lists, `:182-197` the hop limits, `:214-241` 1-hop and the tail cap, `:244-264` 2-hop, `:266-268` scoring, `:470-474` evenly_spaced |
| 6 scar tissue | `hnsw.rs:189-213` derive_config; `config.rs:21-25`; `graph_links/`; `graph_layers_batched.rs`; `gpu/`; `graph_layers_healer.rs` |

Read order: Step 4's `dispatch.rs:94-197` first (it is the thesis),
then chase each branch to its implementation.

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

## Done when

Answer each before unfolding it.

- [ ] You can explain why the build structure and the serve structure differ, and what freezing buys.
  <details><summary>Answer</summary>

  `GraphLayersBuilder` (`graph_layers_builder.rs:35`) stores links as
  `Vec<Vec<RwLock<LinksContainer>>>` (:43, through the aliases at
  :30-31) — a lock and a growable container per node per level, so N
  threads can insert concurrently. That layout is terrible to read: a
  pointer chase and an atomic per neighbour list. `GraphLayers`
  (`graph_layers.rs:69`) is the frozen form, with links serialised
  into the compact, bit-packed `graph_links/` representation (:71),
  no locks, and no growth. Freezing buys sequential access on the
  hottest inner loop and removes per-read synchronisation, at the
  cost of a one-time conversion and an inability to mutate — which is
  exactly why deletes need `graph_layers_healer.rs`. Same trade as
  topic 13's CSR.

  </details>

- [ ] You can explain percolation: why a selective filter shatters a proximity graph rather than just shrinking it.
  <details><summary>Answer</summary>

  Because filtering removes *edges*, not just candidates.
  `FilteredScorer::score_points` (`point_scorer.rs:331-346`)
  partitions the neighbour list with `check_batched` (:338) and
  truncates the non-matches away (:343) before scoring, so a rejected
  neighbour never enters the candidate heap and every path through it
  is gone. A random graph with average degree K loses its giant
  connected component once the survival fraction drops below ~1/K.
  With qdrant's default `m0 = 2·m = 32` (`config.rs:47`,
  `types.rs:1683`) that cliff is at 1/32 = 3.1% selectivity. Below
  it, greedy search does not return *worse* results — it returns
  results from whichever island the walk started in, and increasing
  `ef` does not help, because the answer is not in the reachable
  component. qdrant measures rather than assumes:
  `hnsw/build/additional_links.rs:131` sets an edge-drop probability
  of `1 − 2/K = 0.9375` (the variable named `percolation` is the drop
  probability, so edges survive with 2/K), samples the main graph's
  connectivity three times with different RNG draws and keeps the max
  (:133-145), then uses that number to skip payload blocks that are
  already connected enough (:222-235).

  </details>

- [ ] You can describe the per-query decision qdrant makes and name the inputs it uses (cardinality estimate, thresholds).
  <details><summary>Answer</summary>

  `hnsw/read_view/dispatch.rs`. Inputs: (1) is there a filter at all
  (:60, :94); (2) `estimate_cardinality` from the payload index
  (:104-105) — topic 10's estimator, giving `min`, `exp` and `max`;
  (3) `available_vector_count` (:99), used to rescale the
  point-level estimate to vectors (:106-110) and as the denominator
  of selectivity (:238); (4) `full_scan_threshold`, a point count
  derived from a KiB budget (`hnsw.rs:194-203`): `max` below it goes
  to brute force over the matching ids (:132), `min` above it goes to
  the graph (:146), and a range straddling it is settled by sampling
  (:161-170); (5) inside the graph branch, whether the query enabled
  ACORN (:223-228) and `max_selectivity`, default
  `ACORN_MAX_SELECTIVITY_DEFAULT = 0.4` (`types.rs:649`), compared at
  :244. Outputs: exact scan for small survivor sets, ACORN-1 at or
  below 0.4 selectivity when enabled, plain HNSW otherwise. The
  asymmetry justifies the machinery: a wrong brute-force choice
  wastes CPU visibly; a wrong HNSW choice returns wrong answers
  silently.

  </details>

- [ ] You can say what ACORN's 2-hop expansion costs in scoring work and what it buys in connectivity.
  <details><summary>Answer</summary>

  Buys: the reachable neighbourhood grows from ~K·p to ~K²·p, moving
  the percolation cliff from 1/K = 3.1% to roughly 1/K² = 0.098% at
  m0 = 32 — about a 32× wider usable selectivity band. Costs: extra
  link traversals and extra scoring, capped per neighbour rather than
  per expansion. `graph_layers.rs` sets `hop1_limit = hop2_limit =
  get_m(level)` (:185-186) and recomputes `total_limit =
  to_score.len() + hop2_limit` (:247) for each bridge, so each
  bridge can add up to M scored points; with h matches and f bridges
  an expansion queues at most h + f·M. On a plain m0 = 32 list that
  is up to M² (all 32 neighbours failing: 32 × 32 = 1 024), not 2M.
  Payload-block links extend the list, but only the first M failing
  neighbours plus at most M evenly spaced failing tail neighbours
  become bridges (:230-241), so f ≤ 2M — for a 128-link list,
  32 + 64 × 32 = 2 080 instead of 128 × 32 = 4 096. The visited
  lists (:169, :174) can only lower that. Crucially :230-232 makes
  only *failing* neighbours into bridges, so a fully-passing
  neighbourhood triggers no 2-hop work at all — the cost is
  proportional to how much the filter actually removed. Versus
  `payload_m`: extra links are RAM spent at build time and only for
  payload fields you indexed; ACORN is CPU spent at query time and
  works for arbitrary filters.

  </details>

- [ ] You can compute where the percolation cliff sits for qdrant's default graph, and what the code samples instead.
  <details><summary>Answer</summary>

  Default `m = 16` (`types.rs:1683`), `m0 = m*2 = 32`
  (`config.rs:47`), so K ≈ 32 — though `additional_links.rs:114`
  measures the real average rather than assuming it. Cliff: 1/K =
  1/32 = 3.1% survival. qdrant samples at 2/K = 6.3% survival,
  because the comment at :123-129 says the graph is too
  noise-sensitive exactly at the bifurcation; at 2/K each point keeps
  on average 32 × 2/32 = 2 edges, against 1 at the cliff. The
  variable `percolation` at :131 is `1 − 2/K = 0.9375`, the
  probability that `subgraph_connectivity` *drops* an edge
  (`graph_layers_builder.rs:177-181`), which is easy to misread as
  the survival fraction.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including why `full_scan_threshold` is expressed in bytes.
  <details><summary>Answer</summary>

  For question 4 specifically: because the cost a brute-force scan is
  bounded by is bytes streamed, not points visited (topic 12).
  `derive_config` (`hnsw.rs:194-203`) converts a KiB budget into a
  point count by dividing by the measured average vector size, so the
  same 10 000 KiB default (`types.rs:2407`) means 20 000 points at
  d=128 f32 (512 B each) but only 1 666 points at d=1536 (6144 B
  each). A point-count knob would silently mean twelve times more
  work on the larger collection.

  </details>

## References

**Papers**
- Patel, Kraft, Guestrin, Zaharia — "ACORN: Performant and
  Predicate-Agnostic Search Over Vector Embeddings and Structured
  Data" (SIGMOD 2024,
  [arXiv:2403.04871](https://arxiv.org/abs/2403.04871)). Optional,
  and note qdrant implements the cheaper **ACORN-1** variant, as
  `graph_layers.rs:153-154` says in its own doc comment.
- The HNSW paper itself is
  [reading-hnsw-paper.md](reading-hnsw-paper.md).

**Code** — all qdrant v1.20.0, `qdrant/qdrant@222b2e44a`, pinned in
`resources/codebases.md`.

| file:line | what |
|---|---|
| `lib/segment/src/index/hnsw_index/graph_layers_builder.rs:35,43,322,433-447` | builder, RwLock'd links, mL, the floored level draw |
| `lib/segment/src/index/hnsw_index/graph_layers_builder.rs:110-211,370` | subgraph_connectivity (bond percolation); block links appended |
| `lib/segment/src/index/hnsw_index/links_container.rs:9-31,71-102` | the tie-aware redundancy test; Algorithm 4 |
| `lib/segment/src/index/hnsw_index/graph_layers.rs:69,114,157,276,470-474` | frozen graph, Alg 2, ACORN-1, the ef=1 descent, evenly_spaced |
| `lib/segment/src/index/hnsw_index/search_context.rs:10-57` | Algorithm 2's two heaps |
| `lib/segment/src/index/visited_pool.rs:9,19-22,86-92,105-135` | the u8-stamp visited list and its pool |
| `lib/segment/src/index/hnsw_index/point_scorer.rs:331-346` | partition-and-truncate before scoring — the reason filters disconnect |
| `lib/segment/src/index/hnsw_index/hnsw/build/additional_links.rs:114-145,187-192,222-235` | the percolation measurement and the per-block decisions |
| `lib/segment/src/index/hnsw_index/hnsw/read_view/dispatch.rs:94-197,218-249` | the planner: plain vs graph, then HNSW vs ACORN |
| `lib/segment/src/index/hnsw_index/hnsw/read_view/search.rs:56,200,285` | oversampling call site; the exact-scan fallback |
| `lib/segment/src/index/hnsw_index/graph.rs:67,219-273` | filtered entry points and seeds |
| `lib/segment/src/index/hnsw_index/hnsw.rs:189-213` | derive_config: KiB → points |
| `lib/segment/src/index/hnsw_index/hnsw/old_index.rs:256-261` | payload_m falls back to m |
| `lib/segment/src/index/hnsw_index/config.rs:21-25,47,49,53` | point-count threshold, m0 = 2m, ef defaults to ef_construct, payload_m0 |
| `lib/segment/src/index/vector_index_search_common.rs:26-44` | get_oversampled_top's definition |
| `lib/segment/src/types.rs:649,852-859,875-879,1678-1692,2407` | ACORN threshold, full_scan_threshold docs, payload_m, HNSW defaults |
| `lib/segment/src/common/mod.rs:243` | `BYTES_IN_KB = 1024` |
| `lib/common/common/src/defaults.rs:50` | `POOL_KEEP_LIMIT` |
