# Abacus: Cascades that keep a Pareto frontier, fed by a bandit

Abacus (Russo, Liu, Sudhir, Vitagliano, Cafarella, Kraska & Madden, PVLDB
19(5), 2026; arXiv 2505.14661) is the first optimizer for semantic operators
that looks like the optimizers of topic 10: rules enumerate physical
implementations, a cost model scores them, and a Cascades-style search picks
a plan. What it has to change is everything a relational optimizer takes for
granted. There are no statistics, so operator costs and qualities are
*measured* by running operators on a few inputs — and with roughly 3,000
physical choices per query, it cannot measure them all. And there is a third
dimension, quality, so "the cheapest plan" is not the question; "the best
plan under $1" is, and the principle of optimality that makes dynamic
programming work does not survive the constraint. Abacus's answers are a
**Pareto-Cascades** search and a **multi-armed bandit** that decides what to
measure. This chapter builds both from zero, works each on small numbers,
reads the evaluation (whose prose and tables disagree), and then reads the
optimizer in Palimpzest, where the cost model turns out to be both richer and
different from the paper's Equation 1.

## The problem in one sentence

A semantic map can be implemented about 2,800 different ways, each with an
unknown quality, dollar cost and latency that can only be learned by paying to
run it — so how does an optimizer find the best plan under a budget without
sampling them all?

## The concepts, step by step

### Step 1 — physical operators for a semantic operator

> **In:** a logical plan of semantic operators (scan, filter, map, join,
> top-k — Table 1), as LOTUS or Palimpzest programs produce it.
> **Out:** the size of the search space: per-operator alternatives and their
> product across the plan — the problem Steps 3 and 4 attack.

As in any optimizer, a **logical operator** says *what* to compute and a
**physical operator** says *how*. For a semantic map or filter, Abacus's
**implementation rules** (§4.1) generate physical operators from four
techniques:

- **Model selection** — one call to one LLM, parameterized by which model.
- **Mixture-of-Agents** — 1–3 "proposer" models answer, an "aggregator"
  model combines them; parameterized by the models and the proposers'
  **temperature** (0.0, 0.4 or 0.8; temperature is the randomness of the
  model's sampling).
- **Reduced-Context generation** — split the input into chunks (1,000, 2,000
  or 4,000 characters), embed them, keep the top k (1, 2 or 4) most similar to
  the instruction, and send only those.
- **Critique-and-Refine** — generate, critique with a second model, refine
  with a third.

For joins, two rules: a nested-loop join (one LLM call per pair) and an
**Embedding Join** that drops pairs below a low similarity threshold, accepts
pairs above a high one, and asks the LLM in between — LOTUS's cascade as one
physical operator. With six models these rules give "approximately 2,800
different operators" for a map (§2.3) and "∼3,000 physical operators" in all
(§4.1). A plan with M semantic operators and N choices each has **O(N^M)**
physical plans before reordering (§2.3).

Worked: a three-operator plan (filter, map, map — Figure 1) with 2,800
choices per operator has 2,800³ ≈ 2.2 × 10¹⁰ physical plans. Measuring each
on even one input is out of the question.

Why it matters: the relational answer to a large space is a cost model that
is *cheap to evaluate* (statistics + formulas). Abacus has no statistics, so
its cost model must be built from measurements, and the measuring is the
expensive part.

### Step 2 — the cost model: three metrics, composed per operator

> **In:** per-operator estimates of quality, cost and latency (to be measured
> in Step 4).
> **Out:** a plan-level estimate built from operator-level ones — which is
> what lets Step 3 search a space it never samples.

Abacus measures **operators**, not plans, and composes (§2.3, Equation 1):

```
plan quality  p̂_q = Π over operators  ô_q,i
plan cost     p̂_c = Σ over operators  ô_c,i
plan latency  p̂_l = max over paths  Σ over operators on the path  ô_l,i
```

**Quality** is an operator's score against labels (or an LLM judge, o4-mini
by default, when labels are missing, §2.2); **cost** is dollars; **latency**
seconds. The product for quality assumes each operator's errors are
**independent** of the others' — the paper calls this out: a filter reading a
map's summary is correlated with the map's quality, and "this cost model will
fail to capture" it (§2.3).

Worked with Figure 2's illustrative per-operator estimates (a filter at
quality 0.95, $0.001, 0.0001 s; two maps at 0.31, $0.070, 1.1 s and 0.84,
$0.310, 3.7 s): plan quality 0.95 × 0.31 × 0.84 = **0.247**, plan cost
0.001 + 0.070 + 0.310 = **$0.381**, latency on the single path 0.0001 + 1.1 +
3.7 = **4.80 s**. (Figure 2's numbers are an illustration in the paper, not
measurements.)

Why it matters: Equation 1 is what makes the search tractable — 2,800 × 3
measured operators stand in for 2.2 × 10¹⁰ plans. Step 7 shows that the code
weights each operator's cost by its *input cardinality*, which Equation 1
does not.

### Step 3 — Pareto-Cascades: dynamic programming under a constraint

> **In:** the per-operator estimates of Step 2 and the user's objective
> (e.g. "maximize quality subject to cost ≤ $1").
> **Out:** the plan Abacus returns — and the reason it must keep many
> subplans per group instead of one.

**Cascades** (Graefe 1995; topic 10's [reading-selinger-cascades.md](../10-query-planning/reading-selinger-cascades.md))
organizes the search into **groups** — each group is a set of equivalent
subplans producing the same result — and keeps the best physical expression
per group, relying on the **principle of optimality**: every subplan of an
optimal plan is itself optimal (§3.1). With one objective, that is true. With
a constraint it is false: the cheapest way to compute a group may be the one
that blows the budget once the remaining operators are added.

A subplan **dominates** another if it is at least as good on every metric and
strictly better on one; the **Pareto frontier** is the set of subplans no
other subplan dominates. Theorem 3.1: under Equation 1's independence
assumption, every subplan of a Pareto-optimal plan is Pareto-optimal. So each
group keeps its Pareto frontier instead of one winner, and the final choice is
made once, at the root, against the constraint (Algorithm 4).

Worked: objective "max quality, cost ≤ $11". The group computing the first
operator has three candidates: A (quality 0.9, $10), B (0.8, $1), C (0.7, $2).
C is dominated by B (worse on both) and dropped; A and B are both on the
frontier. The next operator has M1 (0.9, $2) and M2 (0.5, $0.5). Keeping only
the per-group quality winner A forces A + M2 (A + M1 costs $12): quality
0.9 × 0.5 = **0.45**. Keeping the frontier finds B + M1: 0.8 × 0.9 = **0.72**
at $3. The greedy plan is feasible and 37.5% worse.

Why it matters: this is the classic trade of multi-objective query
optimization — the frontier makes each group's state bigger (bounded by the
number of sampled operators, §3.2) in exchange for not losing the constrained
optimum. The ablation (§4.6) measures the benefit: Pareto-Cascades satisfies
constraints **1.2×** more often than the greedy constrained variant.

### Step 4 — the bandit: deciding what to measure

> **In:** the search space of Step 1 and a **sample budget** B (in samples or
> dollars).
> **Out:** a set of per-operator estimates good enough for Step 3 — produced
> by spending the budget on operators that might be on the frontier.

A **multi-armed bandit** (MAB) is the problem of repeatedly choosing among
options ("arms") with unknown payoffs, each pull giving a noisy observation,
balancing **exploration** (trying arms you know little about) against
**exploitation** (pulling arms that look best). An **upper confidence bound**
(UCB) algorithm pulls by an optimistic estimate: mean plus a bonus that
shrinks as an arm is sampled more. With far more arms than samples it is an
**infinite-armed** bandit (§3.3).

Abacus keeps, per logical operator, a **frontier** of k physical operators
(default k = 6) and a **reservoir** of unsampled ones. Each round, every
frontier operator processes j inputs (default j = 4), and for every metric m
Abacus computes (§3.3):

```
ucb_m,i = μ_m,i + α · √(log N / n_i)        lcb_m,i = μ_m,i − α · √(log N / n_i)
```

where μ is operator i's sample mean, N the total samples drawn, n_i operator
i's samples, and α is "0.5 times the spread between the largest and smallest
observed metric values". An operator whose bounds do **not overlap** those of
any operator on the current (mean-based) Pareto frontier is evicted and
replaced from the reservoir (Algorithm 5).

Worked, on quality alone: three operators, 8 samples each, N = 24; mean
qualities 0.60, 0.75, 0.90. α = 0.5 × (0.90 − 0.60) = 0.15; √(ln 24 / 8) =
0.630; radius 0.15 × 0.630 = **0.095**. The 0.90 operator is the frontier,
with lcb 0.805. The 0.60 operator's ucb is 0.695 < 0.805 — no overlap, evicted.
The 0.75 operator's ucb is 0.845 > 0.805 — still possibly optimal, kept.

The budget: the evaluation sets B to **50 × the number of semantic
operators**, so with k = 6 and j = 4 "roughly half of the sample budget
(50 − 6 · 4 = 26) is used for exploration" (§4.3) — the first 24 samples per
operator go to the initial frontier.

Why it matters: the bandit is how Abacus prices its own optimization. Its
cost "is bounded above by the sample budget" (§2.2) — the analogue of a
relational optimizer's planning time, except billed in dollars. Table 2
splits it out: $0.17–$0.19 of optimization per benchmark.

### Step 5 — priors: the cheapest samples are the ones you do not take

> **In:** the bandit of Step 4.
> **Out:** a way to start the frontier near the right answer, and how much
> it is worth.

A **prior** here is "a simple dictionary mapping operators to a three-tuple
of their perceived quality, cost, and latency on a [0,1] scale" (§2.2). With
priors, the initial frontier is the operators closest to the prior Pareto
frontier rather than a random draw (§2.2, "Operator Sampling"). §4.4 compares
a *naive* prior (each model's MMLU-Pro score and per-token price) with a
*sample-based* prior (each operator run on 5 training examples).

Measured: at a fixed sample budget, priors give up to **1.60×** (CUAD) and
**1.43×** (BioDEX) better quality unconstrained, and up to **3.02×** and
**2.01×** under a cost constraint set at the 25th percentile of unconstrained
plan costs (§4.4). The introduction's "up to 3.04x" does not match the 3.02× in
§4.4's text; the figure behind both is Figure 6, a plot. In the ablation
(§4.6), priors improve the objective **3.5×** and constraint satisfaction
**1.8×** on average.

Why it matters: the constrained case benefits most, "because identifying a
good Pareto frontier of operators is more difficult than identifying a single
best operator" (§4.4). A relational optimizer's equivalent is a histogram it
already has; Abacus has to buy or borrow one.

### Step 6 — what the evaluation shows, and where prose and tables disagree

> **In:** the whole optimizer (Steps 1–5).
> **Out:** the numbers worth keeping, derived from Tables 2–3, and the
> paper's own negative result.

Table 2 (max-quality objective, 10 trials each; GPT-4o-mini,
text-embedding-3-small and clip-ViT-B-32 for every system):

| benchmark | Abacus quality | next best | Abacus total $ | next best $ | Abacus total s | next best s |
|---|---|---|---|---|---|---|
| BioDEX (RP@K) | 0.261 | LOTUS 0.216 | 0.89 | 18.9 | 450 | 2,348 |
| CUAD (F1) | 0.662 | DocETL 0.475 | 0.69 | 7.05 | 450 | 1,820 |
| MMQA (F1) | 0.304 | LOTUS 0.284 | 13.1 | 14.3 | 1,149 | 1,208 |

Derived: quality +20.8%, +39.4%, +7.0%; cost ratios 21.2×, 10.2×, 1.09×
(mean **10.8×**); time ratios 5.22×, 4.04×, 1.05× (mean **3.4×**). The
abstract's "10.8x cheaper and 3.4x faster" is exactly those means, and its
"6.7%–39.4%" matches except for MMQA, where Table 2's means give 0.304 / 0.284 − 1 =
7.0%, not 6.7%. But **§4.3's prose says "20.3%, 18.7%,
and 39.2% better mean quality" and "12.6x cheaper and 2.7x faster"** — numbers
that do not follow from Table 2 in any order. Quote the table.

Table 3 is the paper's own negative result. Asked to *minimize cost*, Abacus
cuts execution cost 2.50× on BioDEX ($0.70 → $0.28) and 4.25× on CUAD
($0.51 → $0.12), but on MMQA the "Reduction" is **0.81×** — the min-cost plan
costs more ($16.0) than the max-quality plan ($12.9). §4.3 explains: 75% of
max-quality MMQA plans already used the EmbeddingJoin, and its high variance
means "the minimum cost / latency plans achieve higher cost / latency than
their quality maximizing counterparts" when averaged over trials. And the
CUAD min-cost plan's quality is 0.05 — cheap, and nearly useless.

One more number worth keeping: LOTUS's sampled join thresholds, in the worst
case, produced joins "which require > 100,000 LLM calls" (§4.3) — the
variance SemBench also reports ([reading-sembench.md](reading-sembench.md),
Step 5).

Why it matters: an optimizer whose cost estimates come from a handful of
noisy samples inherits the noise. The MMQA row is what a high-variance
physical operator does to a cost-based choice.

### Step 7 — the code: cardinality, joins, and the bandit's bonus

> **In:** Steps 2–4 as the paper states them.
> **Out:** what Palimpzest computes at `807ed30` — where it matches
> the paper and where it does more.

Abacus is Palimpzest's optimizer (`README.md:55`;
`website/docs/user-guide/optimization.mdx:22` — `.optimize_and_run()`).
Every anchor below is `mitdbg/palimpzest` at commit `807ed30`
(`pyproject.toml` version 1.5.3).

The sample-based cost model prices an operator from its measured per-record
estimates:

```python
# src/palimpzest/query/optimizer/cost_model.py:246-261 — SampleBasedCostModel.__call__, at 807ed30
246          est_input_cardinality = (
247              source_op_estimates.cardinality * right_source_op_estimates.cardinality
248              if isinstance(operator, JoinOp)
249              else source_op_estimates.cardinality
250          )
251          op_estimates = OperatorCostEstimates(
252              cardinality=est_selectivity * est_input_cardinality,
253              time_per_record=est_time_per_record,
254              cost_per_record=est_cost_per_record,
255              quality=est_quality,
256          )
257
258          # compute estimates for this operator
259          op_time = op_estimates.time_per_record * est_input_cardinality
260          op_cost = op_estimates.cost_per_record * est_input_cardinality
261          op_quality = op_estimates.quality
```

The line to focus on is **260**: an operator's cost is its per-record cost
times its *input cardinality*, and line 252 makes its output cardinality the
measured **selectivity** times that input. So the model is not Equation 1's
flat sum: a filter placed first shrinks every later operator's bill, which is
exactly the predicate-ordering effect of this topic's lane 1a. And line 247
makes a join's input the **product** of its inputs' cardinalities — the N1·N2
of the nested loop, priced in.

Worked with Step 2's numbers on 1,000 documents and a filter of selectivity
0.3: 1,000 × $0.001 + 300 × $0.070 + 300 × $0.310 = **$115.00**, against
Equation 1's per-record $0.381 × 1,000 = $381 if every operator saw every
document.

Quality composes as a product for a pipeline (`src/palimpzest/core/models.py:976`,
`self.quality *= other.quality`) — Equation 1 — but for a join:

```python
# src/palimpzest/core/models.py:960-965 — PlanCost.join_add, quality, at 807ed30
960          for model_field in ["quality", "quality_lower_bound", "quality_upper_bound"]:
961              op_field_value = getattr(self, model_field)
962              left_plan_field_value = getattr(left_plan_cost, model_field)
963              right_plan_field_value = getattr(right_plan_cost, model_field)
964              if op_field_value is not None and left_plan_field_value is not None and right_plan_field_value is not None:
965                  dct[model_field] = op_field_value * ((left_plan_field_value + right_plan_field_value) / 2.0)
```

Line 965: the join's quality times the **average** of its inputs' qualities,
not their product. Two inputs at 0.8 and 0.9 feeding a 0.95 join: code 0.95 ×
0.85 = **0.808**; Equation 1's product would give 0.95 × 0.8 × 0.9 =
**0.684**. Latency at a join is the max of the two inputs under parallel
execution (lines 955–956), which is Equation 1's max over paths.

The bandit's bounds:

```python
# src/palimpzest/query/execution/mab_execution_strategy.py:470-485 — UCB/LCB per operator, at 807ed30
470          cost_alpha = 0.5 * (np.max(list(full_op_id_to_mean_cost.values())) - np.min(list(full_op_id_to_mean_cost.values())))
471          time_alpha = 0.5 * (np.max(list(full_op_id_to_mean_time.values())) - np.min(list(full_op_id_to_mean_time.values())))
472          quality_alpha = 0.5 * (np.max(list(full_op_id_to_mean_quality.values())) - np.min(list(full_op_id_to_mean_quality.values())))
473          selectivity_alpha = 0.5 * (np.max(list(full_op_id_to_mean_selectivity.values())) - np.min(list(full_op_id_to_mean_selectivity.values())))
474
475          # compute metrics for each physical operator
476          op_metrics = {}
477          for full_op_id in full_op_id_to_record_op_stats:
478              sample_ratio = np.sqrt(np.log(total_num_samples) / full_op_id_to_num_samples[full_op_id])
479              exploration_terms = np.array([cost_alpha * sample_ratio, time_alpha * sample_ratio, quality_alpha * sample_ratio, selectivity_alpha * sample_ratio])
480              mean_terms = (full_op_id_to_mean_cost[full_op_id], full_op_id_to_mean_time[full_op_id], full_op_id_to_mean_quality[full_op_id], full_op_id_to_mean_selectivity[full_op_id])
     # ... 481-482: comment on not clipping ...
483              lcb_terms = mean_terms - exploration_terms
484              ucb_terms = mean_terms + exploration_terms
485              op_metrics[full_op_id] = {"mean": mean_terms, "lcb": lcb_terms, "ucb": ucb_terms}
```

Lines 470–478 are §3.3's formula exactly: α = half the spread per metric,
bonus α · √(log N / n_i). Line 473 adds a fourth metric the paper's Algorithm
5 does not list — **selectivity** — and the dominance test uses it for
filters (`mab_execution_strategy.py:544`): a filter that passes fewer rows is
better, because (line 260 above) it makes everything downstream cheaper. The
overlap test itself (`:533-547`) compares an operator's *optimistic* bounds
(lcb cost, lcb time, ucb quality) with each frontier operator's *pessimistic*
ones, and `:562-569` brings a previously evicted operator back if it
re-enters the frontier — also beyond Algorithm 5.

Why it matters: the code is a better cost model than the paper's equation —
it knows about cardinality and selectivity, which is where the large factors
are — and a different one for joins. Anyone reproducing Abacus's plan choices
from Equation 1 will not get the code's answers.

## How to read the paper with these concepts in hand

| section | what to take from it | step |
|---|---|---|
| §1, Figure 1 | the problem; max-quality vs max-quality@cost plans | 1, 3 |
| §2.1–2.2, Table 1, Figure 2, Algorithm 1 | operators, inputs (validation data, priors, judge), the four-stage pipeline | 1, 4, 5 |
| §2.3, Equation 1 | O(N^M), the composed cost model and its independence assumption | 1, 2 |
| §3.1–3.2, Algorithms 2–4, Theorem 3.1 | Cascades, why constraints break optimality, Pareto-Cascades | 3 |
| §3.3, Algorithm 5 | UCB/LCB, α, frontier and reservoir | 4 |
| §4.1 | the implementation rules and their parameters | 1 |
| §4.3, Tables 2–3 | the headline comparison; MMQA's 0.81×; >100,000 LLM calls | 6 |
| §4.4–4.6, Figures 6–8 | priors, relaxed constraints, ablation | 3, 5 |
| §5 | operator dependencies; sequential frontier processing | 2 |

## Questions to answer

1. Equation 1 multiplies qualities. Construct a two-operator plan where the
   true quality is higher than the product, and one where it is lower.
2. Line 260 weights cost by input cardinality. What does that do to the
   relative value of a cheap, unselective filter and an expensive, selective
   one — and which classic optimizer rule does it reproduce?
3. α is half the spread of observed means. What happens to exploration when
   all sampled operators happen to look alike?
4. MMQA's min-cost plan cost more than the max-quality plan. What would an
   optimizer need to know about an operator, beyond its mean cost, to avoid
   that?
5. Abacus's optimization cost is a sample budget. How should that budget
   scale with the number of rows the plan will then process?

## Done when

Answer each before unfolding it.

- [ ] You can explain why a constraint breaks the principle of optimality, and work the A/B/M1/M2 example.

  <details><summary>Answer</summary>

  With one objective, the best plan's subplans are the best subplans, so each
  Cascades group keeps one winner. With "max quality, cost ≤ $11", the
  best-quality first operator A ($10, 0.9) leaves only $1 for the rest, so
  only M2 fits: quality 0.9 × 0.5 = 0.45. The lower-quality B ($1, 0.8)
  leaves room for M1: 0.8 × 0.9 = 0.72 at $3. Keeping each group's Pareto
  frontier (A and B; C is dominated by B) preserves the constrained optimum;
  Theorem 3.1 says, under operator independence, that is enough.

  </details>

- [ ] You can compute a UCB/LCB eviction by hand.

  <details><summary>Answer</summary>

  Means 0.60, 0.75, 0.90 at 8 samples each, N = 24: α = 0.5 × 0.30 = 0.15;
  bonus 0.15 × √(ln 24 / 8) = 0.15 × 0.630 = 0.095. Frontier operator (0.90)
  has lcb 0.805. The 0.60 operator's ucb 0.695 does not reach it — evicted;
  the 0.75 operator's ucb 0.845 does — kept for more samples. Same formula as
  `mab_execution_strategy.py:470-484`.

  </details>

- [ ] You can derive the abstract's 10.8× and 3.4× from Table 2 and say what §4.3's prose gets wrong.

  <details><summary>Answer</summary>

  Next-best total cost over Abacus's: 18.9 / 0.89 = 21.2, 7.05 / 0.69 = 10.2,
  14.3 / 13.1 = 1.09; mean 10.8×. Time: 2,348 / 450 = 5.22, 1,820 / 450 =
  4.04, 1,208 / 1,149 = 1.05; mean 3.4×. §4.3's text says "20.3%, 18.7%, and
  39.2%" better quality and "12.6x cheaper and 2.7x faster", which Table 2
  does not produce (its qualities give +20.8%, +39.4%, +7.0%).

  </details>

- [ ] You can state the paper's own negative result and its explanation.

  <details><summary>Answer</summary>

  Table 3, MinCost on MMQA: reduction 0.81× — the cost-minimizing plan
  executes for $16.0 against the quality-maximizing plan's $12.9. Most
  max-quality plans already used EmbeddingJoin (75% of trials), leaving
  little to cut, and EmbeddingJoin's high variance in cost and quality means
  averaged results can invert (§4.3). Also: CUAD's min-cost plan reaches
  quality 0.05.

  </details>

- [ ] You can name two ways Palimpzest's cost model differs from Equation 1, with the line for each.

  <details><summary>Answer</summary>

  (1) Cost is per-record cost × input cardinality, and output cardinality is
  measured selectivity × input (`cost_model.py:252, 260`), so filter
  placement changes downstream cost; Equation 1 sums operator costs flat.
  (2) A join's quality is its own quality times the *mean* of its inputs'
  (`models.py:965`), not the product. Also: the bandit tracks selectivity as a
  fourth metric (`mab_execution_strategy.py:473, 544`).

  </details>

## References

**Paper** — Matthew Russo, Chunwei Liu, Sivaprasad Sudhir, Gerardo
Vitagliano, Michael Cafarella, Tim Kraska, Samuel Madden. "Abacus: A
Cost-Based Optimizer for Semantic Operator Systems." PVLDB 19(5): 1060–1073,
2026. doi:10.14778/3796195.3796215; [arXiv:2505.14661](https://arxiv.org/abs/2505.14661).
Artifact: [github.com/mitdbg/palimpzest](https://github.com/mitdbg/palimpzest).

| where | what this chapter takes from it |
|---|---|
| Abstract, §1 | 6.7%–39.4%, 10.8×, 3.4×; §1's up to 3.04× with priors |
| §2.2, Figure 2 | validation data, priors, o4-mini judge; illustrative per-operator estimates |
| §2.3, Equation 1 | O(N^M), ~2,800 map operators, independence |
| §3.1–3.3, Theorem 3.1, Algorithms 4–5 | Pareto-Cascades; UCB/LCB; α = 0.5 × spread |
| §4.1 | rules and parameters; ~3,000 operators; the six models |
| §4.3, Tables 2–3 | the comparison; k = 6, j = 4, budget 50 × operators; 0.81×; >100,000 calls |
| §4.4–4.6 | 1.60× / 1.43×, 3.02× / 2.01×; ablation 3.5×, 1.8×, 1.2×, 1.4× / 1.3× |

**Code** — `mitdbg/palimpzest` at `807ed30` (version 1.5.3; pinned in
[resources/codebases.md](../../resources/codebases.md)):
`src/palimpzest/query/optimizer/cost_model.py:210-267`,
`src/palimpzest/core/models.py:928-987`,
`src/palimpzest/query/execution/mab_execution_strategy.py:369-580`,
`src/palimpzest/query/optimizer/tasks.py:423-500` (Pareto input plan costs).

**This topic** — [README.md](README.md) Step 2 (rank ordering, which line
260 encodes), [reading-lotus-source.md](reading-lotus-source.md) (the
EmbeddingJoin's ancestor), topic 10's
[reading-selinger-cascades.md](../10-query-planning/reading-selinger-cascades.md)
(Cascades without the frontier).
