# Topic 47 notes — semi-join reduction in practice

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin semijoin_bench`. Seeded generator (`gen.rs`,
`GraphParams::LANE1`, seed 47): every count below reproduces exactly on any
machine. Timings are best of 3 in one run; other agents were benchmarking on
the same machine, so `sysctl -n vm.loadavg` was recorded with each run.

| run | load average (1/5/15) at start | lane-1 timings (good / bad / reduce / good-red / bad-red, ms) |
|---|---|---|
| quoted in README | 4.39 5.68 7.46 | 6.94 / 37.94 / 1.50 / 0.54 / 0.52 |
| earlier, same day | 8.16 5.29 4.74 | 8.37 / 44.61 / 2.33 / 0.59 / 0.52 |
| earlier, same day | 4.52 4.09 4.31 | 7.47 / 35.90 / 1.58 / 0.61 / 0.57 |
| stub build (verify shape) | 6.98 6.18 7.60 | 7.33 / 39.54 / 1.47 / 0.57 / 0.53 |

Ordering of the timings is stable across runs (bad ≫ good > reduce > reduced
plans); the absolute values move by ~20% with load. Quote counts, not ms.

### Lane 1 — one bad order, before and after the reduction

| plan | join 1 | join 2 | join 3 | intermediates | × OUT | build rows |
|---|---|---|---|---|---|---|
| good rare_tag→has_tag→likes→knows | 1,000 | 2,036 | 20,210 | 23,246 | 1.15 | 400,000 |
| bad knows→likes→has_tag→rare_tag | 1,000,000 | 2,000,000 | 20,210 | 3,020,210 | 149.44 | 200,010 |
| good, after full reduction | 868 | 2,036 | 20,210 | 23,114 | 1.14 | 22,233 |
| bad, after full reduction | 20,084 | 20,210 | 20,210 | 60,504 | 2.99 | 2,903 |

- OUT = 20,210; checksum `a77191f1c5106d95` in all four plans.
- Reduction: forward 3,035 inserts + 400,000 probes; backward 22,233 inserts
  + 3,035 probes; 428,303 hash ops = 1.07 × N (N = 400,010).
- Sizes base / after forward / after both: knows 200,000 / 19,340 / 19,340;
  likes 100,000 / 2,025 / 2,025; has_tag 100,000 / 1,000 / 868; rare_tag
  10 / 10 / 10.
- **Hand predictions that check out:** 1,000,000 and 2,000,000 are exact
  (5 likes per person, 2 tags per post). knows′ ≈ 200,000 × (1 − (1 −
  (1 − 0.99²))⁵) = 19,100 (measured 19,340). has_tag after the backward pass
  ≈ 1,000 × (1 − e⁻²) = 865 (measured 868).
- **Negative result:** on the good order the reduction costs 428,303 hash
  operations to save 132 intermediates. It is insurance, not a speed-up.
- **Timing surprise:** the good order is slower (6.94 ms) than reduction +
  bad order (1.50 + 0.52 ms), because its left-deep shape builds hash tables
  on knows and likes (400,000 build rows). Intermediate counts are not the
  whole cost of a plan.

### Lane 1b — filter accuracy at equal bit budgets (1,000,000 absent keys)

| keys | bits | bits/key | classic Bloom FPR (k) | formula | register-blocked FPR |
|---|---|---|---|---|---|
| 1,000 | 16,384 | 16.38 | 0.00045 (11) | 0.00038 | 0.00548 |
| 10,000 | 131,072 | 13.11 | 0.00181 (9) | 0.00184 | 0.00901 |
| 100,000 | 2,097,152 | 20.97 | 0.00006 (15) | 0.00004 | 0.00249 |

Register-blocked = DuckDB v2.0.0's layout (`d54cbbe3df`: 4 bits in one 64-bit
word, sized at ≥ 12 bits/key rounded to a power of two) with this crate's hash.
These sizes are what DuckDB allocates for an exact build count; its `Seal` step
folds a filter only when the key set is sparser than that sizing assumes, which
none of these three rows is (folded density⁴ 2.2%, 4.4%, 1.0% against its
0.646% target). An
independent Python simulation of the same layout while writing the DuckDB
guide gave 0.0045 at 1,000 keys and 0.0091 at 10,000 — same shape.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 2, 8 bits/key: total survivors (exact 22,243) | | |
| lane 2, 8 bits/key: which relation carries most of the inherited rows? | | |
| lane 2, 16 bits/key: bad-order intermediates vs exact 60,504 | | |
| lane 2: does a filter's per-key FPR match (1 − e^(−kn/m))^k? | | |
| lane 3, bad order, into lower builds: intermediates | | |
| lane 3, bad order, probe spine only: intermediates | | |
| lane 3, good order: rows put into hash tables | | |
| exercise 2: inherited rows at 8 bits/key with the register-blocked filter | | |

<details><summary>Author's expectations while designing the lanes (written down here after the reference ran — hindsight is possible, so weigh them accordingly)</summary>

- Lane 3, bad order, into lower builds: the cascade leaves exactly the forward-pass sizes, so 60,504 intermediates — the same as the full reduction. (Held.)
- Lane 3, bad order, probe spine only: essentially no help, since the bitmap that can reach knows is built from unfiltered likes. (Held: 3,020,210.)
- Lane 3, good order: the top build (knows) is unfiltered, ~400,000 rows built. (Held: 386,439 and 400,000.)
- Lane 2: no numeric expectation was formed for the inherited column; that it is 5× the filters' own false positives at 8 bits/key was the surprise of the topic.

</details>

## Reference numbers for the stubs

From reference implementations of `transfer::bloom_transfer` and
`bitmap::bitmap_cascade` written to the contracts in the stub doc comments,
run against lane 1's data (load 4.39 at start), all 15 tests green, then
removed. Counts are exact; your implementation must reproduce them.

### Lane 2 — Bloom-filter predicate transfer

| bits/key | survivors | not exact | filter FP | inherited | worst per-key FPR (theory) | bad-order intermediates | ms |
|---|---|---|---|---|---|---|---|
| 2 | 264,558 | 242,315 | 42,144 | 200,171 | 0.3912 (0.3928) | 839,862 | 14.33 |
| 4 | 198,190 | 175,947 | 24,189 | 151,758 | 0.1478 (0.1468) | 436,532 | 12.82 |
| 8 | 58,087 | 35,844 | 6,004 | 29,840 | 0.0226 (0.0215) | 106,100 | 11.63 |
| 16 | 22,851 | 608 | 159 | 449 | 0.0007 (0.0005) | 60,960 | 9.96 |

Detail at 8 bits/key (k = 6):

| pass | from → to | keys | bits | neg. keys | FP keys | FP rows | FPR (keys) | theory |
|---|---|---|---|---|---|---|---|---|
| F | rare_tag → has_tag | 10 | 128 | 990 | 6 | 606 | 0.0061 | 0.0027 |
| F | has_tag → likes | 1,589 | 12,736 | 41,853 | 923 | 2,142 | 0.0221 | 0.0214 |
| F | likes → knows | 4,811 | 38,528 | 15,188 | 343 | 3,340 | 0.0226 | 0.0215 |
| B | knows → likes | 5,154 | 41,280 | 0 | 0 | 0 | 0 | 0.0215 |
| B | likes → has_tag | 2,292 | 18,368 | 220 | 5 | 6 | 0.0227 | 0.0214 |
| B | has_tag → rare_tag | 16 | 128 | 0 | 0 | 0 | 0 | 0.0216 |

| relation | base | exact | survived | filter FP | inherited |
|---|---|---|---|---|---|
| knows | 200,000 | 19,340 | 51,328 | 3,340 | 28,648 |
| likes | 100,000 | 2,025 | 5,359 | 2,142 | 1,192 |
| has_tag | 100,000 | 868 | 1,390 | 522 | 0 |
| rare_tag | 10 | 10 | 10 | 0 | 0 |

- **False positives are per key, rows inherit them.** rare_tag → has_tag let
  6 of 990 absent tags through; each tag has ~100 posts, so 606 rows. The
  first draft of the FPR test measured per *row* and failed for exactly this
  reason (10 keys, 90 distinct negatives in the small test graph); the
  contract now measures per distinct key.
- 10-key filters round 80 bits up to 128, so their theory column (0.0027) is
  lower than the 8-bits/key nominal 0.0215; 6 FP keys of 990 is 0.6% — noisy
  at that sample size.
- PT's §3.5 model predicts knows's own false positives as 180,660 × 0.0215 =
  3,884; measured 3,340. The model has no term for the 28,648 inherited rows.

### Lane 3 — bitmap pushdown, exact bitmaps

| plan | pushdown | scan 0 | build 1 | build 2 | build 3 | build rows | bitmap probes | intermediates | ms |
|---|---|---|---|---|---|---|---|---|---|
| good rare_tag→has_tag→likes→knows | probe spine only | 10 | 100,000 | 100,000 | 200,000 | 400,000 | 10 | 23,246 | 8.92 |
| good | into lower builds | 10 | 86,444 | 99,995 | 200,000 | 386,439 | 200,010 | 23,114 | 9.92 |
| bad knows→likes→has_tag→rare_tag | probe spine only | 200,000 | 100,000 | 100,000 | 10 | 200,010 | 200,000 | 3,020,210 | 39.14 |
| bad | into lower builds | 19,340 | 2,025 | 1,000 | 10 | 3,035 | 400,000 | 60,504 | 2.08 |

- Bad + into lower builds = lane 1's after-forward sizes exactly; its 60,504
  intermediates equal the full two-pass reduction's — the CIDR'26 §3.3
  one-pass claim, reproduced. has_tag stays at 1,000 (not 868) and it does
  not matter: the 132 unliked posts are never probed.
- Probe spine only gives the bad order nothing; the bitmap that does reach
  knows (on b) was built from unfiltered likes.

## Paper numbers worth keeping (each checked against the PDF)

- **RPT+ (VLDB'26).** Geomean over DuckDB v1.3.0: 1.47× JOB, 1.28× SQLStorm,
  1.17× TPC-H, 1.01× Appian (abstract, §1, §9, Figure 11). SQLStorm: 18,251
  queries, 13,308 finish, 2,837 fail, 2,106 time out (§6.1). RPT 555 + 3,185
  = 28.1% below 0.9×; RPT+ 24 + 247 = 2.1% (Tables 1–2). Worst RPT+ query
  13615 at 0.37× (§6.6.3). Constants: γ = 100K, τ_sel = 0.35, τ_prog = 0.6,
  M_avail = 64 GB, τ_stop = 0.9 (§5.4); Figure 9 and §5.3 use 0.95 instead —
  minor inconsistency. CSBF 20 bits/key, 7 hashes, 2.48 cycles/tuple, FPR
  6.1 × 10⁻⁵ (§5.4). Min–max inflation E[range] ≈ n − 2/p (§4.2); JOB 07c
  BF 6 holds 14 vs 378 items at 24 vs 16 bits/key, ~4× (§6.6.2). "Up to
  500×" (§6.1) is prose about Figure 10; Table 2's >100× bucket is 12
  queries averaging 226.29×.
- **SQL Server bitmaps (CIDR'26).** TPC-H SF100: 7 of 22 queries > 2× (Q2
  3.47, Q5 2.33, Q7 3.00, Q8 2.86, Q17 2.55, Q20 2.01, Q21 2.36); 12 acyclic
  queries instance-optimal (prose — Table 1's check marks do not survive text
  extraction); 5 queries with optimizer slowdown (Q5, Q7, Q8, Q18, Q19).
  Pre-filter gaps: Q3 5.23% vs Yannakakis 1.25%, Q8 1.92% vs 0.16%
  (Table 1). Q12: 15 M orders → 2.96 M vs 2.92 M perfect (§3.1). Q3
  estimate with bitmap contexts 3.3 M vs 323 M (§4.1). Telemetry: 7% of join
  queries > 8-way (up to 1,000 tables), 15% hybrid join types (§6).
  Discrepancies: "deployed since 2012" (§1) vs "shipped in SQL Server 2014"
  (acknowledgments); rowgroups "~900 rows" (§3.1, probably a lost K — not
  used).
- **Predicate Transfer (CIDR'24).** PredTrans vs NoPredTrans 4.1× / 4.0×,
  BloomJoin 3.3× / 3.2×, Yannakakis 4.2× / 4.8× at SF1 / SF10 (§4.2); 15 of
  20 join queries improve; Q2 47×/44× (SF1) and 63×/61× (SF10) over
  NoPredTrans/BloomJoin. Q5: join inputs 98%/96%/64% smaller than
  NoPredTrans/BloomJoin/Yannakakis at SF1, 98/92/67% at SF10 (Tables 1–2);
  transfer phase 13×/16× faster than the semi-join phase; join-order spread
  up to 12% vs up to 45× (Figure 6). **Does not hold as stated:** the
  introduction's "3.3× (up to 61×) … Yannakakis … 4.8× (up to 47×)" mixes
  an SF1 average (3.3×) with an SF10 one (4.8×), and the only 47× in §4.2 is
  Q2 against NoPredTrans at SF1, not against Yannakakis.
- **IN predicates (CIDR'26).** Wrong results: ClickHouse 25.8, Hyper 9.1.0,
  DuckDB 1.3.0, Snowflake 9.21.0, Redshift 1.0.118447, Firebolt Core 4.23.5
  (§1). Right mark join 9 ms vs left 15 ms at 10⁶ × 10⁶ (4.5 / 7.5 ns per
  tuple), break-even slope ≈ 1.3 (§7, Figure 10).
- **Yannakakis (VLDB'81).** Not obtained: the VLDB 1981 archive returned
  HTTP 403 and no other copy was found. Its guarantees are quoted from the
  three papers above (PT §2.2, RPT+ §2.1, CIDR'26 §2.1) and checked by this
  crate's tests; no number is attributed to it.

## Cross-topic threads

- Topic 10's §4 (RPT+) and §5 (SQL Server bitmaps) and topic 11's §5 (the IN
  paper) are the one-paragraph versions of guides 2–4 here; both checked claims
  there (1.47/1.28/1.17/1.01×; 7 of 22 > 2×, 12 instance-optimal) match.
- Topic 44's Free Join chapter places Yannakakis as the acyclic case; lane
  1's chain is the simplest instance.
- Topic 11's IN-predicate paragraph and guide 4 here are the same paper; guide
  4 adds the algorithm.

## Open questions

- Does DuckDB's pause back off as traced? Step 6 of
  reading-duckdb-join-filters.md says a filter that stays unselective is
  evaluated for 6 vectors, skipped for 4, evaluated for 6, skipped for 14, and
  so on; confirm with a DuckDB build and a probe column whose first six
  vectors all match.
- Does DataFusion populate a dynamic filter routed into a lower join's build
  side before that build scans? Routing allows it (`exec.rs:1983-1995`, key transfer `:1970-1988`);
  execution order was not traced.
- How large is the inherited term on a star schema, where there is no second
  hop?

## Implementation log

- [ ] `transfer::bloom_transfer` — 5 tests green; lane 2 matches the table
- [ ] `bitmap::bitmap_cascade` — 4 tests green; lane 3 matches the table
- [ ] prediction table reconciled
- [ ] exercise 2 (register-blocked transfer) and exercise 4 (dynamic abandon)
