# Topic 42 notes — Recommendations & social graphs

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| lane 1: popularity hit-rate@50 | weak, maybe 0.05 | **0.340** — the bestseller list is a real baseline |
| lane 1: popularity personalization | 0.000 | **0.155**, and only because own-items are filtered out |
| lane 1: basic walk hit-rate@50 | much better than popularity | **0.403** — only marginally better |
| lane 1: basic walk overlap with bestsellers | ~0.2 | **0.451** — half of it is the bestseller list |
| lane 2: 8 query pins vs 1 | +0.1 | (stub — reference: 0.403 → **0.823**, the biggest single win) |
| lane 2: multi-hit boost | +0.05 | (stub — reference: **−0.02**. No gain. See below.) |
| lane 2: early-stop step saving | ~50% | (stub — reference: **35% of full steps, 2.2× faster**) |
| lane 2: early-stop top-50 overlap | ≥0.7 | (stub — reference: **0.793**, hit rate unchanged) |
| lane 2: walk latency | ~1 ms | (stub — reference: 2.12 ms full, **0.97 ms early-stopped**) |
| lane 3: random baseline accuracy | ~0.3% | (stub — reference: **0.314%**, right in the paper's 0.147–0.475% band) |
| lane 3: common neighbours | 20× | (stub — reference: **20.7×**) |
| lane 3: Jaccard | ~common | (stub — reference: **25.6×** — best on this generator) |
| lane 3: Adamic/Adar | best | (stub — reference: **22.3×**, above common neighbours as in the paper) |
| lane 3: preferential attachment | worst | (stub — reference: **1.9×** — barely above chance) |

Three mechanics worth memorizing.

**The popularity trap is real and it is subtle.** On a power-law graph
the bestseller list gets a third of users right, so a recommender that
"beats popularity on hit rate" may simply *be* popularity. Lane 1's
third column is the one to watch: an unbiased random walk personalizes
(0.820) yet still fills 45% of every list with globally popular items,
because its stationary distribution goes as degree. Any evaluation
without a popularity-overlap column is missing the failure mode.

**The biggest win in lane 2 is the least clever idea.** Going from one
query pin to eight — Pixie's innovation 2, plus the sub-linear step
allocation that keeps low-degree pins alive — takes hit rate from 0.403
to 0.823. Early stopping then buys a 2.2× speedup for free. The elegant
part, the multi-hit booster, buys nothing here.

**A published trick encodes a domain assumption.** Equation 3 is a bet
that a pin at the intersection of several of your interests beats one
deep inside a single interest. That is a claim about people. This
generator draws its held-out item from the same distribution as the
training items, so reachability from several query pins carries no
information about the answer, and the boost is a no-op at best. The
arithmetic is right (the unit test pins `(√2+√2)² = 8` against a
single-source 4); the premise is absent. Exercise 4 builds a graph where
it is present. **Measure the assumption before shipping the trick.**

## Guide-question checklist

- [ ] reading-pixie.md Q1–Q5
- [ ] reading-graphjet.md Q1–Q5
- [ ] reading-tao.md Q1–Q5
- [ ] reading-link-prediction.md Q1–Q5

## Paper numbers worth keeping

| fact | source |
|---|---|
| pruned Pinterest graph: **1B boards, 2B pins, 17B edges in ~120 GB** on one r3.8xlarge | Pixie §1 |
| **p99 < 60 ms**, ~1,200 req/s per server, ~100,000 req/s cluster-wide | Pixie §1 |
| real-time vs day-old recommendations: **30–50% higher engagement** | Pixie §2 |
| step allocation `s_q = |E(q)|·(C − log|E(q)|)`, C = max over ALL pins | Pixie Eq. 1 |
| multi-hit boost `V[p] = (Σ_q √V_q[p])²` | Pixie Eq. 3 |
| early stopping n_p=2000, n_v=4: **84% overlap at 1/3 the runtime**; n_v=6 halves it | Pixie §4.2 |
| pruning at δ=0.91: **F1 peaks 58% above unpruned with 20% of the edges** | Pixie §4.3 |
| hit rate top-10/100/1000: Pixie **6.3 / 23.1 / 52.2%** vs content-combined 2.1 / 4.6 / 10.5% | Pixie Table 1 |
| language biasing: En→Slovak **2.13% → 42.55%**; En→Japanese 16.35% → 80.33% | Pixie Table 3 |
| A/B lifts: homefeed **+48%**, related pins +13%, localization +48–75% | Pixie Table 2 |
| HugePages 4 KB → 2 MB: **512× fewer page-table entries**, 2× requests at half the runtime on VMs | Pixie §3.3 |
| GraphJet: **1M edge insertions/s**, 500 rec/s per server, **p50 19 / p90 27 / p99 33 ms** | GraphJet §6 |
| **O(10⁹) edges in <30 GB**; >99.99% success over 30 days | GraphJet §6 |
| edge pools: `P_r` holds `n/2^{r−1}` slices of `2^r` edges; degree 25 → `P1(1),P2(2),P3(0),P4(0)` | GraphJet §4.1.2 |
| "the more that we observe an edge incident to a vertex, the more likely that more edges will follow" | GraphJet §4.1.2 |
| 3 bits of edge type + 29 bits of vertex id = **537M vertices per segment** | GraphJet §4.1.1 |
| ten billion edges as a naïve edge list = **80 GB**, "well in the range of memory available" | GraphJet §2.1 |
| Redis `LPUSH` rejected for two reasons: no memory-allocation optimization, **no pruning mechanism** | GraphJet §7.3 |
| MagicRecs' reformulation: temporal edge detection = **intersection of adjacency lists**; ~7 s median | GraphJet §2.3 |
| TAO: `Object (id)→(otype,kv*)`, `Assoc (id1,atype,id2)→(time,kv*)`, list newest-first | TAO §3.1, §3.4 |
| **read misses are 25× as frequent as writes** | TAO §4.5 |
| read hit rate **96.4%**; `assoc_get` 1.0 ms p50 hit / 5.8 ms p50 miss / 143 ms p99 miss | TAO §8 |
| write latency **12.1 ms** in-region, **74.4 = 58.1 + 16.3 ms** from 58 ms away | TAO §8 |
| failed queries over 90 days: **4.9 × 10⁻⁶**; replication lag <1 s for 85% of the window | TAO §8 |
| association count cached in **14 bytes** (negative entry 10) → 20% more cache entries | TAO §5.1 |
| **1% of `assoc_count` results are ≥512K**; **64% of non-empty ranges return exactly 1 edge** | TAO §7 |
| per-atype query limit typically **6,000** | TAO §3.4 |
| link prediction random baseline: **0.147%–0.475%** correct | Liben-Nowell Fig. 3 |
| preferential attachment **4.7–15.2×** vs common neighbours **18.0–47.2×**, Adamic/Adar **16.8–54.8×** | Liben-Nowell Fig. 3 |
| "There is no single clear winner among the techniques" | Liben-Nowell §4 |

## Cross-topic threads (worked)

- **Topic 38 ↔ 42**: Pixie's walk, GraphJet's circle of trust and
  HippoRAG's PPR are one primitive — a random walk with restart over a
  bipartite or entity graph — chosen in all three cases because the cost
  is a function of steps, not of corpus or graph size. Compare the
  seeding: HippoRAG seeds from query entities, Pixie from recent
  engagements, GraphJet from the circle of trust.
- **Topic 23 / 39 ↔ 42**: Adamic/Adar's `1/log|Γ(z)|`, IDF, and FRAUDAR's
  `1/log(d+5)` are one idea in three fields. Discount the evidence
  everybody shares.
- **Topic 9 ↔ 42**: GraphJet deletes the entire latch hierarchy by
  making the writer singular — one thread off a Kafka queue, memory
  barriers instead of locks. The design question topic 9 answers with
  epochs and lock-free structures, GraphJet answers by not having the
  problem.
- **Topic 12 ↔ 42**: GraphJet's sealed-segment relayout (write-optimized
  while hot, copied contiguously once immutable) is an LSM compaction
  applied to adjacency lists. TAO's 14-byte association count is a
  columnar encoder's instinct in a cache.
- **Topic 26 ↔ 42**: the alias method — O(n) preprocessing, O(1) draws —
  is what makes GraphJet's cross-segment sampling free; bit-packing edge
  type into the vertex id is dictionary narrowing.
- **Topic 6 ↔ 42**: TAO's cache is slab allocation, LRU and per-type
  arenas, with association counts in a pointer-free direct-mapped cache.
  Buffer management, in a graph store.
- **Topic 36 ↔ 42**: TAO shards associations by `id1` so every query
  hits one server — which is *why* the API has no multi-hop traversal —
  and clones hot shards rather than re-partitioning them.
- **Topic 40 ↔ 42**: TAO and Zanzibar face the same hot-spot problem.
  TAO answers with shard cloning and client-side caching keyed on access
  rate; Zanzibar with consistent hashing, a lock table and timestamp
  quantization. Two production answers, worth comparing point by point.
- **Topic 25 ↔ 42**: Liben-Nowell's low-rank-approximation
  meta-approach is where matrix factorization and, eventually, graph
  embeddings come from — and topic 41's Elliptic result is the reminder
  that the learned version does not automatically win.

## Open questions

- The multi-hit booster showed no gain at either interest count. Is
  there a *graph* property (rather than a behavioural one) under which
  Equation 3 helps, or is it purely a bet about engagement? Exercise 4
  should settle it.
- Pixie's pruning improves F1 by 58% while removing 80% of edges.
  Liben-Nowell's clustering meta-approach is the same move. Nobody in
  either paper characterises *which* graphs this works on — is there a
  statistic that predicts it before you try?
- GraphJet stores no edge timestamps and reports the quality loss as
  negligible beyond the window. That claim is made for Twitter's
  interaction graph at a particular window size; exercise 6 asks where
  it stops being true.
- TAO has no multi-hop traversal because associations shard by `id1`.
  What is the cheapest change to that physical design that makes two
  hops affordable, and does it survive the 1%-of-nodes-with-512K-edges
  tail?
