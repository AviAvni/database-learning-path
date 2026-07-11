# Memgraph: skip lists, edge vectors, delta MVCC

memgraph is the "in-memory, pointer-rich, OLTP-first" corner of the
design space: no CSR anywhere. It shows what you get when you optimize
for concurrent mutation instead of scan bandwidth — and it reuses two
things you've already read: the lazy-locking skip list (topic 9) and
delta-chain MVCC (topic 8's N2O ordering). Focus: `src/storage/v2/`.

## 1. The vertex is the store

`src/storage/v2/vertex.hpp:32` — the whole per-node state in one
struct:

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

- `vertex.hpp:29` `Edges = small_vector<tuple<EdgeTypeId, Vertex*, EdgeRef>>`
  — each edge appears in BOTH endpoints' vectors (like neo4j's two
  chains, but contiguous per vertex). Expand = walk one vector:
  better locality than neo4j's scattered records, still not CSR — each
  `Vertex*` dereference is a fresh miss.
- `PointerPack<Delta, 2>` — the delta pointer with `kDeletedBit` and
  `kNonSeqDeltasBit` smuggled in the low bits (`:62-63`). Bit-packing
  ledger entry: flags in pointer alignment bits.
- Vertices live in a concurrent skip list keyed by Gid (topic 9's
  accessor/GC design) — the "table" is the skip list, no pages.

## 2. Delta MVCC (topic 8 cashed in)

`vertex.hpp:33-37` — the constructor asserts a new vertex starts with a
`DELETE_OBJECT` delta: memgraph stores the NEWEST version in place and
deltas UNDO backwards (N2O). A fresh vertex's undo is "didn't exist."
Readers walk `vertex.delta()` chains until they hit their snapshot;
old deltas are GC'd. Per-vertex `RWSpinLock` + delta chain = writers
don't block readers, exactly topic 8's design, at vertex granularity.

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

## 3. What this architecture buys / costs

```
                    memgraph              CSR/matrix engines
 add edge           push to 2 vectors     delta overlay + merge
 delete edge        swap-remove           tombstone (DM)
 expand 1 node      walk contiguous vec   slice (same-ish!)
 expand frontier    pointer soup          SpMV, streams
 memory             ptr-heavy, per-obj    offsets+targets, dense
 durability         snapshot + WAL        checkpoint matrices
```

The per-vertex edge vector is actually FINE for single-node expand —
it's contiguous. The loss is at frontier scale: 10K frontier nodes =
10K scattered vector headers + Vertex* targets that point anywhere.
No batch-level structure to stream.

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
