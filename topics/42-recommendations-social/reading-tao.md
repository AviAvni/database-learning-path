# TAO: two shapes, five queries, and a 96.4% hit rate

Pixie and GraphJet answer "what should I show this person?". TAO answers the much less glamorous
question underneath it — "what edges does this node have?" — for every page view on Facebook. It
is the paper to read when you want to know what a social graph *store* actually has to do, as
opposed to what a graph database markets. The striking thing is how little of it is graph theory.
The data model is two shapes. The query API is five calls. Everything else is caching, and the
caching is entirely shaped by two measured facts about social data: most of it is old but most
queries want the newest, and reads outnumber writes 25 to 1.

## The problem in one sentence

**Serve the edges of a constantly-changing, tightly-interconnected graph to every page render in
every data centre, at a latency budget of about a millisecond, where a single page may touch
hundreds of objects and associations.**

## The concepts, step by step

### Step 1 — Why not memcache over MySQL

Facebook already had a lookaside cache. TAO exists because three specific things went wrong with
it, and each is a general lesson:

- **Inefficient edge lists.** "A key-value cache is not a good semantic fit for lists of edges;
  queries must always fetch the entire edge list and changes to a single edge require the entire
  list to be reloaded." Basic list support fixes only the first problem; concurrent incremental
  updates to cached lists need much more.
- **Distributed control logic.** In a lookaside architecture the control logic runs on clients
  that cannot talk to each other, which "increases the number of failure modes, and makes it
  difficult to avoid thundering herds". Moving control into the cache lets one coordinator solve
  it.
- **Expensive read-after-write consistency.** Asynchronous master/slave replication means a write
  is not immediately visible in a replica's region. Restricting the data model to objects and
  associations lets the replica's cache be updated at write time and lets graph semantics
  interpret cache-maintenance messages — read-after-write for all clients of a cache, without
  inter-regional communication.

The pattern: **a narrower data model buys you consistency and control you cannot get generically.**

### Step 2 — Objects and associations

```
   Object:  (id)              → (otype, (key → value)*)
   Assoc:   (id1, atype, id2) → (time,  (key → value)*)
```

64-bit ids, unique across all object types. At most one association of a given type between any
two objects. A per-type schema lists the allowed keys. And "each association has a 32-bit time
field, which plays a central role in queries" — remember that; it is the whole of Step 4.

The modelling guidance is worth keeping: "Actions may be encoded either as objects or
associations. ... Associations naturally model actions that can happen at most once or record
state transitions, such as the acceptance of an event invitation, while repeatable actions are
better represented as objects." Cathy's comment is an object (she may comment again); David's
'like' is an association (he can only like it once).

Bidirectionality is handled by declaring an **inverse type**: creations, updates and deletions are
automatically coupled with the inverse operation. Not atomically, though — "TAO does not provide
atomicity between the two updates. If a failure occurs the forward may exist without an inverse;
these *hanging* associations are scheduled for repair by an asynchronous job." A production
system choosing eventual repair over distributed transactions, explicitly.

### Step 3 — The query API, in full

```
   assoc_get(id1, atype, id2set, high?, low?)
   assoc_count(id1, atype)
   assoc_range(id1, atype, pos, limit)
   assoc_time_range(id1, atype, high, low, limit)
```

Plus three writes (`assoc_add`, `assoc_delete`, `assoc_change_type`) and the object CRUD. That is
the entire interface. Note what is *not* there: no multi-hop traversal, no pattern matching, no
path queries. TAO's stated goal is "not to support a complete set of graph queries, but to provide
sufficient expressiveness to handle most application needs while allowing a scalable and
efficient implementation."

A per-atype upper bound caps the limit, "typically 6,000". To walk a longer list, clients page
with `pos` or `high`.

### Step 4 — Creation-time locality, and why lists are newest-first

The organising principle:

> **Association List**: `(id1, atype) → [a_new … a_old]`
>
> A characteristic of the social graph is that most of the data is old, but many of the queries
> are for the newest subset. This *creation-time locality* arises whenever an application focuses
> on recent items.

So association lists are stored in descending time order, and the cache holds *contiguous
prefixes* of them. Everything downstream follows from that one representation choice:

- `assoc_range(id1, atype, 0, 50)` — "50 most recent comments" — is a prefix read.
- The optional time bounds on `assoc_get` exist "to improve cacheability for large association
  lists".
- And the invalidation rule has to change (Step 6).

### Step 5 — The caching hierarchy

Three layers, each solving a different problem:

```
   clients
      │  (all-to-all, out-of-order multiplexed protocol —
      │   TAO may hit the DB, so head-of-line blocking is fatal)
      ▼
   FOLLOWER tier(s)   ← clients only ever talk to these
      │  forward read misses and ALL writes to the leader
      ▼
   LEADER tier        ← one per region; serializes writes per shard,
      │                 protects the DB from thundering herds
      ▼
   MySQL shards       ← objects in one table, associations in another,
                        every association on the shard of its id1
```

Sharding: "An association is stored on the shard of its `id1`, so that every association query can
be served from a single server." That is the single most important physical-design decision in
the paper, and it is why the API has no multi-hop traversal — one hop, one server.

Two tiers rather than one because a single tier "is more prone to hot spots and they have a
quadratic growth in all-to-all connections".

Geographically: master/slave regions, because "read misses by followers are 25 times as frequent
as writes in our workloads" so reads must be local even if writes cross an ocean. Write latency in
the master's region is **12.1 ms**; from a region 58 ms away it is **74.4 = 58.1 + 16.3 ms** — the
round trip, plus the work.

### Step 6 — Refill, not invalidate

The subtle consequence of caching prefixes:

> Since we cache only contiguous prefixes of association lists, invalidating an association might
> truncate the list and discard many edges. Instead, the leader sends a *refill* message to notify
> followers about an association write. If a follower has cached the association, then the refill
> request triggers a query to the leader to update the follower's now-stale association list.

Invalidate a prefix and the follower loses everything after the invalidated edge, then has to
re-fetch it all. Refill sends the update instead. A cache-coherence protocol designed around the
*shape* of the cached value — worth remembering next time you reach for a blanket invalidate.

### Step 7 — Hot spots: cloning and client-side caching

Shards map to cache servers by consistent hashing, which "can lead to load imbalance: some
followers will shoulder a larger portion of the request load". Two mechanisms:

- **Shard cloning**: reads to a hot shard are served by *multiple* followers in a tier, with
  consistency messages sent to all of them.
- **Client-side caching with an access-rate threshold**: "In our workloads, it is not uncommon for
  a popular object to be queried orders of magnitude more often than other objects." When a
  follower answers, it includes the item's access rate; above a threshold the client caches the
  data and a version number, and subsequent replies can omit the data if unchanged.

Compare with topic 40's Zanzibar, which solves the same problem with consistent hashing, a lock
table and timestamp quantization. Same disease, different medicine.

### Step 8 — Memory engineering

TAO's cache is a slab allocator with LRU and a dynamic slab rebalancer, partitioned into
**arenas** by object or association type — "This allows us to extend the cache lifetime of
important types, or to prevent poor cache citizens from evicting the data of better-behaved
types."

And the optimization that shows how tight the budget is: for small fixed-size items such as
association counts, pointer overhead in a hash table becomes significant, so they live in
direct-mapped 8-way associative caches with no pointers at all.

> This lets us map (id1, atype) to a 32-bit count in 14 bytes; a negative entry, which records the
> absence of any id2 for an (id1, atype), takes only 10 bytes. This optimization allows us to hold
> about 20% more items in cache for a given system configuration.

14 bytes for a cached count. Read that as the standard for how seriously to take per-entry
overhead in a cache holding a social graph.

### Step 9 — What the workload actually looks like

Two distributions from the production trace, both with long tails you must design for:

- `assoc_count`: **1% of returned counts were ≥512K**. High-degree nodes are not hypothetical.
  (§5.4: "Many objects have more than 6,000 associations with the same atype emanating from
  them", so TAO treats those specially.)
- `assoc_range` / `assoc_time_range`: **64% of non-empty results had exactly 1 edge**, and 13% of
  those had a limit of 1. Meanwhile "12% of the queries had limit = 1, but 95% of the remaining
  queries had limit ≥ 1000. Less than 1% of the return values for queries with a limit ≥ 1
  actually reached the limit."

So the common case is a single edge and the requested limit is usually enormous and almost never
reached. Both facts have to be true of your test workload or your benchmark is fiction.

Data sizes: average association payload 97.8 bytes (for the 60.5% that carry data at all);
average object payload 673 bytes; **39.5% of associations queried contained no data**.

### Step 10 — The performance envelope

144 GB RAM, 2× 8-core Xeon E5-2660 at 2.2 GHz, 10 GbE. Client-observed, including network and the
PHP client stack:

| operation | hit p50 | hit avg | hit p99 | miss p50 | miss avg | miss p99 |
|---|---|---|---|---|---|---|
| `assoc_count` | 1.1 | 2.5 | 28.9 | 5.0 | 26.2 | 186.8 |
| `assoc_get` | 1.0 | 2.4 | 25.9 | 5.8 | 14.5 | 143.1 |
| `assoc_range` | 1.1 | 2.3 | 24.8 | 5.4 | 11.2 | 93.6 |
| `assoc_time_range` | 1.3 | 3.2 | 32.8 | 5.8 | 11.9 | 47.2 |
| `obj_get` | 1.0 | 2.4 | 27.0 | 8.2 | 75.3 | 186.4 |

(milliseconds.) Overall read hit rate **96.4%**; peak follower throughput rises from ~350K to
~600K requests/s as hit rate goes 85% → 99%. Availability: over 90 days the fraction of failed
queries was **4.9 × 10⁻⁶**. Replication lag: slaves are within 1 s of the master 85% of the time,
within 3 s 99%, within 10 s 99.8%.

Note the hit/miss gap. A hit is ~1 ms and a miss is 5–8 ms at p50 and up to 186 ms at p99. At a
96.4% hit rate the *average* is fine and the tail is entirely made of misses — which is why every
mechanism in Steps 6–8 is about protecting the hit rate rather than making misses faster.

## How to read the paper (with the concepts in hand)

- **§2.1.** The three failures of lookaside caching. Each is a lesson; name them before moving on.
- **§3.1 + Figure 1.** Objects and associations, the 32-bit time field, and the
  actions-as-objects-or-associations guidance.
- **§3.3.** Inverse types and hanging associations. Note the deliberate non-atomicity.
- **§3.4.** The four association queries and the association-list definition. This is the core.
- **§4.1–4.2.** Sharding by `id1`; the leader/follower split and why two tiers.
- **§4.4.** Refill versus invalidate. Read the sentence about truncating prefixes twice.
- **§4.5 + Figure 2.** Master/slave regions and the 25×-reads-to-writes justification.
- **§5.1.** Arenas, slabs, and the 14-byte association count.
- **§5.3–5.4.** Shard cloning, client-side caching with access rates, high-degree objects.
- **§7 + Figures 4–6.** The workload distributions. The 1%-of-counts-≥512K and
  64%-return-one-edge facts are the ones to keep.
- **§8 + Figures 7–9.** Throughput vs hit rate, the latency table, write latency across regions.
- **After the paper.** Do exercise 7: implement `assoc_range` over a time-ordered list with a
  cached prefix, then implement invalidation *and* refill, and construct the case where
  invalidation throws away edges refill would have kept.

## Questions to answer in notes.md

1. TAO's API has no multi-hop traversal. Name the physical-design decision that makes this a
   feature rather than a limitation, and say what would have to change to support two hops
   efficiently.
2. Refill instead of invalidate. Construct a concrete case — an association list of 1,000 edges,
   a write in the middle — and count the edges lost and re-fetched under each policy.
3. 64% of non-empty range results return exactly one edge, while 95% of queries ask for ≥1000.
   What does that tell you about how the clients were written, and what would you change in the
   API to make the common case cheaper?
4. Association counts are cached in 14 bytes with no pointers, buying 20% more cache entries.
   Estimate the equivalent saving for a graph engine that stores per-node degree as a property in
   a general key–value property map. Is 20% worth a special case?
5. Compare TAO's hot-spot handling (shard cloning, client-side caching with access rates) with
   Zanzibar's (consistent-hashed distributed cache, lock table, timestamp quantization) from topic
   40. Which mechanisms are equivalent, which are unique, and what does the difference say about
   the two workloads?

## Done when

- [ ] You can write the two data shapes and the four association queries from memory.
- [ ] You can define creation-time locality and say what it forces about list ordering and caching.
- [ ] You can explain refill-versus-invalidate and why prefix caching demands it.
- [ ] You can quote the 25×-reads-to-writes ratio and say which design decisions follow from it.
- [ ] You can give the hit/miss latency gap and explain why the tail is made of misses.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Bronson, Amsden, Cabrera, Chakka, Dimov, Ding, Ferris, Giardullo, Kulkarni, Li, Marchukov,
  Petrov, Puzar, Song, Venkataramani. *TAO: Facebook's Distributed Data Store for the Social
  Graph.* USENIX ATC 2013 —
  [PDF](https://www.usenix.org/system/files/conference/atc13/atc13-bronson.pdf).
- Nishtala et al. *Scaling Memcache at Facebook.* NSDI 2013 — the lookaside architecture TAO
  replaced, and the source of leases and remote markers.
- Topic 6 (buffer pool) — slab allocation, LRU, arenas; topic 36 (sharding) — shard-by-`id1` and
  cloning versus migration; topic 40 (Zanzibar) — the same hot-spot problem, different medicine;
  topic 28 (cloud-native) — caching trees over a shared storage layer.
