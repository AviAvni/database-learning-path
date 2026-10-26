# Topic 16 — Testing & Correctness Engineering

The topic that separates hobby DBs from production DBs. The unifying
idea: a database is too big to test by example — you need *oracles*
(what should be true) and *generators* (inputs you'd never write by
hand), plus determinism so every failure replays.

```
                 generator ──→ SUT ──→ result
                     │                   │
                     └──→ oracle ────────┴──→ equal? / invariant holds?
```

Every technique in this topic is one choice of generator + oracle:

| technique | generator | oracle |
|---|---|---|
| property testing | random ops | in-memory model |
| DST | random ops + FAULTS + sim clock | model + invariants |
| PQS (SQLancer) | random query around a pivot row per table | "pivot row must appear" |
| TLP / metamorphic | one query, three partitions | self-consistency |
| fuzzing | coverage-guided byte mutation | "doesn't crash" |
| Jepsen/elle | concurrent client histories | linearizability checker |
| Z3 / Cosette | symbolic (ALL inputs at once) | UNSAT = proven equal |

## The problem, measured (bench lane 1, provided — runs today)

`cargo run --release --bin crash_matrix` — 5000 seeded workloads × 40 ops
(50% put / 20% delete / 20% commit / 10% crash) against a KV store with one bug
planted at a time:

```
bug                      caught         rate     first seed
None                          0         0.0%              -
LostDelete                 3738        74.8%              0
NoSyncOnCommit             4980        99.6%              0
TornWriteAccepted          2442        48.8%              3
StaleRead                  4706        94.1%              0
```

**Every planted bug is caught, three of them by the very first seed.** The
per-seed rates (48.8% to 99.6%) are properties of *these planted bugs under
this generator*, not of the technique: a bug is caught by a seed only if the
40 random ops happen to arrange the exact sequence that exposes it. The
decision-useful figure is what a CI run must budget, and the same lane prints
it. With independent seeds, a bug with per-seed rate *p* is missed by *N* seeds
with probability (1 − *p*)^*N*, so reaching confidence *c* takes
*N* = ⌈ln(1 − *c*) / ln(1 − *p*)⌉ seeds. Worked on the slowest bug,
TornWriteAccepted, *p* = 0.4884: ln(0.05) / ln(0.5116) = −2.996 / −0.6704 =
4.47, so **5 seeds** for 95%. The lane checks the formula by cutting the 5000
seeds into disjoint blocks of *N* and counting blocks with at least one hit:

```
bug                         p     N @95%   N @99.9%     blocks hit (N@95%)
LostDelete             0.7476          3          6           1650/1666
NoSyncOnCommit         0.9960          1          2           4980/5000
TornWriteAccepted      0.4884          5         11            977/1000
StaleRead              0.9412          2          3           2493/2500
```

(97.7% of blocks for the 5-seed case, 99.0% for 3 seeds, 99.7% for 2 and 99.6%
for 1: all at or above the 95% target.) Read it as the honest version of the headline: the
spread that looked like "four wildly different odds" is **one to five seeds**
of difference. Nobody ships a harness that runs five seeds, so the real
exposure is the bug whose *p* is 0.001, not 0.49. The lever that moves *p* is
workload length, and the second table in the lane sweeps it (first 2000 seeds):

```
bug                    5 ops  10 ops  20 ops  40 ops  80 ops
LostDelete              5.6%   14.3%   38.5%   74.8%   98.1%
NoSyncOnCommit         82.0%   90.2%   96.4%   99.8%  100.0%
TornWriteAccepted       5.0%   13.3%   27.5%   50.6%   75.5%
StaleRead              22.4%   46.4%   73.5%   94.5%   99.8%
```

Doubling the workload from 40 to 80 ops takes TornWriteAccepted from 50.6% to
75.5% and LostDelete from 74.8% to 98.1%: the bugs need a specific *pattern*
(put, commit, delete, crash in order), and a longer workload contains more
chances for it. When a bug is rare, lengthen the workload before multiplying
the seed count, and rerun this table to see which dial buys more.

The `None` row is the load-bearing one. 0.0% is not a formality: any nonzero
number there means the harness reports divergence on a correct implementation,
and a false-positive oracle is worse than no oracle because it trains you to
ignore it. Check that row first, every time you change the generator.

Note also what is *not* here: no timing claim. The value of deterministic
simulation is that a failure comes with a seed you can replay, and the exercise
lanes are about shrinking that seed's 40 ops to the 3 that matter.

## 1. Deterministic simulation testing (DST)

FoundationDB's gift to the industry (turso, TigerBeetle, Antithesis
built identities on it). Rule: the SUT owns NO nondeterminism —
clock, network, disk, scheduling all come through interfaces backed
by a seeded RNG in test.

```
 real:  code → syscalls → kernel  (time, threads, fsync — nondeterministic)
 DST:   code → traits ──→ SimClock  (ChaCha8 from seed)
                     ├──→ SimFile   (buffered; crash DROPS unsynced,
                     │               may TEAR the last write)
                     └──→ SimNet    (topic 15's sim.rs already did this)
        ⇒ failure = a u64 seed. Re-run seed = same bug, every time.
```

turso's simulator (testing/simulator/) generates *interaction plans*
(workload-distributed SQL + property assertions), executes them over
fault-injecting IO (seeded pread/pwrite faults and latency), and
double-checks by running the same plan twice. Its in-memory backend
adds power loss: every block reverts to its last *synced* state, and
the `FsyncNoWait` property cuts the power while an fsync is pending —
fault coverage the kernel will never give you on demand (topic 5's
crash matrix, automated). What it does not do is make `sync` itself
return an error: that fault is wired up and deliberately ignored
([reading-turso-simulator.md](reading-turso-simulator.md), Step 3).

## 2. Metamorphic oracles: SQLancer

The test-oracle problem: for a random query, who knows the right
answer? SQLancer's insight — you don't need one. You need a second
query whose result must RELATE to the first:

- **PQS** (pivoted query synthesis): pick one existing row from *each*
  table (§3.1 — not one row overall), *synthesize* a WHERE clause that
  evaluates TRUE on that combination (rectify NULLs as you go), assert
  the pivot appears in the result. Finds: expression-evaluation bugs —
  61 of the paper's 99 confirmed bugs, §4.2 Table 3. Needs: an
  expression evaluator of your own, which is the cost of PQS and the
  reason SQLancer now lists it as unmaintained (`sqlancer/README.md:80`);
  the oracle is still present, in eight `Test*PQS.java` files.
- **TLP** (the paper's title is *Query Partitioning*; ternary logic is
  the mechanism): any predicate p splits rows three ways — `p`,
  `NOT p`, `p IS NULL` (SQL is 3-valued!). So
  `Q ≡ Q where p ⊎ Q where NOT p ⊎ Q where p IS NULL`, where `⊎` is
  *multiset* union — with plain set union the identity would not hold
  on duplicates. Finds: optimizer logic bugs. Needs: nothing but a union.
- **NoREC**: run the query optimized (`WHERE p`) and unoptimized
  (`SELECT (p IS TRUE) FROM t`, summed) — counts must match. The
  `IS TRUE` is load-bearing: it collapses NULL to false so the sum
  counts exactly the rows `WHERE p` would return.
  Finds: predicate-pushdown/index bugs.

The same idea carries to graph query languages, where it is harder:
Cypher clauses change the graph while the query runs (a node created by
one `CREATE` is visible to the next `DELETE`), so random generators
either emit shallow queries or invalid ones. **Dinkel** (Wüst, Jiang,
Su, *Dinkel: State-Aware and Granular Framework for Validating Graph
Databases*, VLDB 2026 (PVLDB 19(6)),
[paper](https://www.vldb.org/pvldb/vol19/p1198-jiang.pdf) ·
[code](https://github.com/CelineWuest/dinkel)) tracks query context and
graph schema clause by clause to generate deep, valid queries, and uses
clause- and expression-level rewrites that preserve semantics as its
oracle. On Neo4j, RedisGraph and Memgraph it found 127 bugs, 113
confirmed and 84 fixed; 62 of them were in RedisGraph — FalkorDB's
direct ancestor — of which 48 were confirmed, 29 fixed, and 34 were
crashes (§5, Tables 5–6). It is a ready harness to point at the
capstone's Cypher front end next to the TCK runner in M16.
**PATHTest** (Gui, Lan, Lu, Lu, Pan, *Testing Graph Databases via
Transformations Between Fixed-Length and Variable-Length Queries*, VLDB
2026 (PVLDB 19(9)),
[paper](https://www.vldb.org/pvldb/vol19/p2234-gui.pdf) ·
[code](https://github.com/VLDB2026/VLDB2026_PATHTest)) is TLP's
partition-and-recombine trick specialized to path semantics.
Variable-length patterns such as `(a)-[*1..2]->(b)` go through different
operators and optimizations than the fixed-length patterns they must
agree with, so PATHTest rewrites one into the other — folding a chain of
fixed hops into a bounded variable-length path and re-imposing the
original label constraints as predicates — and compares results. Across
seven systems (Neo4j, Memgraph, AgensGraph, NebulaGraph, JanusGraph,
HugeGraph, TinkerGraph) it found "41 previously unknown bugs … among
which 24 are logic bugs, and 17 correspond to unexpected errors", 29
confirmed and 11 fixed (Abstract, Table 2). FalkorDB was not among the
systems tested; the transformation is a direct test for the capstone's
variable-length traversal.

## 3. Fuzzing

Coverage-guided byte mutation (libFuzzer/AFL via cargo-fuzz) for
anything that PARSES: Cypher text, RESP frames, page/SST decoders.
turso fuzzes expressions/casts/schemas; the capstone reference ships
`fuzz/fuzz_targets/` for runtime + clauses + expressions. Structured
fuzzing (`arbitrary`-derived ASTs, like turso's `fuzz_target!(|expr:
Expr|)`) beats byte soup once the parser is solid.

## 4. Jepsen & elle

Black-box distributed testing: drive real concurrent clients against
a real cluster while injecting partitions (topic 15's failure menu),
record the *history*, then check it against a consistency model.
elle finds cycles in the serialization graph (G0/G1c/G-single...)
in polynomial time by exploiting known list-append semantics.
The redis-raft analysis is the cautionary tale: acked writes lost on
failover — exactly our `stale_leader` test, found in production code.

Finding the anomaly is half the job; a concurrent test case that
triggers an isolation bug is large and nondeterministic, so it cannot be
shrunk by rerunning it the way
[`reading-hypothesis.md`](reading-hypothesis.md) shrinks a
single-threaded case. **Pisco** (Weng, Yang, Hu, Zhang, Pan, Yang, Zhou,
Chen, He, Pan, *Pisco: An Isolation Bug Case Reduction and
Deduplication Framework*, VLDB 2026 (PVLDB 19(6)),
[paper](https://www.vldb.org/pvldb/vol19/p1413-weng.pdf) ·
[code](https://github.com/DBHammer/Pisco)) simulates the DBMS's lock and
version state to fix the order of conflicting operations, then shrinks
with a dependency-aware divide-and-conquer that never drops an operation
the anomaly depends on, and deduplicates reports with a multi-agent LLM
framework. It reached minimal cases in 20.0% of C-Reduce's time and
33.3% of DDMin's (§7.3.1), with a deduplication ratio of up to 91.6%
(§7.4) — the shrinking problem, for the anomalies
[`reading-jepsen.md`](reading-jepsen.md) finds.

## 5. SMT: proving instead of testing

Z3 answers "does there EXIST an input where P ≠ Q?" — testing all
inputs at once. Encode two query plans as formulas over symbolic
rows; UNSAT = rewrite proven, SAT = counterexample row (Cosette).
Perfect fit for topic 10's rewrite rules: filters/projections are
pure logic, exactly Z3's home turf. Z3 itself is a masterclass
codebase: a high-performance search engine over logic (tactics =
query plans for proofs).

## Experiments (`experiments/`)

1. `sim_fs.rs` + `kv.rs` — PROVIDED: a tiny WAL-backed KV store over
   a simulated file system (buffered writes lost on crash, last
   record may TEAR) with four INJECTABLE BUGS: `LostDelete`,
   `NoSyncOnCommit`, `TornWriteAccepted`, `StaleRead`.
2. `dst.rs` — YOU implement: the harness. Seeded op/crash-schedule
   generation, execute against kv + BTreeMap model, recover, verify.
   Tests pin: every injected bug caught within 200 seeds; `Bug::None`
   survives 500 seeds.
3. `shrink.rs` — YOU implement: delta-debugging minimizer — a failing
   op sequence shrinks to a minimal reproducer that still fails.
4. `tlp.rs` — YOU implement: 3-valued predicate evaluator + TLP
   check over a mini row-filter engine with a deliberately buggy
   "optimized" path (NULL-blind pushdown). TLP must catch it; the
   fixed path must pass.
5. `crash_matrix` — PROVIDED (runs without stubs): sweeps
   crash-point × sync policy on the correct KV, reports recovery
   outcomes (topic 5's crash harness, now simulated and exhaustive).

## Reading guides

| guide | chapter |
|---|---|
| [reading-turso-simulator.md](reading-turso-simulator.md) | turso's simulator: every failure is a u64 seed |
| [reading-fdb-simulation.md](reading-fdb-simulation.md) | FoundationDB & Antithesis: the whole cluster in one thread |
| [reading-sqlancer.md](reading-sqlancer.md) | SQLancer: 450+ bugs from three tiny oracles |
| [reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md) | PQS & TLP: solving the test-oracle problem twice |
| [reading-jepsen.md](reading-jepsen.md) | Jepsen & elle: isolation anomalies are cycles |
| [reading-z3.md](reading-z3.md) | Z3 & Cosette: testing every input at once |
| [reading-hypothesis.md](reading-hypothesis.md) | Hypothesis: shrinking a choice sequence, not a value |
| [reading-antithesis.md](reading-antithesis.md) | Antithesis: assertions as a search signal, and a simulation that branches |

## Capstone M16

The correctness spine (reference bar: `fuzz/` with runtime/clauses/
expressions targets, `tck_done.txt`, `flow_tests_done.txt`):

- [ ] openCypher TCK subset runner as the black-box oracle; track
      `tck_done.txt`-style progress
- [ ] proptest model-checking: graph ops (add/delete node/edge,
      property set) vs an in-memory model oracle
- [ ] DST harness: SimClock + fault-injecting IO under M5's WAL —
      the crash matrix becomes exhaustive and seeded
- [ ] cargo-fuzz targets: Cypher parser, RESP framing, page/SST
      decoders
- [ ] Z3: verify two topic-10 rewrite rules equivalent; break one on
      purpose and get the counterexample row
