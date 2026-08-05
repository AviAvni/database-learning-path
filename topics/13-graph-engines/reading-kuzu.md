# Kùzu: DuckDB for graphs

kuzu is "DuckDB for graphs": columnar disk-based storage, vectorized
execution — and two graph-specific ideas worth stealing: CSR that
survives updates via node groups, and a worst-case-optimal Intersect
operator embedded in an otherwise binary-join plan. Before the C++
(read alongside the CIDR '23 system paper), this chapter builds the
design step by step: edges as a columnar table, CSR as an index over
it, the per-node-group update fix, the Intersect operator, and
factorization.

Every code anchor below is **kuzu pinned at `89f0263`**
([`resources/codebases.md`](../../resources/codebases.md)); every
paper claim is from Feng, Jin, Chen, Liu & Salihoğlu, *KÙZU Graph
Database Management System*, CIDR 2023, cited by section. Mind the
gap: the paper is from January 2023 and the pin is much later, so
where the two disagree the code wins and the chapter says so. Two
numbers the previous version of this chapter asserted — the node
group size, and the shape of the CSR — did not survive the check.

## The problem in one sentence

If edges are just rows of a columnar table clustered by source node,
every topic-12 trick (compression, zone maps, vectorized scans)
applies to adjacency for free — the two things a relational engine
still can't do are surviving single-edge inserts into a sorted
structure and joining cyclic patterns without an O(m²) blowup, and
kuzu ships one mechanism for each.

## The concepts, step by step

### Step 1 — adjacency is a columnar table clustered by source

> **In:** a relationship table, i.e. a set of (src, dst, type,
> properties) rows.
> **Out:** those rows laid out as columns, horizontally sliced into
> node groups and sorted by source — so that "expand node i" is a
> range of rows rather than a search.

kuzu stores a relationship table the way DuckDB stores any table — in
**node groups** (horizontal slices, ≈ DuckDB's row groups from topic
12), each column separately — with one twist: the rows are edges,
sorted by source node id, and the first two columns are fixed:

```cpp
// src/include/storage/table/csr_node_group.h
   162  static constexpr common::column_id_t NBR_ID_COLUMN_ID = 0;
   163  static constexpr common::column_id_t REL_ID_COLUMN_ID = 1;
```

```
 rel table (one node group), sorted by src:
 src:      0    0    1    3    3    3   ...
 nbr (c0): 5    9    5    2    7    8   ...
 rel (c1): e0   e1   e2   e3   e4   e5  ...
 props:    ...columns like any table...
```

The paper states the same design and adds the part the header does
not: the edges are **double indexed**, forward and backward, and the
edge *properties* are stored in parallel CSR-shaped structures of
their own —

> "Edges are double indexed and stored in CSR-based adjacency list
> indices …, which are the core join indices in the system to join
> node records. … Edge properties are similarly stored in 'parallel'
> but separate CSR-based structures and double-indexed … This has
> storage and update costs yet ensures that we can scan any node's
> edges and properties of these edges sequentially in both forward
> and backward directions."
> — CIDR '23 §2, *Storage and Indices*

"Storage and update costs" is the paper's own admission that double
indexing doubles the write. That is the same trade memgraph makes by
keeping `in_edges` and `out_edges`
([reading-memgraph-storage.md](reading-memgraph-storage.md) Step 3) —
kuzu just pays it in columns instead of in per-vertex vectors.

Because all of node 3's edges are adjacent rows, "expand node 3" is a
contiguous slice of every column. Why it matters: three other engines
in this topic built custom edge storage; kuzu's bet is that the
columnar machinery already solved storage, and graphs only need two
extra operators on top.

### Step 2 — the CSR header: turning sorted rows into O(1) expand

> **In:** a node group's sorted edge rows and a bound node id.
> **Out:** that node's row range — by array indexing, not by search —
> plus the reason kuzu's CSR is not the textbook one.

To find node 3's slice without searching, each node group carries a
**CSR header**: an offsets array where entry i holds the position
where node i's edges start, so node i's edges are rows
`offsets[i] .. offsets[i+1]`. kuzu stores it as two columns —

```cpp
// src/include/storage/table/csr_node_group.h
   148  struct CSRNodeGroupCheckpointState final : NodeGroupCheckpointState {
   149      Column* csrOffsetColumn;
   150      Column* csrLengthColumn;
   151
   152      std::unique_ptr<InMemChunkedCSRHeader> oldHeader;
   153      std::unique_ptr<InMemChunkedCSRHeader> newHeader;
```

— offsets *and* lengths, which is already a hint: textbook CSR does
not need lengths, because `offsets[i+1] - offsets[i]` is the length.
Storing both means the rows for node i do **not** run right up to the
rows for node i+1. There is slack between them, and the slack is the
whole update story.

**Correction.** The previous version of this chapter described plain
CSR. What kuzu builds is a **packed CSR** — a
packed-memory-array-style layout with deliberate gaps, governed by a
calibrator tree and density thresholds:

```cpp
// src/include/storage/table/csr_node_group.h
    99  // TODO(Guodong): Serialize the info to disk. This should be a config per node group.
   100  struct PackedCSRInfo {
   101      static_assert(common::StorageConfig::NODE_GROUP_SIZE_LOG2 >
   102                    common::StorageConfig::CSR_LEAF_REGION_SIZE_LOG2);
   103      uint64_t calibratorTreeHeight = common::StorageConfig::NODE_GROUP_SIZE_LOG2 -
   104                                      common::StorageConfig::CSR_LEAF_REGION_SIZE_LOG2;
   105      double highDensityStep = (common::StorageConstants::LEAF_HIGH_CSR_DENSITY -
   106                                   common::StorageConstants::PACKED_CSR_DENSITY) /
   107                               static_cast<double>(calibratorTreeHeight);
   108
   109      constexpr PackedCSRInfo() noexcept = default;
   110  };
```

Every constant in that expression is readable. Two live in
`common/constants.h`:

```cpp
// src/include/common/constants.h, struct StorageConstants
    78      static constexpr double PACKED_CSR_DENSITY = 0.8;
    79      static constexpr double LEAF_HIGH_CSR_DENSITY = 1.0;
```

and two come from a CMake-configured header, whose *defaults* are in
the top-level `CMakeLists.txt`:

```cpp
// cmake/templates/system_config.h.in, struct StorageConfig
    47      static constexpr uint64_t NODE_GROUP_SIZE_LOG2 = @KUZU_NODE_GROUP_SIZE_LOG2@;
    48      static constexpr uint64_t NODE_GROUP_SIZE = static_cast<uint64_t>(1) << NODE_GROUP_SIZE_LOG2;
    49      // The number of CSR lists in a leaf region.
    50      static constexpr uint64_t CSR_LEAF_REGION_SIZE_LOG2 =
    51          std::min(static_cast<uint64_t>(10), NODE_GROUP_SIZE_LOG2 - 1);
```

```cmake
# CMakeLists.txt
   126  option(KUZU_NODE_GROUP_SIZE_LOG2 "Log2 of the vector capacity." 17)
   127  if(NOT KUZU_NODE_GROUP_SIZE_LOG2)
   128      set(KUZU_NODE_GROUP_SIZE_LOG2 17)
```

Now do the arithmetic, because it is all determined:

```
 NODE_GROUP_SIZE_LOG2      = 17        →  NODE_GROUP_SIZE      = 131 072 nodes
 CSR_LEAF_REGION_SIZE_LOG2 = min(10, 16) = 10
                                       →  CSR_LEAF_REGION_SIZE = 1 024 CSR lists
 calibratorTreeHeight      = 17 − 10   = 7
 cross-check: 131 072 / 1 024 = 128 leaf regions,  and 2^7 = 128 ✓
 highDensityStep           = (1.0 − 0.8) / 7 = 0.02857…
```

**Correction.** The previous version said a node group is "say, 64K
nodes". It is 2^17 = **131 072** at the default build, twice that.

The densities say what the slack is for: a leaf region is kept at
0.8 occupancy, and the allowed density ramps from 0.8 up to 1.0 over
the 7 levels of the calibrator tree in steps of 0.0286 — so a small
insert fills local slack, a bigger one redistributes within a leaf
region, and only a large one rebalances a whole subtree. Size the
slack on this topic's graph (1 M nodes, 16.0 M directed edges,
[notes.md](notes.md)):

```
 node groups needed = 1 000 000 / 131 072 = 7.63 → 8 groups
 edges per group    = 16.0e6 / 8          = 2.0e6
 slots reserved at density 0.8 = 2.0e6 / 0.8 = 2.5e6
 free slots per group          = 2.5e6 − 2.0e6 = 500 000  (25% headroom)
```

Why it matters: this is the same CSR as FalkorDB's matrices-in-CSR,
arrived at from the relational direction — but with the gaps that
make it writable built into the layout rather than bolted on as a
separate overlay matrix.

### Step 3 — surviving updates: persistent CSR + transient overlay, per node group

> **In:** a stream of single-edge inserts arriving between
> checkpoints.
> **Out:** rows appended to an in-memory chunk plus an index entry,
> with the CSR rebuild deferred to checkpoint and bounded to one node
> group.

The slack of Step 2 absorbs *some* inserts. For the rest, kuzu splits
each node group in two, and the header says so in five lines that are
worth reading before any of the code:

```cpp
// src/include/storage/table/csr_node_group.h
   165  // Data in a CSRNodeGroup is organized as follows:
   166  // - persistent data: checkpointed data or flushed data from batch insert. `persistentChunkGroup`.
   167  // - transient data: data that is being committed but kept in memory. `chunkedGroups`.
   168  // Persistent data are organized in CSR format.
   169  // Transient data are organized similar to normal node groups. Tuples are always appended to the end
   170  // of `chunkedGroups`. We keep an extra csrIndex to track the vector of row indices for each bound
   171  // node.
   172  class CSRNodeGroup final : public NodeGroup {
```

The `csrIndex` entry per bound node is `NodeCSRIndex`
(`csr_node_group.h:30-59`), and it has a nice compression of its own:
if the node's transient rows happen to be consecutive it stores
`isSequential = true` and just a (start, length) pair; otherwise it
stores the explicit row list.

```
 read(node i) = persistent CSR slice  ∪  transient rows from csrIndex[i]
 checkpoint   = rebuild ONE node group's CSR (oldHeader -> newHeader)
```

The rebuild granularity is the point, and now it is a number:

```
 FalkorDB `Delta_Matrix_sync`: rebuilds a whole matrix
                               ≈ 16.0e6 entries on this graph
 kuzu checkpoint:              rebuilds one node group
                               = 16.0e6 / 8 = 2.0e6 rows
 ratio = 8× smaller worst-case stall, and it does not grow with the
         graph — it grows with 131 072 nodes' worth of edges, full stop
```

Same LSM-shaped answer as FalkorDB's Delta_Matrix (read-optimal core +
mutable overlay + deferred merge) with a different merge granularity.
Why it matters: merge granularity decides the worst-case write stall —
bounding it per group is the disk-friendly choice for a system that
checkpoints. The failure mode it does *not* fix is a supernode: all
of one node's edges live in the node group of its **source id**, so a
node with 6 565 edges concentrates 6 565 rows in one group's slack
budget, and repeated inserts on it will force that group's rebuild
over and over while the other seven sit idle.

### Step 4 — the Intersect operator: worst-case optimal joins where they pay

> **In:** a set of bound node pairs and their sorted neighbour lists.
> **Out:** the intersection of those lists — the third variable of a
> cyclic pattern, produced directly instead of enumerated and
> filtered.

For cyclic patterns, binary join plans are asymptotically wrong. The
triangle `(a)->(b), (b)->(c), (a)->(c)` via pairwise joins can
materialize Θ(m²) intermediate pairs when the true output is at most
m^1.5 — the AGM bound, whose statement and attribution are in
[reading-wcoj.md](reading-wcoj.md) Step 2. On this topic's graph:

```
 m = 16.0e6 edges
 binary-join intermediate:  m²    = 2.56e14 pairs
 AGM ceiling on the output: m^1.5 = 16.0e6 × √(16.0e6)
                                  = 16.0e6 × 4 000 = 6.4e10
 ratio = m² / m^1.5 = √m = 4 000×
```

kuzu's physical answer is an `Intersect` operator. Read it in the
header first:

```cpp
// src/include/processor/operator/intersect/intersect.h
    29  class Intersect : public PhysicalOperator {
  //  ... 30-53: elided — constructor, init, getNextTuplesInternal, copy ...
    54  private:
    55      // For each build side, probe its HT and return a vector of matched flat tuples.
    56      void probeHTs();
    57      // Left is always the one with less num of values.
    58      static void twoWayIntersect(common::nodeID_t* leftNodeIDs, common::SelectionVector& lSelVector,
    59          common::nodeID_t* rightNodeIDs, common::SelectionVector& rSelVector);
    60      void intersectLists(const std::vector<common::overflow_value_t>& listsToIntersect);
```

The kernel is a plain sorted merge — which is why the lists must be
sorted, and the source is unambiguous about it:

```cpp
// src/processor/operator/intersect/intersect.cpp
    65  void Intersect::twoWayIntersect(nodeID_t* leftNodeIDs, SelectionVector& lSelVector,
    66      nodeID_t* rightNodeIDs, SelectionVector& rSelVector) {
    67      KU_ASSERT(lSelVector.getSelSize() <= rSelVector.getSelSize());
  //  ... 68-71: elided — buffers and cursors ...
    72      while (leftPosition < lSelVector.getSelSize() && rightPosition < rSelVector.getSelSize()) {
  //  ... 73-74: elided ...
    75          if (leftNodeID < rightNodeID) {
    76              leftPosition++;
    77          } else if (leftNodeID > rightNodeID) {
    78              rightPosition++;
    79          } else {
  //  ... 80-82: elided — record the match in both selection vectors ...
    83              leftPosition++;
    84              rightPosition++;
    85              outputValuePosition++;
    86          }
    87      }
```

Two details make this the skew-aware version rather than the naive
one:

```cpp
// src/processor/operator/intersect/intersect.cpp
   103  static std::vector<uint32_t> swapSmallestListToFront(std::vector<overflow_value_t>& lists) {
  //  ... 104-107: elided ...
   108      for (auto i = 1u; i < lists.size(); i++) {
   109          if (lists[i].numElements < lists[smallestListIdx].numElements) {
   110              smallestListIdx = i;
   111          }
   112      }
```

The smallest list goes first, so the fold starts from the tightest
constraint — which bounds the running intermediate by the *smallest*
degree in the pattern rather than the largest. That is the same idea
as Generic Join picking the smallest candidate set per variable
([reading-wcoj.md](reading-wcoj.md) Step 3).

And the sortedness the merge assumes is guaranteed on the build side,
not hoped for:

```cpp
// src/include/processor/operator/intersect/intersect_build.h
    35  class IntersectBuild final : public HashJoinBuild {
  //  ... 36-44: elided — type tag and constructor ...
    45      uint64_t appendVectors() final {
    46          KU_ASSERT(keyVectors.size() == 1);
    47          return hashTable->appendVectorWithSorting(keyVectors[0], payloadVectors);
    48      }
```

`IntersectBuild` *is* a `HashJoinBuild` with one method overridden —
`appendVectorWithSorting` instead of the plain append. That single
override is the entire difference, and it is why Step 2's CSR
ordering is a precondition rather than a nicety.

Note it is a **hybrid**: the optimizer picks Intersect only where
cyclic patterns make binary joins asymptotically wrong; chains and
trees stay ordinary binary hash joins (the topic-10/11 machinery).
Note also the honest gap between this code and the paper: CIDR '23 §1
describes the wco join as being built on **ASP-Join**
(accumulate-semijoin-probe, a three-pipeline hash join using sideways
information passing), which is not what the `Intersect` operator at
this pin does. Read the operator for what it is — a multiway sorted
intersection over hash-table-resident sorted lists — and read the
paper for the direction the system was heading. Why it matters: WCOJ
is a scalpel, not a religion — kuzu shows it slotting into a standard
vectorized plan as one more operator.

### Step 5 — factorization: defer the cross product

> **In:** a multi-hop pattern whose flat result is a product of
> degrees.
> **Out:** a factorized intermediate — groups plus multiplicities —
> whose size is a *sum* of degrees, with the product deferred until
> some operator actually demands flat tuples.

One-to-many expands multiply rows. The paper's own worked example is
the cleanest statement of the problem:

> "Consider a 𝑘-regular database, where a node 𝑣ᵢ has 𝑘
> outgoing/incoming neighbors … Suppose, Karim has one account 𝑣₁, so
> the output has 𝑘² tuples. Figure 1 shows both the flat and the
> succinct factorized representation of this output."
> — CIDR '23 §1

The factorized form the paper writes for that example is
`T_{v₁} = {k backward neighbours} × (v₁, Karim) × {k forward
neighbours}` — a product *expression*, not a product. Count both
representations:

```
 flat:        k² tuples
 factorized:  k + 1 + k = 2k + 1 values

 k = 11    (this graph's p50 degree, notes.md):
   flat 121,          factorized 23         →  5.3×
 k = 6 565 (this graph's max degree):
   flat 43 099 225,   factorized 13 131     →  3 282×
```

The saving is not a constant factor — it is k²/(2k+1) ≈ k/2, so it
grows with the skew that this topic's headline is about. kuzu keeps
vectors factorized in its `DataChunk`s, and the whole mechanism is one
two-valued enum plus the flag that carries it:

```cpp
// src/include/common/data_chunk/data_chunk_state.h
     8  // F stands for Factorization
     9  enum class FStateType : uint8_t {
    10      FLAT = 0,
    11      UNFLAT = 1,
    12  };
  //  ... 13-24: elided — the class, its capacity ctor, size init ...
    25      bool isFlat() const { return fStateType == FStateType::FLAT; }
    26      void setToFlat() { fStateType = FStateType::FLAT; }
    27      void setToUnflat() { fStateType = FStateType::UNFLAT; }
```

An `UNFLAT` chunk *is* the group — all b's for one a, carried once
with the a-side held flat beside it — and `setToFlat()` is where an
operator pays for the cross product it had been deferring (topic 11's
vector-type flags, pushed further). Aggregations never call it:

```rust
// ILLUSTRATION — not kuzu source. The real flag is
// src/include/common/data_chunk/data_chunk_state.h:9-12 (FLAT/UNFLAT);
// the point here is only the arithmetic that UNFLAT makes legal.
fn two_hop_count(csr: &Csr) -> u64 {
    (0..csr.n)
        .map(|a| {
            csr.neighbors(a).iter()
                .map(|&b| csr.degree(b) as u64)   // multiplicity, not rows
                .sum::<u64>()
        })
        .sum()   // matrix spelling: the grand sum of A²'s path counts
}
```

FalkorDB's matrix spelling of the same fact: A² holds PATH COUNTS as
its values, so the grand sum of A² *is* this number — the algebra
factorizes for you. Why it matters: factorization is the
executor-level answer to the blowup that WCOJ answers at the plan
level; together they are why kuzu can run multi-hop patterns a flat
vectorized engine chokes on.

## Where each step lives in the code

[kuzu](https://github.com/kuzudb/kuzu) pinned at `89f0263`.

| Step | Anchor | What is there |
|---|---|---|
| 1 | `src/include/storage/table/csr_node_group.h:162-163` | column 0 = neighbour id, column 1 = rel id |
| 1 | `src/include/storage/table/csr_node_group.h:21-24` | `csr_list_t` — a (startRow, length) pair |
| 2 | `src/include/storage/table/csr_node_group.h:99-110` | `PackedCSRInfo` — calibrator tree height, density step |
| 2 | `src/include/common/constants.h:78-79` | `PACKED_CSR_DENSITY = 0.8`, `LEAF_HIGH_CSR_DENSITY = 1.0` |
| 2 | `cmake/templates/system_config.h.in:47-55` | node group size, leaf region size, chunk capacity |
| 2 | `CMakeLists.txt:114-130` | the defaults: page 2^12, vector 2^11, node group 2^17 |
| 2 | `src/include/storage/table/csr_node_group.h:114-146` | `CSRNodeGroupScanState`; `header` at `:117`, built at `:141-142` |
| 3 | `src/include/storage/table/csr_node_group.h:165-171` | the design comment — read this first |
| 3 | `src/include/storage/table/csr_node_group.h:172-174` | `class CSRNodeGroup`, `DEFAULT_PACKED_CSR_INFO` |
| 3 | `src/include/storage/table/csr_node_group.h:30-59` | `NodeCSRIndex` — sequential or explicit row list |
| 3 | `src/include/storage/table/csr_node_group.h:148-160` | checkpoint state: `oldHeader` `:152`, `newHeader` `:153` |
| 4 | `src/include/processor/operator/intersect/intersect.h:29, 56-60` | the operator and its three private kernels |
| 4 | `src/processor/operator/intersect/intersect.cpp:65-90` | `twoWayIntersect` — the sorted merge |
| 4 | `src/processor/operator/intersect/intersect.cpp:103-118` | `swapSmallestListToFront` — the skew heuristic |
| 4 | `src/include/processor/operator/intersect/intersect_build.h:35, 45-48` | `appendVectorWithSorting` — where sortedness comes from |
| 5 | `src/include/common/data_chunk/data_chunk_state.h:8-12, 25-27` | `FStateType::FLAT`/`UNFLAT` — the factorization flag |
| 5 | CIDR '23 §1 and §3.1 | the factorized-vector design the code does not narrate |

Read order: the design comment at `csr_node_group.h:165-171`, then
`PackedCSRInfo` at `:99-110` with `constants.h:78-79` and
`system_config.h.in:47-55` open beside it so the constants resolve,
then `intersect.cpp:65-118` — the merge and the smallest-list-first
heuristic are forty readable lines and they are the whole of Step 4.

One number worth carrying between chapters: kuzu's page size is 4 KiB
(`CMakeLists.txt:114-116`, `KUZU_PAGE_SIZE_LOG2 = 12`, corroborated by
CIDR '23 §2 "fixed page sizes (4KB)"), against neo4j's 8 KiB
(`PageCache.java:49`). Halving the page halves the read amplification
of a random single-record lookup and doubles the number of pages a
sequential scan must fault.

## Questions (answer in notes.md)

1. Rebuild-per-node-group bounds update cost. What's the worst case —
   which insert pattern still hurts? (Hint: supernode crossing groups.)
2. Why must adjacency lists be SORTED for Intersect? What does the
   build side (`intersect_build.h`) have to guarantee?
3. Triangle count on m=16M edges: estimate intermediates for binary
   join vs AGM bound. How many × saved?
4. Factorized `count(*)` for 2-hop = Σ over a of deg-products. Write
   the matrix expression that computes the same number. (This is
   hop_bench's count!)
5. Kuzu compresses neighbor-id columns with topic-12 encodings. Which
   encoding wins for CSR targets sorted by src, and why? (Think about
   what's monotonic within a run and what isn't.)

## Done when

Answer each before unfolding it.

- [ ] You can explain how a CSR header turns sorted rows into an O(1) expand, and say why kuzu's header stores lengths as well as offsets.

  <details><summary>Answer</summary>

  The offsets array makes node i's rows the range
  `offsets[i] .. offsets[i+1]` — two array reads and a slice, no
  binary search and no pointer chase, in every column at once because
  the columns are row-aligned.

  It stores lengths too (`csr_node_group.h:149-150`) because this is a
  *packed* CSR: `PackedCSRInfo` (`:99-110`) keeps leaf regions at
  `PACKED_CSR_DENSITY = 0.8` (`constants.h:78`), so there is slack
  between one node's rows and the next node's, and `offsets[i+1] −
  offsets[i]` would count the gap. The length column is what makes the
  gaps invisible to a reader.
  </details>

- [ ] You can describe the persistent-CSR-plus-transient-overlay scheme per node group, and state its worst-case update cost in rows.

  <details><summary>Answer</summary>

  `csr_node_group.h:165-171`: persistent data is the checkpointed
  chunk in CSR format; transient data is appended to in-memory
  `chunkedGroups`, with a `csrIndex` mapping each bound node to its
  transient row indices. A read merges both. At checkpoint the group's
  CSR is rebuilt, `oldHeader` → `newHeader` (`:152-153`).

  The cost is bounded by one node group. Defaults:
  `KUZU_NODE_GROUP_SIZE_LOG2 = 17` (`CMakeLists.txt:126`) →
  131 072 nodes per group, so on a 1 M-node / 16 M-edge graph that is
  8 groups and ~2.0 M rows per rebuild — 8× smaller than FalkorDB's
  whole-matrix `Delta_Matrix_sync`, and it stops growing once the
  graph is bigger than one group.

  The pattern that still hurts: repeated inserts on one supernode.
  All of a node's out-edges live in the group indexed by its source
  id, so a 6 565-degree node keeps forcing the same group's rebuild
  while the other seven groups do nothing.
  </details>

- [ ] You can say why Intersect requires sorted adjacency lists, what breaks without that, and which line guarantees it.

  <details><summary>Answer</summary>

  `twoWayIntersect` (`intersect.cpp:65-90`) is a two-cursor merge: it
  advances whichever side holds the smaller id and emits on equality.
  That is O(|A| + |B|) *only* if both sides are sorted. On unsorted
  input it does not merely get slower — it silently produces the wrong
  answer, because it advances past values it will never revisit.

  The guarantee is `intersect_build.h:45-48`: `IntersectBuild`
  subclasses `HashJoinBuild` and overrides `appendVectors()` to call
  `appendVectorWithSorting`. One method. That is also why Step 2's
  CSR ordering matters — the storage already delivers sorted lists,
  so the build side is cheap.
  </details>

- [ ] You can estimate intermediate sizes for a triangle count under a binary plan against a WCOJ plan on this topic's 16 M edge graph.

  <details><summary>Answer</summary>

  Binary plan: joining two of the three edge relations first
  materializes up to m² = (16.0e6)² = 2.56e14 candidate pairs before
  the third edge filters them.

  WCOJ: the AGM bound caps the *output* at m^{ρ*} with ρ* = 3/2 for
  the triangle, i.e. m^1.5 = 16.0e6 × 4 000 = 6.4e10, and a
  worst-case-optimal algorithm runs in time proportional to that
  bound rather than to the intermediate.

  Ratio m²/m^1.5 = √m = 4 000×. The attribution matters and is in
  [reading-wcoj.md](reading-wcoj.md): the upper bound is Grohe–Marx's,
  the matching lower bound is Atserias–Grohe–Marx's, and "AGM bound"
  names the pair.
  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  Question 4's matrix expression: the factorized 2-hop count is
  `Σ_a Σ_{b ∈ N(a)} deg(b)`, which is the grand sum of A² — i.e.
  `1ᵀ A² 1` over the integer semiring, where A² accumulates path
  counts as its values. That is the same number `hop_bench` computes,
  and it is computable without ever materializing the
  `Σ_a deg(a)·deg(b)` tuples a flat plan would build.

  Question 5: within one node group the neighbour-id column is sorted
  *within each source's run* but restarts at every new source, so it
  is piecewise-monotonic rather than monotonic. Frame-of-reference or
  delta encoding per run wins; a single global delta encoding does
  not, because it hits a large negative delta at every run boundary.
  </details>

## References

**Papers**
- Feng, Jin, Chen, Liu, Salihoğlu — "KÙZU Graph Database Management
  System" (CIDR 2023). §1 has the k-regular factorization example and
  the ASP-Join description; §2 *Storage and Indices* has the
  double-indexed CSR, the 4 KB page size and the GClock buffer
  manager; §3.1 covers factorized vectors. Note the paper predates the
  pinned revision — where they disagree, the code is the authority.

**Code** (all line numbers verified at kuzu `89f0263`)

| File | Lines | What |
|---|---|---|
| `src/include/storage/table/csr_node_group.h` | 21-24, 30-59 | `csr_list_t`, `NodeCSRIndex` |
| `src/include/storage/table/csr_node_group.h` | 99-110 | `PackedCSRInfo` |
| `src/include/storage/table/csr_node_group.h` | 114-146 | scan state; header at 117, built at 141-142 |
| `src/include/storage/table/csr_node_group.h` | 148-160 | checkpoint state, `oldHeader`/`newHeader` |
| `src/include/storage/table/csr_node_group.h` | 162-163, 165-172 | column ids; the design comment; the class |
| `src/include/common/constants.h` | 78-79 | packed-CSR densities |
| `cmake/templates/system_config.h.in` | 25, 30-31, 47-55 | vector capacity, page size, node group / leaf region |
| `CMakeLists.txt` | 114-130 | the defaults for all three |
| `src/include/processor/operator/intersect/intersect.h` | 29, 54-60 | the operator, its kernels |
| `src/processor/operator/intersect/intersect.cpp` | 65-90, 103-118 | the sorted merge; smallest-list-first |
| `src/include/processor/operator/intersect/intersect_build.h` | 35, 45-48 | the one overridden method |
| `src/include/common/data_chunk/data_chunk_state.h` | 8-12, 25-27 | `FStateType` — flat vs unflat |
| `src/antlr4/Cypher.g4` | 917 lines | the grammar, if you want to see the surface language |
