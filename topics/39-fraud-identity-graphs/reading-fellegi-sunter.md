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

Pick an upper cutoff T_μ and a lower cutoff T_λ. If R is at or above T_μ, designate the
pair a match; if R is at or below T_λ, designate it a nonmatch; in between, send it to
clerical review.

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

Under conditional independence of fields given the class (exactly the naive Bayes
assumption), the log-likelihood ratio splits into a sum of per-field contributions. With
`m_i = P(agree on field i | M)` and `u_i = P(agree on field i | U)`:

```
  field agrees:     w_i = +log2( m_i / u_i )              (positive bits)
  field disagrees:  w_i = +log2( (1 - m_i) / (1 - u_i) )  (negative bits)

  γ = [ last=agree, first=agree, dob=agree, city=disagree, phone=agree ]
        +7.3          +8.7         +11.6       -3.4           +10.6
        └──────────────────── sum ────────────────────┘  log2 R = 34.8 bits
```

Intuition: u_i for a random pair is roughly 1/pool-size of the field, so rare values are
worth more bits; m_i is dominated by the field's typo rate. Score a pair by summing bits
and compare against a threshold in bits. This is exactly what splink calls match weights.

### Step 4 — String comparators: exact equality throws away a quarter of your matches

Exact character-by-character comparison misses more than 25% of true matches in census
data, purely from typos. Jaro's comparator counts common characters within a sliding
window plus transpositions; Winkler's variant boosts agreement when the strings share a
common prefix (typos cluster at the ends of names). The survey's key move is folding
*partial* agreement back into the Fellegi–Sunter framework: instead of a binary
agree/disagree, discount the agreement weight as string similarity falls, interpolating
between the full `+log2(m/u)` and the disagreement weight. The likelihood-ratio skeleton
is unchanged; only the γ alphabet gets richer.

### Step 5 — EM: fitting m, u, and p with zero labeled data

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

Winkler's 2004 example: two files of roughly 10^8.5 records each imply ~10^17 raw pairs;
11 blocking criteria cut that to ~10^12 pairs while retaining 99.5% of true matches.
Database hook: a blocking key is a hash-partition key (topic 36), and multi-pass blocking
is just multiple shuffles over the same data.

### Step 7 — Production scale: BigMatch and the census pipeline

BigMatch is the Census Bureau's production blocking-and-matching engine: it handles
workloads on the order of 100M × 4B record comparisons at roughly 100k pairs/sec, and —
the performance-engineering punchline — evaluates all 10 blocking passes *simultaneously
in one pass over the data*, rather than re-reading the files per pass. Think of it as a
multi-key hash join with the smaller file's indexes resident in memory. The whole survey
is really a systems paper wearing statistics clothing: optimal decision rule, measured
error rates, comparator microbenchmarks, and an engine that streams the big file once.

### Step 8 — The local experiment: er.rs, and one EM per blocking pass

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

- [ ] You can write the two-threshold decision rule from memory and explain, in one
      paragraph, what Fellegi–Sunter proved optimal about it.
- [ ] You have hand-computed per-field match weights in bits from (m, u) and matched them
      to the experiment's measured values.
- [ ] Your er.rs run reproduces blocking (~271k pairs from ~112.5M), EM-fitted m within a
      point or two of (1−typo)², and precision/recall ≈ 0.989/0.992 at 12 bits.
- [ ] You can explain why exact string comparison loses over 25% of census matches and how
      Jaro–Winkler similarity is discounted into the weights.

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
