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

## The problem in one sentence

Serve many concurrent transactions mutating the graph — edge inserts,
property updates, deletes — at in-memory OLTP latency, without readers
ever blocking on writers; the price is paid later, at traversal scale,
in pointer-chasing bandwidth.

## The concepts, step by step

### Step 1 — no pages, no CSR: the graph is a heap of vertex objects

memgraph represents each node as a plain heap-allocated C++ object
holding *everything* about that node — labels, both edge lists,
properties, a lock, a version-chain pointer — and the "table" is a
concurrent skip list (topic 9's lazy-locking accessor/GC design) keyed
by Gid (the node's global id). There is no page layout to respect, no
global read-optimized structure to rebuild on write: mutating node
42's state touches node 42's object, full stop. Why it matters: this
is the maximally write-friendly end of the topic's spectrum — every
other engine in this topic maintains some shared read-optimized
structure and therefore needs delta machinery; memgraph's "delta
machinery" is just... objects, plus MVCC (Step 4).

### Step 2 — the Vertex struct: the whole per-node state in one place

The entire chapter is one struct — every field is a design decision:

```cpp
struct Vertex {
  const Gid gid;
  utils::small_vector<LabelId, ...> labels;   // :41 inline until it spills
  Edges in_edges;                             // :43 small_vector of triples
  Edges out_edges;                            // :44
  PropertyStore properties;                   // :46 packed blob, not columns
  mutable utils::RWSpinLock lock;             // :47 per-vertex latch
  utils::PointerPack<Delta, 2> delta_;        // :66 MVCC chain head + 2 flag bits
};
```

Notes on the choices: `small_vector` stores its first few elements
*inline* in the struct (no heap allocation) and spills to the heap
only past that — a big win because power-law degree distributions mean
MOST nodes have few labels/edges. Properties are a packed per-node
blob, not columns — great for "load this node's properties", useless
for topic 12-style columnar filters. And `PointerPack<Delta, 2>`
smuggles two flag bits (`kDeletedBit`, `kNonSeqDeltasBit`, `:62-63`)
into the alignment bits of the delta pointer — the bit-packing ledger
again. Why it matters: one struct = one cache-line-friendly home for
the OLTP hot path; every access pattern beyond single-node suffers
for it.

### Step 3 — every edge is stored twice: per-endpoint vectors

Each edge appears in BOTH endpoints' vectors — `Edges` is a
`small_vector` of `(EdgeTypeId, Vertex*, EdgeRef)` triples
(`vertex.hpp:29`), so both "who do I point at?" (out_edges) and "who
points at me?" (in_edges) are answered locally, without a global
reverse index. Compare neo4j's two chains threading one shared record:
memgraph instead duplicates the entry but makes each copy *contiguous
per vertex*. Expand of one node = walk one contiguous vector — better
locality than neo4j's scattered records. The catch: each entry is a
16-byte triple whose `Vertex*` target points anywhere in the heap, so
the moment you *follow* the neighbors (2-hop, frontier), you're back
to a cache miss per hop:

```
 expand(A):      walk A's vector      — contiguous, prefetchable
 expand 10K frontier:  10K scattered vector headers
                       + Vertex* targets that point anywhere
```

Why it matters: "contiguous per vertex" is enough for OLTP-shaped
1-hop reads, and structurally incapable of the streaming that CSR
gives frontier-scale traversals — this one step is most of the
memgraph-vs-kuzu/FalkorDB performance story.

### Step 4 — MVCC by undo deltas (topic 8 cashed in)

memgraph keeps the NEWEST version of each vertex in place and hangs a
chain of **undo deltas** off it — each delta says how to reverse one
change (N2O ordering: newest-to-oldest, topic 8) — so a reader with an
older snapshot walks the chain backwards, undoing changes until the
state is old enough for its timestamp:

```rust
// N2O read: start from the newest (in-place) state and UNDO backwards
// until the chain is old enough for this reader's snapshot
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

The constructor even asserts a new vertex starts with a
`DELETE_OBJECT` delta (`vertex.hpp:33-37`) — a fresh vertex's undo is
"didn't exist." Old deltas are GC'd once no snapshot needs them.
Combined with the per-vertex `RWSpinLock`, writers never block readers
— exactly topic 8's design, at vertex granularity. Why it matters:
N2O bets that most readers are fresh (0 undo hops) — the right bet for
OLTP — and delta chains per *object* mean a hot vertex's history is
one locality-friendly chain rather than scattered version rows.

### Step 5 — the ledger: what this architecture buys and costs

Put the four steps against the CSR/matrix side of the topic:

```
                    memgraph              CSR/matrix engines
 add edge           push to 2 vectors     delta overlay + merge
 delete edge        swap-remove           tombstone (DM)
 expand 1 node      walk contiguous vec   slice (same-ish!)
 expand frontier    pointer soup          SpMV, streams
 memory             ptr-heavy, per-obj    offsets+targets, dense
 durability         snapshot + WAL        checkpoint matrices
```

The verdict the table encodes: single-object operations are memgraph's
home turf (no overlay, no merge, no rebuild — just object mutation
under a spinlock), and per-vertex expand is genuinely competitive
because the vector is contiguous. The losses are at frontier scale —
10K frontier nodes = 10K scattered headers, no batch-level structure
to stream — and in memory (16-byte triples with pointers vs 8-byte
offsets; per-object allocator overhead on top). Why it matters: this
is the cleanest existence proof in the topic that the mutation-vs-scan
tension is architectural, not an implementation detail — memgraph
simply picked the other end from FalkorDB.

## Where each step lives in the code

One file carries the whole chapter —
`src/storage/v2/vertex.hpp` in the [memgraph](https://github.com/memgraph/memgraph)
clone from topic 9:

- **Step 2** — the `Vertex` struct at `vertex.hpp:32`: labels `:41`,
  `in_edges`/`out_edges` `:43-44`, `properties` `:46`, `lock` `:47`,
  `delta_` `:66`; the smuggled flag bits at `:62-63`.
- **Step 3** — `vertex.hpp:29`:
  `Edges = small_vector<tuple<EdgeTypeId, Vertex*, EdgeRef>>`.
- **Step 4** — the `DELETE_OBJECT` constructor assertion at
  `vertex.hpp:33-37`; the delta types and GC are the topic-9 machinery
  reused (skip-list vertex store, accessor-based GC).

Read order: the struct top to bottom, pausing at each field to name
the decision it encodes — then re-read Step 5's table and check every
row against a field.

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

## References

**Code**
- [memgraph](https://github.com/memgraph/memgraph) (cloned for
  topic 9) — `src/storage/v2/vertex.hpp` is the whole chapter in one
  struct; the skip-list vertex store and delta GC are the topic-9
  machinery reused
