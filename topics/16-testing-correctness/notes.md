# Topic 16 notes — testing & correctness engineering

## Meta-surprise (recorded before you even start)

The provided crash_matrix caught a REAL bug in this crate's own
"correct" KV during development: recovery didn't truncate the WAL
tail, so torn leftover records silently joined the NEXT commit's
batch (Bug::None showed 72.7% divergence). Tail repair fixed it to
0.0%. The tooling paid for itself before the exercises began —
that's the whole thesis of the topic.

## Baseline (provided, measured 2026-07-10)

crash_matrix: 5000 seeds × 40 ops (50/20/20/10 put/del/commit/crash),
inline oracle harness:

| bug | caught | rate | first seed |
|---|---|---|---|
| None | 0 | 0.0% | — |
| LostDelete | 3738 | 74.8% | 0 |
| NoSyncOnCommit | 4980 | 99.6% | 0 |
| TornWriteAccepted | 2442 | 48.8% | 3 |
| StaleRead | 4706 | 94.1% | 0 |

Each sweep ≈ 0.02 s for 5000 seeds — 200K simulated crash-recoveries
per second. This is why DST beats kill -9 loops (topic 5's harness
took seconds per crash).

## Predictions (fill BEFORE implementing dst.rs / shrink.rs / tlp.rs)

| question | prediction | actual |
|---|---|---|
| will your dst.rs rates match the table above? (same weights, same seeds — should be exact) | | |
| shrink: 40-op LostDelete failure → how many ops after ddmin? (theoretical min = 5) | | |
| ddmin replay calls to shrink one case | | |
| TLP: % of 100 random preds (depth 3, 25% NULLs) that expose the null-blind engine | | |
| which bug needs the MOST seeds to catch with only 5% crash weight? | | |

## Implementation log

- [ ] dst.rs: gen_ops + run_case + lockstep post-crash check — all
      6 tests green (incl. determinism replay)
- [ ] shrink.rs: ddmin — 1-minimal repro ≤ 10 ops
- [ ] tlp.rs: Kleene eval3 + partition check — correct engine passes
      100 preds, null-blind engine caught
- [ ] dst_run output (shrunk repros per bug) recorded here:
- [ ] optional: cargo-fuzz a real target (needs nightly)

Surprises / dead ends:

## Questions from the reading guides

### turso simulator (reading-turso-simulator.md)

1. Why now() must ADVANCE time:
2. Per-file-stem fault targeting — which bug class:
3. turso sim vs Antithesis — what each can't test:
4. Why stateful shrinking is harder than pure-input shrinking:
5. Three properties to port to Cypher:

### FDB / Antithesis (reading-fdb-simulation.md)

1. Disk-lies vs Raft's assumptions (+ VSR/TigerBeetle answer):
2. Why BUGGIFY branches don't invalidate the test:
3. The four escapes (compiler/kernel/sim-bug/wall-clock) — who catches:
4. Why simulation outruns real time:
5. Our engine's remaining nondeterminism sources for M16:

### SQLancer code (reading-sqlancer.md)

1. PQS false-positive triage (whose evaluator is wrong):
2. Why TLP's p must be deterministic:
3. NoREC-visible vs -invisible topic 10 rules:
4. turso properties → PQS/TLP/NoREC mapping:
5. Cypher TLP: what plays NULL in a graph pattern:

### PQS + TLP papers (reading-pqs-tlp-papers.md)

1. Why rectification wastes no generated expressions:
2. A bug PQS misses but TLP catches (and vice versa):
3. Why AVG doesn't decompose (↔ topic 11 partial aggregation):
4. Why index-present/absent runs sharpen the oracles:
5. First three Cypher TLP recombinations + their ⊎:

### Jepsen / elle (reading-jepsen.md)

1. What SIGSTOP models that kill -9 doesn't:
2. What elle can't check over plain registers:
3. Pure-rw cycle = write skew — permitted/forbidden where:
4. ReadIndex fix in one sentence + cost:
5. Mini-elle over topic 15's sim — what determinism trivializes:

### Z3 / Cosette (reading-z3.md)

1. Why DB rewrite proofs stay quantifier-free:
2. Hash-consing → what becomes pointer compare:
3. NOT (a = b) vs a <> b under NULLs — Z3's verdict:
4. The set-valid bag-invalid rewrite:
5. Encodings for filter-commute and filter-past-projection:

## Cross-topic threads

- sim_fs's buffered/synced/torn model = topic 5's crash matrix made
  exhaustive; the WAL-tail-truncation bug it caught is topic 5's
  "recovery must repair the tail" lesson, relearned the hard way.
- The topology of every oracle here = topic 15's sim.rs: seed →
  deterministic world → invariant check. DST is the single-node
  version of what sim.rs does for clusters.
- TLP's 3-valued trap is topic 10/11's expression semantics; the
  null-blind engine is a predicate-pushdown bug in miniature.
- Z3 tactics = query plans for proofs (probe = cardinality estimate,
  tactic pipeline = rewrite rules, solver = executor).
- elle's dependency-graph cycles = topic 8's serialization graph
  testing, recovered from history instead of tracked in the engine.

## M16 log (correctness spine)

- [ ] TCK subset runner + tck_done.txt tracking (reference has both)
- [ ] proptest: graph ops vs BTreeMap-of-adjacency model oracle
- [ ] DST: SimClock + fault-injecting IO under M5's WAL
- [ ] cargo-fuzz: Cypher parser, RESP framing, page/SST decoders
      (reference bar: fuzz_target_runtime + expressions/ + clauses/)
- [ ] Z3: two topic-10 rewrites verified; one broken on purpose →
      counterexample row recorded here:

## Done when

- All dst/shrink/tlp tests green; dst_run prints ≤10-op repros for
  all four bugs; prediction table filled.
- Reading-guide questions answered; M16 fuzz-target list committed.
