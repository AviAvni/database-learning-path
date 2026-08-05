# BM25: a derivation, not folklore

BM25 looks like folklore (two magic constants, a weird fraction) but
it's a derivation: rank documents by P(relevant|doc)/P(irrelevant|doc)
under increasingly honest assumptions. Robertson & Zaragoza's 2009
monograph is the two inventors showing their work, 30 years after
Robertson-Spärck Jones — and every piece of the formula answers
"what breaks if I drop it". This chapter climbs the derivation one
rung at a time — the probabilistic starting point, the idf shape,
saturation, length normalization — then maps every piece to a line
of tantivy and explains why the next chapter (WAND) depends on one
property of this formula.

Source pins: the monograph is Robertson & Zaragoza, *The
Probabilistic Relevance Framework: BM25 and Beyond*, Foundations and
Trends in IR 3(4), 2009 (equation and section numbers below are from
it); code anchors are tantivy at `7152d53`.

## The problem in one sentence

Given a query and 100K candidate documents, produce one number per
document such that sorting by it puts the relevant ones on top —
and do it in one multiply-add per posting, because ranking runs
inside the tightest loop the search engine has.

## The concepts, step by step

### Step 1 — ranking as probability: the one principled starting point

> **In:** a query, a document, and the wish to order documents by usefulness.
> **Out:** the odds ratio P(rel|doc)/P(irrel|doc) as the thing to sort by, and the fact that any monotonic transform (logs, sums) ranks identically.

The **probability ranking principle** (monograph §2.3) says: sort
documents by P(relevant | document) — the probability a user with
this query would judge the document relevant — and no other ordering
does better on average. Since only the *order* matters, any monotonic
transform is equally good, and the odds ratio
P(relevant|doc)/P(irrelevant|doc) turns products of per-term
probabilities into sums of logs. Everything in BM25 is this ratio
under successively weaker simplifying assumptions. The whole ladder,
which Steps 2–4 climb rung by rung:

```
  binary independence model (§3.1)
      terms present/absent, independent  =>  score = Σ log-odds per term
      └─ no relevance info => the idf shape (Eq 3.3):  log (N - df + 0.5)/(df + 0.5)
  + term frequency via 2-Poisson "eliteness" (§3.4.1)
      docs are elite/non-elite for a term; tf is a noisy signal of eliteness
      => tf weight must SATURATE:  tf / (tf + k1)   ← not log(tf), not raw tf
  + document length (§3.4.5)
      long docs: more of everything => normalize tf by len/avg_len,
      but only partially (verbosity vs scope hypothesis) => the B knob (Eq 3.12)
  = BM25 (Eq 3.15):
      Σ idf(t) · tf / (tf + k1·(1 - b + b·len/avg_len))
      (tantivy/Lucene multiply the numerator by (k1+1); §3.5.1 — see Step 3)
```

### Step 2 — the binary independence model, and where idf comes from

> **In:** the log-odds sum from Step 1 and the usual case of *no* relevance judgments.
> **Out:** the per-term weight collapses to a function of one statistic, df, giving the idf shape — big for rare terms, ~0 for ubiquitous ones.

Assume each term is merely present or absent in a doc (binary), and
terms are independent of each other — the **binary independence
model** (monograph §3.1). Then the log-odds ratio decomposes into a
per-term weight summed over query terms present in the doc. With *no*
relevance judgments available (the usual case), setting R = r = 0 in
the Robertson-Spärck Jones weight collapses it to a function of one
statistic — **df** (document frequency: how many of the N docs
contain the term) — which the monograph calls a close approximation
to classical idf (Eq 3.3):

```
idf(t) = log (N − df + 0.5) / (df + 0.5)          (monograph Eq 3.3)
```

This is **idf** (inverse document frequency): rare terms get big
weights, terms in half the corpus get ~0. Worked on our 100K corpus:

```
df = 159:     (100000 − 159 + 0.5)/(159 + 0.5) = 99841.5/159.5 = 625.97
              log(625.97) = 6.44                  → big weight
df = 100000:  (100000 − 100000 + 0.5)/(100000 + 0.5) = 0.5/100000.5
              log(5.0e-6) = −12.2                 → NEGATIVE (see below)
```

The +0.5s are smoothing (a Jeffreys prior) so df=0 and df=N don't
produce infinities (question 2). Note the plain Eq 3.3 goes *negative*
for a term in nearly every document — which is why Lucene/tantivy add
a +1 inside the log (Step 5): `ln(1 + 625.97) = 6.44` for df=159 (same
to two places), but `ln(1 + 5.0e-6) ≈ 0.000005` for df=100000, never
below zero. Cost of the model's honesty: binary presence ignores that
a doc mentioning `fox` 12 times is more about foxes than one
mentioning it once — Step 3's job.

### Step 3 — term frequency must saturate: the 2-Poisson argument

> **In:** the observation that tf should raise the score, but a doc repeating one word 500× is not 500× more relevant.
> **Out:** a saturating tf weight tf/(tf+k1) that approaches a ceiling, with k1 tuning how fast — the ceiling is what WAND later exploits.

**tf** (term frequency: occurrences of the term in this doc) should
raise the score — but not linearly. The 2-Poisson **eliteness**
model (monograph §3.4.1) says a doc either *is about* the term
("elite") or isn't, and tf is only a noisy signal of that hidden bit:
going 0→3 occurrences is strong evidence of eliteness, 50→53 is
nothing. Working the model through (Eq 3.11) yields a weight that
**saturates**:

```
raw:  tf / (tf + k1)             → 1 as tf → ∞    (Eq 3.11, times wRSJ)
tantivy/Lucene:  tf·(k1+1) / (tf + k1)  → k1+1     (the §3.5.1 variant)
```

The `(k1+1)` numerator is the monograph's §3.5.1 variant: "the same
for all terms, and therefore does not affect the ranking" — it just
makes a single-occurrence term score the same as under the bare RSJ
weight. tantivy uses it (Step 5). **K1** (=1.2 in tantivy) sets how
fast the ceiling is approached. Worked at K1=1.2, len=avg (so the
denominator's length term is just k1):

```
tf-weight = tf·(k1+1)/(tf+k1),  ceiling = k1+1 = 2.2
  tf = 1:   1·2.2/(1+1.2)  = 2.2/2.2  = 1.00   → 1.00 of 2.2 = 45%
  tf = 11:  11·2.2/(11+1.2)= 24.2/12.2= 1.98   → 1.98 of 2.2 = 90%
```

So the first occurrence already buys 45% of the ceiling and the
eleventh only reaches 90% (question 1). What breaks without
saturation: a doc repeating `quick` 500× beats a doc with `quick fox`
— the spam magnet. Neither raw tf nor log(tf) has the bounded
ceiling; the *bound* is what WAND will exploit (Step 6).

### Step 4 — document length: normalize, but only partly

> **In:** long documents carry more of every term, inflating tf regardless of relevance.
> **Out:** a soft length-normalization knob b ∈ [0,1] (Eq 3.12) that divides tf by (1−b+b·len/avg) before saturation — b=0 off, b=1 full.

Long documents have more of every term, so tf must be discounted by
doc length — but *how much* depends on *why* the doc is long: pure
verbosity (same content, more words → fully normalize) or wider
scope (genuinely more topics → don't). Truth is in between, so BM25
interpolates with knob `b ∈ [0,1]` (monograph §3.4.5, Eq 3.12),
scaling k1 in the denominator by the soft-normalization factor:

```
B = (1 − b + b · len/avg_len)          (Eq 3.12; b = 0.75 in tantivy)
denominator uses k1·B in place of k1
```

What breaks at the extremes: b=0 (no normalization) → encyclopedic
docs win everything; b=1 (full) → long docs can never win, even
legitimately comprehensive ones. Assembling Steps 2–4 gives BM25
(monograph Eq 3.15) — and note each piece failed *toward* a
concrete pathology:

- no saturation → keyword-stuffing spam wins;
- no length norm → longest doc wins;
- full length norm → longest doc always loses.

### Step 5 — in code: precompute everything, one multiply-add per posting

> **In:** the assembled BM25 formula and a query-time budget of one arithmetic op per posting.
> **Out:** idf is per-term (from the dictionary), the length term is a 256-entry table keyed by a 1-byte fieldnorm, and (k1+1) is folded into a per-term `weight` — leaving `weight · tf/(tf+norm)` per posting.

At query time, idf is per-term (known from the dictionary before
any posting is read) and the length-norm denominator is per-doc —
both precomputable, leaving one multiply-add per posting. tantivy's
scorer, quoted (note it splits the textbook single fraction: `(1+K1)`
is folded into `weight` at construction, and the hot path multiplies
that by `tf/(tf+norm)`):

```rust
// tantivy src/query/bm25.rs:8-9, 52-60 and 158-192 (elided)
     8  const K1: Score = 1.2;
     9  const B: Score = 0.75;
    52  pub(crate) fn idf(doc_freq: u64, doc_count: u64) -> Score {
    53      assert!(doc_count >= doc_freq, "{doc_count} >= {doc_freq}");
    54      let x = ((doc_count - doc_freq) as Score + 0.5) / (doc_freq as Score + 0.5);
    55      (1.0 + x).ln()                              // +1: Lucene tweak, never < 0
    56  }
    58  fn cached_tf_component(fieldnorm: u32, average_fieldnorm: Score) -> Score {
    59      K1 * (1.0 - B + B * fieldnorm as Score / average_fieldnorm)  // per-fieldnorm norm
    60  }
   159      let weight = idf_explain.value() * (1.0 + K1);  // fold (k1+1) into weight
   179  pub fn score(&self, fieldnorm_id: u8, term_freq: u32) -> Score {
   180      self.weight * self.tf_factor(fieldnorm_id, term_freq)   // one mul per posting
   181  }
   189  pub(crate) fn tf_factor(&self, fieldnorm_id: u8, term_freq: u32) -> Score {
   190      let term_freq = term_freq as Score;
   191      let norm = self.cache[fieldnorm_id as usize];          // 256-entry table lookup
   192      term_freq / (term_freq + norm)
   193  }
```

| formula piece | anchor |
|---|---|
| K1=1.2, B=0.75 — tantivy/Lucene convention, *not* stated by the monograph (which gives no defaults, only the §3.5 range 1.2<k1<2, 0.5<b<0.8) | bm25.rs:8-9 |
| idf with +1 under the ln (Lucene tweak: never negative when df > N/2); the bare monograph form is Eq 3.3 | bm25.rs:52-56 |
| `K1·(1 − B + B·fieldnorm/average_fieldnorm)` precomputed per fieldnorm byte | bm25.rs:58-60 |
| fieldnorm quantized to 1 byte → a 256-entry cache table (`compute_tf_cache`) | bm25.rs:62-68 |
| `(k1+1)` folded into `weight`; hot path is `weight · tf/(tf+norm)` | bm25.rs:159, :180, :189-192 |

Lucene's extra trick: doc length (**fieldnorm**) is quantized to a
u8 (lossy!), so the entire length-normalization term becomes a
256-entry lookup table (`compute_tf_cache`, bm25.rs:62-68). Our
`bm25.rs` keeps exact lengths; the experiments' block maxima would be
*slightly* different under quantization (question 4).

### Step 6 — why WAND loves BM25: the score has a ceiling

> **In:** the saturating tf weight (bounded by k1+1) and a per-term idf.
> **Out:** a static, index-time per-term ceiling idf·(k1+1) — the monotone upper bound the next chapter's skipping depends on.

Because tf saturates at (K1+1) and fieldnorm has a minimum, every
term's contribution is bounded for ALL docs:

```
score(t, d) ≤ idf(t) · (K1 + 1)
```

tantivy computes exactly this ceiling in `max_score` (bm25.rs:184-186:
`self.score(255u8, 2_013_265_944)` — max fieldnorm id, saturating tf)
— a static per-term ceiling, computable at index time, refinable per
128-doc block. The next chapter's entire algorithm (skip every doc
whose summed ceilings can't beat the current top-k) rests on this
monotone bound existing. Learned/neural scorers without such bounds
lose it — which is why neural rerankers run AFTER a BM25/WAND first
stage, never instead of it.

## How to read the paper (with the concepts in hand)

The 2009 monograph is ~60 pages; you need three sections, and it is
worth knowing where each rung actually lives (the subsection numbers
below are verified against the monograph's table of contents):

- **§2.3** The Probability Ranking Principle — the probabilistic
  starting point (Step 1).
- **§3.1 The Binary Independence Model** — read carefully: the RSJ
  weight and, with no relevance info, the idf of Eq 3.3 (Step 2).
  (§3.2 is Relevance Feedback and §3.3 Blind Feedback — skip on a
  first pass; they are *not* the tf/length rungs.)
- **§3.4 The Eliteness Model and BM25** — the heart. §3.4.1 the
  2-Poisson/eliteness argument and saturation (Step 3), §3.4.5
  Document Length and the B factor (Step 4), Eq 3.15 the assembled
  classic BM25. §3.5.1 lists the `(k1+1)`-numerator variant tantivy
  uses.
- **§3.5 / §5** — parameters: the monograph gives *no* prescribed
  defaults ("the model provides no guidance"), only the empirical
  range "0.5 < b < 0.8 and 1.2 < k1 < 2 are reasonably good" (§3.5);
  §5 is parameter optimisation. The specific 1.2/0.75 are Lucene's
  choice, not the paper's. (§4.2 is "The Unified Model" — a
  comparison, *not* where the constants come from.)
- The rest (BM25F for fields §3.6, relevance feedback §3.2) — skim;
  return for M23's per-field weighting if needed.

## Questions (answer in notes.md)

1. Derive the tf-saturation limit: as tf→∞ the weight → K1+1. At
   K1=1.2, len=avg, what tf reaches 90% of the ceiling (the text
   worked it to ≈11)? What does that say about keyword stuffing?
2. The +0.5s in idf are a smoothing (Jeffreys prior). What happens
   at df=0 and df=N without them, and separately, why does Lucene's
   +1-under-the-ln matter only near df=N?
3. b=0.75: our corpus has uniform lengths 50-150. Predict how much
   scores change b=0.75 → b=0 here vs on a corpus of tweets+books.
4. Lucene's 1-byte fieldnorm: worst-case relative score error vs
   exact lengths? Why is this fine for ranking but would corrupt our
   oracle-equality test?
5. RSJ weights need relevance judgments (§3.1/§3.2); idf is the
   no-information special case. Where would M23 get click/edge
   feedback to use the full RSJ weight, and is it worth it?

## Done when

Answer each before unfolding it.

- [ ] You can explain where idf comes from, rather than asserting it.
  <details><summary>the derivation, not the formula</summary>
  From the binary independence model (§3.1): sum per-term log-odds of
  relevance; with no relevance judgments, set R=r=0, and the RSJ
  weight reduces to the idf of Eq 3.3, `log((N−df+0.5)/(df+0.5))`. It
  is the no-relevance-information special case of a probabilistic term
  weight, not an axiom.
  </details>
- [ ] You can derive the tf saturation limit and say what it approaches as tf grows.
  <details><summary>the ceiling</summary>
  `tf·(k1+1)/(tf+k1) → k1+1` as tf→∞ (the tantivy/Lucene §3.5.1 form;
  the bare Eq 3.11 form `tf/(tf+k1) → 1`). At k1=1.2 the ceiling is
  2.2; tf=1 gives 1.0 (45%), tf=11 ≈ 1.98 (90%).
  </details>
- [ ] You can explain what b controls and predict its effect on a corpus with near-uniform lengths.
  <details><summary>soft length normalization</summary>
  b scales how much tf is divided by len/avg_len (Eq 3.12: factor
  `1−b+b·len/avg_len`). On near-uniform lengths (our 50–150 corpus)
  len/avg≈1 so the factor ≈1 for any b — b barely moves scores. On
  tweets+books the same b swings scores hard.
  </details>
- [ ] You can say why WAND needs BM25's score ceiling and what a scorer without one costs.
  <details><summary>the monotone bound</summary>
  WAND skips any doc whose summed per-term ceilings can't beat θ.
  BM25's ceiling is `idf·(k1+1)` (tantivy `max_score`, bm25.rs:184).
  A scorer without a static upper bound (a neural reranker) can't be
  skipped safely, so it must run as a second stage over WAND's
  candidates.
  </details>
- [ ] You can state what the +0.5 smoothing terms are doing.
  <details><summary>Jeffreys prior</summary>
  They keep `(N−df+0.5)/(df+0.5)` finite and defined at df=0 and df=N
  — a Jeffreys (½-count) prior on the presence/absence probabilities,
  so no term produces ±∞.
  </details>
- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>check</summary>
  Five answers in notes.md, each tied to an equation/section of the
  monograph or a line of bm25.rs — not folklore.
  </details>

## References

**Papers**
- Robertson, Zaragoza — "The Probabilistic Relevance Framework:
  BM25 and Beyond" (Foundations and Trends in IR 3(4), 2009) — §2.3
  the ranking principle, §3.1 the BIM and idf (Eq 3.3), §3.4 the
  eliteness model, saturation and length (Eq 3.15), §3.5.1 the
  `(k1+1)` variant, §3.5 the empirical k1/b range

**Code**
- [tantivy](https://github.com/quickwit-oss/tantivy) `@7152d53`
  `src/query/bm25.rs` — K1/B at :8-9, idf at :52-56, the precomputed
  fieldnorm table at :58-68, `weight`/`score`/`tf_factor` at
  :159/:180/:189-192, the WAND ceiling `max_score` at :184-186
