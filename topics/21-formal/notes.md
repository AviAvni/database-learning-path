# Topic 21 notes — formal methods & verification

## Baseline (provided code, Apple M3 Pro, measured 2026-07-10)

### Hand-ordered rewriter (eqsat_bench, 20 seeds/depth)

| depth | in cost | hand cost | µs/expr | firings |
|---|---|---|---|---|
| 4 | 31 | 21 | 5.4 | 4 |
| 6 | 127 | 90 | 25.2 | 15 |
| 8 | 511 | 355 | 129.7 | 61 |
| 10 | 2047 | 1390 | 534.8 | 248 |

~30% cost reduction, ~2 µs per rule firing (clone-heavy rebuild —
each pass reallocates the whole tree). Linear in size, no search:
this is the baseline egg must beat on *quality* while inevitably
losing on time.

### The ordering trap

`(a*2)/2`: hand rewriter → `(a<<1)/2`, **cost 5** (1 firing,
18.8 µs). R2 (strength reduction) destroys the Mul before R4
(div-reassoc) can see it. egg keeps both forms → should reach `a`,
cost 1.

### TLC on WalReplication.tla (tla2tools.jar, java 17)

| config | states generated | distinct | depth | result | time |
|---|---|---|---|---|---|
| SyncCommit=TRUE | 2583 | 1080 | 14 | Durability holds | <1 s |
| SyncCommit=FALSE | 183 | 123 | 5 | **VIOLATED** | <1 s |

The counterexample is 5 states: Append → Commit(no quorum) →
Crash(r1) → Failover(r2, empty log) ⇒ committed=1 > wal[r2]=0.
The postgres `synchronous_commit=off` data-loss story, found
exhaustively in under a second on a 3-replica/3-entry model.
Removing one conjunct (the quorum gate) flips 1080-states-safe to
counterexample-at-depth-5 — invariants are load-bearing.

## Predictions (fill BEFORE implementing the stub)

| question | prediction | actual |
|---|---|---|
| egg on trap: cost, iterations to find `a` | | |
| egg µs/expr at depth 6 vs hand's 25 µs — slowdown ×? | | |
| e-graph enodes at depth 8 input (511 nodes) with comm rules | | |
| which StopReason at depth 10 with node_limit 10k | | |
| egg cost vs hand cost at depth 10 — how much better? | | |
| add assoc rules for + and *: still terminates under limits? | | |

## Implementation log

- [ ] eqsat.rs egg_optimize — all 3 tests green (trap → cost 1)
- [ ] prediction table reconciled
- [ ] stretch: ConstantFold as an e-class Analysis (egg tutorial
      pattern) — replaces the div-same-folds-constants trick
- [ ] stretch: Rejoin(r) in WalReplication → find the stale-primary
      trace → re-derive terms (reading-tlaplus-raft.md Q1)
- [ ] stretch: DAG-cost extraction (lp_extract) vs greedy on a
      shared subexpression

Surprises / dead ends:

- TLC found the SyncCommit=FALSE violation after only 123 states —
  BFS means the *shortest* counterexample comes out first, which is
  why TLC traces are readable.

## Questions from the reading guides

Each guide now ends with its own `## Questions (answer in notes.md)`
list — six per guide, and they changed when the guides were rewritten
against the depth rules. Answer them against the guide you just read
rather than against a copy that can drift out of sync; several of the
old entries here encoded claims the rewrite corrected (Z3 does *not* use
Nelson–Oppen, and Lean's runtime is Beans, not Perceus).

- [reading-aws-cacm15.md](reading-aws-cacm15.md) — answers:
- [reading-egg-popl21.md](reading-egg-popl21.md) — answers:
- [reading-z3-tacas08.md](reading-z3-tacas08.md) — answers:
- [reading-tlaplus-raft.md](reading-tlaplus-raft.md) — answers:
- [reading-lean-perceus.md](reading-lean-perceus.md) — answers:

## Cross-topic threads

- Deferred rebuild (egg) = delta-matrix wait (20) = LSM compaction
  (4): batch the invariant repair, amortize the fixup.
- E-graph = Cascades memo (10) + congruence; hand.rs IS topic 10's
  push_down/reorder pipeline, now with a measured miss.
- e-graph hashcons = topic 8's hash table; machine.rs pattern VM =
  topic 19's bytecode interpreter (same Bind/Scan/Compare shape).
- Runner limits = topic 10's DP cutoff = topic 19's jit_above_cost:
  every search needs a budget gate.
- TLC exhaustive interleavings vs proptest sampling (16): same state
  machine, different quantifier (∀ vs ∃-sampled).
- Z3 backtracking trail = topic 8's undo log; justifications = WAL
  for unions.

## M21 log (capstone)

- [ ] TLA+ spec of MVCC visibility (or adapt WalReplication) + TLC
      in CI (java -cp tla2tools.jar, seconds at model scale)
- [ ] Lean 4 proof: delta-matrix invariant DP∩M=∅ preserved by
      set/remove — calibrate proof-vs-test cost
- [ ] optional egg rewrite stage in planner: node-limited Runner,
      cardinality estimates as e-class analysis

## Done when

- eqsat stub green (trap → cost 1); prediction table reconciled;
- TLC both configs re-run and understood line-by-line; one stretch
  (Rejoin or ConstantFold analysis) attempted;
- guide questions answered; M21 spec outline drafted.
