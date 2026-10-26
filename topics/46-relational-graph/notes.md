# Topic 46 notes — graph queries on relational engines

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin pgq_bench`. Every count below is exact and
reproduces on any machine (integer-only generator, sorted edge list, hash maps
probed but never iterated). Milliseconds are best of three on this machine, with
`sysctl -n vm.loadavg` recorded next to each run: other agents were running
benchmarks concurrently, so the timing rows are indicative, the counts are the
result.

### Lane 1a — the paper's running example

| quantity | this crate | Yasser et al. §2.2.1 |
|---|---|---|
| flat tuples of `R(a1,a2) ⋈ R(a2,a3)` | 13 | 13 |
| flat values | 39 | 39 |
| values over T1 (`a1 → a2 → a3`) | 22 | 22 |
| values over T2 (`a2 → {a1, a3}`) | 15 | 15 |

### Lane 1b — counts (seed 46, average out-degree 8, 70% preferential)

| |V| | |E| | max out / in | pattern | flat tuples (all joins) | flat values | f-rep values | flat / f |
|---|---|---|---|---|---|---|---|
| 10,000 | 77,164 | 1,463 / 1,396 | 2-hop | 6,665,086 | 19,995,258 | 161,624 | 124× |
| 10,000 | 77,164 | | 3-hop | 241,301,569 | 958,541,190 | 6,738,545 | 142× |
| 20,000 | 155,727 | 2,363 / 2,304 | 2-hop | 17,759,290 | 53,277,870 | 326,298 | 163× |
| 20,000 | 155,727 | | 3-hop | 739,497,555 | 2,940,230,930 | 17,875,125 | 164× |
| 40,000 | 313,512 | 3,884 / 3,897 | 2-hop | 47,900,847 | 143,702,541 | 656,622 | 219× |
| 40,000 | 313,512 | | 3-hop | 2,272,724,241 | 9,042,996,117 | 47,997,149 | 188× |
| 40,000 (uniform) | 319,962 | 20 / 22 | 2-hop | 2,560,772 | 7,682,316 | 679,805 | 11× |
| 40,000 (uniform) | 319,962 | | 3-hop | 23,058,939 | 89,674,984 | 3,240,016 | 28× |

Checks that make the counts trustworthy:

- The 3-hop flat-tuples column is join 1's output plus join 2's: at 10,000,
  6,665,086 + 234,636,483 = 241,301,569. The 3-hop *result* is 234,636,483
  there and 2,224,823,394 at 40,000.
- Hash join, CSR expansion and the degree formula agree on the count in every
  row (the bench prints it); `fsize`'s formulas are checked against a
  brute-force distinct-prefix count in `fsize::tests`.
- Flat values are 3 per 2-hop tuple and 4 per 3-hop tuple: at 40,000,
  3 × 47,900,847 + 4 × 2,224,823,394 = 143,702,541 + 8,899,293,576 =
  9,042,996,117. ✓
- The 3-hop f-representation (47,997,149) is close to the 2-hop flat count
  (47,900,847) by construction: its `d` level costs `Σ_(b,c) out(c) =
  Σ_c in(c)·out(c)` restricted to `c` with out-edges, which is the 2-hop count.
- Lower bound on the number of distinct middles at 40,000: 656,622 − 2 ×
  313,512 = 29,598 (each middle stores 1 value plus its in- and out-lists, and
  the lists together hold at most every edge twice).

### Lane 1b — timings

Run A, load `{ 4.28 4.07 4.32 }`:

| |V| | pattern | hash join ms | CSR expand ms | factorized count ms | ns / tuple (hash) |
|---|---|---|---|---|---|
| 10,000 | 2-hop | 2.3 | 1.7 | 0.01 | 0.35 |
| 10,000 | 3-hop | 70.0 | 53.6 | 0.07 | 0.29 |
| 20,000 | 2-hop | 4.7 | 3.5 | 0.02 | 0.27 |
| 20,000 | 3-hop | 202.0 | 155.1 | 0.22 | 0.27 |
| 40,000 | 2-hop | 10.1 | 7.5 | 0.04 | 0.21 |
| 40,000 | 3-hop | 624.2 | 464.3 | 0.45 | 0.27 |

Run B, load `{ 12.47 8.19 8.18 }` (same binary, stubs in place): hash join
2.4 / 101.0 / 5.3 / 322.5 / 19.3 / 1458.2 ms, CSR 2.1 / 71.4 / 3.8 / 215.3 /
10.4 / 588.7 ms for the same six rows — up to 2.3× slower, counts identical.

Sanity, as the bench prints it: the fastest hash-join row is 0.21 ns per
produced tuple. That is plausible only because this flat plan writes nothing:
the last join adds each matching value into a `black_box`ed sink, a loop the
compiler vectorises over contiguous `Vec<u32>` match lists. A plan that
materialises tuples into an output vector would cost several times more per
tuple; the number is a lower bound on flat enumeration, not an estimate of a
real engine's cost.

## C FalkorDB on the same graph (measured 2026-10-26, Apple M3 Pro)

`python3 experiments/cengine.py 10000 20000 --explain` (with `pgq_edges` built
and `FALKORDB_MODULE` set). It starts `redis-server` 8.6.2 on port 16479 with
`~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so` (the C engine, a
RelWithDebInfo build of 2026-07-24 that reports module version 999999, i.e. a
development build; the clone's `main` is at `5e475e6c92`), loads lane 1b's edge
list through `UNWIND … CREATE`, runs each query twice and records the server's
own "Query internal execution time". The graph is the lane's: `pgq_edges`
prints the same `power_law(seed 46, 70%)` list, and the load is checked against
the bench's edge counts (77,164 and 155,727). Default server configuration,
default thread count, no index on the traversal. Run through the bench lock;
load average 17 at the start, 6 at the end.

Two forms of each query, because FalkorDB's planner chooses between them by
which variables the query mentions (the `GRAPH.EXPLAIN` plans are in the run
output and quoted below):

```
-- every hop used:  MATCH (a)-[:E]->(v1)-[:E]->(v2)-[:E]->(v3)
                    RETURN sum(1 + 0*id(v1) + 0*id(v2))
   plan: Aggregate <- Traverse (v2)->(v3) <- Traverse (v1)->(v2) <- Traverse (a)->(v1) <- All Node Scan (a)
-- endpoints only:  MATCH (a)-[:E]->(v1)-[:E]->(v2)-[:E]->(v3) RETURN count(*)
   plan: Aggregate <- Traverse (a)->(v3) <- All Node Scan (a)
```

| |V| | pattern | form | count returned | server ms (best of 2) | ns per counted row | bench: result tuples |
|---|---|---|---|---|---|---|
| 10,000 | 2-hop | every hop used | 6,665,086 | 592.5 | 88.9 | 6,665,086 |
| 10,000 | 2-hop | endpoints only | 5,809,343 | 325.2 | 56.0 | |
| 10,000 | 3-hop | every hop used | 234,636,483 | 30,091.5 | 128.3 | 234,636,483 |
| 10,000 | 3-hop | endpoints only | 45,794,367 | 2,474.7 | 54.0 | |
| 20,000 | 2-hop | every hop used | 17,759,290 | 1,588.2 | 89.4 | 17,759,290 |
| 20,000 | 2-hop | endpoints only | 15,926,364 | 1,033.3 | 64.9 | |
| 20,000 | 3-hop | every hop used | 721,738,265 | 87,416.0 | 121.1 | 721,738,265 |
| 20,000 | 3-hop | endpoints only | 157,710,285 | 8,067.3 | 51.2 | |

(The bench column is the last join's output, the 3-hop *result*: at 10,000
241,301,569 − 6,665,086 = 234,636,483; at 20,000 739,497,555 − 17,759,290 =
721,738,265. The "tuples" column of the README counts both joins.)

What it says:

- **Same counts, so it is the same graph and the same semantics** — on the
  "every hop used" form. Four counts equal the bench's exactly.
- **The C engine is two to three orders of magnitude behind the Rust crate's
  flat plans per tuple, and that is not an apples-to-apples gap.** 3-hop at
  10,000: 30,091.5 ms against the crate's 70.0 ms (hash join) and 53.6 ms (CSR
  expansion), 430× and 560×; per row 128 ns against 0.2–0.35 ns. The Rust
  lanes enumerate into a `black_box`ed sum; FalkorDB builds a record per row
  and pushes it through the Cypher pipeline (project, aggregate), so the
  comparison prices "row at a time through an execution engine" against "bare
  loop". The README's note that 0.21–0.35 ns/tuple is a lower bound on flat
  enumeration is this row: a real engine's cost per flat tuple on this graph is
  ~100 ns (88.9–128.3 measured here), roughly 300–600× the bare loop.
- **The C engine does not factorize.** The 3-hop plan enumerates every one of
  the 234,636,483 rows (and 721,738,265 at 20,000) — the representation it
  works in is the flat tuple, exactly the Step-1 blow-up. Its cost per row
  stays near 120–130 ns as the result grows: 234.6M → 721.7M rows is 3.1×,
  30,091.5 → 87,416.0 ms is 2.9×.
- **The "endpoints only" form is a different query, not a faster plan.** When
  only the first and last variables are used, FalkorDB's planner folds the
  chain into one algebraic expression (`Conditional Traverse | (a)->(v3)`,
  i.e. A·A·A over a boolean semiring) and emits one row per distinct endpoint
  pair. The counts are not Cypher's path counts: 5,809,343 against 6,665,086
  at 2-hop. They are the number of non-zero entries of A² (and A³): checked
  independently from the same edge list with a dense matrix product (numpy):
  nnz(A²) = 5,809,343, Σ(A²) = 6,665,086, nnz(A³) = 45,794,367. So `count(*)`
  over a pattern whose intermediate vertices are not mentioned returns the
  *set* semantics number, while mentioning an intermediate vertex anywhere
  (even `count(b)`) switches to the bag number. Whether that is intended is
  not checked here; it is a question for the FalkorDB team and for the M46
  capstone, whose `count(*)` over a factorized intermediate must return the
  *bag* number. Set semantics *is* a factorization of sorts — it collapses
  the middle — and it is 12.2× faster at 3-hop/10,000 (2,474.7 against
  30,091.5 ms) and 10.8× at 20,000 for 5.1× and 4.6× fewer rows.
- **Plans**: `All Node Scan` (no label on the pattern), a `Conditional
  Traverse` per hop, `Aggregate`. No hash join, no semi-join, no index use —
  the adjacency is the delta/relation matrix, and each `Conditional Traverse`
  multiplies the current frontier of source nodes by it and iterates the
  result row by row.

### Lane 3 (provided half) — `a·b*` from 257 sources, |V| = 40,000

78,165 `a`-edges and 235,347 `b`-edges (label = one in four by a hash of the
endpoints). `WITH RECURSIVE … UNION`, semi-naive, with hash joins:
**6,536,711 pairs, 10 rounds, 40,832,336 tuples derived**; 675.9 ms at load
`{ 4.18 4.05 4.31 }`, 1,652.2 ms at load `{ 12.85 8.41 8.26 }`.

| k (max `b`-steps) | pairs (set) | walks (bag) | walks / pairs |
|---|---|---|---|
| 0 | 522 | 522 | 1.0× |
| 1 | 65,261 | 66,846 | 1.0× |
| 2 | 1,053,852 | 2,265,519 | 2.1× |
| 3 | 3,831,352 | 145,183,850 | 37.9× |
| 4 | 5,845,226 | 6,679,113,406 | 1,142.7× |
| 6 | 6,530,133 | 18,103,099,983,508 | 2.8 × 10⁶× |
| 8 | 6,536,682 | 48,051,021,435,475,869 | 7.4 × 10⁹× |

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 2: `extend` output values at |V| = 40,000 | | |
| lane 2: `extend` time vs lane 1's flat 3-hop hash join | | |
| lane 2: `count()` time — µs or ms? | | |
| lane 2: values and tuples left after `retain_d(d % 8 == 0)` | | |
| lane 2: does the filter shrink values by about 8×? why or why not? | | |
| lane 3: product BFS states visited | | |
| lane 3: product BFS edges scanned vs semi-naive's 40,832,336 | | |
| lane 3: product BFS time vs semi-naive — ratio? | | |
| exercise 2: peak memory of a materialized 2-hop intermediate at 40,000 | | |

<details>
<summary>Author's prediction</summary>

| stub lane | prediction | reasoning |
|---|---|---|
| `extend` values | 47,997,149 | lane 1's closed form is the spec; any other number is a bug |
| `extend` time | 20–60 ms, ≥ 10× faster than the 624 ms flat 3-hop | it writes ~48M values once; the flat plan touches 2.27B tuples |
| `count()` | tens of µs | one pass over ~30K `b` positions with two offset subtractions each |
| `retain_d(d % 8)` | values ≈ 1/8 of the `d` level plus most of `a`; tuples ≈ 1/8 | `d` dominates the size; hubs keep almost every `c` alive |
| states | 6,536,711 + 257 | state 0 holds only the source; state 1 holds exactly the answers |
| edges scanned | equal to 40,832,336 | set semantics: each pair joined with its b-edges once — same as BFS |
| BFS vs semi-naive time | 3–8× faster | same steps; array slot vs hash-set insert per step |

</details>

## Reference numbers for the exercise lanes

From reference implementations of `factorized::extend`, `ThreeHop::{values,
count, flatten, retain_d}` and `rpq::product_bfs`, written, run against the
same seed and tests (all 13 tests passed), then replaced by `todo!()`.
Load `{ 4.18 4.05 4.31 }` unless stated.

**Lane 2, |V| = 40,000:**

| step | result | time |
|---|---|---|
| provided `two_hop` | 656,622 values (closed form 656,622), represents 47,900,847 tuples | 4.2 ms |
| `extend` (bottom insert of `R(c,d)` under `c`) | 47,997,149 values = closed form; invariants ok | 20.7 ms |
| `count()` without flattening | 2,224,823,394 = the flat join's count | 0.044 ms |
| `retain_d(d % 8 == 0)` with cascade | 6,632,319 values, 286,772,642 tuples; invariants ok | 26.0 ms |

Build plus count is 4.2 + 20.7 + 0.044 ≈ 25 ms against 624.2 ms for the flat
hash-join count in run A — about 25×, while the factorized form *is* the whole
result (it can be flattened, filtered or aggregated further) and the flat plan
produced only a number. A second reference run at load `{ 6.72 4.63 4.49 }`
gave 5.7 / 30.0 / 0.050 / 33.7 ms with identical counts.

**Lane 3, |V| = 40,000, 257 sources:**

| | pairs | states | edges scanned | time |
|---|---|---|---|---|
| provided semi-naive | 6,536,711 | — | 40,832,336 derived | 675.9 ms |
| reference `product_bfs` | 6,536,711 | 6,536,968 | 40,832,336 | 134.2 ms (5.0×) |

States = 257 sources + 6,536,711 answers = 6,536,968 ✓; edges scanned equals
tuples derived exactly — the spec of the second lane-3 test. A second run gave
132.2 ms against 650.7 ms (4.9×) at load `{ 3.64 3.93 4.26 }`.

## Paper numbers worth keeping

- **Lehner & Neumann (VLDB'26).** Figure 2: 16 → 10 → 6 values, "62.5%" saved
  (§2.2). Break-even: top inserts at 10–12 duplicates per key, bottom inserts at
  36; bottom inserts 1.9× the cost of top inserts (§6.1). CE benchmark, 2,557
  queries: −15% geomean on all acyclic, +6.9% on acyclic > 10 ms, +52% on
  > 100 ms, 10 queries > 10×; cyclic −32%, 35 faster by up to 3.3×; vs Umbra
  −58% geomean, 294 acyclic faster (§6.2). Parallel 18× average; bottom inserts
  in 49% of queries and 2,091 joins (§6.4). **Not reconstructable:** the
  abstract's "over 25×" and "over 19% of CE queries".
- **FFX (SIGMOD'26).** 1,951 plans on Google: packed wins 1,883, loses 68, mean
  2.08×, max 9.39× (§7.2; printed 96.53%/3.47% vs computed 96.51%/3.49%).
  Table 4: packing alone 1.66× and 1.46× on flat-order plans; 5.32× and 9.39×
  with branching. **Prose/table mismatches:** RT in ms not s; "up to 108.4×"
  fewer calls vs 114.13× in the table; "1.35–3.59×" cycles mixes rows and omits
  8.14×. LSQB SF10 Q1: 1.28 s vs Kùzu 591.10 vs DuckDB 18.43, with hand-picked
  orders (§7.3.3).
- **CRPQ planning (VLDB'26).** Table 5: WDBench 5.5544 → 2.6662 s (2.0833 =
  ratio of averages), timeouts 186 → 60 (−67.7%); LDBC SNB vs Neo4j 5.3256 →
  3.1638 s (1.6833), timeouts 10 → 10.
- **DuckPGQ (CIDR'23).** Only Umbra completed LSQB Q6 at SF100 and Q9 at SF30;
  no system Q9 at SF100; MS-BFS scales 6× on 16 threads (§4).
- **TurboLynx (VLDB'26) Table 4.** DuckPGQ 15,905 ms vs DuckDB 3,467 ms
  geomean, LDBC SF100, same 84-query denominator (4.6×); SF10 1,289 vs 652.
  **Inconsistent row:** Umbra's printed ratios (7.74× at SF100, 0.58× TPC-H
  SF100, …) do not equal its ms quotients (27.6, 9.26, …); DuckPGQ SF100
  printed 183.9 vs computed 189.3; Kùzu DBpedia printed 18.88 vs 17.88.

## Questions from the reading guides

- [reading-lehner-neumann-factorized.md](reading-lehner-neumann-factorized.md) — answers:
- [reading-ffx-factorized-vectorized.md](reading-ffx-factorized-vectorized.md) — answers:
- [reading-crpq-unified-planning.md](reading-crpq-unified-planning.md) — answers:
- [reading-duckpgq.md](reading-duckpgq.md) — answers:
- [reading-postgres-sqlpgq.md](reading-postgres-sqlpgq.md) — answers:

## Cross-topic threads

- Lane 3's equality (semi-naive derived = BFS scanned) is topic 27's delta rule
  meeting topic 24's BFS.
- The degree-product count is topic 20's `trace`-free version of `A·A`'s sum:
  `Σ_b in(b)·out(b)` is the sum of the entries of `A²`.
- PostgreSQL's plan-time product over element tables is the same arithmetic as
  lane 1's run-time product over neighbours.

## Open questions

- The 3-hop f-representation is still `O(2-hop count)`. Which f-tree, if any,
  makes a 4-hop path `O(|E|)`-ish on this generator, and what does FFX's N²
  worst case (§2.2.3) say?
- DuckPGQ's key estimate (`c0·c1/cj`) undercounts keys on skewed joins. Does
  that ever make `SideCollapses` fire on a join that should stay flat?
- Is DuckPGQ's `{l,u}` = shortest-distance filter intentional SQL/PGQ semantics
  or a known gap? (Construct the four-vertex counterexample, then check the
  repository's tests and issues.)

## Implementation log

- [ ] `factorized::extend`, `ThreeHop::{values, count, flatten, retain_d}` —
      four tests green; lane 2 rows match the reference table
- [ ] `rpq::product_bfs` — both tests green
- [ ] prediction table reconciled
- [ ] stretch: exercise 4 (64-lane multi-source BFS) and exercise 5 (bounded
      walks vs shortest-distance pairs)
