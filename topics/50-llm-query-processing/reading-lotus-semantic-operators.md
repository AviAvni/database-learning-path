# Semantic operators: a gold algorithm, and guarantees relative to it

LOTUS (Patel, Jha, Pan, Gupta, Asawa, Guestrin & Zaharia; arXiv 2407.11418,
published as "Semantic Operators and Their Optimization" in PVLDB 18(11),
2025) is the paper that gave "an LLM inside a query plan" a relational shape.
It does two things. First, it defines a small algebra — filter, join, top-k,
aggregate, group-by, map — whose predicates are written in natural language,
and pins down what each one *means* by naming a reference implementation.
Second, it shows how to replace that reference with something cheaper while
promising, with a stated probability, that the answer stays close to it. The
second half is the classic approximate-query-processing bargain, and its fine
print is the most important sentence in the paper: the promise is made
*relative to the reference implementation*, not relative to the truth. This
chapter builds both halves from zero, works the statistics on this topic's
own simulated numbers, and then reads the evaluation — including three places
where the paper disagrees with itself.

Every section, table and figure number below is from arXiv **v3** (1 March
2025), the version read for this chapter.

## The problem in one sentence

A filter whose predicate is "does this review sound sarcastic?" costs one
language-model call per row — dollars and seconds instead of nanoseconds — so
how do you skip most of those calls and still be able to say how wrong the
answer might be?

## The concepts, step by step

### Step 1 — a semantic operator, and the gold algorithm that defines it

> **In:** nothing yet — a relational filter `WHERE p(row)` as every database
> already runs it.
> **Out:** the semantic operator, its **gold algorithm**, and the cost of that
> algorithm in model calls — the quantity every later step tries to reduce.

A **large language model** (LLM) is a program that, given a text prompt,
returns text; for our purposes it is a function you pay to call. A
**semantic operator** is a relational operator whose predicate, projection or
ranking criterion is a natural-language instruction evaluated by such a model
instead of a code expression. LOTUS calls the instruction a **langex**
(language expression, §2.1): a template with column references, such as
`"the {abstract} makes the most outrageous claim"`.

Because natural language is ambiguous and models are imprecise, "the rows for
which the langex is true" does not define an answer on its own. LOTUS fixes
that with a **gold algorithm** (§2.1): a specific, tractable way of
orchestrating a specific model over the data, whose output *is* the
operator's correct answer by definition. The model it uses is the **oracle**
— in practice "the strongest LLM a practitioner has available" (footnote 1).

Table 1 lists the gold algorithms; three matter here:

| operator | gold algorithm (Table 1, §2.3) | oracle calls for N (or N1, N2) rows |
|---|---|---|
| `sem_filter` | one call per row, one row per prompt | N |
| `sem_join` | nested loop, one call per pair | N1 · N2 |
| `sem_topk` | quick-select with pairwise comparisons | rounds of N − 1 comparisons |

Worked: a filter over 10,000 reviews is 10,000 oracle calls. A join of 250
patient articles against the BioDEX label set is the 6,092,500 calls Table 5
lists for the gold algorithm — 6,092,500 / 250 = 24,370 candidate labels per
article, which is the "24,000 possible drug-reaction labels" of §5.2 to two
significant figures.

Why it matters: "gold" is a definition, not ground truth. A gold filter run
with an oracle that is right 97% of the time is *correct by definition* and
wrong about 3% of rows. Every guarantee below is measured against it.

### Step 2 — precision, recall, and what "close to gold" means

> **In:** the gold algorithm's output from Step 1 (the set of rows the oracle
> keeps).
> **Out:** three numbers — precision, recall, F1 — that measure how far a
> cheaper plan's output is from it; Steps 3–5 use them as targets.

For a filter or join, compare the set a plan returns (`out`) with the set the
gold algorithm returns (`gold`):

- **precision** = |out ∩ gold| / |out| — of the rows returned, the fraction
  gold also returns;
- **recall** = |out ∩ gold| / |gold| — of gold's rows, the fraction returned;
- **F1** = 2 · precision · recall / (precision + recall) — their harmonic
  mean, a single number that is low if either is low.

LOTUS's definition of a **correct optimization** (§2.2) is one that reduces
cost while meeting an **accuracy target** γ — for filters and joins a recall
target γ_R and a precision target γ_P — **with probability 1 − δ**, where δ is
the user's allowed **failure probability**. Its evaluation default is
γ = 0.9, δ = 0.2 (§5, "LOTUS").

Worked, on this topic's lane 1a (`semop_bench`, simulated): the query keeps
reviews where two predicates hold. Ground truth has 297 rows; the gold set
(oracle on every row, 97% accurate per predicate) has 364. They share 276.
Scored against *truth*, gold itself has precision 276 / 364 = 0.758, recall
276 / 297 = 0.929, F1 = 2 · 276 / (364 + 297) = 552 / 661 = **0.835**. Scored
against *gold*, gold has F1 1.000 by definition. A plan that reproduced the
truth exactly would score 0.835 against gold — and fail a γ = 0.9 target.

Why it matters: the target is relative. That is a design choice, and a
defensible one (truth is unavailable at query time; the oracle is), but it
means the guarantee is about agreeing with the oracle, errors included.
Lane 1c' of this topic's bench shows the consequence: once the proxy is more
accurate than the oracle, an F1-vs-gold target *forbids* using it.

### Step 3 — the proxy model and the two-threshold cascade

> **In:** the targets from Step 2 and a second, cheaper model.
> **Out:** a decision rule with two thresholds, τ+ and τ−, that splits rows
> into "accept", "reject" and "ask the oracle" — the object Step 4 learns.

A **proxy model** (A in Algorithm 1) is a cheap model that returns a
**score** — a number saying how likely the row passes — rather than a yes/no.
For filters LOTUS uses a small LLM and takes the score from the
**log-probability** the model assigns to the output token "True" (the log of
the probability the model gives that token), re-scaled by quantiles over the
whole relation (§3.1 and its footnote 5). A **model cascade** runs the cheap
model first and the expensive one only where the cheap one is unsure.

Algorithm 1 (§3.1) learns two thresholds and then applies one pass:

```
score ≥ τ+            → keep the row (proxy's answer)
τ− < score < τ+       → ask the oracle, keep its answer
score ≤ τ−            → drop the row (proxy's answer)
```

(The pseudocode's last branch is written `A(t) ≥ τ−`, i.e. the oracle band is
closed at τ−; the prose says "less than or equal to τ−" is rejected. The
difference is one boundary point.)

Worked, on lane 1c of this topic's bench (simulated, both predicates, band
[0.5 − h, 0.5 + h]): at h = 0.30 the cascade asks the oracle 7,702 times
instead of 13,090 and returns a set with precision 0.895 and recall 0.909
against gold. The band is where the cost is: every row inside it is an oracle
call, and every row outside it is a proxy decision that may be wrong.

Why it matters: errors come from only two places. A row above τ+ that gold
rejects costs precision; a row at or below τ− that gold keeps costs recall.
Rows in the band cost money but no accuracy. Choosing τ+ is therefore a
precision problem and choosing τ− a recall problem — which is how Step 4
splits δ.

### Step 4 — learning the thresholds from a sample, with a correction

> **In:** the two-threshold rule from Step 3 and the targets γ_R, γ_P, δ from
> Step 2.
> **Out:** thresholds learned on an oracle-labelled sample, corrected so the
> targets hold on the whole table with probability 1 − δ.

The thresholds must be chosen before the full pass, so LOTUS labels a
**sample** S of rows with both models (Algorithm 1's first lines). Two
statistical ideas make that sample usable:

- **Importance sampling** draws rows with probability weighted towards the
  ones that matter (here, by proxy score) and corrects each sampled row's
  contribution by the inverse of its draw probability, so estimates stay
  unbiased. LOTUS "defensively mixes a uniform sample" (§3.1, following SUPG
  [38]) so low-score rows still appear.
- A **confidence interval** is a range that contains the true value of a
  quantity with a stated probability. Picking the threshold that *just*
  meets the target on the sample uses the sample's estimate as if it were
  exact; half the time the true value is on the wrong side.

That second point is not hypothetical. Lane 2 of this topic's bench learns
the thresholds for one predicate (γ_R = γ_P = 0.9, δ = 0.2, 300-row sample)
by fitting the sample exactly, 100 times on 100 seeded tables: **68 of the
100** miss a target on the full table — more than three times δ.

The fix is to shift each target by the interval's radius. LOTUS uses "the
central limit theorem and the normal approximation" (§3.1) — the sample mean
of n values with standard deviation σ is within about σ · √(2 ln(1/δ′) / n)
of the true mean with probability 1 − δ′ — and spends half of δ on each
threshold: "each with an error probability of δ/2". Choosing among many
candidate thresholds is a **multiple-hypothesis testing** problem (each
candidate is a test, and the chance that *some* candidate looks good by luck
grows with their number), which LOTUS says it corrects for.

Worked: a 300-row sample of a predicate with selectivity 0.31 holds about 93
gold positives. For recall, the fraction q of positives the proxy would drop
(score ≤ τ−) must stay below 1 − 0.9 = 0.10. With δ′ = δ/2 = 0.1 the
normal-approximation radius is √(q(1 − q)) · √(2 ln 10 / 93) =
√(q(1 − q)) · 0.2225, so the largest sample miss-rate you may accept solves
q + 0.2225 √(q(1 − q)) ≤ 0.10: **q ≈ 0.051**, about half the naive 0.10. The
distribution-free alternative (Hoeffding's inequality, radius √(ln(1/δ′) /
2n) = √(2.303 / 186) = **0.111**) exceeds 0.10 by itself — at n = 93 it can
certify no τ− above zero at all. Lane 2's reference numbers in
[notes.md](notes.md) are those two learners: 6% of trials missed with 4,504
band calls, and 0% missed with 9,122.

Why it matters: the correction is what you pay for the guarantee, and it is
paid in oracle calls (4,504 against the naive 2,204 in that lane). Its size is
set by the sample size and by how many thresholds you scan.

### Step 5 — joins: two proxies, and an optimizer that picks the cheaper one

> **In:** the cascade from Steps 3–4 and the nested-loop gold join from Step
> 1 (N1 · N2 oracle calls).
> **Out:** two join plans with different proxies, and the rule LOTUS uses to
> choose between them — its one cost-based decision.

For joins even a small-LLM proxy per pair is quadratic, so LOTUS uses an
**embedding** proxy: an embedding is a vector a model assigns to a text so
that similar texts get nearby vectors, and the **similarity** of two rows is
a dot product of their vectors — one model call per *row*, not per pair (§3.2).
Two plans:

- **sim-filter**: score each pair by the similarity of the two join keys,
  then cascade.
- **project-sim-filter**: first run a semantic map over the left table
  ("which dataset does this abstract use?", an **ungrounded** projection —
  done without seeing the right table), then score by similarity of the
  *projected* value to the right key. It helps when matching pairs are not
  similar as written.

LOTUS learns thresholds for both, computes how many oracle calls each would
need, and runs the cheaper (§3.2, last paragraph). Table 5 (BioDEX, Llama-70B):

| plan | RP@5 | RP@10 | time (s) | LM calls |
|---|---|---|---|---|
| Plan 1, sim-filter | 0.1541 | 0.170 | 12,563 | 27,687 |
| Plan 2, project-sim-filter (chosen) | 0.212 | 0.213 | 2,116 | 5,290 |
| gold algorithm | — | — | 2,144,560 | 6,092,500 |

(RP@K, **rank-precision at K**, is the precision of the top K labels in the
ranking, the BioDEX metric.) Plan 2 needs 27,687 / 5,290 = 5.2× fewer calls
than Plan 1 and 6,092,500 / 5,290 = **1,152×** fewer than gold — the "1,000×"
of the abstract. Its RP@5 is 0.212 / 0.1541 − 1 = **37.6%** higher, matching
§5.2's "37% higher RP@5 and 25% higher RP@10" (0.213 / 0.170 − 1 = 25.3%).

Why it matters: this is cost-based optimization in the classic sense —
enumerate plans, estimate each one's cost, run the cheapest — with the
estimate obtained by *running the threshold learner* on a sample. The cost
model counts oracle calls in the band, and reading-lotus-source.md shows it
counts nothing else.

### Step 6 — top-k by pairwise comparison, and why quick-select

> **In:** the gold-algorithm idea from Step 1, applied to ranking.
> **Out:** the call counts of three ranking algorithms, and the reason LOTUS
> prefers the one that is not the cheapest in calls.

LOTUS ranks with **pairwise comparisons** (prompt the model with two rows,
ask which ranks higher) because point-wise scores and list-wise prompts "have
been shown to face quality issues" (§2.3). Three algorithms over comparisons
are candidate gold algorithms: a quadratic all-pairs sort, a heap-based
top-k, and **quick-select**, which picks a pivot, compares every remaining
row to it in one parallel round, and recurses on one side.

Table 7's quadratic column is the check: SciFact re-ranks 100 retrieved
articles and records **4,950** calls = 100 · 99 / 2; HellaSwag-bench ranks
200 abstracts and records **19,900** = 200 · 199 / 2. The heap and
quick-select need 192.6 / 237.0 calls on SciFact and 241.5 / 448.1 on
HellaSwag (20 trials averaged, hence the fractions). Quick-select makes *more*
calls than the heap on both, yet runs 40.5 s against 60.0 s and 49.4 s
against 59.4 s, because "each round is fully parallelizable" while heap
updates are sequential (§5.3).

Why it matters: calls are not latency. With requests issued in parallel, the
cost that matters for wall time is the number of *rounds*; the same trade
shows up in this topic's bench, where the cascade's proxy pass and oracle
pass are separate waves.

### Step 7 — what the evaluation shows, and where it disagrees with itself

> **In:** the whole construction (Steps 1–6).
> **Out:** the numbers worth keeping, each with its table, and three
> inconsistencies a careful reader should not repeat.

The fact-checking pipeline (FEVER, Table 2) is the cleanest end-to-end
number: the unoptimized LOTUS program reaches 91.2% accuracy in 329.1 s
batched; the optimized one (filter cascade, Llama-8B proxy, γ = 0.9,
δ = 0.2) reaches 91.0% in 190.0 s — **1.73×** faster at 91.0 / 91.2 =
**99.8%** of the accuracy, exactly the intro's "1.7× faster and obtains 99.8%
accuracy". FacTool's 5,396.11 s against the optimized 190.0 s is the "28×".
Figure 9 reports the guarantee holding: observed failure rates over 50 trials
below the configured δ (Figure 9d), and more oracle calls at higher targets
and with a weaker proxy (Figure 9c) — the shape lanes 1c and 2 of this topic
reproduce.

Three places the paper disagrees with itself (v3):

1. **"170%" or "100%"?** The abstract says LOTUS matches or exceeds recent
   LLM-based analytics systems "by up to 170%"; the introduction says "by up
   to 100%", and §5's bullet list says "up to 170% higher accuracy" against
   systems and "exceeds it by up to 100%" against hand-written pipelines. The
   two figures describe different comparisons; the abstract's sentence
   conflates them.
2. **Figure 9's line styles.** The caption says "a Llama-8B proxy (dashed
   lines) or a weaker TinyLlama proxy (solid lines)"; the text says "the
   Llama-8B proxy (solid lines) and a weaker proxy, with TinyLlama-1B (dashed
   lines)". One of them is backwards, and the figure alone cannot say which.
3. **The chosen join plan, twice.** Table 3 reports "LOTUS Join" at RP@5
   0.212, 5,644 LM calls, 2,340 s; Table 5 reports the same plan ("Plan 2
   (chosen)", RP@5 0.212) at 5,290 calls and 2,116 s. The 354-call gap is
   consistent with Table 3 including the threshold-learning sample, but the
   paper does not say so.

Why it matters: none of these changes the paper's argument, but each is the
kind of number that gets quoted. Quote the table, not the abstract.

## How to read the paper with these concepts in hand

| section | what to take from it | step |
|---|---|---|
| §1, Figure 1 | the FacTool comparison and the 1.7× / 99.8% optimized program | 7 |
| §2.1–2.2, Table 1 | langex, world model M, gold algorithm, correct optimization (γ, δ) | 1, 2 |
| §2.3 | gold algorithms per operator; pairwise top-k; hierarchical sem_agg | 1, 6 |
| §3.1, Algorithm 1 | proxy scores from log-probabilities, importance sampling, τ+/τ− at δ/2 each | 3, 4 |
| §3.2, Figure 3 | sim-filter vs project-sim-filter, choose the cheaper | 5 |
| §3.3–3.4 | group-by with an embedding proxy; similarity-guided pivot for top-k | 6 |
| §5, Tables 2–7 | evaluation; read the tables, then the prose | 5, 6, 7 |
| §5.5, Figure 9 | the guarantee measured: observed vs target recall/precision/δ | 4, 7 |

Read §3.1 next to the code: [reading-lotus-source.md](reading-lotus-source.md)
walks `cascade_utils.py`, where the paper's one paragraph of statistics
becomes 100 lines and acquires details the paper does not mention.

## Questions to answer

1. LOTUS's guarantee is relative to the gold algorithm. Name a workload where
   that is exactly what a user wants, and one where it is not.
2. Why does the recall threshold get δ/2 and not δ? What would go wrong if
   each threshold got the full δ?
3. Project-sim-filter adds N1 semantic-map calls before any join work. Under
   what selectivity of the join does that pay?
4. Quick-select makes more comparisons than the heap and finishes sooner.
   Write the latency model (calls, rounds, parallelism) that predicts it.
5. A proxy that is better than the oracle: what should a system do with it,
   and what does a gold-relative target make it do?

## Done when

Answer each before unfolding it.

- [ ] You can say what a gold algorithm is and why LOTUS needs one.

  <details><summary>Answer</summary>

  A natural-language predicate does not define an answer: two models, or two
  ways of prompting one model, give different sets. LOTUS defines each
  operator's correct output as the output of one named, tractable algorithm
  run with one named model (the oracle) — the gold algorithm (§2.1). That is
  what makes "an optimization is correct" a checkable statement: an optimized
  plan is compared against the gold output. The price is that "correct" now
  means "agrees with the oracle", including where the oracle is wrong.

  </details>

- [ ] You can compute precision, recall and F1 of the gold set against ground truth for lane 1a, and say why a perfect plan would fail a γ = 0.9 target.

  <details><summary>Answer</summary>

  Gold has 364 rows, truth 297, overlap 276: precision 276 / 364 = 0.758,
  recall 276 / 297 = 0.929, F1 = 552 / 661 = 0.835. A plan that returned the
  297 true rows would be scored *against gold* — it would share the same 276
  rows, so its F1 vs gold would be 0.835 too, below 0.9. The target measures
  agreement with the oracle, and a truthful plan disagrees with the oracle's
  errors.

  </details>

- [ ] You can explain where a two-threshold cascade can lose precision, where it can lose recall, and where it cannot lose either.

  <details><summary>Answer</summary>

  Rows with proxy score ≥ τ+ are kept on the proxy's word: a row there that
  gold rejects is a false positive, so precision is lost only above τ+. Rows
  with score ≤ τ− are dropped on the proxy's word: a row there that gold keeps
  is a false negative, so recall is lost only at or below τ−. Rows in between
  get the oracle's answer, which is gold by definition — they cost oracle
  calls and no accuracy. Hence τ+ is learned for precision and τ− for recall,
  each with half of δ.

  </details>

- [ ] You can work the recall correction for a 93-positive sample at δ/2 = 0.1, both ways, and say which learner each lane-2 reference number belongs to.

  <details><summary>Answer</summary>

  Normal approximation: radius √(q(1 − q)) · √(2 ln 10 / 93) = 0.2225 √(q(1 −
  q)); the largest acceptable sample miss rate solves q + 0.2225 √(q(1 − q)) ≤
  0.10, giving q ≈ 0.051 (so τ− sits lower than the naive fit). Hoeffding:
  radius √(ln 10 / 186) = 0.111 regardless of q, already above the 0.10
  budget, so no positive τ− can be certified. In notes.md the variance-aware
  learner misses 6% of trials with 4,504 band calls; the Hoeffding learner
  misses 0% with 9,122 — it is valid and nearly useless.

  </details>

- [ ] You can state Table 5's numbers and derive the "1,000×" and "37%" from them.

  <details><summary>Answer</summary>

  Plan 1 (sim-filter): 27,687 calls, RP@5 0.1541. Plan 2 (project-sim-filter,
  chosen): 5,290 calls, RP@5 0.212. Gold: 6,092,500 calls. 6,092,500 / 5,290 =
  1,152 — the abstract's "1,000×" rounds down. 0.212 / 0.1541 = 1.376, the
  "37% higher RP@5" of §5.2; 0.213 / 0.170 = 1.253 is the "25%" for RP@10.

  </details>

- [ ] You can name the three internal inconsistencies and say which number to quote for each.

  <details><summary>Answer</summary>

  (1) "up to 170%" (abstract) vs "up to 100%" (introduction): §5 shows 170% is
  against LLM analytics systems and 100% against hand-written pipelines —
  quote whichever comparison you mean, with its table. (2) Figure 9's caption
  and text swap which proxy is solid and which dashed — quote Figure 9 only
  for its qualitative shape. (3) Table 3 gives the chosen join 5,644 calls and
  2,340 s, Table 5 gives 5,290 and 2,116 at the same RP@5 — quote Table 5 for
  the plan comparison and say which table you used.

  </details>

## References

**Paper** — Liana Patel, Siddharth Jha, Melissa Pan, Harshit Gupta, Parth
Asawa, Carlos Guestrin, Matei Zaharia. "Semantic Operators: A Declarative
Model for Rich, AI-based Data Processing." [arXiv:2407.11418v3](https://arxiv.org/abs/2407.11418)
(1 March 2025); published as "Semantic Operators and Their Optimization:
Enabling LLM-Based Data Processing with Accuracy Guarantees in LOTUS", PVLDB
18(11): 4171–4184, 2025. Code: [github.com/lotus-data/lotus](https://github.com/lotus-data/lotus).

| where | what this chapter takes from it |
|---|---|
| Abstract, §1, Figure 1 | 1,000×; 170% / 100%; 3.6×; 1.7× and 99.8% |
| §2.1–2.3, Table 1 | langex, oracle, gold algorithms, γ and δ |
| §3.1, Algorithm 1, footnote 5 | log-probability proxy, importance sampling, δ/2 per threshold |
| §3.2, Table 5 | sim-filter, project-sim-filter; 27,687 / 5,290 / 6,092,500 calls |
| §5 intro, §5.1, Table 2 | γ = 0.9, δ = 0.2 defaults; 91.2 / 91.0 / 80.9; 329.1 / 190.0 / 5,396.11 s |
| §5.2, Tables 3–4 | 5,644 calls, 2,340 s |
| §5.3, Tables 6–7 | 4,950 and 19,900 quadratic comparisons; quick-select vs heap |
| §5.5, Figure 9 | the guarantee measured; caption vs text line styles |

**This topic** — [README.md](README.md) (lanes 1c, 1c′ and 2),
[notes.md](notes.md) (lane 2 reference learners),
[reading-lotus-source.md](reading-lotus-source.md) (the cascade in code),
[reading-abacus.md](reading-abacus.md) (an optimizer that treats LOTUS's
cascade as one physical operator among thousands).
