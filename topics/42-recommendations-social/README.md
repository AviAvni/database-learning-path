# Topic 42 — Recommendations & Social Graphs

Fifth of six graph use-case deep dives: the workload that pays for graph
infrastructure. Three production papers, all reaching the same
unfashionable conclusion — **put the graph in RAM on one machine and
walk it**. Pinterest's **Pixie** (WWW'18) answers from 17 billion edges
at p99 under 60 ms with a biased random walk. Twitter's **GraphJet**
(VLDB'16) ingests a million edges a second into a temporally-bounded
bipartite graph and serves recommendations at p50 19 ms. Facebook's
**TAO** (ATC'13) is the other half of the problem — not the recommender
but the *store*, two data shapes and five queries at a 96.4% cache hit
rate. Against all three sits **Liben-Nowell & Kleinberg** (2003), who
showed that plain topology predicts future links 20–55× better than
chance, and that the degree-only measure everyone reaches for first is
the worst of the family.

## The problem, measured (bench lane 1, provided — runs today)

```
   3000 users x 6000 items, 30 communities, 60000 training edges, 6000 held out

   recommender          hit-rate@50   personalization   overlap w/ bestsellers
   popularity               0.340             0.155                   0.923
   basic walk               0.403             0.820                   0.451
```

Popularity is not a weak baseline. On a power-law graph the bestseller
list gets a third of users right, and its personalization score is only
nonzero at all because we filter out items each user already has —
everybody is being handed the same list. Pixie's unmodified Algorithm 1
does personalize, but **45% of what it returns is the bestseller list
again**, because an unbiased walk's visit distribution drifts toward
degree. That is Pixie's own complaint, stated in §3.1: "In classical
random walk low degree nodes with fewer edges contribute less signal.
This is undesirable because smaller boards ... are more likely to
produce highly relevant recommendations."

## Pixie: four ideas on top of a random walk

```
   Algorithm 1              Algorithm 2 + 3
   ───────────              ───────────────
   one query pin       ->   weighted query SET, steps allocated
   uniform edge pick   ->   biased by user features (language, topic)
   sum visit counts    ->   multi-hit boost  V[p] = (Σ_q √V_q[p])²
   fixed N steps       ->   stop when n_p pins have n_v visits
```

The step allocation is the subtle one. A high-degree query pin needs
more steps (its walk diffuses); allocating *linearly* in degree starves
a low-degree pin of even one step, and silently drops a whole interest
of the user. Pixie's Equation 1 grows sub-linearly:

```
   s_q = |E(q)| · (C − log|E(q)|)        C = max over ALL pins of log|E(p)|
   N_q = w_q · s_q / Σ_r s_r
```

Measured lane 2, 300 users, 8 query pins each, 30,000 steps:

| recommender | hit-rate@50 | personalization | overlap w/ bestsellers |
|---|---|---|---|
| 8 pins, no boost | 0.823 | 0.815 | 0.435 |
| pixie (full) | 0.803 | 0.788 | 0.445 |
| pixie (early stop) | 0.807 | 0.809 | 0.416 |

```
   full walk:   9000004 steps, 2.12 ms/query
   early stop:  3170675 steps (35% of full), 0.97 ms/query (2.2x faster)
   early-stopped top-50 overlaps the full walk by 0.793
```

Two results, and one of them is negative.

**Early stopping works**, at almost exactly the margin the paper claims:
~35% of the steps, 2.2× faster, top-50 overlap 0.79, hit rate unchanged.
Pixie reports 84% overlap at a third of the runtime; this is the same
trade.

**The multi-hit booster does not help here** — not at one interest per
user and not at three (`interests_per_user: 3` gives 0.563 unboosted
against 0.547 boosted). Summing raw visit counts scores *slightly
better* than Equation 3 in both regimes. That is not an implementation
bug; the unit test pins the arithmetic exactly ((√2+√2)² = 8 against a
single-source 4). It is the generator failing to contain the booster's
premise. Equation 3 is a bet that a pin at the intersection of several
of your interests is more engaging than one deep inside a single
interest — a claim about *people*, not about graphs — and this generator
draws its held-out item from the same distribution as the training
items, so reachability from several query pins carries no extra
information about the answer. Exercise 4 asks you to build a graph where
the premise does hold. The lesson is the transferable one: **a published
trick encodes a domain assumption, and you owe it a measurement on your
own data before you ship it.**

Pixie's numbers for scale: a pruned graph of 1 billion boards, 2 billion
pins and 17 billion edges in **~120 GB** on one AWS r3.8xlarge, **p99
under 60 ms**, ~1,200 requests/s per server, ~100,000 across the
cluster. Hit rate on "which pin will this user save next", against
content-based baselines:

| method | top 10 | top 100 | top 1000 |
|---|---|---|---|
| content-based (textual) | 1.0% | 2.2% | 4.8% |
| content-based (visual) | 1.1% | 2.4% | 4.5% |
| content-based (combined) | 2.1% | 4.6% | 10.5% |
| **Pixie (graph-based)** | **6.3%** | **23.1%** | **52.2%** |

And the result that should change how you think about data cleaning:
Pinterest prunes boards by topic entropy and edges by LDA cosine
similarity, and at pruning factor δ = 0.91 the F1 **peaks 58% above the
unpruned graph while keeping only 20% of the edges**. A smaller graph
that recommends better, and fits on a cheaper machine.

## GraphJet: a storage engine shaped like a power law

```
   edge pools — slice sizes double, because degree begets degree

   P1 |..|..|..|..|      slices of 2^1 edges,  2^1 * n capacity
   P2 |....|....|        slices of 2^2 edges,  n/2 slices
   P3 |........|         slices of 2^3 edges,  n/4 slices
   P4 |................| slices of 2^4 edges,  n/8 slices

   a vertex of degree 25:   v -> 25 : P1(1), P2(2), P3(0), P4(0)
                            (2 + 4 + 8 + 16 = 30, so 5 slots spare in P4)
```

Twitter's justification is one sentence and it is the whole idea: "the
more that we observe an edge incident to a vertex, the more likely that
more edges will follow. Hence, it makes sense to exponentially increase
the amount of allocated space each time." Preferential attachment, used
as an allocator policy.

The rest of the design is a catalogue of things a real-time graph store
needs:

- **Temporally-partitioned index segments.** Only the newest accepts
  writes; the rest are immutable. Segments older than *n* hours are
  discarded whole — coarse-grained pruning that "does not have a
  noticeable impact on recommendation quality".
- **No deletes and no edge timestamps.** Both are explicit trade-offs.
  Interactions are point events that cannot be undone, and beyond the
  window "we've found it hard to design algorithms that provide
  substantial gains by exploiting edge timestamps".
- **Single writer, many readers.** Insertions come from one thread
  reading a Kafka queue, so memory barriers replace locks entirely.
- **Sealed-segment relayout.** Once a segment stops accepting edges a
  background thread lays every adjacency list out end to end with no
  gaps, so iteration touches contiguous memory and sampled neighbours
  share cache lines.
- **Alias-method sampling** across segments: O(n) preprocessing, O(1)
  draws, with segment selection weighted by that vertex's degree in each
  segment — which makes sampling across all segments equivalent to
  sampling uniformly from all edges.
- **Bit-packing**: edge type plus segment-internal vertex id in one
  32-bit integer; 3 bits of type leaves 2²⁹ ≈ 537M vertices per segment.

Measured, on two 6-core Xeon E5-2620 v2 at 2.10 GHz: **1 million edge
insertions/s** during cold-start catch-up, **500 recommendation
requests/s** per server at **p50 19 ms / p90 27 ms / p99 33 ms**
end-to-end, several million edge reads/s, **O(10⁹) edges in under 30 GB**,
and >99.99% success over a typical 30-day period.

§7.3 is the paragraph to read twice if you work on a Redis-based graph
engine. GraphJet evaluates Redis `LPUSH` as an adjacency-list store and
rejects it for two named reasons: it "lacks the memory allocation
optimizations in GraphJet", and it "lacks a mechanism for pruning these
lists". That is a two-item feature list, written by someone who wanted
to use Redis and could not.

## TAO: the social graph as two shapes and five queries

```
   Object:  (id)              → (otype, (key → value)*)
   Assoc:   (id1, atype, id2) → (time,  (key → value)*)

   Association List:  (id1, atype) → [a_new … a_old]     ordered by time, DESC

   assoc_get(id1, atype, id2set, high?, low?)
   assoc_count(id1, atype)
   assoc_range(id1, atype, pos, limit)
   assoc_time_range(id1, atype, high, low, limit)
```

That is the entire data model of a system serving Facebook. The design
decision worth stealing is **creation-time locality**: "most of the data
is old, but many of the queries are for the newest subset", so
association lists are stored newest-first and the cache holds *prefixes*
of them. Which in turn forces an unusual invalidation rule — a leader
sends a **refill** rather than an invalidate for association writes,
because invalidating would truncate a cached prefix and throw away edges
the follower would then have to re-fetch.

Measured in production (144 GB RAM, 2× 8-core Xeon E5-2660, 10 GbE):

| operation | hit p50 | hit p99 | miss p50 | miss p99 |
|---|---|---|---|---|
| `assoc_count` | 1.1 ms | 28.9 | 5.0 | 186.8 |
| `assoc_get` | 1.0 | 25.9 | 5.8 | 143.1 |
| `assoc_range` | 1.1 | 24.8 | 5.4 | 93.6 |
| `obj_get` | 1.0 | 27.0 | 8.2 | 186.4 |

Overall read hit rate **96.4%**; peak follower throughput ~500K–600K
requests/s at high hit rates; write latency **12.1 ms** in the master's
region and **74.4 ms** from a region 58 ms away (58.1 + 16.3). Read
misses are **25× as frequent as writes**, which is the number the whole
leader/follower hierarchy is designed around. Failed queries over 90
days: **4.9 × 10⁻⁶**.

Two distribution facts to keep: **1% of `assoc_count` results are
≥512K** (the high-degree tail is real and must be handled specially),
and **64% of non-empty range results return exactly one edge** (so the
common case is tiny and the tail is enormous — plan for both).

## Link prediction: topology alone, measured

Lane 3, on a collaboration graph where a random guess is right 0.314% of
the time:

```
   predictor                  hits / n     factor over random
   preferential attachment       6 / 985            1.9x
   common neighbors             64 / 985           20.7x
   Jaccard                      79 / 985           25.6x
   Adamic/Adar                  69 / 985           22.3x
```

and Liben-Nowell & Kleinberg's Figure 3 on five arXiv co-authorship
networks, same shape:

| predictor | astro-ph | cond-mat | gr-qc | hep-ph | hep-th |
|---|---|---|---|---|---|
| *random is correct* | 0.475% | 0.147% | 0.341% | 0.207% | 0.153% |
| common neighbors | 18.0 | 41.1 | 27.2 | 27.0 | 47.2 |
| **preferential attachment** | **4.7** | **6.1** | **7.6** | **15.2** | **7.5** |
| Adamic/Adar | 16.8 | 54.8 | 30.1 | 33.3 | 50.5 |
| Jaccard | 16.4 | 42.3 | 19.9 | 27.7 | 41.7 |
| Katz (weighted) β=0.005 | 13.4 | 54.8 | 30.1 | 24.0 | 52.2 |

Preferential attachment scores `|Γ(x)|·|Γ(y)|` — pure degree, never
asking whether the two nodes have anything in common — and it is the
worst of the family on four of five networks. It is lane 1's popularity
baseline in a link-prediction costume, losing for the same reason.
Adamic/Adar is the counterweight: common neighbours with each shared
neighbour discounted by `1/log|Γ(z)|`, so a mutual friend who knows
everybody counts for almost nothing. Topic 23 calls that inverse
document frequency and topic 39 calls it FRAUDAR's column weights.

## Reading guides

1. [reading-pixie.md](reading-pixie.md) — Pixie WWW'18: the four innovations, and why graph size does not enter the cost.
2. [reading-graphjet.md](reading-graphjet.md) — GraphJet VLDB'16: index segments, doubling edge pools, and four generations of getting it wrong first.
3. [reading-tao.md](reading-tao.md) — TAO ATC'13: two shapes, five queries, and what a 96.4% hit rate is built out of.
4. [reading-link-prediction.md](reading-link-prediction.md) — Liben-Nowell & Kleinberg: the measure catalogue, and why degree alone loses.

## Experiments

```
cd experiments
cargo test              # 2 provided tests pass; 8 fix the contract for your stubs
cargo run --release --bin social_bench
```

- `graphs.rs` (PROVIDED) — the bipartite interaction graph (communities,
  Zipf popularity, multi-interest users, held-out engagements) and the
  Liben-Nowell collaboration graph (preferential attachment + triadic
  closure, train/test split, `core` filter); plus the baselines
  (`popularity_topk`, `basic_random_walk` = Pixie's Algorithm 1) and the
  metrics (`hit_rate`, `personalization`, `popularity_overlap`,
  `evaluate` with factor-over-random).
- `pixie.rs` (stub) — `allocate_steps` (Equation 1–2), `walk_per_query`,
  `multi_hit_boost` (Equation 3), `pixie_walk` with early stopping.
- `linkpred.rs` (stub) — `common_neighbors`, `jaccard`, `adamic_adar`,
  `preferential_attachment`.

Bench lanes: 1 = the popularity trap (provided, above). 2 = the Pixie
ablation (reference: early stopping at 35% of steps / 2.2× / 0.793
overlap; the multi-hit booster showing **no** gain, at either interest
count). 3 = link prediction (reference: PA 1.9× vs common neighbours
20.7× / Jaccard 25.6× / Adamic-Adar 22.3× over a 0.314%-accurate
random predictor).

## Exercises

1. Implement the stubs until all 10 tests pass and lanes 2–3 print.
2. **Biasing the walk.** Pixie's `PersonalizedNeighbor(E, U)` is the
   innovation the crate leaves out, and the paper's sharpest number:
   for an English query pin, target-language content in the results goes
   from **2.13% to 42.55%** (Slovak) and **16.35% to 80.33%** (Japanese).
   Give each item a "language" attribute and each user a preferred one,
   make neighbour selection prefer matching edges, and reproduce the
   shape of Table 3. Then measure what it costs per step.
3. **Sub-linear allocation, ablated.** Replace Equation 1 with a linear
   allocation `N_q ∝ w_q · |E(q)|` and measure how many query pins get
   zero steps, and what that does to hit rate. Then try uniform
   allocation. Which of the three would you ship?
4. **Make the booster earn its place.** Lane 2 finds no gain from
   Equation 3 because the generator's held-out item does not favour
   cross-interest candidates. Add a `cross_interest_bias` to
   `BipartiteConfig` that makes held-out items more likely to sit in the
   overlap of a user's interests, sweep it, and find the point at which
   the multi-hit boost starts to pay. Report the crossover.
5. **Edge pools.** Implement GraphJet §4.1.2's allocator: pools `P_r`
   holding slices of `2^r` edges, a vertex represented as
   `d : P1(k1), P2(k2), …`. Measure bytes-per-edge and insertion cost
   against a `Vec<u32>` that doubles, at a Zipf degree distribution.
   Where does the pool win, and where does the pointer chasing hurt?
6. **Temporal windows.** Add GraphJet's segment model to
   `graphs.rs`: append edges into a newest segment, seal it at a size
   threshold, discard segments older than *n*, and sample across
   segments with the alias method. Measure recommendation quality
   against window size — GraphJet claims the loss is not noticeable, so
   find where it starts to be.
7. **TAO's association list.** Implement `assoc_range` and
   `assoc_time_range` over a time-ordered adjacency list with a cached
   prefix, then implement invalidation *and* refill. Construct the case
   where invalidation throws away edges a refill would have kept, and
   measure the extra fetches.

## Cross-topic threads

- **Topic 38 (GraphRAG)**: Pixie's walk and HippoRAG's personalized
  PageRank are the same primitive with different seeds and different
  scoring — and both are chosen over iterative retrieval for the same
  reason, that the cost is a function of steps rather than of corpus.
- **Topic 23 (full-text) / topic 39 (fraud)**: Adamic/Adar's
  `1/log|Γ(z)|`, IDF, and FRAUDAR's `1/log(d+5)` column weights are one
  idea in three fields — discount the evidence that everybody shares.
- **Topic 26 (probabilistic & indexing)**: GraphJet's alias method is
  O(1) sampling from a discrete distribution, and its bit-packed
  segment-internal ids are the same dictionary-narrowing move as a
  columnar encoder.
- **Topic 36 (sharding)**: TAO shards by `id1` so every association
  query is served from one server, and clones hot shards rather than
  re-partitioning. Compare with topic 36's slot migration.
- **Topic 28 (cloud-native)**: TAO's leader/follower hierarchy and
  master/slave regions are a caching tree over a shared storage layer —
  the same shape as Aurora's, with different consistency.
- **Topic 6 (buffer pool)**: TAO's cache is a slab allocator with LRU
  and per-type arenas, and its association counts live in a
  direct-mapped 8-way associative cache with no pointers. That is buffer
  management, in a graph store.
- **Topic 12 (columnar)**: GraphJet's sealed-segment relayout — copy
  everything contiguously once the data stops changing — is exactly the
  write-optimized-then-read-optimized split of an LSM compaction.

## Capstone M42 — a real-time recommendation path on the Rust engine

- A **temporally-bounded bipartite interaction store** over M31's
  storage with GraphJet's index segments and doubling edge pools:
  single-writer with memory-barrier reads, background relayout of sealed
  segments, alias-method sampling across segments.
- A **Pixie-shaped random-walk procedure** taking a weighted query set,
  with sub-linear step allocation and early stopping.
- A **TAO-shaped association-list API** — `assoc_range`,
  `assoc_time_range`, `assoc_count` — with time-ordered lists and cached
  counts on the property layer.
- Deliverable numbers: edge ingest rate vs GraphJet's 1M/s;
  recommendation p50/p99 vs its 19/33 ms at 500 req/s; memory per edge
  vs its <30 GB per O(10⁹) edges; walk latency vs `social_bench` lane 2;
  and the comparison the GraphJet paper explicitly invites — a Redis
  adjacency-list baseline on the same workload, measured on both counts
  §7.3 names.
