# splink: Fellegi–Sunter compiled to SQL

splink is the UK Ministry of Justice's open-source record-linkage engine, and it makes one
architectural bet: the entire Fellegi–Sunter probabilistic linkage pipeline — blocking, EM
parameter estimation, pairwise scoring, and clustering — can be expressed as generated SQL.
The Python layer never touches a record; it builds SQL strings and hands them to a pluggable
dialect (DuckDB, Spark, SQLite, PostgreSQL), so the same model definition scales from a
laptop to a cluster. This guide reads the code at commit `04189f5` in `~/repos/splink`,
and ties each stage back to the miniature Rust reimplementation in this topic's experiments.

## The problem in one sentence

**Given millions of records with no shared key, decide which ones refer to the same real-world entity — without labeled training data, and without comparing all n² pairs.**

## The concepts, step by step

### Step 1 — The Linker façade: a model is comparisons + blocking rules

`Linker` (`linker.py:66`) is the API surface. You construct it with a settings object —
a list of comparisons (how to compare each field, as a ladder of agreement levels) and a
list of blocking rules (which candidate pairs to even look at) — plus a `db_api` that
selects the SQL backend. Everything downstream is a method on this object that emits SQL.
The five-stage pipeline:

```
  records ──> [1 blocking rules] ──> candidate pairs
                     │
  [2 u: random sampling]   [3 m,p: one EM session per rule]
                     │             │
                     └──────┬──────┘
                            v
              [4 predict: sum match weights ──> probability]
                            v
              [5 cluster: connected components at threshold]
```

### Step 2 — Blocking: SQL self-joins instead of n² pairs

`block_using_rules_sqls` (`blocking.py:747`) compiles each blocking rule into a SQL
self-join (e.g. equal `dob`), and unions multiple rules with deduplication so a pair
matched by two rules is scored once. Before committing to a rule, the pre-flight analysis
in `blocking_analysis.py:349` counts how many comparisons a rule would generate — the
difference between a feasible job and an accidental cross join.

```
  rule 1: l.dob = r.dob          ──> self-join on dob
  rule 2: l.last_name = r.last_name ──> self-join on last_name
                    │
                UNION + dedup (pair scored once even if both rules fire)
                    v
  full space: 112,492,500 pairs ──> blocked: 271,012 pairs (415×, experiment)
```

On Spark, these equi-join blocking passes become hash shuffles — the same partitioning
story as topic 36: the blocking key is the shuffle key, and a skewed key (a very common
surname) becomes a hot partition exactly like a skewed shard.

### Step 3 — u probabilities: random pairs are almost all nonmatches

`estimate_u_using_random_sampling` (`linker_components/training.py:163`) estimates u —
the probability that a *nonmatching* pair agrees on a comparison level — by sampling
random record pairs and requiring no labels at all: in the full n² space, almost every
pair is a nonmatch, so raw agreement rates among random pairs ≈ u. The experiment
measures u = [0.0052 0.0021 0.0003 0.0051 0.0006] from 200k random pairs, ≈ 1/pool-size
per field, exactly as the birthday-collision intuition predicts. Note the asymmetry that
makes this work: u needs no blocking and no labels because random pairs are dominated by
nonmatches, whereas m (agreement among true matches) is the hard parameter — random
sampling almost never draws a match, which is why m needs EM on blocked pairs (Step 4).

### Step 4 — m and the prior via EM, one session per blocking rule

`estimate_parameters_using_expectation_maximisation` (`linker_components/training.py:231`)
runs ONE EM session per blocking rule, and — the crucial trick — *excludes* the comparisons
on the blocking-rule columns from that session: every candidate pair agrees on them by
construction, so including them degenerates the fit. m values estimated by multiple
sessions are averaged, and the prior p is fitted alongside. The core loop is
`expectation_maximisation` (`expectation_maximisation.py:225`); the E-step SQL is built at
`expectation_maximisation.py:18` (`compute_new_parameters_sql`), the M-step
(`maximisation_step`) at `:193`. E-step: score every blocked pair with current m/u/p to
get a match probability. M-step: recompute m, u, p as probability-weighted agreement
rates. Repeat to convergence — each iteration is one SQL round trip.

```
  session A: block on last_name        session B: block on dob
  ┌───────────────────────────┐        ┌───────────────────────────┐
  │ last_name  [MASKED]       │        │ last_name  m estimated    │
  │ first_name m estimated    │        │ first_name m estimated    │
  │ dob        m estimated    │        │ dob        [MASKED]       │
  └───────────────────────────┘        └───────────────────────────┘
        m(dob) etc. averaged across sessions; p also fitted
```

The experiment reproduces the degeneracy: keep the blocked field in and the fitted prior
p races to 1.0; mask it and you get m = [0.80 0.86 0.94 0.78 0.90], p = 0.184.

### Step 5 — Comparison ladders: graded agreement, per-level m/u

A comparison is not boolean. `ComparisonLevel` (`comparison_level.py:148`) represents one
rung of an ordered ladder, each rung carrying its own m (`comparison_level.py:190`) and u
(`:191`); the library ships graded levels like `LevenshteinLevel`
(`comparison_level_library.py:406`), `JaroWinklerLevel` (`:458`), and `JaroLevel` (`:493`).
The match weight per level is `log2(m/u)` bits, computed at `comparison_level.py:426`.
The ladder is ordered: each pair lands on the first rung whose condition it satisfies,
so the "else" rung soaks up disagreement and carries a negative weight.

```
  first_name ladder            m      u      weight = log2(m/u)
  ├─ exact match             0.70   0.005    +7.1 bits
  ├─ jaro_winkler >= 0.9     0.20   0.010    +4.3 bits
  └─ else (all other pairs)  0.10   0.985    -3.3 bits
```

### Step 6 — Term frequency: "Smith" is worth fewer bits

`_tf_adjustment_sql` (`comparison_level.py:667`) implements term-frequency adjustment:
agreeing on the surname "Smith" carries less evidence than agreeing on a rare surname,
so the adjustment scales the level's weight by the token's frequency relative to the
average frequency. This is the information-theoretic reading of Fellegi–Sunter made
concrete: the bits you earn from an agreement depend on how surprising the agreement is,
and it is again all generated SQL — a join against a per-column TF table. For a
performance-minded reader this is the interesting part: TF tables are computed once and
reused across predictions, and the adjustment is a multiplicative factor on the weight
(additive in log space), so it composes with the ladder without changing the model shape.

### Step 7 — Predict: sum the bits, squash to a probability

`predict()` (`linker_components/inference.py:294`) is the user-facing scoring entry point:
block, compute the comparison-vector level per field per pair, then sum. The pairwise
scoring SQL is assembled in `predict.py:42`, and `_combine_prior_and_mws` (`predict.py:203`)
folds in the prior as `log2(p/(1-p))` and converts the total match weight to a probability.

```
  prior  log2(p/(1-p))      = -2.1 bits
  first_name (jw >= 0.9)    = +4.3 bits
  last_name  (exact, TF-adj)= +6.0 bits
  dob        (else)         = -3.3 bits
                       mw   = +4.9 bits
  P(match) = 1 / (1 + 2^(-mw)) = 0.968
```

Conditional independence lets the per-field weights simply add — the same naive-Bayes
assumption the 1969 paper makes. Correlated fields (city and postcode) double-count
evidence; splink's answer is model design (combine them into one comparison), not a
richer dependency model, because independence is what keeps scoring a single SQL pass.

### Step 8 — Clustering: connected components in SQL

`cluster_pairwise_predictions_at_threshold` (`linker_components/clustering.py:43`)
thresholds the pairwise scores and calls the iterative connected-components algorithm in
`graph_operations/connected_components.py:121` — a union-find-equivalent computed as
repeated SQL passes until representatives stabilize, yielding one cluster id per record.
The experiment does the same with an in-memory union-find: linking at 12 bits gives
precision 0.989, recall 0.992, in 48 ms for 15k records — same algorithm, different
substrate. The dialect layer (`dialects.py:24`, `SplinkDialect`) is why all of this ports:
DuckDB at `:270`, Spark at `:402`, SQLite at `:532`, PostgreSQL at `:674` — one model,
four engines. Every stage in this guide — blocking, EM, prediction, clustering — is
generated SQL, which is the whole reason the same settings object runs single-node
DuckDB during development and a Spark cluster in production.

## Where each step lives in the code

All paths under `splink/internals/` in `~/repos/splink` @ `04189f5`.

| Step | Concept | Anchor |
|---|---|---|
| 1 | `Linker` façade, settings + db_api | `linker.py:66` |
| 2 | Blocking rules compiled to SQL self-joins, union + dedup | `blocking.py:747` |
| 2 | Pre-flight comparison counts per blocking rule | `blocking_analysis.py:349` |
| 3 | u from random sampling, no labels | `linker_components/training.py:163` |
| 4 | One EM session per blocking rule, blocked columns excluded | `linker_components/training.py:231` |
| 4 | EM core loop; E-step SQL; M-step | `expectation_maximisation.py:225`, `:18`, `:193` |
| 5 | `ComparisonLevel`; m at `:190`, u at `:191`; weight `log2(m/u)` | `comparison_level.py:148`, `:426` |
| 5 | Graded levels: Levenshtein / JaroWinkler / Jaro | `comparison_level_library.py:406`, `:458`, `:493` |
| 6 | Term-frequency adjustment SQL | `comparison_level.py:667` |
| 7 | `predict()` entry point; scoring SQL; prior + weights → probability | `linker_components/inference.py:294`, `predict.py:42`, `:203` |
| 8 | Threshold + clustering; iterative connected components in SQL | `linker_components/clustering.py:43`, `graph_operations/connected_components.py:121` |
| all | `SplinkDialect`: DuckDB/Spark/SQLite/PostgreSQL | `dialects.py:24`, `:270`, `:402`, `:532`, `:674` |

## Questions to answer in notes.md

1. Why does including the blocking-rule columns in an EM session degenerate the fit, and how does the experiment (`er.rs`) demonstrate the failure mode (fitted p → 1.0)?
2. Random pairs give u for free because nonmatches dominate the n² space — but what bias does this introduce for u when the dataset contains many duplicates, and how would you detect it?
3. Trace one comparison ladder from `comparison_level_library.py` to the final SQL CASE expression: where do m, u, and the TF adjustment each enter the generated query?
4. splink runs connected components as iterated SQL joins (`connected_components.py:121`) rather than in-memory union-find. What is the per-iteration cost on Spark, and when does the SQL formulation win over pulling edges into memory?
5. m values estimated by multiple EM sessions are averaged (`training.py:231`). When would the sessions disagree substantially, and what does that disagreement tell you about the conditional-independence assumption?

## Done when

- [ ] You can sketch the five-stage pipeline from memory and name the file that owns each stage.
- [ ] You can explain, in bits, how a pair's match weight is assembled (prior + per-level `log2(m/u)` + TF adjustment) and squashed via `1/(1+2^(-mw))`.
- [ ] You can state why each EM session masks its own blocking columns, and reproduce the degeneracy in the local experiment.
- [ ] You have run or re-read `experiments/src/er.rs` and matched each of its phases (u sampling, masked EM, blocking, 12-bit threshold, union-find) to its splink counterpart.

## References

- Repo: `~/repos/splink` @ commit `04189f5` (moj-analytical-services/splink) — read under `splink/internals/`.
- Fellegi, I. & Sunter, A. (1969). "A Theory for Record Linkage." *JASA* 64(328) — the m/u model, agreement weights, optimal decision rule.
- Winkler, W. (2006). "Overview of Record Linkage and Current Research Directions." US Census Bureau — survey covering EM estimation, string comparators (Jaro–Winkler), and TF adjustments.
- splink docs: topic guides on the Fellegi–Sunter model and training (the paper trail from the 1969 model to the implementation).
- Local experiment: `topics/39-fraud-identity-graphs/experiments/src/er.rs` — miniature of the same pipeline (u = [0.0052 0.0021 0.0003 0.0051 0.0006]; m = [0.80 0.86 0.94 0.78 0.90], p = 0.184; blocking 112,492,500 → 271,012 pairs; precision 0.989 / recall 0.992 at 12 bits, 48 ms for 15k records).
- Cross-topic: blocking self-joins on Spark = hash shuffles (topic 36); step 8 clustering = connected components, same union-find as the experiment.
