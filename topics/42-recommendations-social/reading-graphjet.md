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

Because the slice sizes are fixed and known, "we know from the vertex degree where to insert the
next edge and how much space is left in the current slice" — no per-vertex metadata beyond the
degree and the slot indices. Exercise 5 asks you to build it and measure bytes-per-edge against a
doubling `Vec`.

### Step 7 — One writer, no locks

"Since we adopt a single-writer, multi-reader design, there is no need to worry about write–write
conflicts." Edge insertions all come from one thread reading a Kafka queue; reads are served by
many threads; and "judicious use of memory barriers is sufficient to address memory visibility
issues across multiple threads. Memory barriers are sufficiently lightweight that the performance
penalties are acceptable."

One writer is enough because it can sustain a million edges a second, which is more than the
stream produces. The design is worth contrasting with everything topic 9 builds: when you can
make the writer singular, the entire latch hierarchy disappears.

### Step 8 — Sealed-segment relayout, and O(1) sampling

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

The recommendation algorithms are random walks on the bipartite graph. **Full SALSA**: start from
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

- [ ] You can state the single-server argument and its 80 GB arithmetic.
- [ ] You can name all four generations and what killed each.
- [ ] You can draw the index-segment picture and say what immutability buys.
- [ ] You can write the edge-pool layout for an arbitrary degree.
- [ ] You can explain why single-writer removes the need for locks entirely.
- [ ] You can quote §7.3's two gaps and connect them to capstone M42.
- [ ] You wrote answers to all five questions in notes.md.

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
