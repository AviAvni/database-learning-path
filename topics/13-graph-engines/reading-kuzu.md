# Kùzu: DuckDB for graphs

kuzu is "DuckDB for graphs": columnar disk-based storage, vectorized
execution — and two graph-specific ideas worth stealing: CSR that
survives updates via node groups, and a worst-case-optimal Intersect
operator embedded in an otherwise binary-join plan. Before the C++
(read alongside the CIDR '23 system paper), this chapter builds the
design step by step: edges as a columnar table, CSR as an index over
it, the per-node-group update fix, the Intersect operator, and
factorization.

## The problem in one sentence

If edges are just rows of a columnar table clustered by source node,
every topic-12 trick (compression, zone maps, vectorized scans)
applies to adjacency for free — the two things a relational engine
still can't do are surviving single-edge inserts into a sorted
structure and joining cyclic patterns without an O(m²) blowup, and
kuzu ships one mechanism for each.

## The concepts, step by step

### Step 1 — adjacency is a columnar table clustered by source

kuzu stores a relationship table the way DuckDB stores any table —
in **node groups** (horizontal slices, ≈ DuckDB's row groups from
topic 12), each column separately — with one twist: the rows are
edges, sorted by source node id, and column 0 is the neighbor id,
column 1 the rel id, edge properties follow:

```
 rel table (one node group), sorted by src:
 src:      0    0    1    3    3    3   ...
 nbr (c0): 5    9    5    2    7    8   ...
 rel (c1): e0   e1   e2   e3   e4   e5  ...
 props:    ...columns like any table...
```

Because all of node 3's edges are adjacent rows, "expand node 3" is a
contiguous slice of every column — and compression, zone maps, and
vectorized scans apply to edges for free. Why it matters: three other
engines in this topic built custom edge storage; kuzu's bet is that
the columnar machinery already solved storage, and graphs only need
two extra operators on top.

### Step 2 — the CSR header: turning sorted rows into O(1) expand

To find node 3's slice without searching, each node group carries a
**CSR header** (compressed sparse row — an offsets array where entry
i holds the position where node i's edges start; node i's edges are
rows `offsets[i] .. offsets[i+1]`). So adjacency = a columnar table
clustered by src **with a CSR index on top**:

```
 offsets: [0, 2, 3, 3, 6, ...]      neighbors(3) = rows 3..6 of the group
```

The header costs one array per node group and turns expand into slice
arithmetic — no binary search, no pointer chase. Why it matters: this
is the same CSR as FalkorDB's matrices-in-CSR, arrived at from the
relational direction — the representations converge; what differs is
the machinery around them.

### Step 3 — surviving updates: persistent CSR + transient overlay, per node group

CSR hates single-edge inserts (everything after the insertion point
shifts), so kuzu splits each node group in two: **persistent data**
(the checkpointed, CSR-formatted chunk) plus **transient data**
(in-memory chunked buffers, append-only, with a `csrIndex` mapping
bound node → row indices); reads merge both. At checkpoint, the
transient rows are merged into a rebuilt CSR *for that node group
only* — **update pain is bounded per node group**, not per graph:

```
 read(node i) = persistent CSR slice  ∪  transient rows for i
 checkpoint   = rebuild ONE node group's CSR (oldHeader -> newHeader)
```

Same LSM-shaped answer as FalkorDB's Delta_Matrix (read-optimal core +
mutable overlay + deferred merge), with a different merge granularity:
FalkorDB rebuilds a whole matrix on `wait()`; kuzu rebuilds one node
group of, say, 64K nodes. Why it matters: merge granularity decides
the worst-case write stall — bounding it per group is the
disk-friendly choice for a system that checkpoints.

### Step 4 — the Intersect operator: worst-case optimal joins where they pay

For cyclic patterns, binary join plans are asymptotically wrong — the
triangle `(a)->(b), (b)->(c), (a)->(c)` via pairwise joins can
materialize Θ(m²) intermediate (a,b,c-candidate) pairs when the true
output is at most m^1.5 (the AGM bound — see
[reading-wcoj.md](reading-wcoj.md) for the theory). kuzu's fix is a
physical `Intersect` operator: binary-join to get (a,b) pairs, then
for each pair **intersect N(a) ∩ N(b)** — the sorted neighbor lists
from Step 2's CSR slices — to produce c directly, never enumerating
candidates a later edge would kill. The build side
(`intersect_build.h`) prepares sorted adjacency lists in a hash table
keyed by node.

Note it's a **hybrid**: the optimizer picks Intersect only where
cyclic patterns make binary joins asymptotically wrong; chains and
trees stay ordinary binary hash joins (the topic-10/11 machinery).
Why it matters: WCOJ (worst-case optimal join — a join algorithm whose
runtime matches the AGM output bound) is a scalpel, not a religion —
kuzu shows it slotting into a standard vectorized plan as one more
operator.

### Step 5 — factorization: defer the cross product

One-to-many expands multiply rows: flat execution of
`MATCH (a)-[]->(b)-[]->(c)` materializes Σ deg(a)·deg(b) tuples — on a
power-law graph, billions of rows to represent what is structurally
"for each a: a list of b's; for each b: a list of c's". kuzu keeps
vectors **factorized**: a DataChunk can carry "unflat" vectors — a
group (all b's for one a) with a multiplicity — deferring the cross
product until an operator truly needs flat tuples (topic 11's
vector-type flags, pushed further). Aggregations never need it:

```rust
// factorized count(*) for a 2-hop: multiply group SIZES, never
// materialize the Σ deg(a)·deg(b) tuples a flat plan would build
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
its values — the algebra factorizes for you. Why it matters:
factorization is the executor-level answer to the blowup that WCOJ
answers at the plan level; together they're why kuzu can run
multi-hop patterns a flat vectorized engine chokes on.

## Where each step lives in the code

A shallow clone of [kuzu](https://github.com/kuzudb/kuzu); two headers
carry the chapter:

- **Steps 1–3** — `src/include/storage/table/csr_node_group.h`:
  - `:165-171` — the design comment (read it first: persistent CSR
    chunk + transient in-memory chunked groups + `csrIndex`, reads
    merge both — the storage story in one comment)
  - `:172` — `class CSRNodeGroup final : public NodeGroup` — rel
    tables reuse the node-group machinery; `:162-163` — column 0 is
    neighbor id, column 1 rel id, properties follow
  - `InMemChunkedCSRHeader` (`:117`, `:141`) — offsets+lengths built
    in memory; checkpoint's per-group rebuild via
    `oldHeader`/`newHeader` (`:152-153`)
- **Step 4** — `src/include/processor/operator/intersect/intersect.h:29`
  `class Intersect : public PhysicalOperator`, plus
  `intersect_build.h:35` (building sorted adjacency lists into a hash
  table keyed by node).
- **Step 5** — no single anchor; the CIDR '23 paper's
  §vectorization/factorization discussion is the part the code doesn't
  narrate — read it after the two headers.

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

## References

**Papers**
- Feng, Gupta, Jin, et al. — "KÙZU Graph Database Management System"
  (CIDR 2023) — the §vectorization/factorization discussion is the
  part the code doesn't narrate

**Code**
- [kuzu](https://github.com/kuzudb/kuzu) (shallow clone) —
  `src/include/storage/table/csr_node_group.h` (the design comment at
  :165-171 is the storage story),
  `src/include/processor/operator/intersect/intersect.h` +
  `intersect_build.h` (the WCOJ operator)
