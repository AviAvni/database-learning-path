# Topic 52 notes — graph reordering and compression

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin compress_bench`. Seeded generator (`gen.rs`,
`GenParams::LANE1`, seed 52; `shuffle_perm` seed 1 for the random ids): every
count, size and miss rate below reproduces exactly on any machine — the
bits-per-edge, LogGap, simulated-miss and BFS-reach columns were identical
between two runs. Timings are best of 5 (scans) and of 2 (BFS) in one run,
taken through the machine's bench lock with `getloadavg` recorded.

| run | load average (1/5/15) at start | CSR scan, random → planted (ns/edge) | varint scan, random → planted | BFS, random → planted (ms) |
|---|---|---|---|---|
| quoted in README (locked) | 5.27 7.64 13.42 | 3.27 → 2.44 | 9.95 → 5.44 | 2541 → 1859 |
| stub-shape check (not locked) | 14.93 17.64 22.75 | 3.87 → 2.81 | 12.17 → 6.28 | 2809 → 2202 |

The ordering of the timings is stable across the two runs (random slowest,
planted fastest scan, BFS order fastest BFS); absolute values move by about 20%
with load. Quote the sizes and counts, not the milliseconds.

### Lane 1 — one graph, four numberings

| ordering | order ms | LogGap | γ bits/edge | varint bits/edge | + vertex arrays | sim. miss % | CSR ns/e | varint ns/e | BFS ms |
|---|---|---|---|---|---|---|---|---|---|
| random (as loaded) | 87 | 18.01 | 35.78 | 24.11 | 30.04 | 90.3 | 3.27 | 9.95 | 2541.0 |
| in-degree sorted | 308 | 17.74 | 35.35 | 24.00 | 29.93 | 85.9 | 3.06 | 9.24 | 2535.5 |
| BFS | 1023 | 14.68 | 29.68 | 20.98 | 26.91 | 62.0 | 2.71 | 7.95 | 1591.7 |
| planted (oracle) | 0 | 8.72 | 18.17 | 14.40 | 20.33 | 23.5 | 2.44 | 5.44 | 1859.1 |

- n = 6,000,000; m = 64,747,251; max out-degree 1,756; 21,972 planted
  communities. Plain CSR: 32 bits/edge, 34.97 with the offset array.
- Scan checksum `e5358af47cfd3b71`; BFS from the three highest-degree vertices
  reaches 17,992,203 vertices in total, examines 194,160,603 edges, depth sum 26
  — identical under every labelling.
- **The headline.** Same graph: random → planted ids is 24.11 → 14.40
  varint bits/edge (1.67×), simulated misses 90.3% → 23.5%, CSR gather 3.27 →
  2.44 ns/edge (1.34×), BFS 1.37×.
- **Negative result 1:** in-degree sorting changes the byte-coded size by 0.5%
  (24.11 → 24.00) and the miss rate by 4.4 points. Hubs get small ids, lists stay
  scattered.
- **Negative result 2 (for a size-only reading of compression):** the varint scan
  is 2.2× slower than the CSR scan with the best ids (5.44 vs 2.44 ns/edge) and
  3.0× slower with random ids (9.95 vs 3.27). One thread: the decode is the
  bottleneck, not memory. Ligra+'s "compressed is faster" is a 40-thread result.
- **Why 6 M vertices.** At n = 2,000,000 (16 MB of values — a `GenParams` change)
  the gather's value array fits the 16 MB performance-cluster L2 of this machine
  and the orderings did not separate in time: random scanned at 1.37 ns/edge and
  planted at 2.63 in a locked run, the wrong way round, while the simulated miss
  rates (72% vs 18%) still separated cleanly. The simulated cache is what catches
  locality on a graph that fits; the timing lane needs the array to exceed the real
  cache. Apple's line is 128 bytes (`hw.cachelinesize`); the model uses 64.
- Hand check: a random 3-byte gap is `n/d` = 6,000,000 / 10.79 ≈ 555,000, under
  2²¹, so 3 bytes = 24 bits; measured 24.11. A planted intra-community gap is
  under 512, so 1–2 bytes; measured 14.40 mixes 75% of those with long edges.

### Lane 1c — bytes per edge in FalkorDB's matrices (optional probes)

`experiments/probe/graphblas_bytes.c`, linked against FalkorDB's own
`libgraphblas.a` (GraphBLAS v10.5.0, `bin/macos-arm64v8-release/GraphBLAS/`);
1,000,000 × 1,000,000, 8,000,000 uniform edge draws (7,999,964 distinct). Exact.

| matrix | bytes | bytes/edge | offsets/indices |
|---|---|---|---|
| `GrB_BOOL`, all true (iso) | 36,000,108 | 4.500 | 32-bit |
| `GrB_BOOL`, iso off | 44,000,064 | 5.500 | 32-bit |
| `GrB_UINT64`, edge ids | 99,999,812 | 12.500 | 32-bit |

`experiments/probe/falkordb_memory_gen.py` + the bulk loader +
`GRAPH.MEMORY USAGE` (module version 999999 built Jul 24, C engine; 200,000
nodes, seeded random edges; sizes are whole MB):

| edges | relation_matrices_sz_mb | total_graph_sz_mb | edge block |
|---|---|---|---|
| 1,000,000 | 26 | 36 | |
| 2,000,000 | 49 | 68 | 17 |
| 4,000,000 | 94 | 131 | |

Slope 1 M → 4 M: relation matrices (94 − 26) / 3 = 22.7 MB per million edges =
**23.8 B/edge** (MB = 2²⁰); whole graph 95 / 3 = 31.7 MB per million = 33.2
B/edge. The settled-matrix arithmetic from the probe predicts 17–18 B/edge for a
relation plus its boolean transpose; the gap is open (exercise 6).

## Predictions (fill BEFORE implementing the stubs and before running lane 1 again)

| question | prediction | actual |
|---|---|---|
| lane 1: varint bits/edge, random vs planted | | |
| lane 1: in-degree-sorted size vs random's | | |
| lane 1: varint scan ns/edge ÷ CSR scan ns/edge, planted ids | | |
| lane 1: which ordering has the fastest BFS | | |
| lane 2: bisection's varint bits/edge vs the planted oracle's | | |
| lane 2: LogGap of bisection vs the oracle's | | |
| lane 2: time to compute the ordering, vs BFS's | | |
| lane 3: BV-coded bits/edge vs gaps-only, lane-1 graph, planted ids | | |
| lane 3: same on the web-like graph | | |
| lane 3: decode-scan ns/edge vs varint's | | |
| exercise 4: thread count where varint overtakes CSR | | |
| exercise 6: which of pending deltas / 64-bit indices explains 23.8 vs 17–18 | | |

<details><summary>Author's expectations while designing the lanes (written down here after the reference ran — hindsight is possible, so weigh them accordingly)</summary>

- Lane 1, random vs planted: a large size gap, expected from the gap-length
  argument (24 vs 14 bits is a 3-byte vs 1–2-byte difference). Held.
- Lane 1, in-degree sorted: expected a small improvement, not none. Measured
  0.5%: the larger surprise.
- Lane 2: expected bisection to beat the oracle on size, since it optimises the
  objective the oracle merely satisfies. It beat it on LogGap (8.51 vs 8.72) and
  lost by 0.8% on bytes (14.52 vs 14.40): byte granularity does not reward
  every bit of LogGap.
- Lane 3, lane-1 graph: expected references to help a little; they cost 1% —
  the generator has no list similarity, which was not obvious until measured.
  The web-like variant was added to construct the premise.

</details>

## Reference numbers for the exercise lanes

From reference implementations of `bisect::bisection_order` and
`refcode::encode`/`decode_scan` written to the contracts in the stub doc
comments, run through the same bench (load 5.27 at start, bench lock held), all
23 tests green (the 15 provided and the 8 stubs'; the reference also carried a
code round-trip test of its own), then removed. Sizes are
exact; your implementation must reproduce them to the digit only if it makes
the same choices (the bisection's ties, the BV encoder's field codes); expect
the same shape and a few percent.

### Lane 2 — recursive graph bisection (leaf 32, 20 iterations, seed 1, 8 threads)

| ordering | order ms | LogGap | γ | varint | + vertex | sim. miss % | CSR ns/e | varint ns/e | BFS ms |
|---|---|---|---|---|---|---|---|---|---|
| bisection (BP) | 204,425 | 8.51 | 17.82 | 14.52 | 20.45 | 26.0 | 2.59 | 5.97 | 1898.0 |
| planted (oracle) | 0 | 8.72 | 18.17 | 14.40 | 20.33 | 23.5 | 2.44 | 5.44 | 1859.1 |
| BFS | 1,023 | 14.68 | 29.68 | 20.98 | 26.91 | 62.0 | 2.71 | 7.95 | 1591.7 |

- Small-graph test (`GenParams::SMALL`, random ids, leaf 32, 8 iterations):
  LogGap random 9.951, BFS 8.335, bisection 5.788 — 42% below random.
- Implementation: transpose the graph so each data vertex finds its queries; per
  bisection, recompute degrees per query from the current sides, compute gains
  with `d1·(log₂ n1 − log₂(d1+1)) + d2·(…)`, sort each half by (gain
  descending, id), swap pairs while the gains sum > 0; the top 3 recursion
  levels split into 8 threads with private scratch (so results do not depend on
  scheduling). The cost is ~200 s for 64.7 M edges on 8 threads — 200× BFS's.

### Lane 3 — WebGraph-style coding (W = 7, R = 3, L_min = 4; ζ₃ residuals, γ else, unary references)

| graph | ordering | gaps-only | varint | BV coded | enc ms | BV ns/e | varint ns/e | CSR ns/e |
|---|---|---|---|---|---|---|---|---|
| lane-1 (independent lists) | random | 26.11 | 24.11 | 26.30 | 10,325 | 28.50 | 10.04 | 3.28 |
| lane-1 | BFS | 22.10 | 20.98 | 21.97 | 10,331 | 28.05 | 8.78 | 2.76 |
| lane-1 | planted | 14.41 | 14.40 | 14.56 | 10,359 | 24.27 | 5.22 | 2.34 |
| lane-1 | bisection | 14.20 | 14.52 | 14.35 | 10,253 | 24.26 | 5.38 | 2.28 |
| web-like (40% copies) | random | 26.17 | 24.13 | 26.35 | 10,351 | 28.16 | 10.03 | 3.18 |
| web-like | planted | 14.44 | 14.39 | **10.15** | 9,462 | 21.98 | 4.63 | 2.01 |

("gaps-only" is the same coder with `window = 0` and no intervals, i.e. ζ₃ gaps
— not the byte varint.)

- **Negative result:** on the lane-1 graph references and intervals cost 1%
  (14.56 vs 14.41 planted; 26.30 vs 26.11 random): the lists are independent
  draws, so there is nothing to copy and the per-list reference field is pure
  overhead. On the web-like graph the same coder is 29.7% below gaps-only
  (10.15 vs 14.44) — WebGraph's similarity premise, constructed and verified.
  Random ids kill it again: 26.35 vs 26.17.
- Bit codes (γ/ζ₃) are not better than the byte varint on gaps alone: at random
  ids gaps-only is 26.11 bits against 24.11 for varint — a 17-bit gap costs more
  in ζ₃ than in three bytes. On planted ids they tie (14.41 vs 14.40).
- BV decode-scan is 24–28 ns/edge: ~10× the CSR scan and 4.7× the varint scan
  on the web-like graph. The reference decoder is eager (it materialises the
  reference list and merges three sorted sequences into a `Vec`) and untuned;
  WebGraph's lazy iterators exist to avoid this (paper §6, footnote 4).
- Enc time ~10 s for 65 M edges: 8 trial encodings (window 7 + none) per vertex.

## Paper and source numbers worth keeping (each checked against the PDF or the code)

- **WebGraph (WWW'04).** Gap distribution on an 18.5 M-page `.uk` snapshot:
  power law, exponent 1.21 (1.20 transposed), Figures 1–2. Table 7 (`L_min =
  3`, ζ₃ residuals), bits per link, `.uk` 300 M links: `R = ∞` 2.75 / 2.38 /
  2.22 at `W = 1 / 3 / 7`; `R = 3` 3.87 / 3.25 / 3.00; `R = 1` 5.05 / 3.91 /
  3.46. WebBase 118 M pages, 1 G links, best cell 3.08 (transpose 2.89);
  LINK database v3 5.61 / 5.66 (§3). Table 8 (2.4 GHz Pentium, Java, `.uk`):
  sequential 198 / 206 / 233 ns per link at `R = ∞ / 3 / 1`; random access with
  the full offset array 31,237 / 611 / 442 ns; the full offset array is 141.3
  MiB against a 79.0 MiB graph at `R = ∞`. Discrepancy: the paper's formula
  gives ν(−2) = 3, its worked example and Table 5 print 5; `dsi-bitstream`'s
  `to_nat` is the formula.
- **Recursive graph bisection (KDD'16, arXiv v1).** Table 2, BV bits per edge:
  web-Google Natural 20.08 → BP 4.68 (Multiscale 4.10 is lower); LiveJournal
  14.61 → 10.73 (LLP 11.12; Natural beats BFS 14.69 and Minhash 15.07);
  Twitter 21.56 → 11.62. LogGap vs BV bits Pearson 0.9853 (§5.4). Depth
  `log₂ n − 5`, 20 iterations (§5.3); `O(m log n + n log² n)` (Theorem 2).
  LiveJournal 3 min, Twitter 149 min for BP on 32 cores; LLP 23 min on
  LiveJournal (§5.5).
- **Ligra+ (DCC'15).** 49–56% of Ligra's size on average (byte 53%, byte-RLE
  56%, nibble 49%), up to 3× / 4× on `nlpkkt240` and `uk-union`; at most 19
  bits per edge against 32 (Figure 5, Table 1); about 14% faster on average with
  the fastest scheme at 40 cores. Table 2 rows: Twitter PageRank T1 48.6 s
  Ligra / 73.5 byte; T40 2.68 / 2.74; uk-union PageRank T1 74.4 / 56.9, T40
  4.89 / 2.26. "The locality of our real-world graphs is already quite good
  without reordering" (§4).
- **Gorder replication (ReScience 2021).** Gorder first in 40 of 81 series, RCM
  24, ChDFS 16 (Figure 6a); up to 2.5× over the original order (Diameter,
  `sdarc`), 3.7× over random (PageRank, `wiki`). `sdarc` PageRank RAM-miss
  rate: Gorder 3.1%, RCM 3.7%, ChDFS 4.3%, InDegSort 6.0%, Original 6.8%,
  Random 9.0% (Table 3b). Ordering time on `sdarc` (1.94 G edges): InDegSort 85
  s, ChDFS 76 s, RCM 406 s, Gorder 32,488 s (Table 2).
- **FalkorDB / GraphBLAS (this repo's probes).** Settled GraphBLAS v10.5.0
  matrix, 1 M × 1 M, 8 M entries: all-true `GrB_BOOL` 4.5 B/edge (iso),
  non-iso 5.5, `GrB_UINT64` edge ids 12.5, all with 32-bit indices.
  `GRAPH.MEMORY USAGE` slope on bulk-loaded graphs: relation matrices 23.8
  B/edge, whole graph 33.2 B/edge (1 M → 4 M edges, 200 k nodes).

## Cross-topic threads

- **Topic 24 (Leiden) ↔ 52.** Leiden's communities are the cheap version of what
  LLP and bisection compute: vertices sorted by (community, degree) are
  `planted (oracle)` in lane 1, the best order the generator can hand you.
  [reading-louvain-leiden.md](../24-graph-algorithms/reading-louvain-leiden.md)
  builds the algorithm that would produce it on a real graph.
- **Topic 20 (GraphBLAS) ↔ 52.** [reading-graphblas-bytes.md](reading-graphblas-bytes.md)
  counts the bytes of the matrices topic 20 builds; renumbering is a permutation
  of rows and columns of those matrices and changes no result.
- **Topic 13 (graph engines) ↔ 52.** The record stores of Neo4j and Memgraph
  assign ids by allocation; the question this topic raises is whether an engine
  should ever renumber, and what a "frozen" read-mostly adjacency segment could
  do with a compressed layout.
- **Topic 51 (dynamic graph storage) ↔ 52.** A renumbering pass changes every
  stored id; a store that takes writes continuously has to decide when to pay
  for it ([topic 51](../51-dynamic-graph-storage/README.md)).
- **Topic 48 (memory tiering) ↔ 52.** Vertex renumbering by access frequency
  is the same lever as 48's hot/cold placement, applied to ids instead of
  pages.

## Open questions

- Why does FalkorDB's `GRAPH.MEMORY USAGE` slope (23.8 B/edge for relation
  matrices) exceed the settled-matrix arithmetic (17–18)? Pending deltas, index
  width inside the module, or accounting — the smallest experiment is README
  exercise 6.
- Lane 1's scans are single-threaded; Ligra+'s claim is about 40 cores. At what
  thread count does the varint scan overtake the CSR scan on this machine?
- Does bisection's advantage over the planted order on `LogGap` survive on a
  graph with overlapping communities?
- k²-trees (Brisaboa et al.) are the other family for adjacency matrices; the
  paper was not obtained, and no number about them is claimed here.
