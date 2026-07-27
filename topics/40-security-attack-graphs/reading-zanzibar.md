# Zanzibar: authorization is a reachability query, at 10M QPS

Every application eventually grows an authorization system, and every one of them is a graph
database that nobody called a graph database. Zanzibar is Google's, extracted into one service:
more than two trillion relation tuples over ~100 TB, more than 10 million client queries per
second, 3.0 ms median for a Check, and availability above 99.999% for three years. The paper is
valuable to a database engineer for an unusual reason — the *core operation is a recursive graph
traversal*, so every technique in this book shows up under pressure: a denormalized
transitive-closure index with a galloping set intersection, cache-key canonicalization,
quantized snapshot timestamps so cache entries are shareable, a lock table against stampedes,
request hedging, and a consistency token invented specifically because a cache here is a security
bug. SpiceDB is the open-source implementation, and reading the two together is the fastest way
to see which parts of the paper are essential and which are Google-shaped.

## The problem in one sentence

**"Can user U do R to object O?" is `∃` a path from U to `O#R` through a graph of relation tuples
and rewrite rules — and it has to answer in single-digit milliseconds, at the head of every user
request, without ever using a stale ACL on new content.**

## The concepts, step by step

### Step 1 — Relation tuples: one row shape for everything

```
   ⟨tuple⟩   ::= ⟨object⟩ '#' ⟨relation⟩ '@' ⟨user⟩
   ⟨object⟩  ::= ⟨namespace⟩ ':' ⟨object_id⟩
   ⟨user⟩    ::= ⟨user_id⟩ | ⟨userset⟩
   ⟨userset⟩ ::= ⟨object⟩ '#' ⟨relation⟩
```

Four examples from Table 1:

```
   doc:readme#owner@10                    user 10 owns doc:readme
   group:eng#member@11                    user 11 is in group:eng
   doc:readme#viewer@group:eng#member     members of group:eng may view doc:readme
   doc:readme#parent@folder:A#...         doc:readme lives in folder:A
```

The third line is the whole design. The "user" slot can hold a *userset* — another
`object#relation` pair — so groups, group nesting, and ACL inheritance are all the same row shape.
There is no separate group table. Zanzibar's own summary: defining the model around tuples
"allows us to unify the concepts of ACLs and groups and to support efficient reads and
incremental updates". The primary key is `(shard ID, object ID, relation, user, commit
timestamp)` — note `commit timestamp` in the key: multiple versions live in different rows, which
is what makes snapshot reads at any timestamp within the GC window possible.

### Step 2 — Userset rewrite rules: three leaf types

Storing a tuple per object per relation would be wasteful and rigid, so relations are defined by
*rewrite rules* in a namespace config (Figure 1 in the paper):

```
   relation { name: "viewer"
     userset_rewrite { union {
       child { _this {} }                                   // stored tuples
       child { computed_userset { relation: "editor" } }     // every editor is a viewer
       child { tuple_to_userset {                            // and so is anyone who can
         tupleset { relation: "parent" }                     //   view the parent folder
         computed_userset { object: $TUPLE_USERSET_OBJECT
                            relation: "viewer" } } }
     } }
   }
```

Three leaf kinds and that is all:

- **`_this`** — the tuples actually stored for this `object#relation`.
- **`computed_userset`** — same object, different relation. ACL inheritance within an object
  ("editors are viewers").
- **`tuple_to_userset`** — the *arrow*. Follow a tupleset from this object (here `parent`), and
  for every tuple it returns, evaluate a relation on *that* object. This is what makes "inherit
  from the containing folder" one rule instead of a rewrite per hierarchy level.

Leaves combine with union, intersection and exclusion. In SpiceDB the whole tree is
`internal/graph/check.go:539` `checkUsersetRewrite` dispatching to `:567` `runSetOperation`, with
`:623` `checkComputedUserset` and `:699` `TraitsForArrowRelation` handling the two derived kinds.

### Step 3 — Check is a graph traversal, stated as one recursion

§3.2.3, and it is worth memorising:

```
   CHECK(U, ⟨object#relation⟩) =
       ∃ tuple ⟨object#relation@U⟩
     ∨ ∃ tuple ⟨object#relation@U'⟩, where U' = ⟨object'#relation'⟩ s.t. CHECK(U, U')
```

Either the user is here directly, or some userset stored here contains them, recursively. The
paper calls it "pointer chasing" and immediately names the failure mode: it "can be expensive
when indirect ACLs or groups are deep or wide".

Two implementation notes from the paper that SpiceDB mirrors exactly. Leaf nodes of the boolean
expression are evaluated **concurrently**, and "when the outcome of one node determines the result
of a subtree, evaluation of other nodes in the subtree is cancelled" — short-circuiting a
concurrent union. And reads are **pooled**: leaf evaluations for one Check are grouped to minimise
Spanner RPCs. SpiceDB's version is `dispatch/graph/graph.go:49`, `defaultConcurrencyLimit = 50`.

Measured in lane 3, on a chain of nested groups with 8 decoy sub-groups per level:

```
   nesting depth   tuple reads   check µs
               2            19       0.46
               4            55       0.99
               8           127       2.91
              16           271       5.68
              32           559      11.28
```

Linear in depth × width, exactly as the recursion predicts. That is the curve Leopard exists to
flatten.

### Step 4 — Leopard: the transitive closure as two sorted integer lists

§3.2.4. For namespaces with deep or wide group structure, Zanzibar precomputes membership into a
specialised index over "named sets" of `(T, s, e)` tuples — set type, set id, element id. Group
membership uses two set types:

```
   GROUP2GROUP(s) → {e}   e is a group that is directly OR indirectly a sub-group of s
   MEMBER2GROUP(u) → {e}  e is a group u is a DIRECT member of
```

and then the check is a set intersection:

```
   U ∈ G   ⟺   ( MEMBER2GROUP(U) ∩ GROUP2GROUP(G) ) ≠ ∅
```

The storage detail is the one that will feel familiar: "Index tuples are stored as ordered lists
of integers in a structure such as a skip list, thus allowing for efficient union and
intersections among sets. For example, evaluating the intersection between two sets, A and B,
requires only **O(min(|A|,|B|))** skip-list seeks."

That is topic 23's galloping intersect, doing authorization. Not `O(|A|+|B|)` — you iterate the
smaller set and *seek* into the larger, which is why the asymmetry between "this user is in three
groups" and "this group has 100,000 descendants" costs nothing. Lane 3 measures both against each
other on a lopsided pair: galloping finds a needle at the far end of a 500,000-element list in
tens of probes where a linear merge walks the whole way.

The indexed side of lane 3's table:

```
   nesting depth   index probes   index µs   index entries
               2              4       0.00             443
               8              8       0.01            1986
              32             12       0.01           11393
```

Flat in depth, ~1000× cheaper at depth 32, and the tax is visible in the last column: 6672 stored
tuples become 11393 index entries (1.7×), and the closure grows quadratically in chain depth.
Topic 1's RUM conjecture with a security label.

### Step 5 — What the index costs you: freshness

An offline pipeline reads periodic snapshots of the tuples, recursively expands the ACL graph,
and ships shards that Leopard servers hot-swap. Which means the index is *stale by construction*
and cannot serve a consistent read on its own. The fix is an incremental layer: Leopard's indexer
subscribes to Zanzibar's **Watch** API, receives a temporally ordered stream of tuple changes,
and transforms them into `(T, s, e, t, d)` index updates — `t` a timestamp, `d` a deletion
marker. At query time, updates with timestamps ≤ the query timestamp are merged on top of the
offline snapshot.

The cost of maintaining a transitive closure incrementally is exactly what you would fear: "a
single Zanzibar tuple addition or deletion may yield potentially tens of thousands of discrete
Leopard tuple events", because one group-to-group edge change re-parents an entire subtree. In
production the incremental layer writes ~500 index updates/sec at the median, ~1.5K at p99.

Performance: Leopard serves **1.56M QPS median / 2.22M p99**, responding in **under 150 µs at the
median and under 1 ms at p99**. Shards are usually served entirely from memory.

### Step 6 — Zookies and the new enemy problem

A cache is normally a latency decision. In authorization it is a correctness decision, and the
paper gives two concrete failures (§2.2):

```
   Example A — neglecting ACL update order
     1. Alice removes Bob from a folder's ACL
     2. Alice asks Charlie to move documents into that folder (docs inherit folder ACLs)
     3. Bob must not see the new documents — but will, if the check ignores the
        ordering between the two ACL changes

   Example B — misapplying an old ACL to new content
     1. Alice removes Bob from a document's ACL
     2. Alice asks Charlie to add new content to the document
     3. Bob must not see the new content — but will, if the check runs against a
        stale ACL from before his removal
```

Zanzibar's answer is not "read the latest" — that would mean a global round trip per check. It is
a protocol with the client. On a content change the client requests a **zookie**: an opaque token
encoding a global timestamp guaranteed to be ≥ every prior ACL write, stored atomically with the
content. Subsequent checks pass the zookie, and Zanzibar evaluates at *any* snapshot ≥ it. The
`≥` is the whole trick — the client pins a lower bound on freshness, and the server keeps its
freedom to pick a timestamp that is already replicated locally, which is what lets the vast
majority of checks be served without a cross-region round trip.

The traffic split is the proof it works: `Safe` requests (zookie more than 10 s old, servable
locally) run **about two orders of magnitude** more often than `Recent` ones, and the latency
table shows why that matters:

```
                    p50      p95      p99
   Check  Safe      3.0     9.46     15.0   ms
   Check  Recent    2.86    60.0     76.3   ms
   Write             127    233      401    ms
```

Same median, 4× the p95, because `Recent` often needs the leader replica.

### Step 7 — Hot spots: the frontier

§3.2.5 opens with "We found the handling of hot spots to be the most critical frontier in our
pursuit of low latency and high availability." Popular objects concentrate reads on one storage
server, and authorization traffic is bursty by nature (one search results page fires hundreds of
checks that share indirect ACLs). Four mechanisms:

1. **Distributed cache with consistent hashing** across the serving cluster, for both reads and
   *intermediate* check results, plus Slicer for hot-key distribution. Checks are forwarded by a
   key derived from the object id, so a check on `object#relation` and the reads of other
   relations on the same object land on the same server — "effectively forming cache trees".
2. **Timestamp quantization.** Evaluation timestamps are rounded up to a coarse granularity —
   one or ten seconds — so that "the vast majority of recent checks and reads to be evaluated at
   the same timestamps and to share cache results, despite having microsecond-resolution
   timestamps in cache keys". Rounding *up* is safe: Spanner will wait for TrueTime if the
   timestamp is in the future.
3. **A lock table** per server against the cache stampede: concurrent requests with the same
   cache key, only one proceeds, the rest block.
4. **Prefetching** all relation tuples of a detected hot object, and **delayed eager
   cancellation** when there are waiters on a lock-table entry.

The reported hit rates are the surprise. Checks: **10% on the delegate's cache**, 12% more saved
by the lock table; the delegator's cache hits under 2%. "While these hit rates appear very low,
they prevent **500K internal RPCs per second** from creating hot spots." A cache can be worth
building at a 10% hit rate if what it protects is a tail, not a mean.

### Step 8 — SpiceDB: which parts are essential

Reading the implementation tells you which of the above is Google-specific and which is inherent.
Inherent, all present in `internal/`:

- The recursion and the rewrite tree: `graph/check.go:99` → `:165` → `:304` → `:539` → `:567`.
- Set algebra over results: `graph/membershipset.go:122/:132/:156`
  (`UnionWith`/`IntersectWith`/`Subtract`) — but with a twist Zanzibar does not have. SpiceDB
  supports *caveats*, so a membership result can be conditional, and the set operations combine
  caveat expressions rather than booleans. `HasDeterminedMember` at `:216` is the "is this a
  definite yes" predicate.
- The reverse direction: `graph/lookupsubjects.go:430` `lookupViaTupleToUserset` — walking the
  arrow backwards to enumerate who can access something, which is Zanzibar's Expand.
- The cache: `dispatch/caching/caching.go:59`/`:156`, keyed by
  `dispatch/keys/computed.go:58` `checkRequestToKeyWithCanonical` — a `uint64` over a
  *canonicalized* relation expression, so two schemas that mean the same thing share cache
  entries. This is timestamp quantization's cousin: normalize the key so more requests collide.
- The lock table: `dispatch/singleflight/singleflight.go:47` is Zanzibar's §3.2.5 mechanism 3,
  named after the Go idiom.
- The fan-out bound: `dispatch/graph/graph.go:42` `ConcurrencyLimits`, default 50.

Google-specific: Spanner and TrueTime (SpiceDB runs on Postgres/CRDB/Spanner/MySQL and gets its
consistency from whichever), Leopard as a separate service, and Slicer.

## Where each step lives in the code

Repo: [`~/repos/spicedb`](https://github.com/authzed/spicedb) @ `8422483`, paths under `internal/`.

| step | anchor | what to read for |
|---|---|---|
| 2, 3 | `graph/check.go:99` `Check` | the entry point; note `ValidatedCheckRequest` at `:63` |
| 3 | `graph/check.go:165` `checkInternal` | one level of the recursion, with hint handling |
| 3 | `graph/check.go:304` `checkDirect` | the `_this` case — read the stored tuples |
| 2 | `graph/check.go:539/:567` | `checkUsersetRewrite` → `runSetOperation`: union / intersection / exclusion |
| 2 | `graph/check.go:623/:699` | `computed_userset` and the `tuple_to_userset` arrow |
| 3 | `graph/check.go:561` `dispatch` | where a subcheck becomes another (possibly remote) request |
| 4 | `graph/membershipset.go:41/:122/:132/:156` | set algebra with caveats; `:216` `HasDeterminedMember` |
| 4 | `graph/lookupsubjects.go:430`, `:253` | reverse traversal, including the intersection case |
| 7 | `dispatch/caching/caching.go:59/:156` | the check cache and its metrics |
| 7 | `dispatch/keys/computed.go:41/:58` | `DispatchCacheKey uint64` over a canonicalized expression |
| 7 | `dispatch/singleflight/singleflight.go:47` | the lock table |
| 3, 7 | `dispatch/graph/graph.go:42/:49/:274` | concurrency limits and the local dispatcher |
| — | `graph/limits.go:13` | `limitTracker` — bounding result counts on streaming lookups |

## Questions to answer in notes.md

1. Write the three rewrite leaf kinds and, for each, one policy you *cannot* express without it.
   Then say which one makes Check's cost depend on data rather than on schema.
2. Zanzibar reports a 10% cache hit rate for checks on the delegate side and calls it worthwhile.
   Reconcile that with the usual "a cache below 80% is not worth the complexity" instinct — what
   is the cache actually protecting, and what would you have to measure to know it is working?
3. Timestamp quantization rounds evaluation timestamps up to 1 or 10 seconds so cache keys
   collide. Explain why rounding *up* is safe and rounding *down* would be a correctness bug, in
   terms of the zookie's `≥` guarantee.
4. Lane 3 shows the index is flat in nesting depth and the tuple store is linear, at a 1.7× space
   cost. At what write rate does the incremental maintenance of the closure cost more than the
   reads it saves? Set up the arithmetic using the paper's ~500 updates/sec and 1.56M QPS.
5. SpiceDB's `MembershipSet` carries caveat expressions, so `IntersectWith` combines conditions
   rather than booleans. What does that do to the Leopard trick — can you still precompute a
   transitive closure when membership is conditional on request context?

## Done when

- [ ] You can write the relation tuple grammar from memory and explain why the user slot holds a
      userset.
- [ ] You can state the Check recursion and name the two leaf kinds that make it recursive.
- [ ] You can explain Leopard's two set types and why the intersection is `O(min(|A|,|B|))`.
- [ ] You can give both new-enemy examples and explain how a zookie prevents each.
- [ ] Your `authz.rs` reproduces lane 3: 19→559 tuple reads against 4→12 index probes across
      nesting depth 2→32, with the index agreeing with pointer chasing on every pair.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Pang, Cáceres, Burrows, Chen, Dave, Germer, Golynski, Graney, Kang, Kissner, Korn, Parmar,
  Richards, Wang. *Zanzibar: Google's Consistent, Global Authorization System.* USENIX ATC 2019 —
  [PDF](https://www.usenix.org/system/files/atc19-pang.pdf).
- Code: [authzed/spicedb](https://github.com/authzed/spicedb) — `internal/graph/`,
  `internal/dispatch/`.
- Corbett et al. *Spanner: Google's Globally-Distributed Database.* OSDI 2012 — TrueTime, which
  the zookie protocol is built on (topic 29).
- Local exercise stub: `topics/40-security-attack-graphs/experiments/authz.rs` — `check_pointer`,
  `LeopardIndex::build`, `intersect_galloping`.
- Topic 23 (full-text) — the galloping intersect; topic 1 (RUM) — the denormalization trade;
  topic 29 (distributed transactions) — external consistency and snapshot reads.
