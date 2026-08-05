# Qdrant's HNSW: filtered search is a planner problem

Production HNSW: the paper plus five years of scar tissue — and
filtering, qdrant's actual specialty. The payoff of this chapter is
watching a query planner appear inside an index; before the code, it
builds the ideas in order — the build/serve split, the pooled search
machinery, why filters shatter graphs (percolation), and the
per-query decision that picks HNSW / brute force / ACORN from an
estimated cardinality. This chapter assumes
[reading-hnsw-paper.md](reading-hnsw-paper.md).

Every `file:line` below was read at **`qdrant/qdrant@44ad62f`**, the
revision pinned in `resources/codebases.md`; re-verify any of them
with `python3 tools/pinned-source.py show qdrant <path> -r A:B`. Most
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
indexed points that pass the filter (qdrant computes it as
`cardinality / available_vector_count`, `hnsw/search.rs:80`) — so
*low* selectivity means a *restrictive* filter. **Percolation
threshold** is the survival fraction below which a random graph stops
having one giant connected component.

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
  storage is `links_layers: Vec<Vec<RwLock<LinksContainer>>>` (:43),
  one lock per node per level, so threads insert in parallel. It
  holds the paper's build parameters: `ef_construct` (:38),
  `level_factor` (:39-40), `use_heuristic` (:41-42), and
  `link_new_point` (:414), which is Algorithm 1.
- **`GraphLayers`** (`graph_layers.rs:74`) — the frozen serve-side
  graph. `search_on_level` (:109) is Algorithm 2, `search_entry`
  (:248) is Algorithm 5's ef=1 descent — its doc comment says
  *"Beam size is 1"*.

The same builder/immutable split as CSR in topic 13: pay a
conversion step once, serve reads from the compact form forever.

Two details in the builder are worth checking against the paper
before you trust your mental model.

```rust
// graph_layers_builder.rs — the level constant and the level draw,
// 317 and 384-393, with the body of new_with_params elided.
   317              level_factor: 1.0 / (max(hnsw_m.m, 2) as f64).ln(),
// ... 318-383: the rest of the constructor, and link bookkeeping ...
   384      fn get_random_layer<R>(&self, rng: &mut R) -> usize
   385      where
   386          R: Rng + ?Sized,
   387      {
   388          let distribution = Uniform::new(0.0, 1.0).unwrap();
   389          let sample: f64 = rng.sample(distribution);
   390          let picked_level = -sample.ln() * self.level_factor;
   391          picked_level.round() as usize
   392      }
```

`level_factor` at :317 is the paper's `mL = 1/ln(M)` exactly (§4.1),
with a floor of `ln 2` so M=1 cannot divide by zero. But :391 calls
**`.round()`** where Algorithm 1 line 4 **floors**. That is not
cosmetic. Under flooring, `P(level ≥ 1) = M^(-1) = 1/16 = 6.3%`;
under rounding, a point is promoted whenever `-ln(U)·mL ≥ 0.5`, i.e.
`U ≤ M^(-1/2) = 1/4`, so **25%** of points get a level above 0 — four
times as many, and correspondingly more upper-layer link memory.

The Algorithm 4 heuristic is a *flag*, not a given: `use_heuristic`
(:41-42) selects between `link_with_heuristic` (:529) and
`link_without_heuristic` (:555). The heuristic itself lives one file
over, in `links_container.rs`, and its load-bearing line is a single
`continue`:

```rust
// links_container.rs — fill_from_sorted_with_heuristic, 47-71,
// with the setup lines elided. This is the paper's Algorithm 4.
    47      pub fn fill_from_sorted_with_heuristic(
// ... 48-58: signature, clearing self.links, the outer loop header ...
    59          'outer: for candidate in candidates {
    60              for &existing in self.links.iter() {
    61                  if score(candidate.idx, existing) > candidate.score {
    62                      continue 'outer;
    63                  }
    64              }
    65              self.links.push(candidate.idx);
    66              if self.links.len() >= level_m {
    67                  break;
    68              }
    69          }
```

Line 61 is Algorithm 4 line 11 (*"if e is closer to q compared to any
element from R"*) with the inequality flipped, because qdrant's
`score` is a **similarity** — higher means closer — so "closer to an
already-kept neighbour than to the query" reads as
`score(candidate, existing) > candidate.score`. Note also what is
*not* here: neither of the paper's `extendCandidates` nor
`keepPrunedConnections` flags exists in this implementation.

### Step 2 — the search machinery: two heaps and a pooled visited set

> **In:** a frozen `GraphLayers` and a query. **Out:** the two
> structures Algorithm 2 needs, and why one of them is pooled rather
> than allocated.

`SearchContext` (`search_context.rs:8`) is Algorithm 2's state in
one struct: `nearest`, a `FixedLengthPriorityQueue` of size ef (:10 —
the paper's W), and `candidates`, a `BinaryHeap` (:12 — the paper's
C). `new(ef)` is :16-21, `lower_bound` (:23-28) is the stop test's
input, and `process_candidate` (:32-40) is the line-13 admission
test.

The hot structure is the **visited set**: every scored node checks
and sets membership, so it is touched more often than anything else
in the query. Allocating and zeroing one per query would dominate
small searches, so qdrant pools them. `VisitedListHandle`
(`lib/segment/src/index/visited_pool.rs:9`) hands out reusable lists,
and the "clearing" is a generation stamp:

```rust
// visited_pool.rs — VisitedList and next_iteration, 19-22 and 78-84,
// with check/check_and_update_visited elided.
    19  pub struct VisitedList {
    20      current_iter: u8,
    21      visit_counters: Vec<u8>,
    22  }
// ... 23-77: new(), resize(), check(), check_and_update_visited() ...
    78      fn next_iteration(&mut self) {
    79          self.current_iter = self.current_iter.wrapping_add(1);
    80          if self.current_iter == 0 {
    81              self.current_iter = 1;
    82              self.visit_counters.fill(0);
    83          }
    84      }
```

The counters are `u8`, one byte per point, and a query is "cleared"
by incrementing `current_iter`. Work the cost:

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
(`visited_pool.rs:97-99`) wraps a `RwLock<Vec<VisitedList>>`, `get`
(:108-120) borrows one, and `return_back` (:122-127) puts it back
subject to `POOL_KEEP_LIMIT`. That last part is the difference from
topic 13's single-threaded stamp trick: queries are concurrent, so
each in-flight query needs its *own* list, and the pool bounds how
many the process keeps alive.

### Step 3 — percolation: why filters shatter graphs

> **In:** an HNSW graph with average level-0 degree K, and a filter
> passing a fraction p of points. **Out:** the critical p below which
> greedy search cannot work at all, and the measurement qdrant takes
> at build time instead of assuming it.

Percolation theory studies when a graph falls apart as you randomly
delete nodes. A random graph with average degree K stays connected
while more than ~1/K of nodes survive and disintegrates into islands
below that. A filter that rejects nodes during traversal *is* node
deletion from the walk's point of view — and in qdrant it literally
is: `point_scorer.rs:231` does
`point_ids.retain(|id| self.filters.check_vector(*id))` **before
scoring**, so a rejected neighbour never enters the candidate heap
and the edge through it does not exist.

```
 survival fraction p:      1.0 ────────── ~1/K ──────────── 0.0
 filtered graph:           connected      │    islands
 greedy search:            works          │    strands near start,
                                          │    recall cliff
```

qdrant does not assume the threshold — it computes one and then
measures around it:

```rust
// hnsw/build.rs — the percolation sampling point and the
// connectivity measurement, 378-397, with the debug! line elided.
   378              // According to percolation theory, random graph becomes disconnected
   379              // if 1/K points are left, where K is average number of links per point
   380              // So we need to sample connectivity relative to this bifurcation point, but
   381              // not exactly at 1/K, as at this point graph is very sensitive to noise.
   382              //
   383              // Instead, we choose sampling point at 2/K, which expects graph to still be
   384              // mostly connected, but still have some measurable disconnected components.
   385
   386              let percolation = 1. - 2. / (average_links_per_0_level_int as f32);
   387
   388              let required_connectivity = if average_links_per_0_level_int >= 4 {
   389                  let global_graph_connectivity = [
   390                      graph_layers_builder.subgraph_connectivity(rng, &all_points, percolation),
   391                      graph_layers_builder.subgraph_connectivity(rng, &all_points, percolation),
   392                      graph_layers_builder.subgraph_connectivity(rng, &all_points, percolation),
   393                  ];
// ... 395: debug!("graph connectivity: ...") ...
   397                  global_graph_connectivity
```

Read :386 carefully, because the variable name inverts the meaning:
`percolation` is the **drop** fraction, so the *survival* fraction it
samples at is `2/K`, exactly what the comment says. Put qdrant's own
default in:

```
  K = average links per point on level 0 ≈ m0 = 2·m = 32
      (config.rs:46 sets m0 = m*2; types.rs:1412 sets m = 16;
       :369 measures the real average rather than assuming it)

  percolation threshold  1/K = 1/32 = 3.1%   ← the cliff
  sampling point         2/K = 2/32 = 6.3%   ← where qdrant looks
  `percolation` variable 1 − 2/32 = 0.9375   ← the DROP fraction
```

So the code deletes 93.75% of points three times (:389-393,
different RNG draws), measures the largest surviving component each
time, and takes the max (:397-400). The guard at :388 skips this
entirely for graphs with average degree below 4, where the
arithmetic is meaningless.

If the main graph would shatter for an indexed payload category, the
build adds extra category-aware links — `payload_m` (`hnsw.rs:93`,
declared at `types.rs:684-686`, with `payload_m0 = payload_m * 2` at
`config.rs:52`) — so each category's subgraph is navigable on its
own. The failure mode is *measured during build*: topic 0's
discipline, inside an index builder.

### Step 4 — the per-query decision: a planner inside the index

> **In:** a query with an optional filter. **Out:** one of three
> algorithms, chosen from an estimated cardinality — topic 10's
> optimiser, living inside a vector index.

With the cliff located, each query picks its algorithm. The whole
decision is 27 lines:

```rust
// hnsw/search.rs — the algorithm choice, 59-85, with the NOTE
// comment at 64-67 elided.
    59          let mut algorithm = SearchAlgorithm::Hnsw;
    60          if acorn_enabled
    61              && self.config.m0 != 0
    62              && let Some(filter) = filter
    63          {
// ... 64-67: a NOTE about unfiltered searches on heavily-deleted segments ...
    69              let available_vector_count = vector_storage.available_vector_count();
    70              let selectivity = if available_vector_count == 0 {
    71                  1.0
    72              } else {
    73                  let query_point_cardinality =
    74                      payload_index.with_view(|v| v.estimate_cardinality(filter, &hw_counter))?;
    75                  let query_cardinality = adjust_to_available_vectors(
    76                      query_point_cardinality,
    77                      available_vector_count,
    78                      id_tracker.available_point_count(),
    79                  );
    80                  query_cardinality.exp as f64 / available_vector_count as f64
    81              };
    82              if selectivity <= acorn_max_selectivity {
    83                  algorithm = SearchAlgorithm::Acorn;
    84              }
    85          }
```

Line 74 is topic 10's cardinality estimator, called from inside a
vector index. Line 80 divides its *expected* value (`.exp`, the
point estimate of a `CardinalityEstimation`) by the live vector count
to get selectivity, after :75-79 rescales the payload index's
estimate — which counts *points* — to the number of *vectors*
actually present in this segment. Line 82's threshold defaults to
`ACORN_MAX_SELECTIVITY_DEFAULT = 0.4` (`lib/segment/src/types.rs:556`).

The full menu:

| selectivity | plan | why |
|---|---|---|
| > 0.4 | plain HNSW, `FilteredScorer` rejecting during traversal | the graph stays connected well above 1/K, so deleting nodes from the walk is safe |
| ≤ 0.4, above the size floor | ACORN (Step 5) | connected enough to walk, too sparse to walk naively |
| below `full_scan_threshold` | `search_plain_batched` (`hnsw/search.rs:264`) | the surviving id list is small enough to score exactly, and exact beats an approximate walk over a shattered graph |

The cost of getting this wrong is asymmetric, which is the argument
for a planner rather than a constant: brute-forcing a 90% filter
scans ~900k points for nothing, while HNSW-ing a 1% filter returns
garbage — and unlike the first mistake, the second one is silent.

One anchor to keep straight: `get_oversampled_top` appears at
`hnsw/search.rs:57`, but that is only the call site. Its definition
is in a different module,
`lib/segment/src/index/vector_index_search_common.rs:27-45`, and it
belongs to quantization rather than to the planner — see
[reading-qdrant-quantization.md](reading-qdrant-quantization.md).

### Step 5 — ACORN: traverse through the blocked nodes

> **In:** a query in the awkward band — restrictive enough to shatter
> the graph, broad enough that scanning is wasteful. **Out:** a walk
> that stays connected without any extra links, and the scoring bill
> it runs up.

For the middle band, qdrant implements **ACORN-1** — the paper's
cheap variant, and `graph_layers.rs:155`'s doc comment names it
that specifically, so do not read the generic ACORN paper and expect
a match. The idea: when expanding a node, treat filtered-out
neighbours as passable wires rather than walls, and reach their
neighbours instead.

```
 1-hop, filtered:   ● ──✗── ✗ ──✗── ●     walk stops at the wall
 ACORN 2-hop:       ● ──(✗)──(✗)── ●      blocked nodes relay,
                        pass-through        only ● gets scored
```

The implementation is precise about who pays:

```rust
// graph_layers.rs — search_on_level_acorn's expansion, 199-240,
// with the closure boilerplate kept because it is the point.
   199              _ = self.try_for_each_link(candidate.idx, level, |hop1| {
   200                  if hop1_visited_list.check_and_update_visited(hop1) {
   201                      return ControlFlow::Continue(());
   202                  }
   203
   204                  if points_scorer.filters().check_vector(hop1) {
   205                      to_score.push(hop1);
   206                      if to_score.len() >= hop1_limit {
   207                          return ControlFlow::Break(());
   208                      }
   209                  } else {
   210                      to_explore.push(hop1);
   211                  }
   212                  ControlFlow::Continue(())
   213              });
// ... 215-218: the 2-hop loop header and the stop check ...
   219                  let total_limit = to_score.len() + hop2_limit;
   220                  _ = self.try_for_each_link(hop1, level, |hop2| {
// ... 221-226: skip anything either visited list has already seen ...
   227                      if points_scorer.filters().check_vector(hop2) {
   228                          hop1_visited_list.check_and_update_visited(hop2);
   229                          to_score.push(hop2);
   230                          if to_score.len() >= total_limit {
   231                              return ControlFlow::Break(());
   232                          }
   233                      }
   234                      ControlFlow::Continue(())
   235                  });
   236              }
   237
   238              points_scorer
   239                  .score_points_unfiltered(&to_score)
   240                  .for_each(|score_point| search_context.process_candidate(score_point));
```

The `else` at :209-211 is the whole design: **only neighbours that
FAIL the filter go into `to_explore`**. A neighbourhood where
everything passes produces an empty `to_explore`, the 2-hop loop
never runs, and ACORN costs exactly what plain HNSW costs. The
expansion is bought only where the filter actually removed something.

Two limits keep the bill bounded: `hop1_limit` and `hop2_limit` are
both set to `get_m(level)` (:181-182), and the 2-hop pass stops at
`total_limit = to_score.len() + hop2_limit` (:219), so at most about
`2M` points are scored per expansion rather than the `M²` the naive
formulation suggests. Two pooled visited lists (:167, :173) keep the
hop-1 and hop-2 frontiers from re-scoring each other.

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
threshold at `search.rs:82` can be as generous as 0.4 without the
walk falling apart. The price is more distance computations per
expansion, paid only on queries in the awkward band. Compare
`payload_m`'s extra links from Step 3: RAM paid at build time for
*known* categories versus CPU paid at query time for *arbitrary*
filters — question 2.

### Step 6 — the scar tissue worth grepping

> **In:** a working filtered index. **Out:** the four remainders that
> production forces on you, each a small chapter of its own.

**`full_scan_threshold`** (`hnsw/build.rs:95-104`) decides when not
to build a graph at all. The config knob is in KiB — `types.rs:667`
says *"Minimal size threshold (in KiloBytes)"* with the aside *"Note:
1Kb = 1 vector of size 256"* — and the code turns it into a point
count at build time:

```rust
// hnsw/build.rs — full_scan_threshold, KiB of vectors to a point
// count, 95-104.
    95          let full_scan_threshold = vector_storage_ref
    96              .size_of_available_vectors_in_bytes()
    97              .checked_div(total_vector_count)
    98              .and_then(|avg_vector_size| {
    99                  hnsw_config
   100                      .full_scan_threshold
   101                      .saturating_mul(BYTES_IN_KB)
   102                      .checked_div(avg_vector_size)
   103              })
   104              .unwrap_or(1);
```

:97 computes the average vector size in bytes, and :99-102 divides
the configured KiB budget by it. With
`DEFAULT_FULL_SCAN_THRESHOLD = 10_000` (`types.rs:1872`) and
`BYTES_IN_KB = 1024` (`lib/segment/src/common/mod.rs:239`):

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

The other three:

- **`graph_links.rs`** — the serialised link format, delta-compressed;
  topic 12's encodings applied to graph edges.
- **`gpu/`** — GPU-built HNSW (topic 18 preview).
- **`graph_layers_healer.rs`** — repairing the graph around deleted
  points instead of rebuilding: the paper's deletes wart, patched.

## Where each step lives in the code

Paths are relative to `lib/segment/src/index/hnsw_index/` **except**
where marked. All verified at `qdrant/qdrant@44ad62f`.

| step | anchors |
|---|---|
| 1 build side | `graph_layers_builder.rs:35` builder, `:38` ef_construct, `:41-42` use_heuristic, `:43` the RwLock'd link layers, `:317` level_factor, `:384-393` get_random_layer (`.round()` at :391), `:414` link_new_point, `:529`/`:555` the two link paths |
| 1 heuristic | `links_container.rs:47-71` — Algorithm 4; `:61` is the test |
| 1 serve side | `graph_layers.rs:74` GraphLayers, `:109` search_on_level, `:248` search_entry, `:531` search |
| 2 machinery | `search_context.rs:8` SearchContext, `:10`/`:12` the two heaps, `:32-40` process_candidate; **`../visited_pool.rs`** (i.e. `lib/segment/src/index/visited_pool.rs`) `:9` handle, `:19-22` the u8 counters, `:78-84` next_iteration, `:97-127` the pool |
| 3 percolation | `hnsw/build.rs:366-370` measured average degree, `:378-386` the 2/K sampling point, `:388-400` three samples and the max; `hnsw.rs:93` payload_m; `../../types.rs:684-686` its config |
| 4 the planner | `hnsw/search.rs:59-85` the algorithm choice, `:74` estimate_cardinality, `:80` selectivity, `:264` search_plain_batched; `../../types.rs:556` ACORN_MAX_SELECTIVITY_DEFAULT = 0.4; **`../vector_index_search_common.rs:27-45`** get_oversampled_top (the call at `hnsw/search.rs:57` is not the definition) |
| 4 filtering | **`point_scorer.rs:231`** — `retain` before scoring; this is why the graph disconnects |
| 5 ACORN-1 | `graph_layers.rs:155` search_on_level_acorn, `:167`/`:173` two pooled visited lists, `:181-182` the hop limits, `:199-213` 1-hop, `:216-236` 2-hop, `:238-240` scoring |
| 6 scar tissue | `hnsw/build.rs:95-104`; `graph_links.rs`; `gpu/`; `graph_layers_healer.rs` |

Read order: Step 4's `search.rs:59-85` first (it is 27 lines and the
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

## Done when

Answer each before unfolding it.

- [ ] You can explain why the build structure and the serve structure differ, and what freezing buys.
  <details><summary>Answer</summary>

  `GraphLayersBuilder` (`graph_layers_builder.rs:35`) stores links as
  `Vec<Vec<RwLock<LinksContainer>>>` (:43) — a lock and a growable
  container per node per level, so N threads can insert concurrently.
  That layout is terrible to read: a pointer chase and an atomic per
  neighbour list. `GraphLayers` (`graph_layers.rs:74`) is the frozen
  form, with links serialised into the compact `graph_links.rs`
  representation, no locks, and no growth. Freezing buys sequential
  access on the hottest inner loop and removes per-read
  synchronisation, at the cost of a one-time conversion and an
  inability to mutate — which is exactly why deletes need
  `graph_layers_healer.rs`. Same trade as topic 13's CSR.
  </details>

- [ ] You can explain percolation: why a selective filter shatters a proximity graph rather than just shrinking it.
  <details><summary>Answer</summary>

  Because filtering removes *edges*, not just candidates.
  `point_scorer.rs:231` retains only passing ids before scoring, so a
  rejected neighbour never enters the candidate heap and every path
  through it is gone. A random graph with average degree K loses its
  giant connected component once the survival fraction drops below
  ~1/K. With qdrant's default `m0 = 2·m = 32` (`config.rs:46`,
  `types.rs:1412`) that cliff is at 1/32 = 3.1% selectivity. Below
  it, greedy search does not return *worse* results — it returns
  results from whichever island the entry point happened to land in,
  and increasing `ef` does not help, because the answer is not in the
  reachable component. qdrant measures rather than assumes:
  `hnsw/build.rs:386` samples at survival 2/K (the variable named
  `percolation` is the drop fraction, `1 − 2/K = 0.9375`), three
  times with different RNG draws, taking the max (:389-400).
  </details>

- [ ] You can describe the per-query decision qdrant makes and name the inputs it uses (cardinality estimate, thresholds).
  <details><summary>Answer</summary>

  `hnsw/search.rs:59-85`. Inputs: (1) is there a filter at all (:62);
  (2) `estimate_cardinality` from the payload index (:74) — topic
  10's estimator; (3) `available_vector_count` (:69), used both to
  rescale the point-level estimate to vectors (:75-79) and as the
  denominator of selectivity (:80); (4)
  `ACORN_MAX_SELECTIVITY_DEFAULT = 0.4` (`types.rs:556`) at :82; (5)
  separately, `full_scan_threshold` (`hnsw/build.rs:95-104`), which
  routes tiny survivor sets to `search_plain_batched` (:264).
  Outputs: plain HNSW above 0.4 selectivity, ACORN-1 at or below it,
  exact scan when the survivor list is small enough. The asymmetry
  justifies the machinery: a wrong brute-force choice wastes CPU
  visibly; a wrong HNSW choice returns wrong answers silently.
  </details>

- [ ] You can say what ACORN's 2-hop expansion costs in scoring work and what it buys in connectivity.
  <details><summary>Answer</summary>

  Buys: the reachable neighbourhood grows from ~K·p to ~K²·p, moving
  the percolation cliff from 1/K = 3.1% to roughly 1/K² = 0.098% at
  m0 = 32 — about a 32× wider usable selectivity band. Costs: extra
  link traversals and extra scoring, but bounded. `graph_layers.rs`
  sets `hop1_limit = hop2_limit = get_m(level)` (:181-182) and caps
  the 2-hop pass at `total_limit = to_score.len() + hop2_limit`
  (:219), so an expansion scores at most about 2M points, not M².
  Crucially :209-211 puts only *failing* neighbours into
  `to_explore`, so a fully-passing neighbourhood triggers no 2-hop
  work at all — the cost is proportional to how much the filter
  actually removed. Versus `payload_m`: extra links are RAM spent at
  build time and only for payload keys you declared; ACORN is CPU
  spent at query time and works for arbitrary filters.
  </details>

- [ ] You can compute where the percolation cliff sits for qdrant's default graph, and what the code samples instead.
  <details><summary>Answer</summary>

  Default `m = 16` (`types.rs:1412`), `m0 = m*2 = 32`
  (`config.rs:46`), so K ≈ 32 — though `build.rs:368-370` measures
  the real average rather than assuming it. Cliff: 1/K = 1/32 =
  3.1% survival. qdrant samples at 2/K = 6.3% survival, because the
  comment at :380-384 says the graph is too noise-sensitive exactly
  at the bifurcation. The variable `percolation` at :386 is
  `1 − 2/K = 0.9375`, the fraction *dropped*, which is easy to
  misread as the survival fraction.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including why `full_scan_threshold` is expressed in bytes.
  <details><summary>Answer</summary>

  For question 4 specifically: because the cost a brute-force scan is
  bounded by is bytes streamed, not points visited (topic 12).
  `hnsw/build.rs:95-104` converts a KiB budget into a point count at
  build time by dividing by the measured average vector size, so the
  same 10 000 KiB default (`types.rs:1872`) means 20 000 points at
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
  `graph_layers.rs:155` says in its own doc comment.
- The HNSW paper itself is
  [reading-hnsw-paper.md](reading-hnsw-paper.md).

**Code** — all `qdrant/qdrant@44ad62f`, pinned in
`resources/codebases.md`.

| file:line | what |
|---|---|
| `lib/segment/src/index/hnsw_index/graph_layers_builder.rs:35,43,317,384-393` | builder, RwLock'd links, mL, the rounded level draw |
| `lib/segment/src/index/hnsw_index/links_container.rs:47-71` | Algorithm 4; `:61` is the heuristic test |
| `lib/segment/src/index/hnsw_index/graph_layers.rs:74,109,155,248` | frozen graph, Alg 2, ACORN-1, the ef=1 descent |
| `lib/segment/src/index/hnsw_index/search_context.rs:8-40` | Algorithm 2's two heaps |
| `lib/segment/src/index/visited_pool.rs:9,19-22,78-84,97-127` | the u8-stamp visited list and its pool |
| `lib/segment/src/index/hnsw_index/point_scorer.rs:231` | `retain` before scoring — the reason filters disconnect |
| `lib/segment/src/index/hnsw_index/hnsw/build.rs:95-104,366-400` | full_scan_threshold; the percolation measurement |
| `lib/segment/src/index/hnsw_index/hnsw/search.rs:59-85,264` | the planner; the exact-scan fallback |
| `lib/segment/src/index/hnsw_index/hnsw.rs:93` | payload_m |
| `lib/segment/src/index/hnsw_index/config.rs:46,48,52` | m0 = 2m, ef defaults to ef_construct, payload_m0 |
| `lib/segment/src/index/vector_index_search_common.rs:27-45` | get_oversampled_top's definition |
| `lib/segment/src/types.rs:556,667-673,684-686,1409-1422,1872` | ACORN threshold, full_scan_threshold docs, payload_m, HNSW defaults |
| `lib/segment/src/common/mod.rs:239` | `BYTES_IN_KB = 1024` |
