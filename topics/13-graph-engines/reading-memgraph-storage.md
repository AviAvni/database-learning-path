# Memgraph: skip lists, edge vectors, delta MVCC

memgraph is the "in-memory, pointer-rich, OLTP-first" corner of the
design space: no CSR anywhere. It shows what you get when you optimize
for concurrent mutation instead of scan bandwidth — and it reuses two
things you've already read: the lazy-locking skip list (topic 9) and
delta-chain MVCC (topic 8's N2O ordering). Before the code (focus:
`src/storage/v2/`), this chapter builds the design step by step — the
object-per-vertex model, the struct that holds everything, edges
stored twice, undo-delta MVCC, and the ledger of what all this buys
and costs.

Every anchor below is **memgraph pinned at `8f87f6a`**
([`resources/codebases.md`](../../resources/codebases.md)). This is
the one chapter in the topic where the source hands you a *hard*
number rather than an estimate — `vertex.hpp:73` is a `static_assert`
on `sizeof(Vertex)` — so Step 2 spends its arithmetic reconstructing
that number field by field. Doing so also breaks a claim the previous
version of this chapter made about `small_vector`; Step 3 shows the
line that breaks it.

## The problem in one sentence

Serve many concurrent transactions mutating the graph — edge inserts,
property updates, deletes — at in-memory OLTP latency, without readers
ever blocking on writers; the price is paid later, at traversal scale,
in pointer-chasing bandwidth.

## The concepts, step by step

### Step 1 — no pages, no CSR: the graph is a heap of vertex objects

> **In:** a node id (a `Gid`).
> **Out:** a pointer to a heap object holding that node's entire
> state — with no page, no slot, and no global adjacency structure in
> between.

memgraph represents each node as a plain heap-allocated C++ object
holding *everything* about that node — labels, both edge lists,
properties, a lock, a version-chain pointer — and the "table" is a
concurrent skip list (topic 9's lazy-locking accessor/GC design) keyed
by `Gid`, the node's global id (`id_types.hpp:56` defines `Gid` over
`uint64_t`).

There is no page layout to respect and no global read-optimized
structure to rebuild on write: mutating node 42's state touches node
42's object, full stop. Contrast the other two engines in this topic —
neo4j must place the record in a page and thread it into two chains;
FalkorDB must route the write into `delta_plus` and eventually rebuild
a matrix. Why it matters: this is the maximally write-friendly end of
the topic's spectrum — every other engine here maintains some shared
read-optimized structure and therefore needs delta machinery;
memgraph's "delta machinery" is just... objects, plus MVCC (Step 4).

### Step 2 — the Vertex struct: the whole per-node state in one place

> **In:** the 83-line header `src/storage/v2/vertex.hpp`.
> **Out:** the seven fields of `Vertex`, their individual sizes, and a
> reconstruction of the 80 bytes the file asserts they total.

The entire chapter is one struct, and every field is a design
decision:

```cpp
// src/storage/v2/vertex.hpp
    32  struct Vertex {
  //  ... 33-38: elided — the constructor and its MG_ASSERT, see Step 4 ...
    39    const Gid gid;
    40
    41    utils::small_vector<LabelId, memory::DbAwareAllocator<LabelId>> labels;
    42
    43    Edges in_edges;
    44    Edges out_edges;
    45
    46    PropertyStore properties;
    47    mutable utils::RWSpinLock lock;
  //  ... 48-60: elided — delta()/SetDelta()/deleted() accessors ...
    61   private:
    62    static constexpr int kDeletedBit = 0;
    63    static constexpr int kNonSeqDeltasBit = 1;
    64
    65    utils::PointerPack<Delta, 2> delta_;
    66  };
```

**Correction.** The previous version of this chapter placed `delta_`
at `:66`. Line 66 is the closing brace; `delta_` is at **`:65`**.

Ten lines further down the file states its own size, and this is the
one number in this topic that cannot drift without the build failing:

```cpp
// src/storage/v2/vertex.hpp
    72  static_assert(alignof(Vertex) >= 8, "The Vertex should be aligned to at least 8!");
    73  static_assert(sizeof(Vertex) == 80, "If this changes documentation needs changing");
```

Reconstruct the 80. Each size below is read from its own header, not
guessed:

| field | type | size | where the size comes from |
|---|---|---|---|
| `gid` | `Gid` | 8 | `id_types.hpp:56` — `Gid` wraps `uint64_t` |
| `labels` | `small_vector<LabelId, …>` | 16 | `small_vector.hpp:609-610` — `static_assert(sizeof(small_vector<int>) == 16)` |
| `in_edges` | `Edges` | 16 | same, `small_vector` is always 16 |
| `out_edges` | `Edges` | 16 | same |
| `properties` | `PropertyStore` | 12 | `property_store.hpp:193` — `std::array<uint8_t, sizeof(uint32_t) + sizeof(uint8_t*)>` = 4 + 8 |
| `lock` | `RWSpinLock` | 4 | `rw_spin_lock.hpp:113, 122` — one `uint32_t lock_status_` |
| `delta_` | `PointerPack<Delta, 2>` | 8 | one pointer, two flag bits stolen from its alignment |

```
 8 + 16 + 16 + 16 + 12 + 4 + 8 = 80 ✓  and 80 % 8 == 0, so alignof ≥ 8 holds
```

The `small_vector` is always 16 bytes regardless of element type
because of its layout:

```cpp
// src/utils/small_vector.hpp
   599    uint32_t size_{};                    // max 4 billion
   600    uint32_t capacity_{kSmallCapacity};  // max 4 billion
   601
   602    union {
   603      value_type *buffer_;
   604      uninitialised_storage<value_type, kSmallCapacity ? sizeof(value_type) : 1>
   605          small_buffer_[kSmallCapacity ? kSmallCapacity : 1];
   606    };
   607  };
  //  ... 608: elided ...
   609  static_assert(sizeof(small_vector<int>) == 16);
```

4 + 4 + 8 = 16, where the 8 is either a heap pointer *or* the inline
small buffer — never both. Note what that means: **the inline
capacity can never exceed 8 bytes' worth of elements**, because it
shares a union with a pointer.

Other choices worth naming while you're in the struct.
`PropertyStore` is a packed per-node blob (a 4-byte size plus an
8-byte pointer, `property_store.hpp:193`), not columns — great for
"load this node's properties", useless for topic 12-style columnar
filters. `PointerPack<Delta, 2>` smuggles two flag bits — `kDeletedBit`
and `kNonSeqDeltasBit` at `:62-63`, read through `deleted()` at `:53`
and `has_uncommitted_non_sequential_deltas()` at `:57` — into the
alignment bits of the delta pointer. That is the bit-packing ledger
again, the same move neo4j makes with its header byte
([reading-neo4j-record-store.md](reading-neo4j-record-store.md)
Step 3).

Why it matters: 80 bytes is one and a quarter 64-byte cache lines, so
one struct is very nearly one cache-line-friendly home for the OLTP
hot path — and every access pattern beyond single-node suffers for
what is *not* in those 80 bytes, namely any of the actual edges.

### Step 3 — every edge is stored twice: per-endpoint vectors

> **In:** one edge (type, source, target).
> **Out:** two entries — one in the source's `out_edges`, one in the
> target's `in_edges` — and a per-entry byte cost that decides whether
> the vector is inline or on the heap.

Each edge appears in BOTH endpoints' vectors, so "who do I point at?"
and "who points at me?" are answered locally, without a global reverse
index:

```cpp
// src/storage/v2/vertex.hpp
    29  using EdgeTriple = std::tuple<EdgeTypeId, Vertex *, EdgeRef>;
    30  using Edges = utils::small_vector<EdgeTriple, memory::DbAwareAllocator<EdgeTriple>>;
```

**Correction.** The previous version of this chapter called this "a
16-byte triple" (three times: in this step, in Step 5's ledger, and in
question 4). Size the three members from their own headers:

| member | type | size | source |
|---|---|---|---|
| `EdgeTypeId` | `uint32_t` wrapper | 4 | `id_types.hpp:59` |
| `Vertex *` | pointer | 8 | — |
| `EdgeRef` | `union { Gid gid; Edge *ptr; }` | 8 | `edge_ref.hpp:33-36`; `Gid` is `uint64_t` |

```
 4 + 8 + 8 = 20, rounded up to the 8-byte alignment of its widest member
 sizeof(EdgeTriple) = 24 bytes, not 16
```

**Correction, and it is the bigger one.** The previous version said
`small_vector` "stores its first few elements inline … a big win
because power-law degree distributions mean MOST nodes have few
labels/edges." That is true of `labels` and **false of edges**, and
one line says why:

```cpp
// src/utils/small_vector.hpp
   583    // kSmallCapacity can be 0; in that case we disable the small buffer
   584    constexpr static std::uint32_t kSmallCapacity = sizeof(value_type *) / sizeof(value_type);
  //  ... 585-592: elided ...
   593    constexpr static bool usingSmallBuffer(uint32_t capacity) {
   594      return kSmallCapacity != 0 && capacity == kSmallCapacity;
   595    }
```

The inline capacity is a pointer's worth of elements, integer-divided:

```
 labels:  value_type = LabelId    (4 B)  → kSmallCapacity = 8 / 4  = 2
          → 2 labels stored inline, no allocation
 edges:   value_type = EdgeTriple (24 B) → kSmallCapacity = 8 / 24 = 0
          → usingSmallBuffer() is constant false; the small buffer is
            disabled entirely, and EVERY non-empty edge vector is a
            separate heap allocation
```

So on the bench graph's degree distribution (p50 degree 11, max 6 565,
[notes.md](notes.md)) the p50 node's 11 out-edges are *not* inline —
they are one heap block of 11 × 24 = 264 bytes reached through
`buffer_`. Expanding one vertex is therefore two dependent loads
(vertex → buffer) and then a contiguous walk, not one.

Compare neo4j's two chains threading one shared record: memgraph
instead duplicates the entry but makes each copy *contiguous per
vertex*. That is still a real win — expand of one node walks one
contiguous array instead of neo4j's scattered chain — but the
`Vertex*` in each triple points anywhere in the heap, so the moment
you *follow* the neighbours you are back to a cache miss per hop:

```
 expand(A):      vertex → heap buffer → walk 24 B triples  — contiguous
 expand 10K frontier:  10K vertex objects (scattered)
                     + 10K heap buffers (scattered)
                     + Vertex* targets that point anywhere
```

Price the memory against CSR on this topic's graph (1 M nodes,
16.0 M directed edges, [notes.md](notes.md)):

```
 memgraph: each edge stored twice, 24 B per entry
           2 × 16.0e6 × 24 B = 768 MB of triples
           + 1e6 × 80 B of Vertex structs = 80 MB
           + one malloc header per non-empty edge vector
 CSR:      offsets (1e6+1) × 4 B + targets 16e6 × 4 B = 68 MB
           (reading-neo4j-record-store.md Step 1's comparison basis)
 ratio ≈ 848 MB / 68 MB = 12.5×
```

Why it matters: "contiguous per vertex" is enough for OLTP-shaped
1-hop reads, and structurally incapable of the streaming that CSR
gives frontier-scale traversals — this one step is most of the
memgraph-vs-kuzu/FalkorDB performance story, and the 12.5× memory
ratio is the other half of it.

### Step 4 — MVCC by undo deltas (topic 8 cashed in)

> **In:** a vertex object holding the *newest* state, and a reader
> with an older snapshot timestamp.
> **Out:** the view that reader is entitled to, reconstructed by
> undoing deltas backwards — at a cost proportional to how stale the
> reader is.

memgraph keeps the NEWEST version of each vertex in place and hangs a
chain of **undo deltas** off it. Each delta says how to reverse one
change (**N2O** = newest-to-oldest ordering, topic 8), so a reader
with an older snapshot walks the chain backwards, undoing changes
until the state is old enough for its timestamp:

```rust
// ILLUSTRATION — not memgraph source. The chain head is
// src/storage/v2/vertex.hpp:65 (`delta_`), read via `delta()` at :49;
// the delta actions are src/storage/v2/delta.hpp. This is the shape of
// the N2O walk those declarations imply.
fn read_vertex(v: &Vertex, snapshot_ts: u64) -> VertexView {
    let mut view = v.current_state();            // newest version, in place
    let mut d = v.delta_head();                  // PointerPack: flags in low bits
    while let Some(delta) = d {
        if delta.ts <= snapshot_ts { break; }    // committed before us: done
        delta.undo(&mut view);                   // ADD_LABEL undoes REMOVE, etc.
        d = delta.next();                        // older
    }
    view    // fresh readers pay 0 hops; laggards pay the chain — N2O's bet
}
```

The constructor asserts that a new vertex starts life with a
delete-shaped delta — a fresh vertex's undo is "didn't exist":

```cpp
// src/storage/v2/vertex.hpp
    33    Vertex(Gid gid, Delta *delta) : gid(gid), delta_(delta) {
    34      MG_ASSERT(delta == nullptr || delta->action == Delta::Action::DELETE_OBJECT ||
    35                    delta->action == Delta::Action::DELETE_DESERIALIZED_OBJECT,
    36                "Vertex must be created with an initial DELETE_OBJECT delta!");
    37    }
```

Note there are **two** accepted actions, not one: `DELETE_OBJECT` and
`DELETE_DESERIALIZED_OBJECT` — the second is the on-recovery path,
where the vertex was reconstructed from a snapshot rather than
created by a transaction. The assertion message only names the first,
which is exactly the kind of drift that makes reading the condition
rather than the message worth the habit.

Work N2O's bet on numbers. Suppose a hot vertex takes 1 000 updates
per second and the median reader's snapshot is 1 ms old:

```
 median reader: 1 000 updates/s × 0.001 s = 1 delta to undo
 a 100 ms-stale analytics reader: 1 000 × 0.1 = 100 deltas to undo
 a reader from 10 s ago:          1 000 × 10  = 10 000 deltas
```

N2O makes the fresh reader free and the stale reader linear in
staleness — the correct trade for OLTP, and the wrong one for long
analytical scans. Combined with the per-vertex `RWSpinLock`
(`vertex.hpp:47`, which is writer-friendly per its own doc comment at
`rw_spin_lock.hpp:24-26`), writers never block readers. Old deltas
are GC'd once no snapshot needs them — the topic-9 accessor machinery,
reused. Why it matters: delta chains per *object* mean a hot vertex's
history is one locality-friendly chain rather than version rows
scattered across a heap.

### Step 5 — the ledger: what this architecture buys and costs

> **In:** the four steps above.
> **Out:** a row-by-row comparison against the CSR/matrix side of the
> topic, with every row traceable to a field or a line.

```
                     memgraph                       CSR/matrix engines
 add edge           push to 2 vectors (24 B each)   delta overlay + merge
 delete edge        swap-remove from 2 vectors      tombstone (DM)
 expand 1 node      vertex → buffer → contig. walk  slice (one indirection fewer)
 expand frontier    pointer soup                    SpMV, streams
 memory / edge      2 × 24 B = 48 B                 4 B (CSR targets)
 per-node overhead  80 B Vertex + malloc headers    4 B offsets entry
 durability         snapshot + WAL                  checkpoint matrices
```

The verdict the table encodes: single-object operations are memgraph's
home turf — no overlay, no merge, no rebuild, just object mutation
under a spinlock — and per-vertex expand is genuinely competitive
because the buffer is contiguous. The losses are at frontier scale
(10 K frontier nodes = 10 K scattered vertex objects *plus* 10 K
scattered heap buffers, with no batch-level structure to stream) and
in memory, where the ratio is the 12× computed in Step 3.

Tie it to the topic's headline. The 101× supernode penalty
([FINDINGS.md](../../FINDINGS.md) row 13) was measured on an
adjacency-list oracle whose per-node neighbours are contiguous — that
is memgraph's shape, not neo4j's. So the headline is roughly the
*best case* for this architecture: even with contiguous per-vertex
edges, a two-hop from supernodes costs 495 378 ns against 4 914 ns
from random nodes. Normalised per query that is 78 907 distinct nodes
reached against 1022 — 77× more work — at 6.28 against 4.81 ns per node
reached, so 1.31× of the gap is not explained by volume. Nothing in
Step 1–4's design attacks that residual; only a set-structured
representation (CSR with a visited bitmap, or a masked SpMV) can.

Why it matters: this is the cleanest existence proof in the topic
that the mutation-vs-scan tension is architectural, not an
implementation detail — memgraph simply picked the other end from
FalkorDB.

## Where each step lives in the code

[memgraph](https://github.com/memgraph/memgraph) pinned at `8f87f6a`
(the clone from topic 9). `src/storage/v2/vertex.hpp` is 83 lines and
carries most of the chapter.

| Step | Anchor | What is there |
|---|---|---|
| 1 | `src/storage/v2/id_types.hpp:56-59` | `Gid`=uint64_t, `LabelId`/`PropertyId`/`EdgeTypeId`=uint32_t |
| 2 | `src/storage/v2/vertex.hpp:32` | `struct Vertex` opens |
| 2 | `src/storage/v2/vertex.hpp:39, 41, 43-44, 46-47, 65` | the seven fields (`delta_` is `:65`, not `:66`) |
| 2 | `src/storage/v2/vertex.hpp:53, 57, 62-63` | the two smuggled flag bits and their accessors |
| 2 | `src/storage/v2/vertex.hpp:72-73` | `alignof(Vertex) >= 8`, `sizeof(Vertex) == 80` |
| 2 | `src/storage/v2/property_store.hpp:193` | `PropertyStore` is a 12-byte packed blob handle |
| 2 | `src/utils/rw_spin_lock.hpp:19-26, 113, 122` | the lock's doc, its `uint32_t` status, the member |
| 2, 3 | `src/utils/small_vector.hpp:599-610` | the 16-byte layout and its `static_assert` |
| 3 | `src/storage/v2/vertex.hpp:29-30` | `EdgeTriple` and `Edges` |
| 3 | `src/storage/v2/edge_ref.hpp:22, 33-36` | `EdgeRef` is a `Gid`/`Edge*` union — 8 B |
| 3 | `src/utils/small_vector.hpp:583-584, 593-595` | `kSmallCapacity` = 8/sizeof(T); 0 disables the small buffer |
| 3 | `src/storage/v2/vertex.hpp:68-70` | `kEdgeTypeIdPos`/`kVertexPos`/`kEdgeRefPos` — how the tuple is unpacked |
| 4 | `src/storage/v2/vertex.hpp:33-37` | the constructor's two accepted initial delta actions |
| 4 | `src/storage/v2/vertex.hpp:49, 51` | `delta()` / `SetDelta()` — the chain head accessors |

Read order: `vertex.hpp` top to bottom, pausing at each field to name
the decision it encodes; then jump to `small_vector.hpp:583-584` and
work out `kSmallCapacity` for `LabelId` and for `EdgeTriple` before
reading on; then re-read Step 5's table and check every row against a
field.

## Questions (answer in notes.md)

1. Why must an edge live in both endpoints' vectors? What query breaks
   with out-only? What does FalkorDB maintain instead (see
   Delta_Matrix transposed trio)?
2. `small_vector` inlines a few elements before heap-spilling. Which
   degree distribution fact (power law) makes this a big win?
3. Delta chains are per-OBJECT here, per-VERSION-ROW in postgres.
   Which is better for a graph supernode under concurrent edge
   inserts, and why?
4. memgraph's Expand of one vertex vs kuzu's CSR slice: both
   contiguous. Where does kuzu still win? (Hint: what's IN the vector —
   16-byte triples with a pointer vs 8-byte offsets.)
5. Sketch what an analytics query (PageRank) costs on this layout vs a
   matrix. Where does the memory bus time go?

> Question 2 and question 4 both encode the mistake Step 3 corrects.
> Answer 2 for `labels` (where the premise holds, `kSmallCapacity` = 2)
> and then say why it fails for edges; answer 4 with the real
> `sizeof(EdgeTriple)` = 24, not 16.

## Done when

Answer each before unfolding it.

- [ ] You can draw the Vertex struct, say what per-node state lives in one place, and reconstruct its asserted size.

  <details><summary>Answer</summary>

  Seven fields (`vertex.hpp:39-65`): `gid`, `labels`, `in_edges`,
  `out_edges`, `properties`, `lock`, `delta_`. Everything about a node
  — identity, labels, both adjacency directions, properties, its
  latch, and its version chain — is reachable from one pointer.

  `vertex.hpp:73` asserts `sizeof(Vertex) == 80`, and it checks out:
  8 (`Gid`, `id_types.hpp:56`) + 16 + 16 + 16 (three `small_vector`s,
  `small_vector.hpp:609`) + 12 (`PropertyStore`,
  `property_store.hpp:193`) + 4 (`RWSpinLock`'s `uint32_t`,
  `rw_spin_lock.hpp:113`) + 8 (`PointerPack`) = 80.
  </details>

- [ ] You can explain why every edge is stored twice, which query shape breaks if it is not, and what an entry actually costs.

  <details><summary>Answer</summary>

  Both `in_edges` and `out_edges` are stored per vertex
  (`vertex.hpp:43-44`), so an edge appears in two vectors. Without
  `in_edges`, any pattern that traverses backwards —
  `MATCH (a)<-[:FOLLOWS]-(b)` — would need a global reverse index or
  a full scan. FalkorDB's answer to the same requirement is the
  optional transposed matrix in `struct _Delta_Matrix`
  (`delta_matrix.h:113`), which is one bit per edge rather than a
  second copy of the entry.

  Cost per entry: `EdgeTriple` = `EdgeTypeId` (4) + `Vertex*` (8) +
  `EdgeRef` (8) → 24 B after alignment, so 48 B of adjacency per
  logical edge, versus 4 B per directed edge in a CSR `targets`
  array.
  </details>

- [ ] You can state the difference between per-object delta chains here and per-version rows in postgres, and what each makes cheap.

  <details><summary>Answer</summary>

  memgraph hangs one chain of undo deltas off each *object*
  (`vertex.hpp:65`), newest state in place, N2O. Postgres writes a new
  *row version* per update and leaves the old one in the heap page,
  O2N.

  Per-object chains make a hot object cheap to read fresh (0 hops) and
  keep its history in one place — good for a supernode taking
  concurrent edge inserts, where all the contention is on one vertex.
  Per-version rows make *scans* cheap (no chain to walk; visibility is
  a per-tuple test) and updates cheap to abort, but scatter a hot
  row's history across pages.

  The price of N2O is stated by the arithmetic in Step 4: a reader
  that is Δt stale on an object updated at rate r pays r·Δt undo
  hops. Fresh readers pay nothing; a 10-second-stale analytics reader
  on a 1 000 update/s vertex pays 10 000.
  </details>

- [ ] You can compare the cost of expanding one vertex here against kuzu's CSR slice, and say which workload each layout is built for.

  <details><summary>Answer</summary>

  memgraph: load the `Vertex` (80 B), read `out_edges`' `buffer_`
  pointer, then walk *n* × 24 B triples in the heap block. Two
  dependent loads before the walk starts, because `kSmallCapacity` for
  `EdgeTriple` is 8/24 = 0 and the small buffer is disabled
  (`small_vector.hpp:583-584`) — so the block is never inline.

  CSR: `offsets[i]` and `offsets[i+1]` are two adjacent 4-byte reads
  in one array, then one contiguous slice of 4-byte ids. Six times
  less adjacency data per edge, one indirection fewer, and — the real
  win — *neighbouring nodes' slices are adjacent*, so a frontier scan
  streams while memgraph's frontier scan visits a fresh heap block per
  node.

  memgraph is built for concurrent single-object mutation; CSR is
  built for frontier-scale traversal. Neither is a bug.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including the PageRank cost sketch.

  <details><summary>Answer</summary>

  PageRank is *n* rounds of "for every edge, add source rank / source
  degree to target". On this layout each round visits 1 M scattered
  `Vertex` objects, dereferences 1 M scattered heap buffers, and then
  chases 16 M `Vertex*` pointers to scattered destinations — every one
  of them a dependent load the prefetcher cannot issue early. The bus
  time goes into latency, not bandwidth: the machine is idle waiting.

  As a matrix it is `r ← A^T r`, one SpMV per round, over 68 MB of
  contiguous CSR — the bus time goes into bandwidth, which is the
  resource you can actually saturate. That is the same argument as
  [reading-graphblas-internals.md](reading-graphblas-internals.md)
  Step 1, arriving from the other direction.
  </details>

## References

**Papers**
- Diaconu et al. — "Hekaton: SQL Server's Memory-Optimized OLTP Engine"
  (SIGMOD 2013) and Wu, Arulraj, Lin, Xian, Pavlo — "An Empirical
  Evaluation of In-Memory Multi-Version Concurrency Control" (VLDB 2017)
  — both read in topic 8; they taxonomize exactly the choices Step 4
  shows memgraph making (N2O ordering, delta vs. full-copy version
  storage, GC strategy) — place memgraph in Wu/Pavlo's 5-axis table

**Code** (all line numbers verified at memgraph `8f87f6a`)

| File | Lines | What |
|---|---|---|
| `src/storage/v2/vertex.hpp` | 29-30 | `EdgeTriple`, `Edges` |
| `src/storage/v2/vertex.hpp` | 32-37 | the struct and its constructor assertion (two accepted actions) |
| `src/storage/v2/vertex.hpp` | 39-47, 65 | the seven fields |
| `src/storage/v2/vertex.hpp` | 49-59, 62-63 | delta accessors and the two packed flag bits |
| `src/storage/v2/vertex.hpp` | 68-70, 72-73 | tuple positions; `sizeof(Vertex) == 80` |
| `src/storage/v2/edge_ref.hpp` | 22, 33-36 | the `Gid`/`Edge*` union |
| `src/storage/v2/id_types.hpp` | 56-59 | id widths |
| `src/storage/v2/property_store.hpp` | 193 | the 12-byte property handle |
| `src/utils/small_vector.hpp` | 30-42 | the ASCII layout diagram |
| `src/utils/small_vector.hpp` | 583-584, 593-595 | `kSmallCapacity`, and when the small buffer is disabled |
| `src/utils/small_vector.hpp` | 599-610 | fields and the 16-byte `static_assert` |
| `src/utils/rw_spin_lock.hpp` | 19-26, 113, 122 | writer-friendly RW spinlock in one `uint32_t` |
