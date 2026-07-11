# Reading guide — neo4j's record store (the cost of index-free adjacency)

Repo: `~/repos/neo4j` (shallow clone). Java, but you're reading data
layout, not code style. Everything lives under
`community/record-storage-engine/src/main/java/org/neo4j/kernel/impl/store/`.

## Why this matters

neo4j is the architecture FalkorDB most directly positions against.
"Index-free adjacency" = neighbors are direct pointers, no index
lookup. The bet made sense on 2010 spinning disks (seek = 10 ms, so
any pointer beats a B-tree descent). On DRAM it inverts: a pointer
chase is a ~110 ns cache miss (topic 0), a CSR slice is a prefetchable
stream.

## 1. Fixed-size records

`format/standard/NodeRecordFormat.java:32`:

```java
public static final int RECORD_SIZE = 15;
```

`format/standard/RelationshipRecordFormat.java:35`:

```java
public static final int RECORD_SIZE = 34;
```

Fixed size ⇒ `record address = id * RECORD_SIZE` — the store IS the
index. Read the `readRecord` methods in both files to see the layouts:

```
 Node (15 B):  inUse | nextRel(35b) | nextProp(36b) | labels(40b) | flags
 Rel  (34 B):  inUse | firstNode | secondNode | type
               | firstPrevRel | firstNextRel      ← chain @ first node
               | secondPrevRel | secondNextRel    ← chain @ second node
               | nextProp
```

35-bit pointers: high bits are smuggled into the inUse byte — the
bit-packing ledger again (compare postgres's tuple header, topic 8).

## 2. Relationship chains

`record/RelationshipRecord.java:39-44` — each relationship sits on TWO
doubly-linked lists simultaneously (one per endpoint):

```
 node A ──nextRel──> rel1 ──firstNextRel──> rel4 ──> rel9 ──> NULL
                      │
 node B ──nextRel────rel1 ──secondNextRel─> rel2 ──> ...
```

Expand(A) = walk A's chain: **one 34-byte record read — one potential
cache/page miss — per edge**. The records for one node's chain are
scattered wherever insertion order put them; there is no locality
guarantee. A supernode with 100K edges = 100K dependent loads.
Contrast CSR: `targets[offsets[i]..offsets[i+1]]` — one range,
hardware prefetcher does the rest.

Also note the chain problem neo4j itself acknowledges: deleting a
relationship must unlink from BOTH chains (up to 4 neighbor records
touched), and finding a specific relationship between two nodes means
walking the shorter chain (they store degree for "dense" nodes to pick
the side — see `RelationshipGroup` records).

## 3. Where records WIN

Be fair (topic 0's benchmarking lesson):

- single-edge insert: write one record + patch 2-4 chain pointers —
  no CSR shifting, no delta machinery needed
- update-in-place: fixed-size slots never move; MVCC/undo is
  page-based, not copy-the-adjacency
- uniform record access ("get relationship by id") is O(1) arithmetic

The trade: neo4j optimized the OLTP mutation path and pays on every
traversal; CSR/matrix engines optimize traversal and need an overlay
(kuzu buffers, Delta_Matrix) to survive writes.

## Questions (answer in notes.md)

1. Compute Expand cost for a 1000-edge node: chain walk (assume every
   record is a DRAM miss, ~110 ns) vs CSR slice (assume 10 GB/s
   effective stream, 4 B per neighbor). How many × ?
2. Why 15 B for nodes but 34 B for relationships? What does each field
   buy?
3. The doubly-linked chain gives O(1) delete-given-record. What does
   delete cost in CSR? In Delta_Matrix?
4. neo4j stores properties in a separate chain (`nextProp`). How does
   that compare to M12's columnar property storage for
   `WHERE n.age > 65`?
5. "Index-free adjacency" was a disk-era argument. State the modern
   version of the argument that still holds, and the part that died
   with DRAM.
