# Neo4j's record store: the price of index-free adjacency

neo4j is the architecture FalkorDB most directly positions against,
and this chapter reads its data layout (Java, but you're reading
layout, not code style). Before the code, it builds the design one
step at a time — the 2010 bet, fixed-size records, the two-chain
relationship record, what an expand actually costs in cache misses,
and where the design genuinely wins — then anchors each piece in the
source.

Every anchor below is **neo4j pinned at `eccd584a`**
([`resources/codebases.md`](../../resources/codebases.md)), community
edition, under
`community/record-storage-engine/src/main/java/org/neo4j/kernel/impl/store/`
unless another path is given. Record widths and store layout are the
folklore-richest part of this topic — the "15-byte node record" is
repeated everywhere and is *format-specific*, so Step 2 checks which
format is actually the default before quoting any width, and Step 3
corrects a layout diagram that was missing a byte.

## The problem in one sentence

"Index-free adjacency" — neighbors reachable by following direct
pointers, no index lookup — beat any B-tree descent when a pointer
dereference cost a 10 ms disk seek either way; on DRAM the same
pointer chase costs a ~110 ns cache miss while a contiguous scan
streams at GB/s, and the bet inverts.

## The concepts, step by step

### Step 1 — the bet, and the hardware that aged out from under it

> **In:** a query that has found one node and now wants its
> neighbours.
> **Out:** the two candidate cost models — 2010 disk and 2026 DRAM —
> and the observation that the same design scores oppositely under
> them.

**Index-free adjacency** means each node stores a direct physical
pointer to its relationships, so expanding a node never consults an
index — you pay one pointer dereference per edge, period.

On 2010 spinning disks this was unbeatable. Any access was a ~10 ms
seek, so:

```
 index-free:   1 seek  × 10 ms = 10 ms
 B-tree:     3–4 seeks × 10 ms = 30–40 ms
 advantage: 3–4×, and it does not depend on how you lay records out,
            because nothing is contiguous when everything is a seek
```

On DRAM the cost model changed shape (topic 0). A **dependent load** —
a load whose address came out of the previous load, so the
prefetcher cannot issue it early — costs a ~110 ns last-level miss.
A contiguous array scan streams at ~10 GB/s. Redo the comparison for
one thousand neighbours:

```
 chain walk: 1000 dependent loads × 110 ns          = 110 000 ns = 110 µs
 CSR slice:  1000 × 4 B = 4 000 B at 10 GB/s        =     400 ns = 0.4 µs
 advantage: 275×, and it now runs the other way
```

The pointers didn't get slower — *sequential* got 100× faster, and
pointers can't be sequential. Why it matters: every design decision
below is downstream of this bet, and judging them requires the 2026
cost model, not the 2010 one.

### Step 2 — fixed-size records: the store IS the index

> **In:** a record id, e.g. "node 42".
> **Out:** the (page, offset) pair holding it — computed, not looked
> up — plus the reason the arithmetic is not the one you expect.

neo4j's record formats are fixed-width, so a record's location is
arithmetic rather than an index probe. The two widths, with the field
inventories the source itself writes above them:

```java
// format/standard/NodeRecordFormat.java
    30  public class NodeRecordFormat extends BaseOneByteHeaderRecordFormat<NodeRecord> {
    31      // in_use(byte)+next_rel_id(int)+next_prop_id(int)+labels(5)+extra(byte)
    32      public static final int RECORD_SIZE = 15;
```

```java
// format/standard/RelationshipRecordFormat.java
    30  public class RelationshipRecordFormat extends BaseOneByteHeaderRecordFormat<RelationshipRecord> {
    31      // record header size
    32      // directed|in_use(byte)+first_node(int)+second_node(int)+rel_type(int)+
    33      // first_prev_rel_id(int)+first_next_rel_id+second_prev_rel_id(int)+
    34      // second_next_rel_id+next_prop_id(int)+first-in-chain-markers(1)
    35      public static final int RECORD_SIZE = 34;
```

Check the two inventories add up before trusting them:

```
 node: 1 (in_use) + 4 (next_rel) + 4 (next_prop) + 5 (labels) + 1 (extra) = 15 ✓
 rel:  1 (header) + 4 (first_node) + 4 (second_node) + 4 (rel_type)
     + 4 (first_prev) + 4 (first_next) + 4 (second_prev) + 4 (second_next)
     + 4 (next_prop) + 1 (chain markers)                                  = 34 ✓
```

**Which format is that, though.** The widths above are the
`format/standard/` package. At this pin the standard family is
deprecated and is *not* the default:

```java
// format/FormatFamily.java
    28      STANDARD("standard", true /* isDeprecated */),
    29      ALIGNED("aligned", false),
    30      HIGH_LIMIT("high_limit", true /* isDeprecated */),
```

```java
// format/RecordFormatSelector.java
    66      private static final RecordFormats DEFAULT_FORMAT = PageAligned.LATEST_RECORD_FORMATS;
```

`PageAligned.LATEST_RECORD_FORMATS` is `PageAlignedV5_0`
(`format/aligned/PageAligned.java:28`), and `PageAlignedV5_0`
constructs the *same* two formats with the alignment flag set:

```java
// format/aligned/PageAlignedV5_0.java
    49  /**
  //  ... 50-57: elided — the javadoc explaining the difference from standard ...
    58   * Pages are padded at the end instead of letting a record span 2 pages.
    59   */
  //  ... 60-68: elided ...
    69          return new NodeRecordFormat(true);
  //  ... 70-78: elided ...
    79          return new RelationshipRecordFormat(true);
```

So: **15 and 34 are correct for the default community format**, but
for the alignment reason, not because "neo4j records are 15 bytes".
Neo4j Enterprise also ships a *block format* whose records are not
these at all — its existence shows up even in the community settings
file, e.g. `GraphDatabaseSettings.java:794-795` marks a setting "Not
applicable for the block format". Do not carry the number outside the
family it belongs to.

**Correction.** The previous version of this chapter said the address
is `id × RECORD_SIZE`. It is not — the store is paged, and a record
never straddles a page, so the id is split:

```java
// RecordPageLocationCalculator.java
    35      public static long pageIdForRecord(long id, int recordsPerPage) {
    36          return id / recordsPerPage;
    37      }
  //  ... 38-47: elided — javadoc ...
    48      public static int offsetForId(long id, int recordSize, int recordsPerPage) {
    49          return (int) (id % recordsPerPage) * recordSize;
    50      }
```

`recordsPerPage` is not a constant either — it is computed from the
page size and the record size:

```java
// format/BaseRecordFormat.java
   107      @Override
   108      public int getFilePageSize(int pageSize, int recordSize) {
   109          return pageAligned ? pageSize : Math.min(pageSize, pageSize - pageSize % recordSize);
   110      }
```

```java
// CommonAbstractStore.java
   377          int filePageSize = recordFormat.getFilePageSize(pageCache.pageSize(), recordSize);
  //  ... 378-391: elided — the paged file is mapped ...
   392          recordsPerPage = (filePageSize - pagedFile.pageReservedBytes()) / recordSize;
```

Work it with the default page size — `PageCache.java:49` in
`community/io/` sets `int PAGE_SIZE = 8192`, and take reserved bytes
as 0:

```
 nodes:  8192 / 15 = 546 records per page
         546 × 15  = 8190 B used,  8192 − 8190 =  2 B padding per page
 rels:   8192 / 34 = 240 records per page
         240 × 34  = 8160 B used,  8192 − 8160 = 32 B padding per page

 node 42:        page = 42 / 546        = 0
                 offset = (42 % 546) × 15 = 42 × 15 = 630
 node 1 000 000: page = 1 000 000 / 546  = 1831
                 1 000 000 − 1831 × 546  = 1 000 000 − 999 726 = 274
                 offset = 274 × 15       = 4110
```

Padding overhead: 2/8192 = 0.02% for nodes, 32/8192 = 0.39% for
relationships. Cheap, and it buys the guarantee that reading a record
is one page access rather than possibly two.

Why it matters: fixed size buys O(1) id→record access and trivial
free-space management — but note what a node record does NOT contain:
its neighbors. It contains only the head pointer of a chain.

### Step 3 — the relationship record: one edge on two linked lists

> **In:** the 34 raw bytes of a relationship record.
> **Out:** the decoded fields — including the ones that do not fit in
> their nominal width, and the byte the previous diagram forgot.

Each 34-byte relationship record sits on TWO doubly-linked lists
simultaneously — one chain per endpoint. `firstPrevRel`/`firstNextRel`
thread it into the first node's chain, `secondPrevRel`/`secondNextRel`
into the second node's:

```
 node A ──nextRel──> rel1 ──firstNextRel──> rel4 ──> rel9 ──> NULL
                      │
 node B ──nextRel────rel1 ──secondNextRel─> rel2 ──> ...
```

One physical record, two logical list memberships — so both endpoints
can enumerate their edges without storing the edge twice.

**Correction.** The previous version's layout diagram ended at
`nextProp` and omitted the trailing byte. It is there, it is
load-bearing, and the source documents its four flags:

```java
// format/standard/RelationshipRecordFormat.java, inside read()
    63          byte headerByte = cursor.getByte();
  //  ... 64-69: elided — in-use flag, first-node and next-prop high bits ...
    70              long firstNode = cursor.getInt() & 0xFFFFFFFFL;
    71              long firstNodeMod = (headerByte & 0xEL) << 31;
  //  ... 72-74: elided ...
    75              // [ xxx,    ][    ,    ][    ,    ][    ,    ] second node high order bits,     0x70000000
  //  ... 76-79: elided — the same map for the four chain pointers ...
    80              // [    ,    ][    ,    ][xxxx,xxxx][xxxx,xxxx] type
    81              long typeInt = cursor.getInt();
  //  ... 82: elided ...
    83              int type = (int) (typeInt & 0xFFFF);
  //  ... 84-99: elided — the four chain pointers and nextProp, each with its mod ...
   100              // [    ,   x] 1:st in start node chain,   0x1
   101              // [    ,  x ] 1:st in end node chain,     0x2
   102              // [    , x  ] first is guaranteed dense,  0x4
   103              // [    ,x   ] second is guaranteed dense, 0x8
   104              byte extraByte = cursor.getByte();
```

The corrected diagrams, with the real field widths:

```
 Node (15 B):  header(1) | nextRel(4) | nextProp(4) | labels(5) | extra(1)
   nextRel  = 32 low bits + 3 bits from the header  → 35 bits
   nextProp = 32 low bits + 4 bits from the header  → 36 bits
   labels   = 32 low bits + 8 bits (hsbLabels)      → 40 bits
   extra    bit 0 = dense

 Rel (34 B):   header(1) | firstNode(4) | secondNode(4) | typeInt(4)
             | firstPrevRel(4) | firstNextRel(4)     ← chain @ first node
             | secondPrevRel(4)| secondNextRel(4)    ← chain @ second node
             | nextProp(4) | extraByte(1)
   type     = typeInt & 0xFFFF                       → 16 bits
   the other 16 bits of typeInt carry 3 bits each of secondNode,
   firstPrevRel, firstNextRel, secondPrevRel, secondNextRel
   extraByte bits: first-in-start-chain, first-in-end-chain,
                   first-guaranteed-dense, second-guaranteed-dense
```

The bit-smuggling has a ceiling, and the ceiling is declared:

```java
// format/standard/StandardFormatSettings.java
  //  ... 20-28: elided ...
    29      public static final int NODE_MAXIMUM_ID_BITS = 35;
    30      public static final int RELATIONSHIP_MAXIMUM_ID_BITS = 35;
    31      public static final int PROPERTY_MAXIMUM_ID_BITS = 36;
  //  ... 32-37: elided ...
    38      public static final int RELATIONSHIP_TYPE_TOKEN_MAXIMUM_ID_BITS = 16;
    39      public static final int RELATIONSHIP_GROUP_MAXIMUM_ID_BITS = 35;
  //  ... 40-48: elided ...
    49      static long bitsToMaxId(int bits) {
    50          return (1L << bits) - 1;
    51      }
```

So the store caps are computable, and worth computing because they
are design constraints, not trivia:

```
 nodes / relationships:  2^35 − 1 = 34 359 738 367   ≈ 34.4e9
 properties:             2^36 − 1 = 68 719 476 735   ≈ 68.7e9
 relationship types:     2^16 − 1 = 65 535           ← the tight one
 labels field is 40 bits, but it is an inline label *set*, not an id
```

65 535 relationship types is the constraint that bites first — a
schema that encodes data into type names (`:LIKED_2024_01`) runs out.
Compare postgres's tuple header (topic 8): the same bit-packing
ledger, the same habit of stealing high bits from a flags byte.

The records of one node's chain, however, live wherever *insertion
order* put them in the file; there is no locality guarantee
whatsoever. Why it matters: the chain is the data structure every
traversal walks — its memory layout (scattered) is the whole
performance story of Step 4.

### Step 4 — expand = one dependent load per edge

> **In:** a node record and the relationship store.
> **Out:** that node's neighbour ids, and a count of dependent loads
> — which is the real currency.

Expanding a node means walking its chain: read a record, look at which
endpoint you are, follow the corresponding next pointer — and each
next address is unknown until the current record arrives, so the CPU
cannot prefetch anything:

```rust
// ILLUSTRATION — not neo4j source. The real decode is
// format/standard/RelationshipRecordFormat.java:63-104 and the chain
// fields are record/RelationshipRecord.java:39-42; this is the shape of
// the walk those two files imply.
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

**One 34-byte record read — one potential cache/page miss — per
edge.** Two ways to price it, and they differ by three orders of
magnitude, so state which you mean:

```
 in-memory, records cached, scattered:
   100 000 edges × 110 ns dependent load        = 11.0 ms
 CSR, same 100 000 neighbours, 4 B ids, 10 GB/s:
   400 000 B / 10e9 B/s                         = 40 µs
 ratio                                          = 275×

 cold, records on disk, 240 rels/page (Step 2):
   worst case one page fault per record         = 100 000 page reads
   best case, chain perfectly clustered         = 100 000/240 = 417 page reads
 the 240× spread between those two is exactly what "no locality
 guarantee" costs you, and nothing in the format decides it — insertion
 order does
```

Now anchor it against this topic's own measurement. The bench's
adjacency-list oracle stores each node's neighbours contiguously and
still takes 495 378 ns for a two-hop from supernodes
([notes.md](notes.md); [FINDINGS.md](../../FINDINGS.md) row 13).
Divide:

```
 495 378 ns / 110 ns per dependent load = 4 503 dependent loads' worth
```

A top-100-degree node in that graph has up to 6 565 first-hop edges
alone, and the two-hop expansion is far larger than that. So the
oracle is plainly *not* paying one miss per edge — it is streaming
contiguous slices, and 495 µs is a **lower bound** on what the same
query would cost a record store, which adds a dependent load per edge
on top. Why it matters: this per-edge miss is the line item
FalkorDB's matrices and kuzu's CSR exist to delete.

### Step 5 — chain maintenance: deletes, lookups, and dense nodes

> **In:** a relationship to delete, or a "is there an edge between A
> and B" question.
> **Out:** the cost, and the extra record types neo4j added to keep
> that cost bounded.

The chains create their own bookkeeping costs, which neo4j itself
acknowledges.

- **Delete** must unlink the record from BOTH doubly-linked chains, so
  up to 4 neighbour records are touched and rewritten. That is O(1) —
  the record ids are in the record you already have — but it is 4
  scattered writes.
- **Find a specific relationship between two given nodes** means
  walking a chain until you hit it: O(degree).
- **Dense nodes** get a mitigation. Past a threshold, a node's
  relationships are grouped by type and direction into
  `RelationshipGroup` records:

```java
// format/standard/RelationshipGroupRecordFormat.java
    31      // [type+inUse+highbits,next,firstOut,firstIn,firstLoop,owningNode]
  //  ... 32-37: elided ...
    38      public static final int RECORD_SIZE = 25;
```

The threshold is a setting with a default you can read:

```java
// configuration/GraphDatabaseSettings.java  (community/configuration/...)
   796      public static final Setting<Integer> dense_node_threshold = newBuilder(
   797                      "db.relationship_grouping_threshold", INTEGER, 50)
```

Fifty. Put that against the bench graph's degree distribution
(p50 degree 11, max degree 6 565, [notes.md](notes.md)):

```
 p50 node, degree 11  → 11 < 50   → not dense, one flat chain
 top node, degree 6565 → ≥ 50     → dense, grouped by type+direction
 a group record is 25 B and buys per-type chain heads, so
 "expand only :FOLLOWS out-edges" stops walking the other types
```

Why it matters: linked structures make every structural query a walk;
the mitigations (degree caches, relationship groups) are extra record
types patching the base design's asymptotics — and they only engage
above 50, which is to say only on the tail that this topic's headline
is about.

### Step 6 — where records win

> **In:** the same design, judged on the mutation path instead of the
> traversal path.
> **Out:** the three workloads where fixed-size records are the right
> answer, and the symmetric price the other camp pays.

Be fair (topic 0's benchmarking lesson) — the design has a real home
turf:

- **single-edge insert**: write one 34 B record + patch 2–4 chain
  pointers. Price it against the CSR alternative on the bench graph's
  16.0 M edges with 4 B ids:

  ```
   record store: 34 B write + 4 × 34 B pointer patches = 170 B touched
   raw CSR:      average memmove of half of 16e6 × 4 B = 32 MB touched
   ratio ≈ 188 000×
  ```

  That is why CSR engines need an overlay at all
  ([reading-graphblas-internals.md](reading-graphblas-internals.md)
  Step 7) and neo4j needs none.
- **update-in-place**: fixed-size slots never move, so MVCC/undo is
  page-based rather than copy-the-adjacency.
- **uniform record access**: "get relationship by id" is the Step 2
  page/offset arithmetic — no index, no search.

The trade in one sentence: neo4j optimized the OLTP mutation path and
pays on every traversal; CSR/matrix engines optimize traversal and
need an overlay (kuzu's transient node groups, FalkorDB's
Delta_Matrix) to survive writes. Why it matters: neither side dodges
the tension — they pick opposite ends and buy back the other end with
extra machinery.

## Where each step lives in the code

Paths are relative to
`community/record-storage-engine/src/main/java/org/neo4j/kernel/impl/store/`
except where noted, in [neo4j](https://github.com/neo4j/neo4j) pinned
at `eccd584a`.

| Step | Anchor | What is there |
|---|---|---|
| 2 | `format/standard/NodeRecordFormat.java:31-32` | field inventory comment + `RECORD_SIZE = 15` |
| 2 | `format/standard/RelationshipRecordFormat.java:31-35` | field inventory comment + `RECORD_SIZE = 34` |
| 2 | `format/FormatFamily.java:28-30` | STANDARD and HIGH_LIMIT are deprecated; ALIGNED is not |
| 2 | `format/RecordFormatSelector.java:66` | `DEFAULT_FORMAT = PageAligned.LATEST_RECORD_FORMATS` |
| 2 | `format/aligned/PageAligned.java:28` | that resolves to `PageAlignedV5_0` |
| 2 | `format/aligned/PageAlignedV5_0.java:49-58, 69, 79` | "padded at the end"; same 15/34 formats, aligned flag |
| 2 | `RecordPageLocationCalculator.java:35-37, 48-50` | `pageIdForRecord`, `offsetForId` — the real address arithmetic |
| 2 | `format/BaseRecordFormat.java:107-110` | `getFilePageSize` — where the padding comes from |
| 2 | `CommonAbstractStore.java:377, 392` | `filePageSize`, `recordsPerPage` |
| 2 | `community/io/.../pagecache/PageCache.java:49` | `PAGE_SIZE = 8192` |
| 3 | `format/standard/NodeRecordFormat.java:55-70` | the decode: 35-bit nextRel, 36-bit nextProp, 40-bit labels, dense flag |
| 3 | `format/standard/RelationshipRecordFormat.java:63-104` | the decode, the bit map comments, and the extra byte |
| 3 | `format/standard/StandardFormatSettings.java:29-31, 38-39, 49-51` | the id-width ceilings and `bitsToMaxId` |
| 3–4 | `record/RelationshipRecord.java:39-42` | the four chain fields (was cited as `:39-44`) |
| 3–4 | `record/RelationshipRecord.java:43-44` | `firstInFirstChain`, `firstInSecondChain` — the extra byte's first two flags |
| 5 | `format/standard/RelationshipGroupRecordFormat.java:31-33, 38` | the group layout comment and `RECORD_SIZE = 25` |
| 5 | `community/configuration/.../GraphDatabaseSettings.java:796-797` | `db.relationship_grouping_threshold`, default 50 |

Read order: the two `read` methods first (they make the byte layouts
concrete — `NodeRecordFormat.java:55-70`, then
`RelationshipRecordFormat.java:63-104`), then
`StandardFormatSettings.java` for why the bit-stealing stops where it
does, then `RecordPageLocationCalculator.java` for the address
arithmetic, then trace Step 4's walk mentally against
`RelationshipRecord.java:39-42`.

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

## Done when

Answer each before unfolding it.

- [ ] You can explain the index-free adjacency bet and name the hardware assumption that aged out from under it.

  <details><summary>Answer</summary>

  The bet: store a direct physical pointer from node to relationship
  so expansion never consults an index. The assumption: that *any*
  access costs the same, because it is a ~10 ms seek — under which one
  pointer hop (1 seek) beats a B-tree descent (3–4 seeks) by 3–4×.

  What aged out is not the pointer, it is the alternative. Sequential
  access got ~100× faster while dependent random access did not, so
  the same thousand neighbours are 1000 × 110 ns = 110 µs as a chain
  walk and 4 000 B / 10 GB/s = 0.4 µs as a CSR slice — a 275× reversal.
  </details>

- [ ] You can compute expand cost for a 1000-edge node as dependent loads, compare it to a CSR slice, and say what decides the cold-cache case.

  <details><summary>Answer</summary>

  Warm: 1 000 dependent loads × 110 ns = 110 µs versus 4 000 B at
  10 GB/s = 0.4 µs — 275×.

  Cold, the answer is a range rather than a number, and the range is
  the point. `CommonAbstractStore.java:392` and
  `BaseRecordFormat.java:107-110` give 8192/34 = 240 relationship
  records per 8 KiB page. If the chain is perfectly clustered, 1 000
  records is 1000/240 = 5 page reads (rounding up from 4.17). If it is
  scattered, it is up to 1 000 page reads. Nothing in the record format
  decides which — insertion order does, because a relationship record
  is appended where the free list puts it, not where its chain
  neighbours are.
  </details>

- [ ] You can say what the doubly-linked relationship chain buys and what it costs on insert and delete, with the byte counts.

  <details><summary>Answer</summary>

  It buys: one physical 34 B record serving both endpoints'
  enumeration, so an edge is stored once, not twice; and O(1)
  unlinking given the record, because `firstPrevRel`/`firstNextRel`/
  `secondPrevRel`/`secondNextRel` (`RelationshipRecord.java:39-42`)
  name the neighbours directly.

  It costs: delete touches up to 4 other records (170 B of scattered
  writes); "is there an edge A→B" is O(degree) with no shortcut; and
  the chain has no locality, which is Step 4's whole problem.

  Insert is where it wins outright: ~170 B touched versus ~32 MB for
  an in-place CSR insert on a 16 M-edge graph — about 188 000×.
  </details>

- [ ] You can state the modern version of the index-free adjacency argument — the one that survives the disk era ending — and name the format caveat on the 15/34 numbers.

  <details><summary>Answer</summary>

  What survives: *no index probe on the mutation path*. Insert,
  delete and get-by-id are pure arithmetic plus a bounded number of
  record writes, with no B-tree to split and no adjacency structure to
  rebuild. That is a real advantage and it is why the CSR camp has to
  bolt an overlay on.

  What died: the claim that pointer-following is the *fastest way to
  read* neighbours. On DRAM it is the slowest way, by 275× against a
  contiguous slice.

  The caveat: 15 and 34 belong to `format/standard/` and the aligned
  formats built from it. `FormatFamily.java:28-30` marks STANDARD
  deprecated, `RecordFormatSelector.java:66` makes `PageAligned` the
  default (same widths, page-end padding —
  `PageAlignedV5_0.java:58`), and Enterprise's block format is a
  different layout entirely. Quote the number with its family.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including the 15 B versus 34 B field accounting.

  <details><summary>Answer</summary>

  The accounting is written in the source comments and both add up:
  node = 1 + 4 + 4 + 5 + 1 = 15 (`NodeRecordFormat.java:31`);
  relationship = 1 + 4×8 + 1 = 34
  (`RelationshipRecordFormat.java:32-34`).

  The asymmetry is structural, not arbitrary: a node stores one chain
  *head* (4 B) and nothing about its neighbours, while a relationship
  must name two endpoints (8 B), a type, and *four* chain pointers
  (16 B) because it is a member of two doubly-linked lists at once.
  Sixteen of the relationship record's 34 bytes — 47% — are chain
  maintenance. That is the storage cost of Step 4's design, paid on
  every edge.
  </details>

## References

**Code** (all line numbers verified at neo4j `eccd584a`)

| File (under `community/record-storage-engine/.../impl/store/` unless noted) | Lines | What |
|---|---|---|
| `format/standard/NodeRecordFormat.java` | 31-32, 55-70 | width, field inventory, decode |
| `format/standard/RelationshipRecordFormat.java` | 31-35, 63-104 | width, field inventory, decode, extra byte |
| `format/standard/StandardFormatSettings.java` | 29-31, 38-39, 49-51 | id-width ceilings, `bitsToMaxId` |
| `format/standard/RelationshipGroupRecordFormat.java` | 31-33, 38 | dense-node group record, 25 B |
| `format/FormatFamily.java` | 28-30 | which families are deprecated |
| `format/RecordFormatSelector.java` | 66 | the actual default format |
| `format/aligned/PageAligned.java` | 28 | → `PageAlignedV5_0` |
| `format/aligned/PageAlignedV5_0.java` | 49-58, 69, 79 | page-end padding; same 15/34 formats |
| `format/BaseRecordFormat.java` | 107-110 | `getFilePageSize` |
| `RecordPageLocationCalculator.java` | 35-37, 48-50 | page id and offset from record id |
| `CommonAbstractStore.java` | 377, 392 | `filePageSize`, `recordsPerPage` |
| `record/RelationshipRecord.java` | 36-44 | the two endpoints, type, four chain fields, two chain flags |
| `community/io/.../pagecache/PageCache.java` | 49 | `PAGE_SIZE = 8192` |
| `community/configuration/.../GraphDatabaseSettings.java` | 794-795, 796-797 | block-format caveat; dense-node threshold 50 |
