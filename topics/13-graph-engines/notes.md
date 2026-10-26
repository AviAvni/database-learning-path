# Topic 13 notes — graph engines

## Baseline (provided lane, Apple M3 Pro, measured 2026-07-28)

`cargo run --release --bin hop_bench` — preferential-attachment graph, 1 M
nodes / 16.0 M directed edges, two-hop `COUNT(DISTINCT)` from 10 000 random
sources and from the 100 highest-degree nodes.
Max degree 6565, p50 degree 11.

| impl | source set | ns/query | checksum (sum over the set) |
|---|---|---|---|
| adj_list (oracle) | random (10 000 sources) | 4 914 | 10 220 457 |
| adj_list (oracle) | supernodes (top-100 degree) | **495 378** | 7 890 665 |
| CSR (yours) | | | stub |
| masked SpMV (yours) | | | stub |

**Same query, same graph, same code path: 101× slower depending on where you
start.** That ratio is the single most important fact about graph workloads and
it has no analogue in the topics before this one. A B-tree point lookup costs
what it costs; a two-hop traversal costs whatever the *degree distribution*
under your start node says it costs, and on a scale-free graph that is a
power law with no useful average. The p50 node has 11 neighbours; the top node
has 6565.

Note the checksum column is a **sum over the source set**, and the two sets are
different sizes — 10 000 random sources against 100 supernodes. Normalise before
comparing anything: random reaches **1022** distinct nodes per query,
a supernode reaches **78 907** — **77× more**. Read raw, the sums say the
opposite (7.9 M for the supernodes against 10.2 M for random), which is the
mistake to keep in mind every time a benchmark prints a total.

So the 101× decomposes: **77× more nodes reached**, and **1.31× worse cost per
node reached** (4.81 ns against 6.28 ns). The second factor is the one worth
chasing, and this lane cannot attribute it — it counts distinct nodes, not edges
traversed, so re-walked overlap and the cache cost of a 79 000-node frontier look
identical from here. Exercise 6 in the README adds the edge counter that
separates them.

Checksums must match across all three implementations per source set — that is
the correctness gate before any timing comparison means anything.

## Predictions (fill BEFORE implementing csr.rs / matrix.rs)

Baseline (provided, measured): adj_list 4914 ns/query random,
495378 ns/query supernodes (101× tail); graph 1M nodes / 16M directed
edges, max degree 6565, p50 degree 11.

| impl | sources | predicted vs adj_list (×) | actual ns/query |
|---|---|---|---|
| csr | random | | |
| csr | supernodes | | |
| matrix (SpMV) | random | | |
| matrix (SpMV) | supernodes | | |

| question | prediction | actual |
|---|---|---|
| does csr beat adj_list at all? (per-node vecs are already contiguous — where's the win?) | | |
| matrix vs csr: what does the frontier materialization cost? | | |
| supernode ratio: does CSR shrink the 101× tail or just shift it? | | |
| CSR build time vs adj_list build time | | |

## Implementation log

- [ ] csr.rs: counting-sort build + slice two_hop; all 5 tests green
- [ ] matrix.rs: spmv_masked + two-SpMV two_hop; all 4 tests green
- [ ] hop_bench full table recorded above (checksums match:
      random=10220457, supernodes=7890665)
- [ ] external: same 2-hop count on FalkorDB
      (`MATCH (a)-[*1..2]->(b) WHERE id(a)=<src> RETURN count(DISTINCT b)`)
      — ns/query here:
- [ ] optional: neo4j same query — ns/query:

Surprises / dead ends:

## Reference numbers for the exercise lanes

Read these after filling the worksheet. A reference solution of the two stubs
(counting-sort CSR; two masked SpMVs with reusable frontier buffers) run on
the same seeded graph and sources as the provided lane, with an edge counter
added (measured 2026-10-26, Apple M3 Pro, quiet machine, three repetitions
each, median shown; the adjacency-list oracle was re-measured in the same run
and sits below the 2026-07-28 baseline above because the machine was idle):

| impl | random (10 000 src) ns/query | supernodes (top-100) ns/query | checksum (random / super) |
|---|---|---|---|
| adj_list (oracle, same run) | 3 029 | 296 180 | 10 220 457 / 7 890 665 |
| CSR | **2 503** | **247 659** | 10 220 457 / 7 890 665 |
| masked SpMV over the CSR | 2 628 | 267 001 | 10 220 457 / 7 890 665 |

Build: adjacency list 0.12 s, CSR 0.05 s (counting sort, no per-node
allocation). Per-query structure (deterministic):

| | random | supernodes | ratio |
|---|---|---|---|
| rows opened (src + frontier-1 nodes) | 17 | 1 624 | 95× |
| **edges scanned** (deg(src) + Σ deg over frontier 1) | 1 042 | 89 034 | **85.4×** |
| distinct nodes reached | 1 022 | 78 907 | 77.2× |
| model bytes touched* | 12 560 | 1 040 888 | 82.9× |

\* 4 B target + 4 B `seen` probe per edge scanned, 8 B of offsets per row,
4 B per reached node. This is arithmetic on the counters, not a hardware
measurement; at 247 659 ns for 1.04 MB it is about 4 GB/s, far under the
machine's bandwidth, so the lane is latency-bound (random `seen` probes), not
bandwidth-bound.

Throughput over edges scanned (Medges/s): CSR 416 random / 360 supernodes;
masked SpMV 396 / 333; adjacency list 344 / 301.

What the numbers say:

- **CSR removes a constant, not the tail.** It is **1.21×** faster than the
  adjacency list from random sources and **1.20×** from supernodes — the cost
  of the one extra dependent miss per row (the `Vec` header) that the flat
  layout avoids. The supernode/random ratio barely moves: adj_list **97.8×**,
  CSR **98.9×**, masked SpMV **101.6×**.
- **The 101× is 85× more edges plus 1.16× more time per edge.** Edges scanned
  grow 85.4×; CSR's ns per edge goes 2.40 → 2.78 (1.16×). The extra per-edge
  cost is plausibly the `seen` probes missing cache once the frontier holds 1 623 nodes (not isolated here).
- **The README's 1.31× residual is mostly extra edges, not cache.** Per
  reached node the oracle costs 2.96 ns (random) and 3.75 ns (supernodes),
  1.26×. Edges scanned per reached node are 1.02 and 1.13 — 1.11× of the
  residual is re-walked overlap (an edge into an already-seen node), and the
  remaining ~1.14× is the cache effect above.
- **Masked SpMV is 5–8% slower than CSR**, the price of writing the frontier
  out as a vector (1 623 entries, ~6.5 KB on a supernode) and reading it
  back. The algebra buys uniformity (every hop is the same kernel, masking is
  the dedup), not speed, on one query. Its win is on batched frontiers, where
  one kernel call serves many sources — not measured here.
- **Per-query work is bounded by the frontier.** The added stub test
  `spmv_touches_only_the_frontiers_targets` checks that a hop stamps `seen`
  only at targets of the frontier's rows and that output ≤ Σ degrees of the
  frontier. It cannot count reads, so a full-scan implementation that never
  writes would pass; the edge counter above is the read-side check.

### C FalkorDB on the same graph

Same graph (1 M nodes, 15 998 310 directed edges, loaded with
`falkordb-bulk-insert` 1.0.6 from CSVs written by the same generator; node
ids equal CSV row order, verified with `id(n) = n.id` on a 5-node graph; each
node carries one extra `id` property), same 10 000 random and 100 supernode
sources, query

```
MATCH (a)-[*1..2]->(b) WHERE id(a)=<src> AND id(b)<><src> RETURN count(DISTINCT b)
```

(`id(b)<>src` because a variable-length path may return to its start; the
Rust lane excludes the source). Plan: `NodeByIdSeek` → `Conditional Variable
Length Traverse` → `Filter` → `Aggregate`. **Every one of the 10 100 per-source
counts equals the Rust oracle's** (checksums 10 220 457 and 7 890 665).

Engine: `~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so` (built
2026-07-24; `MODULE LIST` reports `ver 999999`, a dev build that cannot be
traced to a commit — the repo's `main` was at `25babfe4f` when this ran),
redis-server 8.6.2, `THREAD_COUNT 11`, Python client `FalkorDB` 1.8.1, port
6391, one query at a time through the bench lock. `capstone/BASELINES.md`
pins the v4.22.2 build instead; its module is not on this machine.

| | random | supernodes |
|---|---|---|
| FalkorDB server-side time, median / mean | 183 µs / 324 µs | 14.9 ms / 19.2–21.2 ms |
| FalkorDB wall time through the client, median | 419 µs | 14.9–15.8 ms |
| Rust CSR, ns/query (above) | 2.5 µs | 248 µs |
| FalkorDB ÷ Rust CSR (server median) | 73× | 60× |

`RETURN 1` through the same client costs 214 µs, so the client floor is half
of the random-source wall time and none of it is the traversal. Server-side,
the supernode/random ratio is **81×** at the median (the Rust lanes show
~99×): a fixed per-query cost (parse, plan-cache lookup, result set)
compresses it. The supernode query scans 89 034 edges in 14.9 ms, about
6 M edges/s against CSR's 360 M — most plausibly the generality of the query
pipeline and variable-length path semantics rather than memory
layout (not profiled). The tail (mean 19–21 ms against median 14.9 ms at the supernodes) is
wider than the Rust lane's.

Method caveat: single-run per cell group, closed loop, a Python client; this
is a service time, not a latency under load.

### Sharper FINDINGS row (proposed)

> The same two-hop query is **~98× slower** from supernodes than from random
> nodes on every layout — adjacency list, CSR and masked SpMV — because it
> scans **85× more edges** (89 034 against 1 042) at 1.16× the cost per edge;
> CSR buys a flat 1.2× and does not touch the tail. C FalkorDB returns the
> same counts at 183 µs / 14.9 ms.

## Questions from the reading guides

### GraphBLAS + Delta_Matrix (reading-graphblas-internals.md)

1. Why delta_minus instead of deleting from M (CSR delete cost):
2. dot3 vs saxpy at frontier 10 vs 10⁶:
3. When BITMAP fits a label matrix:
4. Why (M ∪ DP) ∖ DM reads beat flush-per-write:
5. Delta_Matrix → LSM vocabulary map:

### neo4j record store (reading-neo4j-record-store.md)

1. 1000-edge Expand: chain (~110 ns/edge) vs CSR stream — ×:
2. Why 15 B nodes / 34 B rels — field inventory:
3. Delete cost: chain vs CSR vs Delta_Matrix:
4. Property chains vs M12 columns for `WHERE n.age > 65`:
5. Index-free adjacency: what survives DRAM, what died:

### memgraph storage (reading-memgraph-storage.md)

1. Why edges in both endpoints' vectors; FalkorDB's transposed trio:
2. small_vector inline + power-law degrees:
3. Per-object vs per-row delta chains under supernode edge inserts:
4. memgraph vector vs kuzu CSR slice — 16 B triples vs 8 B ids:
5. PageRank on pointer soup vs matrix — where the bus time goes:

### kuzu (reading-kuzu.md)

1. Node-group-bounded rebuilds — which insert pattern still hurts:
2. Why Intersect needs sorted adjacency:
3. Triangle intermediates: binary join vs AGM at m=16M:
4. Factorized count(*) 2-hop as a matrix expression:
5. Best topic-12 encoding for CSR target columns:

### WCOJ (reading-wcoj.md)

1. Star-graph R⋈S intermediates vs triangle output:
2. AGM bound for the 4-cycle:
3. Galloping and power-law skew:
4. Why `C<A>=A²` never materializes A² (dot3):
5. M10 trigger for intersect vs binary join (pattern cyclicity):

### LDBC SNB (reading-ldbc-snb.md)

1. Why timed dependency-tracked inserts:
2. IC5-ish pattern: anchor + expand count + worst representation:
3. Which architecture flatters itself on uniform-degree graphs:
4. Where multi-hop cardinality estimation dies:
5. SF that fits this Mac per representation (bytes/edge each):

## Cross-topic threads

- Pointer chase vs stream = topic 0's cache ladder deciding graph
  architecture: neo4j chains pay ~110 ns/edge, CSR pays bandwidth.
- Delta overlay everywhere (Delta_Matrix, kuzu transient groups,
  GraphBLAS pending tuples) = topic 4's LSM applied to adjacency.
- Masks = predicate pushdown (topic 10) into the kernel; SpMV
  frontier = topic 11's batch, spelled algebraically.
- memgraph = topic 8's N2O deltas + topic 9's skip list, at vertex
  granularity; bit-smuggling ledger: deleted-flag in delta pointer
  low bits (PointerPack), neo4j's 35-bit pointers in the inUse byte.
- Supernodes = the graph-shaped tail: same lesson as Zipfian keys
  (topic 2) and JOB correlations (topic 10) — uniform data lies.

## M13 log (naive adjacency core — the baseline M20 must beat)

- [ ] node/edge store over adjacency lists + label bitmaps
- [ ] scan-anchor-then-expand for `(a:L)-[:R]->(b)`
- [ ] Expand fills M11 batches; bench engine-vs-raw (interpretation
      overhead = the M11 payoff measurement)
- [ ] update-pain notes for the M20 delta overlay design

## Done when

- csr + matrix tests green; hop_bench table filled with matching
  checksums; FalkorDB external comparison recorded.
- Reading-guide questions answered.
- M13 update-pain notes written.
