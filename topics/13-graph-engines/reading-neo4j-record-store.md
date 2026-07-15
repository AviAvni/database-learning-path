# Neo4j's record store: the price of index-free adjacency

neo4j is the architecture FalkorDB most directly positions against,
and this chapter reads its data layout (Java, but you're reading
layout, not code style). Before the code, it builds the design one
step at a time — the 2010 bet, fixed-size records, the two-chain
relationship record, what an expand actually costs in cache misses,
and where the design genuinely wins — then anchors each piece in the
source.

## The problem in one sentence

"Index-free adjacency" — neighbors reachable by following direct
pointers, no index lookup — beat any B-tree descent when a pointer
dereference cost a 10 ms disk seek either way; on DRAM the same
pointer chase costs a ~110 ns cache miss while a contiguous scan
streams at GB/s, and the bet inverts.

## The concepts, step by step

### Step 1 — the bet, and the hardware that aged out from under it

**Index-free adjacency** means each node stores a direct physical
pointer to its relationships, so expanding a node never consults an
index — you pay one pointer dereference per edge, period. On 2010
spinning disks this was unbeatable: ANY access was a ~10 ms seek, so
one pointer (1 seek) beat a B-tree descent (3–4 seeks) every time. On
DRAM the cost model changed shape (topic 0): a dependent pointer
dereference is a ~110 ns cache miss the prefetcher can't hide, while
a contiguous array scan streams at ~10 GB/s. The pointers didn't get
slower — *sequential* got 100× faster, and pointers can't be
sequential. Why it matters: every design decision below is downstream
of this bet, and judging them requires the 2026 cost model, not the
2010 one.

### Step 2 — fixed-size records: the store IS the index

neo4j stores every node in exactly 15 bytes and every relationship in
exactly 34 bytes, so a record's disk/file position is pure arithmetic
— `address = id × RECORD_SIZE` — and looking up "node 42" or
"relationship 1,000,007" needs no index structure at all:

```java
public static final int RECORD_SIZE = 15;   // NodeRecordFormat.java:32
public static final int RECORD_SIZE = 34;   // RelationshipRecordFormat.java:35
```

```
 Node (15 B):  inUse | nextRel(35b) | nextProp(36b) | labels(40b) | flags
 Rel  (34 B):  inUse | firstNode | secondNode | type
               | firstPrevRel | firstNextRel      ← chain @ first node
               | secondPrevRel | secondNextRel    ← chain @ second node
               | nextProp
```

Squeezing pointers into 15/34 bytes forces bit tricks: pointers are
35 bits (2³⁵ records max per store), with the high bits smuggled into
the inUse byte — the bit-packing ledger again (compare postgres's
tuple header, topic 8). Why it matters: fixed size buys O(1) id→record
access and trivial free-space management — but note what a node record
does NOT contain: its neighbors. It contains only the head pointer of
a chain.

### Step 3 — the relationship record: one edge on two linked lists

Each 34-byte relationship record sits on TWO doubly-linked lists
simultaneously — one chain per endpoint — using the four
prev/next fields you saw in the layout: `firstPrevRel/firstNextRel`
thread it into the first node's chain, `secondPrevRel/secondNextRel`
into the second node's:

```
 node A ──nextRel──> rel1 ──firstNextRel──> rel4 ──> rel9 ──> NULL
                      │
 node B ──nextRel────rel1 ──secondNextRel─> rel2 ──> ...
```

One physical record, two logical list memberships — so both endpoints
can enumerate their edges without storing the edge twice. The records
of one node's chain, however, live wherever *insertion order* put them
in the file; there is no locality guarantee whatsoever. Why it
matters: the chain is the data structure every traversal walks — its
memory layout (scattered) is the whole performance story of Step 4.

### Step 4 — expand = one dependent load per edge

Expanding a node means walking its chain: read a record, look at which
endpoint you are, follow the corresponding next pointer — and each
next address is unknown until the current record arrives, so the CPU
cannot prefetch anything:

```rust
// expand(A) in a record store: a linked-list walk where every hop
// is a dependent load — the CPU cannot prefetch what it hasn't read
fn expand(rels: &[RelRecord], node: &NodeRecord) -> Vec<u64> {
    let mut out = Vec::new();
    let mut r = node.next_rel;
    while r != NIL {
        let rec = &rels[r as usize];             // scattered: likely a miss
        if rec.first_node == node.id {
            out.push(rec.second_node);
            r = rec.first_next_rel;              // ← next hop unknown until
        } else {                                 //   THIS record arrives
            out.push(rec.first_node);
            r = rec.second_next_rel;             // same record, other chain
        }
    }
    out    // CSR spelling: targets[offsets[i]..offsets[i+1]] — one slice
}
```

The arithmetic: **one 34-byte record read — one potential cache/page
miss — per edge**. A supernode with 100K edges = 100K dependent loads
≈ 11 ms of pure memory latency. The CSR (compressed sparse row —
adjacency as one offsets array plus one contiguous neighbors array)
spelling of the same operation is `targets[offsets[i]..offsets[i+1]]`
— one slice, hardware prefetcher does the rest, ~40 µs for the same
100K neighbors at 10 GB/s. This is Drepper's pointer-chase-vs-stream
distinction (topic 0) elevated to an architecture. Why it matters:
this per-edge miss is the line item FalkorDB's matrices and kuzu's
CSR exist to delete.

### Step 5 — chain maintenance: deletes, lookups, and dense nodes

The chains create their own bookkeeping costs, which neo4j itself
acknowledges. Deleting a relationship must unlink it from BOTH
doubly-linked chains — up to 4 neighbor records touched and rewritten.
Finding a *specific* relationship between two given nodes means
walking a chain until you hit it — O(degree) — so neo4j stores the
degree for "dense" nodes and walks the shorter endpoint's chain (see
`RelationshipGroup` records, which also split chains per relationship
type/direction). Why it matters: linked structures make every
structural query a walk; the mitigations (degree caches, relationship
groups) are extra record types patching the base design's asymptotics.

### Step 6 — where records win

Be fair (topic 0's benchmarking lesson) — the design has a real
home turf:

- **single-edge insert**: write one 34 B record + patch 2–4 chain
  pointers — no CSR shifting, no delta-overlay machinery needed at
  all
- **update-in-place**: fixed-size slots never move; MVCC/undo is
  page-based, not copy-the-adjacency
- **uniform record access**: "get relationship by id" is O(1)
  arithmetic (Step 2)

The trade in one sentence: neo4j optimized the OLTP mutation path and
pays on every traversal; CSR/matrix engines optimize traversal and
need an overlay (kuzu's buffers, FalkorDB's Delta_Matrix) to survive
writes. Why it matters: neither side dodges the tension — they pick
opposite ends and buy back the other end with extra machinery.

## Where each step lives in the code

Everything lives under
`community/record-storage-engine/src/main/java/org/neo4j/kernel/impl/store/`
in a shallow clone of [neo4j](https://github.com/neo4j/neo4j):

- **Step 2** — `format/standard/NodeRecordFormat.java:32`
  (`RECORD_SIZE = 15`) and
  `format/standard/RelationshipRecordFormat.java:35`
  (`RECORD_SIZE = 34`). Read both files' `readRecord` methods — they
  ARE the layout diagrams above, including the 35-bit pointer
  reassembly from the inUse byte's high bits.
- **Steps 3–5** — `record/RelationshipRecord.java:39-44` — the four
  chain fields (firstPrevRel/firstNextRel,
  secondPrevRel/secondNextRel). For the dense-node mitigation, grep
  for `RelationshipGroup`.

Read order: the two `readRecord` methods first (they make the byte
layouts concrete), then `RelationshipRecord.java` for the chain
fields, then trace Step 4's walk mentally against them.

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

## References

**Code**
- [neo4j](https://github.com/neo4j/neo4j) (shallow clone) — everything
  lives under
  `community/record-storage-engine/src/main/java/org/neo4j/kernel/impl/store/`:
  `format/standard/NodeRecordFormat.java`,
  `format/standard/RelationshipRecordFormat.java` (read both
  `readRecord` methods for the layouts),
  `record/RelationshipRecord.java`
