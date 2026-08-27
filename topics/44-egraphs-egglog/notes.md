# Topic 44 notes — e-graphs as a database

## Baseline (provided lane, Apple M3 Pro, measured 2026-08-26)

`cargo run --release --bin ematch_bench`. Counters (`bt visits`,
`gj probes`) are exact and reproduce on any machine; µs columns are
best-of-three on this one.

### Lane 1a — `f(a, g(a))`, the non-linear pattern

| N | e-nodes | matches | bt visits | bt µs | gj probes | index µs | gj µs | speedup |
|---|---|---|---|---|---|---|---|---|
| 100 | 300 | 100 | 10,101 | 137.9 | 500 | 71.7 | 24.8 | 1.43x |
| 200 | 600 | 200 | 40,201 | 398.9 | 1,000 | 101.8 | 39.0 | 2.83x |
| 400 | 1200 | 400 | 160,401 | 1119.8 | 2,000 | 92.4 | 37.2 | 8.64x |
| 800 | 2400 | 800 | 640,801 | 2586.1 | 4,000 | 180.4 | 85.0 | 9.75x |
| 1600 | 4800 | 1600 | 2,561,601 | 10152.5 | 8,000 | 322.4 | 145.0 | 21.72x |

Both counters are closed forms, and checking them is how you know the
harness is honest:

- `bt visits = N² + N + 1` — one `Scan` of the single `f`-class, N
  `Bind`s of `f` e-nodes, and N `Bind`s of `g` e-nodes under each.
  At N = 100: 10,000 + 100 + 1 = 10,101. ✓
- `gj probes = 5N` — 2N to intersect `a` (N keys in the lead trie, N
  probes into the other), 2N for `x` (one key each side, N times), N for
  `root`. At N = 100: 500. ✓

Speedup doubles as N doubles, which is what a quadratic-over-linear
ratio has to do. It lags the counter ratio badly:
2,561,601/8,000 = **320×** in units of work against **21.7×** measured.
The gap is the cost of a unit. At N = 1600 a `bt` visit is
10152.5 µs / 2,561,601 = **4.0 ns** (a pointer walk down a
`Vec<ENode>`), while a `gj` probe is 145.0 µs / 8,000 = **18.1 ns** (a
hash lookup) — and the trie build charges another 322.4 µs, so per
probe the join really costs (145.0 + 322.4) µs / 8,000 = **58.4 ns**.
58.4 / 4.0 = 14.7×, and 320 / 14.7 = 21.8, which is the speedup column.
Asymptotics win anyway, but only from about N = 200 on this machine.

### Lane 1b — `f(a, g(b))`, the linear pattern (the negative result)

| N | matches | bt visits | bt µs | gj probes | index µs | gj µs | speedup |
|---|---|---|---|---|---|---|---|
| 100 | 10,000 | 10,101 | 29.2 | 10,103 | 12.8 | 48.4 | 0.48x |
| 400 | 160,000 | 160,401 | 423.4 | 160,403 | 41.9 | 714.7 | 0.56x |
| 1600 | 2,560,000 | 2,561,601 | 6476.3 | 2,561,603 | 175.9 | 11290.4 | 0.56x |

`gj probes = N² + N + 3`. No equality constraint, so every candidate is
an answer and there is nothing to prune: the join does the same work
through a more expensive instruction. Generic join is **1.8× slower**
and that is the correct outcome. POPL'22 measures the same thing (Table
1, "Worst" 0.03; §5.2: "Speedup tends to be greater when the output size
is smaller").

### Lane 2 — one iteration, naive

60,000 tuples, then a delta of 24 tuples (8 new constants):
**20,008 matches re-derived, 100,040 probes, 11.0 ms** — for 8 new
answers. The whole cost of naive evaluation in one row.

The probe count is exact and reproduces everywhere; the µs is the noisiest
figure in this topic — repeat runs on the same machine span **6.5–11.0 ms**,
because this lane materialises 20,008 substitution vectors and is therefore
partly an allocator benchmark. Compare probes, not milliseconds, when you
implement the stub.

### Lane 3 — triangle multi-pattern, generic join only

| V | E | matches | gj probes | gj µs |
|---|---|---|---|---|
| 200 | 1000 | 129 | 10,155 | 94.0 |
| 400 | 2000 | 123 | 20,163 | 198.3 |
| 800 | 4000 | 123 | 39,865 | 353.8 |
| 1600 | 8000 | 138 | 79,416 | 839.5 |

Expected matches `(E/V)³ = 125` (each 3-cycle found three times, once
per rotation) — the answer size is flat while the graph grows 8×, and
generic join's probes grow linearly in E.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| semi-naive probes for a 24-tuple delta (naive: 100,040) | | |
| semi-naive µs vs naive's 11.0 ms — ratio? | | |
| how many of the 8 answers does *each* of the m delta rules produce? | | |
| duplicates across delta rules on this query — how many? | | |
| binary join's largest intermediate at V=1600, E=8000 (est. E²/V) | | |
| binary join µs vs generic join's 839.5 µs at the last row | | |
| does binary join's intermediate grow linearly or quadratically here? | | |
| at what N does lane 1a's speedup cross 1.0 if you cache the tries? | | |

## Implementation log

- [ ] `semi_naive::delta_matches` — both tests green
- [ ] `binary_join::binary_join` — both tests green
- [ ] prediction table reconciled
- [ ] stretch: cache tries across iterations (exercise 4) and re-measure
      lane 2 — the `+idx` / `−idx` distinction of POPL'22 Table 1
- [ ] stretch: congruence closure as a rule over the database
      (exercise 5), checked against `EGraph::rebuild`

Surprises / dead ends:

- The first version of `gj` allocated a `Vec` per intersection key. It
  did not change any counter and it halved the wall clock — a reminder
  that when the counters and the clock disagree, the clock is measuring
  your allocator, not the algorithm.

## Paper numbers worth keeping

- E-matching is **60–90%** of equality saturation's run time (POPL'22
  §1, citing egg's POPL'21 measurements). That is the size of the prize.
- POPL'22 Table 1, `math` at 217,396 e-nodes, index building excluded:
  best ratio **8,575,830×**, median **80.84×**, worst **0.76×**. Six
  orders of magnitude of upside and a real downside, in one row.
- POPL'22 §5: relational e-matching is ~80 lines inside egg plus a
  generic-join library under 500 lines; egg's own e-matcher is ~500
  lines "interconnected to various other parts of egg".
- PLDI'23 §5.3: at iteration 100 on the `math` suite, egglog without
  semi-naive is **3.34×** faster than egg (same e-graph); with
  semi-naive, **9.27×** (and a slightly larger e-graph). Measured on an
  M2 with 16 GB (footnote 8).
- PLDI'23 §5: egglog is ~4,200 lines of Rust. (It has since grown a
  `core-relations` crate that is a database in its own right.)

## Questions from the reading guides

- [reading-relational-ematching.md](reading-relational-ematching.md) — answers:
- [reading-egglog-pldi23.md](reading-egglog-pldi23.md) — answers:
- [reading-egglog-source.md](reading-egglog-source.md) — answers:
- [reading-free-join.md](reading-free-join.md) — answers:

## Cross-topic threads

- Semi-naive evaluation = incremental view maintenance (27) for a
  monotone query. Delta rules are DBSP's join expansion.
- Row timestamps + `GeConst` = LSM sequence numbers + "everything since"
  (4). Deferred rebuild = deferred compaction.
- Generic join / AGM = topic 10's join ordering with a worst-case bound
  instead of a cost model; `plan.rs`'s min-fill elimination is a
  join-order search.
- The triangle query is topic 24's triangle counting, executed by a join
  engine rather than a graph kernel.
- An e-graph over sound rewrites is a generator of equivalent queries —
  topic 16's metamorphic oracle, with a proof attached.

## M44 log (capstone)

- [ ] relational rewrite stage in the planner, both numbers reported
      (plan cost vs hand pass; match time vs backtracking)
- [ ] timestamped e-node table, semi-naive saturation loop
- [ ] one cyclic rewrite pattern, binary plan measured next to GJ

## Done when

- both stubs green and the prediction table reconciled;
- you can say, without looking, which of lane 1a and lane 1b generic
  join loses and why — and predict it from the pattern alone;
- lane 3's intermediate measured rather than estimated;
- guide questions answered; M44 outline drafted.
