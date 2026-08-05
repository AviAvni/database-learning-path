# Fellegi–Sunter record linkage: identity as a likelihood-ratio test

Fraud rings, KYC dedup, and identity graphs all hinge on one primitive: deciding whether two
records — with no shared key, and with typos — refer to the same real-world entity. Winkler's
2006 survey is the best single tour of the machinery the US Census Bureau built around the
Fellegi–Sunter model: a likelihood-ratio test over field-agreement patterns, weights measured
in bits, EM to fit the parameters with zero labeled data, and blocking to avoid the n² pair
explosion. Every modern entity-resolution engine (splink included) is this paper's pipeline
with better tooling. Read it as the spec for the `er.rs` experiment you will implement.

## The problem in one sentence

**Given two files of records with no shared key and noisy fields, decide for each candidate pair whether it is a match, a nonmatch, or needs human review — with provable error-rate guarantees and without comparing all n² pairs.**

## The concepts, step by step

### Step 1 — A record pair is an agreement pattern, and matching is hypothesis testing

> **In:** nothing yet — a single pair of records, compared field by field.
> **Out:** the agreement pattern γ (a bit vector of agree/disagree) and the likelihood ratio R = P(γ|M)/P(γ|U).

Forget records for a moment; look at a *pair* of records. Compare them field by field
(last name, first name, dob, city, phone) and reduce the pair to an agreement pattern γ —
essentially a bit vector of agree/disagree outcomes. Fellegi and Sunter (JASA 1969) framed
linkage as a classical hypothesis test between two populations of pairs: M (both records
refer to the same entity) and U (they do not). The evidence for a pair is the ratio

`R = P(γ | M) / P(γ | U)`

A pattern that is common among true matches but rare among random pairs pushes R up; a
pattern typical of random pairs pushes it down. Everything else in the paper — weights,
EM, comparators — is machinery for computing and thresholding R at scale.

### Step 2 — Two thresholds, and why the rule is optimal

> **In:** the ratio R (or its log) from Step 1, for one candidate pair.
> **Out:** a three-way label — match, clerical review, or nonmatch — via two thresholds, provably minimizing the review region.

Pick an upper cutoff T_μ and a lower cutoff T_λ. Following the paper's rule (Eq. 2): if
R > T_μ, designate the pair a match; if R < T_λ, designate it a nonmatch; if
T_λ ≤ R ≤ T_μ — boundaries included — send it to clerical review.

```
            nonmatch            clerical review              match
  ─────────────────────┬───────────────────────────┬─────────────────────▶ R
                      T_λ                         T_μ
   R below T_λ          │  humans look at these    │        R above T_μ
```

Fellegi–Sunter proved this rule optimal: among all decision rules achieving given
false-match and false-nonmatch rates, it minimizes the clerical-review region. That is the
theorem that justified automation — the 1990 US Census matching went from an estimated
3000 clerks for 3 months down to 200 people for 6 weeks.

### Step 3 — Match weights: log2 R decomposes into per-field bits

> **In:** the ratio R from Step 1 and the fitted (m_i, u_i) per field (Step 5 fits them; here they are given).
> **Out:** a per-field weight table in bits whose signed sum over an agreement pattern equals log2 R.

Under conditional independence of fields given the class (exactly the naive Bayes
assumption), the log-likelihood ratio splits into a sum of per-field contributions. With
**m_i = P(agree on field i | M)** and **u_i = P(agree on field i | U)**, a field that
agrees contributes **+log2(m_i / u_i)** bits and a field that disagrees contributes
**+log2((1 − m_i) / (1 − u_i))** bits (a negative number). Using the experiment's fitted
parameters (m = [0.80 0.86 0.94 0.78 0.90], u = [0.0052 0.0021 0.0003 0.0051 0.0006]):

```
  field    m       u        agree +log2(m/u)   disagree +log2((1-m)/(1-u))
  last    0.80   0.0052         +7.27                  -2.31
  first   0.86   0.0021         +8.68                  -2.83
  dob     0.94   0.0003        +11.61                  -4.06
  city    0.78   0.0051         +7.26                  -2.18
  phone   0.90   0.0006        +10.55                  -3.32
```

Work one pattern — last, first, dob agree, city disagrees, phone agrees:

```
  γ = [ last=agree, first=agree, dob=agree, city=disagree, phone=agree ]
        +7.27         +8.68        +11.61      -2.18          +10.55
        └──────────────────── sum ────────────────────┘  log2 R = 35.93 bits
```

All five fields agreeing sums to 45.36 bits; all five disagreeing to −14.70. Intuition:
u_i for a random pair is roughly 1/pool-size of the field, so rare values are worth more
bits (dob, with a 3650-value pool, pays +11.61 on agreement); m_i is dominated by the
field's typo rate. Score a pair by summing bits and compare against a threshold in bits —
exactly what splink calls match weights. (The 1969 paper writes the weight as any
monotone function of R, e.g. the natural log; bits — that is, log2 — is the splink
convention this topic uses throughout.)

### Step 4 — String comparators: exact equality throws away a quarter of your matches

> **In:** the binary agree/disagree pattern γ from Step 3, which typos corrupt.
> **Out:** a richer γ where a string comparator (Jaro–Winkler) discounts the agreement weight by similarity.

Exact character-by-character comparison misses more than 25% of true matches in census
data, purely from typos. Jaro's comparator counts common characters within a sliding
window plus transpositions; Winkler's variant boosts agreement when the strings share a
common prefix (typos cluster at the ends of names). The survey's key move is folding
*partial* agreement back into the Fellegi–Sunter framework: instead of a binary
agree/disagree, discount the agreement weight as string similarity falls, interpolating
between the full `+log2(m/u)` and the disagreement weight. The likelihood-ratio skeleton
is unchanged; only the γ alphabet gets richer.

### Step 5 — EM: fitting m, u, and p with zero labeled data

> **In:** the observed agreement-pattern counts over candidate pairs — no labels.
> **Out:** the fitted parameter vector (p, m, u) that Steps 2–3 consume, via EM on the latent class.

You never have labeled match/nonmatch pairs at census scale. Treat the class (M or U) of
each pair as a latent variable and run EM over the observed agreement-pattern counts,
fitting the parameter vector (p, m, u) where p is the proportion of matches among
candidate pairs (Winkler 1988 applied EM to latent-class record linkage).

```
        ┌────────────────────────────────────────────────┐
        │ E-step: for each pattern γ, posterior           │
        │   P(M|γ) = p·P(γ|M) / (p·P(γ|M) + (1-p)·P(γ|U)) │
        └───────────────┬────────────────────────────────┘
                        ▼
        ┌────────────────────────────────────────────────┐
        │ M-step: reestimate p, m_i, u_i as posterior-    │
        │ weighted agreement frequencies                  │
        └───────────────┬────────────────────────────────┘
                        └──── repeat until converged ────┘
```

It works because M and U pairs form two well-separated clusters in pattern space. The
survey also covers when conditional independence breaks and general interaction models
are needed — the weights stop being a clean per-field sum, but the decision rule survives.

### Step 6 — Blocking: never score n² pairs

> **In:** the two files of records (n² pairs is infeasible).
> **Out:** a candidate-pair set — the union of several blocking passes on different keys.

Scoring every pair is quadratic death. Only generate candidate pairs that agree on a
cheap blocking key (same postcode, same surname soundex), and run several passes with
*different* keys so a typo in one key cannot hide a duplicate — the union of passes is
the candidate set.

```
  pass 1: block on last name          pass 2: block on dob
  ┌─────────┐ ┌─────────┐             ┌─────────┐ ┌─────────┐
  │ SMITH   │ │ GARCIA  │    union    │ 1979-.. │ │ 1985-.. │
  │ r3 r17  │ │ r5 r9   │   ───────▶  │ r3 r41  │ │ r5 r88  │
  └─────────┘ └─────────┘             └─────────┘ └─────────┘
  pairs = Σ_buckets C(bucket,2)  instead of  C(n,2)
```

Winkler's 2004 example self-matches the 2000 Decennial Census of 300 million records —
10^17 pairs (300M × 300M) — and shows that 11 blocking criteria cut that to a subset of
~10^12 pairs while retaining 99.5% of the true matches. Database hook: a blocking key is a
hash-partition key (topic 36), and multi-pass blocking is just multiple shuffles over the
same data.

### Step 7 — Production scale: BigMatch and the census pipeline

> **In:** the multi-pass blocking + scoring pipeline from Steps 3–6, at census scale.
> **Out:** BigMatch — all 10 passes evaluated in one streaming pass over the big file, at ~100k pairs/sec.

BigMatch is the Census Bureau's production blocking-and-matching engine: it handles
workloads on the order of 100M × 4B record comparisons at roughly 100k pairs/sec, and —
the performance-engineering punchline — evaluates all 10 blocking passes *simultaneously
in one pass over the data*, rather than re-reading the files per pass. Think of it as a
multi-key hash join with the smaller file's indexes resident in memory. The whole survey
is really a systems paper wearing statistics clothing: optimal decision rule, measured
error rates, comparator microbenchmarks, and an engine that streams the big file once.

### Step 8 — The local experiment: er.rs, and one EM per blocking pass

> **In:** the whole pipeline (Steps 3–6) as the er.rs experiment on 15,000 synthetic records.
> **Out:** measured u, EM-fitted m and p, a 415× blocking cut, and precision/recall 0.989/0.992 at a 12-bit threshold.

The stub in `experiments/src/er.rs` generates 15,000 records over 5 fields with value
pools [200, 500, 3650, 200, 2000] and typo rates [0.10, 0.07, 0.03, 0.12, 0.05]. You
estimate u from random pairs (measured [0.0052 0.0021 0.0003 0.0051 0.0006], i.e. about
1/pool-size), then run one fixed-u EM session per blocking pass — last name and dob,
unioned — with the blocked field EXCLUDED from that session: every blocked candidate
agrees on its key by construction, so an EM that includes it degenerates to fitted
p → 1.0. Measured m = [0.80 0.86 0.94 0.78 0.90] against the analytic (1−typo)² =
[0.81 0.87 0.94 0.77 0.90], with p = 0.184. Blocking turns 112,492,500 naive pairs into
271,012 (415× fewer); linking at a 12-bit threshold plus union-find yields pair precision
0.989 / recall 0.992 in ~48 ms. The 12-bit choice is not arbitrary: two-field coincidences
score dob+city ≈ 10.3 bits, dob+first ≈ 10.6, last+phone ≈ 10.95 — all below 12, so no
two-field fluke can cross the bar. splink productionizes the identical split
(estimate_u_using_random_sampling, one EM training session per blocking rule, log2(m/u)
weights, threshold + connected components); see the separate splink code guide.

## How to read the paper (with the concepts in hand)

- **Section 1 (Introduction)** — the framing of Step 1: files without keys, typos, and why
  ad-hoc rules do not give error-rate guarantees. Skim for vocabulary (M, U, γ).
- **Section 2 (The Fellegi–Sunter model)** — Steps 1–3 in full: the likelihood ratio R, the
  two-threshold decision rule and its optimality proof sketch, and the conditional-
  independence decomposition into agreement/disagreement weights. Read closely; derive the
  bit weights yourself for a 2-field toy example before moving on.
- **String comparator material** — Step 4: Jaro's comparator, the Winkler prefix boost, and
  the "more than 25% of matches missed by exact comparison" motivation. Note how partial
  agreement is spliced into the weights rather than replacing the model.
- **Parameter estimation / EM sections** — Step 5: EM on agreement-pattern counts fitting
  (p, m, u), the Winkler 1988 latent-class lineage, and the discussion of interaction
  models when conditional independence fails. Map each E/M step to the loop diagram above.
- **Blocking and scale sections** — Steps 6–7: multi-pass blocking, the 10^17 → 10^12 pairs
  with 11 criteria at 99.5% recall example (Winkler 2004), and BigMatch's
  all-passes-in-one-scan design at ~100k pairs/sec.
- **1990 Census results** — the clerical-review reduction (3000 people × 3 months down to
  200 × 6 weeks) is the empirical payoff of Step 2's optimality theorem.
- **Current research directions (closing sections)** — survey-level pointers; skim, noting
  which limitations (dependence between fields, error-rate estimation) your experiment
  sidesteps by construction.

## Questions to answer in notes.md

1. State the Fellegi–Sunter optimality theorem precisely: what is held fixed, and what is
   minimized? Why does the clerical band exist at all instead of a single threshold?
2. Derive the agreement and disagreement weights in bits for the experiment's dob field
   (pool 3650, typo rate 0.03) and check them against your er.rs output.
3. Why must the blocked field be excluded from its own blocking pass's EM session, and
   what exactly degenerates (which parameter, to what value) if you include it?
4. Where does the 12-bit threshold sit relative to the worst two-field coincidence
   patterns in the experiment, and what does that imply about false-match sources at 13
   or at 10 bits?
5. BigMatch evaluates 10 blocking passes in one scan. Sketch the data structures that make
   that work and relate the design to hash partitioning from topic 36.

## Done when

Answer each before unfolding it.

- [ ] You can write the two-threshold decision rule from memory and explain, in one
      paragraph, what Fellegi–Sunter proved optimal about it.

  <details><summary>Answer</summary>

  Rule (Eq. 2): `R > T_μ` → match; `R < T_λ` → nonmatch; `T_λ ≤ R ≤ T_μ` →
  clerical review (the boundaries themselves fall in the review band). Fellegi and
  Sunter proved that among all decision rules holding the false-match rate ≤ μ and
  the false-nonmatch rate ≤ λ, this likelihood-ratio rule *minimizes the
  probability of the clerical (no-decision) region*.

  The band exists because a single threshold cannot hold both error rates under
  their targets at once — the middle is where the evidence is genuinely
  ambiguous, and routing only that band to humans is what keeps both automated
  error rates bounded. That theorem is what justified automation: the 1990 Census
  matching dropped from an estimated 3000 clerks over 3 months to 200 over 6
  weeks.

  </details>

- [ ] You have hand-computed per-field match weights in bits from (m, u) and matched them
      to the experiment's measured values.

  <details><summary>Answer</summary>

  Agreement weight is `+log2(m/u)`, disagreement is `+log2((1−m)/(1−u))`. From the
  fitted `m = [0.80 0.86 0.94 0.78 0.90]`, `u = [0.0052 0.0021 0.0003 0.0051
  0.0006]`, the agreement weights are `[+7.27 +8.68 +11.61 +7.26 +10.55]` and the
  disagreement weights `[−2.31 −2.83 −4.06 −2.18 −3.32]`.

  dob pays the most on agreement (+11.61) because its 3650-value pool makes u tiny,
  and it also punishes disagreement hardest (−4.06). A full five-field match sums
  to 45.36 bits — far above the 12-bit link threshold — while the pattern last,
  first, dob agree / city disagree / phone agree sums to 35.93 bits.

  </details>

- [ ] Your er.rs run reproduces blocking (~271k pairs from ~112.5M), EM-fitted m within a
      point or two of (1−typo)², and precision/recall ≈ 0.989/0.992 at 12 bits.

  <details><summary>Answer</summary>

  Blocking on last name and dob (unioned) turns 112,492,500 naive pairs into
  271,012 — a 415× cut. EM (one fixed-u session per pass, the blocked field
  excluded) fits `m = [0.80 0.86 0.94 0.78 0.90]` against the analytic `(1−typo)² =
  [0.81 0.87 0.94 0.77 0.90]` (within a point or two) and `p = 0.184`. Linking at a
  12-bit threshold plus union-find gives pair precision 0.989, recall 0.992 in
  ~48 ms.

  Including the blocked field in its own session degenerates the fit: every blocked
  pair agrees on the key by construction, so the field looks perfectly
  discriminating and the fitted prior p is driven to 1.0 — which is why each pass
  excludes its own blocking column.

  </details>

- [ ] You can explain why exact string comparison loses over 25% of census matches and how
      Jaro–Winkler similarity is discounted into the weights.

  <details><summary>Answer</summary>

  On census data more than 25% of true matches disagree on a field's exact string,
  purely from typos and scanning error — the hardest missed matches in Winkler's
  Table 9 were children whose two records shared no name 3-grams at all. Exact
  equality therefore throws those matches away before scoring even starts.

  Jaro's comparator scores partial similarity (common characters in a sliding
  window, minus transpositions); Winkler's variant boosts it when the strings
  share a common prefix, since typos cluster toward the ends of names. That
  similarity in [0, 1] is folded into the weight by interpolating between the full
  agreement weight `+log2(m/u)` and the disagreement weight, so a near-miss earns
  partial positive bits instead of the full negative penalty. The likelihood-ratio
  skeleton is unchanged; only the γ alphabet gets richer.

  </details>

## References

- W. E. Winkler, "Overview of Record Linkage and Current Research Directions," US Census
  Bureau research report, 2006. (The survey this guide walks through.)
- I. P. Fellegi and A. B. Sunter, "A Theory for Record Linkage," JASA 64(328), 1969. (The
  original optimality theorem and two-threshold rule.)
- W. E. Winkler, "Using the EM Algorithm for Weight Computation in the Fellegi–Sunter
  Model of Record Linkage," 1988. (EM applied to latent-class record linkage.)
- Local experiment: `topics/39-fraud-identity-graphs/experiments/src/er.rs` (stub you
  implement) and the measured numbers quoted in Step 8.
- Companion code guide: `topics/39-fraud-identity-graphs/reading-splink.md` (how splink
  maps onto this paper).
