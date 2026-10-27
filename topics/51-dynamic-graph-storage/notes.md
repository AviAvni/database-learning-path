# Topic 51 notes — dynamic graph storage

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin dyngraph_bench`. Seeded generator (`gen.rs`,
`GraphParams::LANE1`, seed 51): every count below reproduces exactly on any
machine — |V| = 1,048,576; |E| = 16,084,545 after de-dup; max out-degree
39,354; 502,214 vertices with no out-edges; the top 1% of vertices own 49.8% of
the edges; the same 2,000 scan queries touch 36,871,342 edges in every store;
the same checksums and final edge counts (16,086,410 after 1% inserts,
16,103,191 after 10%, 16,083,871 after the half-deletes mix) come out of every
store. Timings were taken with `sysctl -n vm.loadavg` recorded; other agents
were running benchmarks on the machine.

| run | load avg at start | lane 1a splice insert (ms) | 1b scan: CSR / empty delta / `Vec<Vec<u32>>` (ns/edge) | 1c half-deletes: per-edge test ÷ adjacency list scan | 1e T = 10,000: ns/write, flush (ms) |
|---|---|---|---|---|---|
| **quoted in README** (`dyngraph_bench`, stub build) | 4.49 | 0.641 | 0.161 / 0.160 / 0.145 | 0.339 ÷ 0.171 = 1.98× | 468, 6.28 |
| second run, same build | 3.71 | 0.696 | 0.268 / 0.225 / 0.207 | 0.357 ÷ 0.197 = 1.81× | 481, 6.45 |
| reference-solution build (lanes 2–3 implemented) | 4.40 | 0.644 | 0.165 / 0.160 / 0.147 | 0.340 ÷ 0.181 = 1.88× | 464, 6.30 |
| branchless-PMA reference build | 4.54 | 0.659 | 0.161 / 0.167 / 0.150 | 0.340 ÷ 0.193 = 1.76× | 502, 6.88 |
| earlier build, per-edge tombstone scan only | 12.77 | 0.710 | 0.188 / 0.170 / 0.154 | 0.360 ÷ 0.219 = 1.64× | 590, 6.84 |
| earlier build, same | 18.13 | 0.723 | 0.238 / 0.262 / 0.207 | 0.369 ÷ 0.197 = 1.87× | 504, 6.58 |

Absolute scan times move by up to 1.7× between runs as other jobs come and go
(the static CSR's 1b row: 0.161–0.268 ns/edge); the *ratios inside a run* and
the flush times (6.28–6.88 ms at T = 10,000, a 10% spread) are stable. Quote
ratios and counts; the README's absolute scan figures are from the quietest
run.

### Lane 1a — one write into a CSR that splices

| | value |
|---|---|
| edge array / offsets | 64.3 MB / 4.2 MB |
| insert at a uniform random position (100 inserts) | **0.641 ms** each (1,561 / s) |
| delete (100 deletes) | 0.664 ms each |
| bytes moved per write (half of both arrays) | ~34 MB → 53 GB/s |

- **Hand prediction that checks out:** topic 20 Step 1 predicts the cost as
  bytes moved ÷ bandwidth; 34 MB at the 30 GB/s that chapter assumed is 1.1 ms,
  and 0.64 ms measured is 1.8× faster because this machine's `memmove` runs at
  ~53 GB/s. The *shape* — linear in the graph — is the claim and it holds.
- The sanity line is part of the result (topic 12's 19,047,619 GB/s lesson):
  53 GB/s is below any memmove this machine can do.

### Lane 1b/1c — the scan, with and without writes

| store | 1b: nothing pending | 1c 0% | 1c 1% | 1c 10% | 1c 10% half deletes |
|---|---|---|---|---|---|
| static CSR | 0.161 | 0.186 | — | — | — |
| CSR + delta (T = 10,000) | 0.160 | 0.176 | 0.186 | 0.191 | **0.189** |
| CSR + delta, per-edge tombstone test | — | — | — | — | **0.339** |
| `Vec<Vec<u32>>` | 0.145 | 0.175 | 0.178 | 0.181 | 0.171 |
| CSR, splice per write | — | — | 0.203 | — | — |

(ns/edge touched; the 1b column is best of 5 on 2,000 queries, the 1c columns
a single pass over 200 blocks of 1,000 operations.)

| store | 1c 1% (2,000 writes) | 1c 10% (20,000) | 1c half deletes (20,000) |
|---|---|---|---|
| CSR + delta | 220 ns/write | 490 (1 flush, 6.60 ms) | 621 (1 flush, 9.21 ms) |
| per-edge tombstone variant | — | — | 489 (1 flush, 6.63 ms) |
| `Vec<Vec<u32>>` | 1,237 | 555 | 390 |
| CSR, splice per write | **615,536** | — | — |

- **Negative result 1 — the per-edge tombstone test.** A scan that compares
  every edge of a tombstoned row against the row's next tombstone is **1.98×**
  the adjacency list's (0.339 vs 0.171); splitting the row at each tombstone
  with a binary search (`Tombstones::Split`, the default) brings it to 0.189,
  1.11×. The stream's deletes are uniform over *edges*, so hubs hold most
  tombstones and a two-hop scan reads hub rows constantly. The 10%-inserts-only
  stream shows none of this (0.191 vs 0.181).
- **Negative result 2 — the delta's write path once the flush is charged.**
  See 1e: at T = 10,000 the steady-state cost per insert is 782 ns against the
  adjacency list's 555.
- The delta's insert is 220 ns at 1% and 490 ns at 10%: the 10% row includes
  one 6.6 ms flush (330 ns per write when spread over 20,000).
- The 10%-with-deletes row's delta write cost (621) is higher than the
  insert-only row's (490) because the `DM` path does a binary search in the
  row and a sorted insert into a tombstone list per delete, and because the
  flush merges tombstones (9.21 ms vs 6.60).
- **Sanity:** every mix prints "N stores, one logical graph — checksums and
  final edge counts agree" (and the program aborts the comparison if not).

### Lane 1d — delta growth, no flush

`D` inserts and `D/10` deletes applied to a delta that never flushes, then
2,000 queries, best of 5.

| D | pending changes | `DP` rows | scan ns/edge | × empty delta | delta bytes per pending change |
|---|---|---|---|---|---|
| 0 | 0 | 0 | 0.171 | 1.00 | — |
| 1,000 | 1,033 | 906 | 0.161 | 0.94 | 76.65 |
| 10,000 | 10,320 | 7,725 | 0.165 | 0.97 | 65.38 |
| 100,000 | 103,132 | 47,804 | 0.186 | 1.09 | 32.96 |
| 1,000,000 | 1,030,138 | 188,772 | 0.366 | **2.14** | 16.24 |

Four runs of this lane with the split tombstone scan give 0.94–1.05× at 10,000
pending, 1.09–1.23× at 100,000 and 2.14–2.40× at a million. The same lane
with the per-edge test (earlier builds) gave 1.4–1.7× at 10,000, 2.1–2.8× at
100,000 and 2.7–3.5× at a million: **most of what looked like "delta growth"
was the tombstone test**, and what is left is flat until the side map no
longer fits the cache (16.24 bytes per pending change × 1,030,138 pending
changes = 16.7 MB of side structure). The cache explanation was not isolated.

### Lane 1e — the flush threshold, at 10% inserts

20,000 writes, 18,646 of them applied inserts.

| T | scan ns/edge | ns/write | flushes | max flush (ms) | mean flush (ms) | ns/write excluding the flush |
|---|---|---|---|---|---|---|
| 1,000 | 0.193 | 2,959 | 18 | 4.93 | 3.11 | 159 |
| 10,000 | 0.188 | 468 | 1 | 6.28 | 6.28 | 154 |
| 100,000 | 0.190 | 154 | 0 | — | — | 154 |
| never | 0.185 | 146 | 0 | — | — | 146 |

- **c + F/T, worked** (reading-falkordb-delta-in-context.md Step 2): T = 1,000:
  18 × 3.11 ms ÷ 20,000 = 2,799 ns, plus 159 = 2,958 (measured 2,959);
  T = 10,000: 6.28 ms ÷ 20,000 = 314, plus 154 = 468 (measured 468).
  **Steady state** at T = 10,000: 6.28 ms ÷ 10,000 = 628 ns per insert, plus
  154 = 782 — 1.4× the adjacency list's 555 in the same stream.
- **Hand prediction that checks out:** a flush reads and rewrites the 68.5 MB
  of arrays, ≥ 137 MB of traffic; at the ~53 GB/s this machine's `memmove`
  reaches that is 2.6 ms, and the T = 1,000 flush (which touches few rows)
  measured 3.11 ms: the floor is one pass over the array.
- The scan column is flat (0.185–0.193): across 20,000 writes no pending
  amount reached the lane-1d knee. It is a single pass and moves by ±30% from
  run to run; quote lane 1d for scan cost, this lane for write cost.
- **Pause:** the largest single flush was 4.93 ms (T = 1,000) and 6.28 ms
  (T = 10,000) — the `max flush ms` column. Not spread: one write waits.

### Lane 1f — bytes per edge (accounted from capacities; allocator overhead excluded)

| store | B/edge |
|---|---|
| static CSR (u32 offsets + u32 edges) | 4.26 |
| CSR + delta, 100,000 pending inserts | 4.43 |
| `Vec<Vec<u32>>` (24-byte header per vertex, 502,214 of them empty) | 5.56 |
| lane 2 reference (sorted blocks, capacity 256) | 7.98 |
| lane 3 reference (PMA, leaf 16, slot = 4 bytes) | 8.87 |

FalkorDB's arrays use 8-byte indices, so its settled matrix is about twice the
CSR row above, and the transposed twin doubles it again; this lane does not
measure GraphBLAS.

## C FalkorDB datapoint (`scripts/falkordb_datapoint.py`)

Same graph and starts as lane 1, loaded into the FalkorDB C engine
(`~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so`, redis-server 8.6.2,
port 6390, shut down afterwards): 1,048,576 nodes and 16,084,545 relations in
23.0 s through the bulk loader. The Cypher is
`MATCH (a)-[:E]->(b)-[:E]->(c) WHERE ID(a) = $s RETURN count(c)` for 200 of lane
1's seeded starts and `MATCH (a), (b) WHERE ID(a) = $u AND ID(b) = $v MERGE
(a)-[:E]->(b)` for inserts. `DELTA_MAX_PENDING_CHANGES` was the default,
10,000.

**The answer is checked.** FalkorDB's `count(c)` equals the Rust-computed
number of *distinct* vertices two hops away for all 200 starts (1,837,771 in
all), not the number of two-hop *paths* (3,742,650): its algebraic traversal
keeps one row per reached vertex. So "edges touched" for a ns/edge figure is
a nominal 3,745,348 (first-hop plus second-hop path edges, what the in-process
stores touch) and the figure is an upper-bound-style comparison, not a like-for-
like one.

Run A (the one quoted in README; load average 5.85 at the start, 4.33 at the
end) and what it showed:

| point | engine µs per two-hop query (200 starts) | × empty | first query after the inserts (µs) |
|---|---|---|---|
| warm-up (20 queries) | 853 | — | 333 |
| empty delta, right after the bulk load | 891 | 1.00 | 142 |
| 5,000 inserted | 921 | 1.03 | 276 |
| 10,000 inserted | 971 | 1.09 | 439 |
| 12,500 inserted (flush crossed) | 947 | 1.06 | 229 |
| 20,000 inserted | 961 | 1.08 | 450 |

| 2,500 inserts | mean (µs) | median | p99 | max (insert #) |
|---|---|---|---|---|
| 0 → 2,500 | 151 | 152 | 190 | 593 (#1753) |
| 2,500 → 5,000 | 199 | 204 | 248 | 366 (#1369) |
| 5,000 → 7,500 | 283 | 276 | 589 | **9,353 (#2456)** |
| 7,500 → 10,000 | 331 | 335 | 454 | 1,360 (#0) |
| 10,000 → 12,500 | 224 | 166 | 417 | **24,156 (#734)** |
| 12,500 → 15,000 | 192 | 194 | 260 | 610 (#1784) |
| 15,000 → 17,500 | 231 | 236 | 289 | 410 (#1223) |
| 17,500 → 20,000 | 276 | 277 | 389 | 769 (#2484) |

- **The flush is the 24.2 ms spike** at insert #10,734 (cumulative; the
  candidate inserts are `MERGE`d and about 7% already exist, so ~10,000 were
  applied — the threshold). It is 72× the preceding batch's median (335 µs) and
  folds whichever of the relation's matrices, its adjacency matrix and their
  transposed twins had crossed the bound. The in-process model's flush of the
  single-direction CSR at T = 10,000 was 6.28 ms; the ratio, 3.8× (4.7× in
  run B), is not explained by a doubled write alone (that predicts 2×) and was
  not decomposed (8-byte indices, `assign` vs. a merge, how many matrices were
  folded).
- A second, smaller spike (9.4 ms at cumulative insert #7,956) has no cause
  this topic verified.
- **Median insert latency tracks the pending count**: 152 → 204 → 276 → 335 µs
  over the first 10,000, then 166 → 194 → 236 → 277 after the flush. The
  slope is ~24 µs per thousand pending changes. The mechanism was not
  verified; a per-query materialisation of `DP` is the first thing to check.
- **Reads are nearly unaffected**: ≤ +9% at any point, no step at the flush.
  At this load the noise on a 200-query mean is a few percent (the warm-up
  run, 20 queries, reads 853 µs).
- `starts.csv`: 200 starts, 2,698 first-hop edges, 3,742,650 second-hop path
  edges, 1,837,771 distinct second-hop vertices; about half of the vertices
  have no out-edges, so many of the 200 queries return immediately — the mean
  is dominated by the few that touch hubs.

**Run B** (load average 4.95 at the start, 4.80 at the end), same script and
state: engine µs per query 916 empty, 1,055 at 5,000 inserted (+15%), 965 at
10,000 (+5%), 978 at 12,500 (+7%), 1,023 at 20,000 (+12%); median insert
latency per 2,500 inserts 168, 203, 279, 337, then **164** after the flush,
188, 252, 334 µs; **29.4 ms** for insert #734 of the 10,000 → 12,500 batch —
the same insert index as run A's 24.2 ms spike, so the flush fires at the same
pending count both times; no 9 ms spike at ~#7,956 in this run (that batch's
max was 618 µs), so run A's was noise or an unrelated GraphBLAS event.
Reads: +5% to +15% over two runs, no step at the flush; the flush spike:
24.2–29.4 ms.

## Reference numbers for the exercise lanes

Read these after filling the worksheet. From reference implementations of
`segblocks::SegBlocks` and `pma::PmaCsr` written to the contracts in the stub
doc comments and run against lane 1's graph (all 15 tests green, then
removed; the solution is not committed). Counts are exact; timings are from
the run at load 4.40.

| | scan, nothing pending (ns/edge) | × the CSR in that run (0.165) | B/edge | ns/write 1% | ns/write 10% | ns/write 10% half deletes |
|---|---|---|---|---|---|---|
| **lane 2** — sorted blocks, capacity 256 | 0.206 | 1.25× | 7.98 | 588 | 404 | 341 |
| **lane 3** — PMA, leaf 16 | 1.376 | **8.3×** | 8.87 | 376 | 359 | 369 |

Scan columns in the three write mixes: lane 2 0.241 / 0.240 / 0.241; lane 3
1.388 / 1.390 / 1.402. A second run of the same build: lane 2 0.202 nothing
pending, 548 / 390 / 331 ns per write; lane 3 1.361, 376 / 355 / 358. Every
mix's checksum matches the CSR + delta run's.

**Lane 2 — what the design is.** Vertices of degree ≤ 256 are one sorted
`Vec`; hubs are a vector of blocks, each a `Vec` with capacity 256, plus a
sorted vector of block minima (a binary search replaces Sortledton's skip
list). A full block splits in half; a block under cap/2 merges with its
previous neighbour (or, if the pair does not fit, rebalances against it); a hub
that shrinks is never converted back. Initial hubs are filled to 3/4.
- Scan 1.25× the CSR and bytes 1.87× — the block-capacity slack (a 3/4-full
  hub block holds 192 of 256 slots) plus the 32-byte per-vertex enum.
- Writes are flat and cheap (404 ns at 10%, no pause): this is what Sortledton
  buys, and the delta cannot offer.

**Lane 3 — what the design is.** One `Vec<u32>` of slots (`0` empty, high bit
marks a sentinel, an edge stored as `v + 1`), capacity a power of two with fill
≤ 0.75 at build; a start-position array per vertex; evenly spread leaves of 16
slots; the density rule of PCSR (upper bound 1.0 at a leaf to 0.75 at the
root); sentinels repaired on every move; deletes only clear a slot.

- **Negative result against the paper.** Scan 8.3× the CSR at 2.1× the bytes,
  where PCSR's authors report "within a factor of 2" on real graphs and within
  25% on synthetic ones (Table II and Figs. 7–8, [reading-packed-csr.md](reading-packed-csr.md)
  Step 4). Mean degree here is 15.3, so per-row costs (two `start[]` loads, a
  chunk loop, a mispredicted exit) matter more than in the paper's
  dense synthetic graphs.
- **A branchless variant** — compact each 64-slot chunk with
  `buf[k] = x; k += (x != 0) as usize`, then visit `buf[..k]` — scans at
  **0.867 ns/edge** in a locked run (CSR 0.161 in the same run: **5.4×**), and
  its inserts are unchanged (358 / 362 / 358 ns). Branch misprediction was
  therefore about two fifths of the loss ((1.376 − 0.867) ÷ (1.376 − 0.165) =
  42%); the rest — a factor of 5.4 ÷ 2.1 ≈ 2.6 beyond the extra bytes — was not
  isolated and is exercise 2.
- Insert 359–376 ns flat with no flush, the cheapest write in the topic; the
  price is the scan.

## Predictions (fill BEFORE running the benchmark or implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 1a: one splice insert into the 64.3 MB edge array, in ms | | |
| lane 1b: static CSR two-hop scan, ns/edge | | |
| lane 1b: `Vec<Vec<u32>>` relative to CSR on the same scan | | |
| lane 1c: insert ns/write for CSR + delta at 1% writes | | |
| lane 1c: scan slowdown of a per-edge tombstone test, half-deletes mix | | |
| lane 1d: scan slowdown at 100,000 pending changes | | |
| lane 1d: scan slowdown at 1,000,000 pending changes | | |
| lane 1e: ns/insert at T = 1,000 | | |
| lane 1e: steady-state ns/insert at T = 10,000, and the adjacency list's | | |
| lane 1e: the flush time F at T = 10,000, from bytes and bandwidth | | |
| lane 2: scan relative to CSR with block capacity 256; B/edge | | |
| lane 3: scan relative to CSR with leaf 16; B/edge | | |
| C FalkorDB: engine microseconds for the two-hop query on 200 seeded starts | | |
| C FalkorDB: ratio of that time at ~5,000 pending to the empty delta | | |
| C FalkorDB: the largest single-insert latency while crossing 10,000 pending | | |

<details><summary>Author's expectations while designing the lanes (written down here after the reference ran — hindsight is possible, so weigh them accordingly)</summary>

- Lane 1d: expected a scan cost rising smoothly with pending changes from the
  first thousand. **Wrong:** with a tombstone-splitting scan it is flat to
  100,000 and bends at a million; the smooth rise was the per-edge tombstone
  test, found only because lane 1c's deletion mix disagreed with 1d.
- Lane 1e: expected the delta to beat `Vec<Vec<u32>>` on inserts at T = 10,000
  because `c` is 3.6× smaller. **Wrong in steady state:** 782 ns against 555.
  The stream average (468) hid it by one unpaid flush.
- Lane 2: expected a 10–20% scan tax; measured 25%.
- Lane 3: expected "about 2×" from the paper; measured 8.3× naive, 5.4×
  branchless.
- C FalkorDB: expected the pending delta to slow a two-hop query measurably at
  5,000 pending. **Wrong:** +3% to +15% over two runs, with no step at the flush (below).

</details>

## Paper numbers worth keeping (each checked against the PDF)

- **Revisiting (arXiv:2502.10959v1)**: Table 5's best-segmented ÷ CSR over 16
  PageRank / triangle-count cells is **1.11× (TC, dl) to 4.10× (PR, ct)**; the
  text's 1.2–53.7× (§6.1.3) and 2.4–11.0× (Introduction) cannot be reproduced
  from it. Table 9: Sortledton with versions 4.09–8.89× CSR's memory, Aspen
  3.32–10.83×, LiveGraph 4.88–16.67×. Derived from Tables 7–8 at 32 threads on
  g5: bytes per scanned edge 14.60 (AdjLst), 22.31 (LiveGraph), 11.18 (Aspen),
  19.13 (Teseo), 21.64 (Sortledton). Table 4 (2^20-neighbour scan): DTLB misses
  1.19e4 (AdjLst) vs 1.94e6–4.32e6 for the block layouts.
- **Sortledton (PVLDB 15(6), 2022)**: ≈1.22× CSR on average and ~2× its space
  (abstract, §9; 18 GB vs 8.3 GB static CSR on graph500-24, §7.3);
  adjacency-list scans within 40% of CSR (§3.1); block ≥ 256 edges matches a
  contiguous vector (§3.2); optimum insert block 128 (Fig. 7); > 3.1 and
  4.8 M checked inserts/s for Teseo and Sortledton at up to 56 threads (§7.4).
  Code: merge threshold 0.45 × block size, not 0.5.
- **Teseo (PVLDB 14(6), 2021)**: thresholds ρ₁ = 0.5, τ₁ = 1, ρₕ = τₕ = 0.75,
  C_S = 4 KB, D = 200 ms, C_L = 1 MB (§2.1, §7); ≈ 2.4 M inserts/s at p = 40
  (§7.2); 3.20 M updates/s (§7.3); scans ≈ 50% apart from CSR (§7.4). The
  printed `high(W)` has `(τₕ + τ₁)`; the code uses the difference
  (`crawler.cpp:295`).
- **PCSR (HPEC 2018)**: PCSR within a factor of two of CSR on real graphs
  (Table II: CSR 1.69–2.78× faster at SpMV/BFS/PageRank), within ~25% on
  synthetic; 20–30% space over an ideal CSR; inserts 3–4× slower than an
  adjacency list, and CSR takes 525× to 70,787× PCSR's time to add 1,000
  edges (Table II).
- **LiveGraph (PVLDB 13(7), 2020)**: TEL edge-scan latency CSR = 43% of TEL's
  (§2.1); Bloom filter 1/16 of a block (§4; code: blocks ≥ 1,024 B); 64-byte
  minimum TEL, power-of-two blocks (§6); compaction every 65,536 transactions.

## Guide-question checklist

- [ ] reading-revisiting-dgs.md Q1–Q4
- [ ] reading-sortledton.md Q1–Q4
- [ ] reading-teseo.md Q1–Q4
- [ ] reading-packed-csr.md Q1–Q4
- [ ] reading-livegraph.md Q1–Q4
- [ ] reading-falkordb-delta-in-context.md Q1–Q4

## Cross-topic threads (worked)

- Topic 20 ↔ 51: topic 20 Step 1's bytes-over-bandwidth estimate is lane 1a's
  prediction; this topic's measured splice (34 MB at 53 GB/s) is the check.
  Its Step 3 write path (four GraphBLAS calls) is lane 1e's `c`.
- Topic 4 ↔ 51: the flush is a one-level compaction; `c + F/T` is write
  amplification with F set by the *settled* size, not the pending size — the
  reason leveled LSMs bound their run sizes.

## Open questions

- Does FalkorDB's mask-based read (`A·DM` as a mask) have the per-edge
  tombstone problem? A deletion-heavy stream against the C engine, scan time
  vs pending deletions, would say; this topic's datapoint only inserts.
- What is FalkorDB's own F? The datapoint sees a 24–29 ms single-insert spike
  crossing 10,000 pending; which matrices it folded (relation, adjacency,
  their transposed twins), the split between the deletion and addition folds,
  and `M`'s size, are not measured.
- Insert latency in the C engine rises with the pending count (below) and
  falls after the flush; the mechanism (a `GrB_wait` over `DP` per query?) is
  not verified.
