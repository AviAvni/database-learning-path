# GraphJet: a storage engine shaped like a power law

Most systems papers describe the system that worked. This one describes four, in order, and
explains why each was thrown away — Cassovary, then a Hadoop pipeline, then MagicRecs, then
GraphJet. That structure is the reason to read it: you get the design space, not just a point in
it. And the technical core is unusually transferable, because GraphJet is a *storage engine*
first and a recommender second. Its memory allocator, its temporal partitioning, its
single-writer concurrency model and its O(1) sampling are the parts you would steal for any
real-time graph store — including one built on Redis, which the paper evaluates and rejects for
two specific, fixable reasons.

## The problem in one sentence

**Maintain a bipartite user–tweet interaction graph that is being written a million edges a second
and read several million edges a second, answer recommendation queries off it in tens of
milliseconds, and never let it grow without bound.**

## The concepts, step by step

### Step 1 — The unconventional bet: one machine

> **In:** nothing yet — this step fixes the design premise the whole engine is built on.
> **Out:** the single-server bet and its 80 GB arithmetic (10 billion edges × 8 bytes), the
> constraint every later step honours: the graph lives in one machine's RAM, so the work is *fitting
> it there* (Steps 4–6) rather than partitioning it.

Twitter's recommendation work began with WTF ("Who To Follow") in 2010, and the first decision
was the one everybody questions:

> One of the key enablers that made WTF possible in such a rapid deployment was what many might
> consider an unconventional design choice: to assume that the entire graph fits into memory *on
> a single server*. While the prevailing wisdom was (and is) to design distributed,
> horizontally-partitioned, scale-out infrastructure, we took exactly the opposite approach of
> scaling up on individual large-memory (but still commodity) servers.

The arithmetic they used to justify it is worth repeating because it is still roughly right:
"Consider a graph with ten billion edges: even a naïve representation as an edge list would occupy
a mere 80 GB, which is well in the range of memory available on commodity servers today."

And the aside, which is a challenge to a whole research area: "although distributed graph stores
and graph processing engines are interesting, we wonder if the focus and effort that the academic
community places on this class of solutions overstates its importance and relevance for solving
real-world problems."

### Step 2 — Four generations, and what each one got wrong

> **In:** the single-server constraint from Step 1.
> **Out:** four systems in sequence (Cassovary → Hadoop → MagicRecs → GraphJet) and the failure
> that killed each — culminating in MagicRecs' reformulation of "B→C edges in a time window" as an
> **intersection of adjacency lists**, which is the primitive GraphJet turns into a storage engine.

- **Cassovary (2010)** — in-memory, single server, snapshots of the follow graph from HDFS,
  computed *circle of trust* (an egocentric random walk = personalized PageRank) and SALSA. It
  worked. Its limit: snapshots could only be refreshed about once a day, so new users got nothing
  — the cold-start problem, on an infrastructure that could not be made fresher.
- **RealGraph on Hadoop (2012)** — richer signals from behaviour logs, no longer assumed to fit
  in memory. Its limit: batch. "nearly all recommendations were generated in batch at roughly
  daily intervals. This, of course, was dissonant with the core 'Twitter experience'."
- **MagicRecs (2013)** — real-time push: when edge `B₂ → C₂` appears, and more than *k* of user
  A's followees follow C within time τ, push C to A. The key reformulation is worth memorising:
  *the task of identifying B→C edges within a temporal interval can be recast as an
  **intersection of adjacency lists***. Median latency ~7 s end to end, "the actual graph queries
  take only a few milliseconds" — nearly all the latency was message propagation.
- **GraphJet (2014)** — MagicRecs generalized: a real *storage engine* with an API rich enough to
  express a range of recommendation algorithms rather than one hard-coded rule.

### Step 3 — The API is five methods, and the omissions are the design

> **In:** MagicRecs' single hard-coded push rule from Step 2.
> **Out:** GraphJet's five-method interface — insert one edge, iterate a vertex's edges, sample `k`
> of them (both left and right) — and the three omissions (no delete, no timestamp, sampling *with
> replacement*) that make Steps 4–8 cheap.

```
   insertEdge(u, t, r)                  insert user→tweet edge of type r
   getLeftVertexEdges(u)                iterator over (t, r) incident to u
   getLeftVertexRandomEdges(u, k)       k edges sampled uniformly WITH replacement
   getRightVertexEdges(t)               the symmetric pair
   getRightVertexRandomEdges(t, k)
```

That is all of it. Three omissions carry the design:

- **No edge deletion.** Justified by the data model: edges are interactions (a retweet, a like),
  which are point events. "it is not meaningful to talk about deleting such edges, since such
  interactions cannot usually be undone." Not supporting deletes "greatly simplifies many aspects
  of the implementation."
- **No edge timestamps.** An explicit space-versus-quality trade: varying the *window* size does
  affect quality, "but beyond knowing that an interaction happened within the last n hours, we've
  found it hard to design algorithms that provide substantial gains by exploiting edge
  timestamps."
- **Sampling with replacement.** So `getRandomEdges(u, k)` can return duplicates when the degree
  is below k — which is fine for a random walk and lets the implementation be O(1).

The consistency guarantee is deliberately narrow: an iterator sees a consistent state *at call
time*, but "multiple gets on the same vertex may indeed return different numbers of edges". That
is enough for a random walk and much cheaper than a snapshot.

### Step 4 — Temporal index segments

> **In:** the write-and-sample API from Step 3, plus the "never grow without bound" requirement from
> the problem sentence.
> **Out:** the graph split into **temporally-ordered index segments** — one active (writable), the
> rest immutable, the oldest dropped whole — the structure Steps 5 (id narrowing), 6 (write-side
> allocator) and 8 (read-side relayout) each exploit.

The graph is partitioned into **temporally-ordered index segments**. Only the newest accepts
writes; the rest are immutable. A segment older than *n* hours is discarded whole.

```
   [seg 0] [seg 1] [seg 2] [seg 3] [seg 4] ← graph edges
    read-only, immutable            ↑ active, single writer
    ↑ discarded when older than n hours
```

Three things fall out of this that are worth noticing:

1. **Pruning is coarse-grained and free.** No per-edge expiry check; you drop a segment.
   "Experiments show that this does not have a noticeable impact on recommendation quality."
2. **Only one segment needs write-optimized structures.** Sealed segments can be reorganised for
   reads (Step 6).
3. **Ids can be narrowed.** Because each segment holds a bounded vertex set, global 64-bit ids
   map to segment-internal ids that fit in far less space (Step 5).

The paper credits the idea to Earlybird, Twitter's real-time search engine — "temporal
partitioning of index segments was also a design borrowed from Earlybird, which allowed us to
prune the interaction graph in an efficient (but coarse-grained) manner". Postings lists and
adjacency lists, again the same problem.

### Step 5 — Id mapping and bit-packing

> **In:** a single segment's bounded vertex set from Step 4.
> **Out:** a segment-internal id — 64-bit external ids hashed into a small per-segment id, then
> **bit-packed** with the edge type into one 32-bit integer, so an adjacency list is just a
> `u32` array. Step 6 allocates space for those arrays.

External vertex ids are 64-bit. Within a segment, they are hashed to a segment-internal id using
double hashing in an open-addressed table — and crucially "we use the hash value as our internal
vertex id", so the table cannot be rehashed to grow. The workaround is a chain of power-of-two
tables: size the first at `2^b` for `b = ⌈lg(n/f)⌉` with load factor `f`, fill to `f·2^b`, then
allocate a second of size `2^{b−1}`, and the internal id in the new table is its hash plus `2^b`
— so the mapping stays reversible.

Then: "We further optimize by bit-packing both the edge type and internal vertex id in a single
32-bit integer ... we might reserve three bits to support eight edge types, which gives us room
for 2²⁹ (approximately 537 million) unique vertex ids." An adjacency list becomes, simply, an
array of 32-bit integers.

### Step 6 — Edge pools: the allocator as a model of the data

> **In:** the 32-bit edge entries from Step 5, arriving one at a time into the active segment.
> **Out:** the **doubling edge-pool allocator** — each adjacency list stored as a chain of
> power-of-two slices (`P_r` holds slices of `2^r` edges) — whose growth curve *is* a bet that the
> data follows a power law. Step 8 tears this down once the segment seals.

This is the part to steal. Adjacency lists cannot be kept contiguous as the graph grows (you
would relocate constantly), so GraphJet stores each list as a chain of **slices** — and the slice
sizes **double**:

```
   P1  slices of 2^1 edges,  array of length 2^1 · n
   P2  slices of 2^2 edges,  n/2 slices
   P3  slices of 2^3 edges,  n/4 slices
   Pr  slices of 2^r edges,  n/2^{r−1} slices

   a vertex of degree 25:   v → 25 : P1(1), P2(2), P3(0), P4(0)
   first 2 edges in slot 1 of P1, next 4 in slot 2 of P2, next 8 in slot 0
   of P3, next 16 in slot 0 of P4 — of which 5 are still free (2+4+8+16 = 30)
```

The justification is a statement about the data, not about memory:

> Our allocation strategy implicitly assumes some type of preferential attachment effect, since
> it is an easy way to explain the existence of power-law distributions: the more that we observe
> an edge incident to a vertex, the more likely that more edges will follow. Hence, it makes
> sense to exponentially increase the amount of allocated space each time.

**Preferential attachment** is the "rich get richer" process — a vertex that already has many edges
is disproportionately likely to gain more — and it is the standard generative story for a
**power-law** degree distribution (a handful of vertices with enormous degree, a very long tail of
tiny ones). The allocator bakes that assumption into its growth curve.

The general rule for an arbitrary degree `d`: the edges fill pools in order, one slice per pool,
where pool `P_r`'s slice holds `2^r` edges. A vertex of degree `d` therefore occupies slices in
`P_1 … P_k`, where `k` is the smallest integer with cumulative capacity
`2^{k+1} − 2 = 2 + 4 + … + 2^k ≥ d`, and its top slice has `2^{k+1} − 2 − d` unused slots. Check it
against the degree-25 case: cumulative capacities are `P1→2, P1..P2→6, P1..P3→14, P1..P4→30`, and
`30 ≥ 25` first at `k = 4`, so the vertex spans `P1..P4` with `30 − 25 = 5` free slots — exactly the
figure above.

Because the slice sizes are fixed and known, "we know from the vertex degree where to insert the
next edge and how much space is left in the current slice" — no per-vertex metadata beyond the
degree and the slot indices. Exercise 5 asks you to build it and measure bytes-per-edge against a
doubling `Vec`.

### Step 7 — One writer, no locks

> **In:** the active segment's edge pools from Step 6, written by one thread and read by many.
> **Out:** the **single-writer, multi-reader** concurrency model — no write–write conflicts to
> guard, only memory-visibility handled with memory barriers — which is why the entire latch
> hierarchy topic 9 builds is simply absent here.

"Since we adopt a single-writer, multi-reader design, there is no need to worry about write–write
conflicts." Edge insertions all come from one thread reading a Kafka queue; reads are served by
many threads; and "judicious use of memory barriers is sufficient to address memory visibility
issues across multiple threads. Memory barriers are sufficiently lightweight that the performance
penalties are acceptable."

One writer is enough because it can sustain a million edges a second, which is more than the
stream produces. The design is worth contrasting with everything topic 9 builds: when you can
make the writer singular, the entire latch hierarchy disappears.

### Step 8 — Sealed-segment relayout, and O(1) sampling

> **In:** a segment that has just stopped accepting edges (sealed), still in the write-optimized
> edge-pool layout of Step 6.
> **Out:** a compacted, gap-free, read-optimized relayout of that segment, plus the **alias method**
> that makes cross-segment sampling O(1) — the primitive Step 9's random walks call.

Once a segment stops accepting edges, a background thread rebuilds it:

> since the graph partition is now immutable, we no longer need the edge pool structure to store
> the adjacency lists. Because we know the final degree of each vertex, we can lay out each
> adjacency list end to end in a large array without any gaps ... This layout guarantees that
> iteration over the edges of a particular vertex will touch contiguous regions in memory ... When
> sampling edges, we also increase the probability that multiple samples reside on the same cache
> line.

Write-optimized while hot, read-optimized once cold. That is an LSM compaction, applied to
adjacency lists.

Sampling across segments needs care: a vertex has different degrees in different segments, so to
sample uniformly from *all* its edges you must pick a segment with probability proportional to
its degree there, then sample uniformly within it. GraphJet uses the **alias method** — O(n)
preprocessing over the n segments, then O(1) per draw — and builds the two tables (probability
and alias) when the iterator is created, "along with other state information to ensure that edges
added after the API call are not visible."

### Step 9 — SALSA, full and subgraph

> **In:** the O(1) edge-sampling primitive from Step 8.
> **Out:** the two recommendation algorithms that ride on it — full **SALSA** and subgraph SALSA —
> and the memory-versus-quality trade between them (roughly half the index, at the cost of
> second-order paths).

The recommendation algorithms are random walks on the bipartite graph. **SALSA** (*Stochastic
Approach for Link-Structure Analysis*) is a bipartite random walk that alternates sides and ranks
vertices by how often the walk visits them. **Full SALSA**: start from
the user (or a *seed set* — the circle of trust, which handles users with no interactions),
alternate left→right→left, restart with probability α, and rank right-hand vertices by visit
distribution.

**Subgraph SALSA** materializes a small subgraph induced by the seed set first, then runs a
PageRank-like weight distribution on it until convergence. The trade is instructive: it is "much
faster than the full SALSA version since it only needs to access the complete interaction graph
once to materialize the subgraph. The subgraph usually fits into cache ... this algorithm only
requires a left-to-right segment index, so the memory consumption in GraphJet is roughly half of
the fully-indexed case." What it gives up: "it ignores all paths from the seed set to the fanout
vertices through other user vertices that are outside of the seed set."

Fitting in cache and halving the index, at the cost of second-order paths. Both ship.

### Step 10 — The numbers

> **In:** the complete engine of Steps 4–9, deployed.
> **Out:** the measured envelope — insertion throughput, per-request latency percentiles, capacity
> per machine, availability — the figures your own build should be judged against.

Two Intel Xeon 6-core E5-2620 v2 at 2.10 GHz:

```
   cold start ......... 1,000,000 edge insertions/s while catching up from Kafka
   steady state ....... tens of thousands of edges/s (the real engagement rate)
   recommendations .... 500 requests/s per server
   latency ............ p50 19 ms, p90 27 ms, p99 33 ms  (end to end, incl. RPC)
   edge reads ......... several million/s
   capacity ........... O(10⁹) edges in under 30 GB of RAM
   availability ....... >99.99% over a typical 30-day period
```

Fault tolerance is replication — every server holds a complete copy — and the Kafka queue is
replicated across data centres, so "GraphJet only needs to handle read-path failures ... even in
the case of catastrophic Kafka failure, GraphJet can continue serving recommendations (albeit
with increasingly stale data in memory)."

### Step 11 — §7.3, the paragraph for anyone building on Redis

> **In:** the allocator (Step 6) and temporal pruning (Step 4) as the two things that distinguish
> GraphJet from a generic list store.
> **Out:** §7.3's verdict — the two specific, named reasons Redis's `LPUSH` cannot be the graph
> store, which is exactly the gap capstone M42 asks you to close.

> It is possible, of course, to use any key–value store to hold the adjacency lists that comprise
> a graph, thus serving as a real-time graph store ... but Redis in particular supports a command
> (LPUSH) that inserts specified values at the head of the list stored at a key. The
> implementation of the command, however, lacks the memory allocation optimizations in GraphJet.
> Furthermore, Redis lacks a mechanism for pruning these lists; although it would be possible to
> implement temporal partitioning, it would basically be replicating some of the main design
> features in GraphJet.

Two named gaps: **the allocator** (Step 6) and **temporal pruning** (Step 4). Written by engineers
who considered Redis and could not use it. Read as a feature list, that is a short one — and it
is exactly what capstone M42 asks you to build.

## How to read the paper (with the concepts in hand)

- **§1 + §2.1.** The single-server bet and its arithmetic. Read the "we wonder if the focus and
  effort the academic community places on this class of solutions overstates its importance"
  paragraph and decide whether you agree.
- **§2.1.2 + Figure 2.** Circle of trust and SALSA. Note circle-of-trust *is* personalized
  PageRank — the same primitive as topic 38's HippoRAG and this topic's Pixie.
- **§2.2–2.3.** The Hadoop generation and MagicRecs. The "recast as an intersection of adjacency
  lists" sentence in §2.3 is the pivot of the whole paper.
- **§3.2 Data model and API.** Five methods. Then spend a minute on each *omission* — deletes,
  timestamps — and what it buys.
- **§3.3 + Figure 5.** Index segments. Draw the picture.
- **§4.1.1.** Id mapping: double hashing, non-rehashable tables, the power-of-two chain, the
  32-bit bit-pack.
- **§4.1.2 + Figure 6.** Edge pools. Work the degree-25 example by hand until `P1(1), P2(2),
  P3(0), P4(0)` is obvious, and find where the 5 spare slots are.
- **§4.1.3.** Sealed-segment relayout.
- **§4.2.** The alias method, and why sampling across segments needs degree-weighted selection.
- **§5.1–5.3.** Full SALSA, subgraph SALSA, and the cosine-similarity query.
- **§6 + Figure 7.** The deployment numbers.
- **§7.3.** The Redis paragraph.
- **After the paper.** Do exercises 5 and 6 — the edge-pool allocator and the segment model — and
  measure bytes-per-edge against a doubling `Vec<u32>` at a Zipf degree distribution.

## Questions to answer in notes.md

1. GraphJet supports no deletes and stores no timestamps. For each, state the property of the
   *data* that makes the omission safe, and name a workload where it would not be.
2. Work the edge-pool arithmetic: for a Zipf degree distribution with exponent 1.1 over a million
   vertices, what fraction of allocated slice space is wasted? Compare against a `Vec<u32>` that
   doubles, and against exact allocation.
3. The id-mapping table cannot be rehashed because the hash value *is* the internal id. Explain
   the power-of-two chain that works around it, and say what it costs on lookup.
4. Sampling across segments needs the alias method with degree-proportional segment selection.
   Show that this is equivalent to sampling uniformly from all of the vertex's edges, and say
   what breaks if you sample segments uniformly instead.
5. §7.3 names two things Redis lacks as an adjacency-list store. For each, sketch what you would
   add to a Redis-module graph engine, and estimate the memory you would save on a Zipf degree
   distribution.

## Done when

Answer each before unfolding it.

- [ ] You can state the single-server argument and its 80 GB arithmetic.

  <details><summary>Answer</summary>

  Twitter bet the whole graph fits in one server's RAM rather than partitioning it (§2.1): "we took
  exactly the opposite approach of scaling up on individual large-memory (but still commodity)
  servers." The arithmetic: a graph of ten billion edges, stored naïvely as an edge list at 8 bytes
  per edge, is "a mere 80 GB, which is well in the range of memory available on commodity servers."
  The payoff is that the hard problems become *fitting it in memory* (id narrowing, the doubling
  allocator) instead of distributed coordination — and the paper openly doubts distributed graph
  stores are as important as the literature treats them.

  </details>

- [ ] You can name all four generations and what killed each.

  <details><summary>Answer</summary>

  **Cassovary (2010)** — in-memory single server, HDFS snapshots, circle of trust + SALSA; killed by
  once-a-day snapshot freshness, so new users got nothing (cold start). **RealGraph on Hadoop
  (2012)** — richer behavioural signals, no longer memory-resident; killed by being batch, "roughly
  daily," dissonant with the live Twitter experience. **MagicRecs (2013)** — real-time push on
  edge arrival, its key move recasting "B→C edges in a time window" as an *intersection of adjacency
  lists*; limited to one hard-coded rule, ~7 s median latency dominated by message propagation.
  **GraphJet (2014)** — MagicRecs generalized into a real storage engine with a five-method API.

  </details>

- [ ] You can draw the index-segment picture and say what immutability buys.

  <details><summary>Answer</summary>

  A row of temporally-ordered segments; only the newest takes writes (single writer), the rest are
  read-only, and a segment older than *n* hours is discarded whole. Immutability buys three things
  (§3.3): pruning is coarse-grained and free (drop a segment, no per-edge expiry — "does not have a
  noticeable impact on recommendation quality"); only the one active segment needs write-optimized
  structures, so sealed segments can be relaid out for reads (Step 8); and each segment's bounded
  vertex set lets 64-bit ids collapse to small segment-internal ids (Step 5). The idea is borrowed
  from Earlybird.

  </details>

- [ ] You can write the edge-pool layout for an arbitrary degree.

  <details><summary>Answer</summary>

  Pool `P_r` holds slices of `2^r` edges; a vertex fills one slice per pool in order. A degree-`d`
  vertex spans `P_1 … P_k`, where `k` is the smallest integer with `2^{k+1} − 2 ≥ d` (cumulative
  capacity `2 + 4 + … + 2^k`), and the top slice has `2^{k+1} − 2 − d` free slots. Degree 25 →
  cumulative `2, 6, 14, 30`; `30 ≥ 25` at `k = 4`, so `P1..P4` with `30 − 25 = 5` free. The doubling
  is deliberate: it "implicitly assumes some type of preferential attachment effect," so more edges
  are expected precisely where edges already exist.

  </details>

- [ ] You can explain why single-writer removes the need for locks entirely.

  <details><summary>Answer</summary>

  All edge insertions come from one thread draining a Kafka queue, so there are no write–write
  conflicts to serialize — the only remaining concern is that readers see writes, which "judicious
  use of memory barriers is sufficient to address," and barriers are cheap enough that the penalty
  is acceptable. It works because a single writer sustains ~1,000,000 edges/s, well above the
  steady-state engagement rate, so one writer is never the bottleneck. Contrast topic 9: when you
  can make the writer singular, the entire latch hierarchy disappears.

  </details>

- [ ] You can quote §7.3's two gaps and connect them to capstone M42.

  <details><summary>Answer</summary>

  §7.3: Redis's `LPUSH` could hold adjacency lists, but "the implementation of the command ... lacks
  the memory allocation optimizations in GraphJet," and "Redis lacks a mechanism for pruning these
  lists; although it would be possible to implement temporal partitioning, it would basically be
  replicating some of the main design features in GraphJet." The two gaps are the doubling
  **allocator** (Step 6) and **temporal pruning via segments** (Step 4) — which is precisely the
  feature list capstone M42 asks you to add to a Redis-module graph store.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five questions live in `notes.md`'s guide-question checklist. The load-bearing ones: Q1 (no
  deletes is safe because interactions are point events that can't be undone; no timestamps is safe
  because window membership carries almost all the signal — both break on a workload with revocable
  edges or timestamp-sensitive scoring); Q3 (the id table can't rehash because the hash *is* the id,
  so it grows by a power-of-two chain and lookup probes each table in turn); Q4 (degree-proportional
  segment selection then uniform within-segment sampling equals uniform over all edges — sampling
  segments uniformly over-weights low-degree segments). Q2 and Q5 are measurements you run yourself.

  </details>

## References

- Sharma, Jiang, Bommannavar, Larson, Lin. *GraphJet: Real-Time Content Recommendations at
  Twitter.* PVLDB 9(13), 2016 — [PDF](http://www.vldb.org/pvldb/vol9/p1281-sharma.pdf).
- Gupta, Goel, Lin, Sharma, Wang, Zadeh. *WTF: The Who to Follow Service at Twitter.* WWW 2013 —
  the first generation, and Cassovary.
- Busch et al. *Earlybird: Real-Time Search at Twitter.* ICDE 2012 — where temporal index
  segments came from.
- Walker's alias method — O(1) sampling from a discrete distribution (topic 26).
- Topic 9 (concurrency) — what single-writer lets you delete; topic 12 (columnar) —
  sealed-segment relayout as compaction; topic 23 (full-text) — postings lists, same problem.
