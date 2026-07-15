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

## The problem in one sentence

Given a query and 100K candidate documents, produce one number per
document such that sorting by it puts the relevant ones on top —
and do it in one multiply-add per posting, because ranking runs
inside the tightest loop the search engine has.

## The concepts, step by step

### Step 1 — ranking as probability: the one principled starting point

The **probabilistic ranking principle** says: sort documents by
P(relevant | document) — the probability a user with this query
would judge the document relevant — and no other ordering does
better on average. Since only the *order* matters, any monotonic
transform is equally good, and the odds ratio
P(relevant|doc)/P(irrelevant|doc) turns products of per-term
probabilities into sums of logs. Everything in BM25 is this ratio
under successively weaker simplifying assumptions. The whole ladder,
which Steps 2–4 climb rung by rung:

```
  binary independence model (§3)
      terms present/absent, independent ⇒ score = Σ log odds per term
      └─ with no relevance info ⇒ the idf shape:  log (N - df + 0.5)/(df + 0.5)
  + term frequency via 2-Poisson "eliteness" (§3.3)
      docs are elite/non-elite for a term; tf is a noisy signal of eliteness
      ⇒ tf weight must SATURATE:  tf·(k1+1)/(tf + k1)   ← not log(tf), not raw tf
  + document length (§3.4)
      long docs: more of everything ⇒ normalize tf by len/avg_len,
      but only partially (verbosity vs scope hypothesis) ⇒ the B knob
  = BM25 (§3.5):
      Σ idf(t) · tf·(k1+1) / (tf + k1·(1 - b + b·len/avg_len))
```

### Step 2 — the binary independence model, and where idf comes from

Assume each term is merely present or absent in a doc (binary), and
terms are independent of each other. Then the log-odds ratio
decomposes into a per-term weight summed over query terms present
in the doc. With *no* relevance judgments available (the usual
case), the weight collapses to a function of one statistic —
**df** (document frequency: how many of the N docs contain the
term):

```
idf(t) = log (N − df + 0.5) / (df + 0.5)
```

This is **idf** (inverse document frequency): rare terms get big
weights, terms in half the corpus get ~0. Concretely, in our 100K
corpus: df=159 → idf ≈ 6.4; df=100K → idf ≈ 0. The +0.5s are
smoothing (a Jeffreys prior) so df=0 and df=N don't produce
infinities (question 2). Cost of the model's honesty: binary
presence ignores that a doc mentioning `fox` 12 times is more
about foxes than one mentioning it once — Step 3's job.

### Step 3 — term frequency must saturate: the 2-Poisson argument

**tf** (term frequency: occurrences of the term in this doc) should
raise the score — but not linearly. The 2-Poisson **eliteness**
model says a doc either *is about* the term ("elite") or isn't, and
tf is only a noisy signal of that hidden bit: going 0→3 occurrences
is strong evidence of eliteness, 50→53 is nothing. Working the
model through yields a weight that **saturates**:

```
tf·(k1+1) / (tf + k1)      → k1+1 as tf → ∞
```

`K1` (≈1.2 by default) sets how fast the ceiling is approached: at
K1=1.2, tf=1 already gives 1.0 of the max 2.2; tf=11 gives ~90%
(question 1). What breaks without it: a doc repeating `quick` 500×
beats a doc with `quick fox` — the spam magnet. Neither raw tf nor
log(tf) has the bounded ceiling; the *bound* is what WAND will
exploit (Step 6).

### Step 4 — document length: normalize, but only partly

Long documents have more of every term, so tf must be discounted by
doc length — but *how much* depends on *why* the doc is long: pure
verbosity (same content, more words → fully normalize) or wider
scope (genuinely more topics → don't). Truth is in between, so BM25
interpolates with knob `b ∈ [0,1]`, replacing K1 in the denominator
with:

```
k1 · (1 − b + b · len/avg_len)         b = 0.75 by default
```

What breaks at the extremes: b=0 (no normalization) → encyclopedic
docs win everything; b=1 (full) → long docs can never win, even
legitimately comprehensive ones. Assembling Steps 2–4 gives BM25
(§3.5 in the ladder above) — and note each piece failed *toward* a
concrete pathology:

- no saturation → keyword-stuffing spam wins;
- no length norm → longest doc wins;
- full length norm → longest doc always loses.

### Step 5 — in code: precompute everything, one multiply-add per posting

At query time, idf is per-term (known from the dictionary before
any posting is read) and the length-norm denominator is per-doc —
both precomputable, leaving one multiply-add per posting. tantivy's
whole scorer (`query/bm25.rs`):

```rust
const K1: f32 = 1.2;
const B: f32 = 0.75;

fn idf(n_docs: f32, df: f32) -> f32 {
    ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln()  // +1: Lucene's tweak,
}                                                   // never negative at df > N/2

fn bm25(idf: f32, tf: f32, len: f32, avg_len: f32) -> f32 {
    let norm = K1 * (1.0 - B + B * len / avg_len);  // Lucene: a 256-entry
    idf * (tf * (K1 + 1.0)) / (tf + norm)           //   table, len as u8
}
// tf → ∞ ⇒ score → idf·(K1+1): the saturation ceiling that makes
// WAND's per-term upper bounds possible (next chapter)
```

| formula piece | anchor |
|---|---|
| K1=1.2, B=0.75 (the paper's "reasonable defaults", §4.2) | bm25.rs:8-9 |
| idf with +1 under the ln (Lucene tweak: never negative when df > N/2) | bm25.rs:52 |
| `K1 * (1 - B + B * fieldnorm / average_fieldnorm)` precomputed per fieldnorm byte | bm25.rs:59 |
| fieldnorm quantized to 1 byte, 256-entry cache table | fieldnorm/ + the `cache` in bm25.rs |

Lucene's extra trick: doc length (**fieldnorm**) is quantized to a
u8 (lossy!), so the entire length-normalization term becomes a
256-entry lookup table. Our `bm25.rs` keeps exact lengths; the
experiments' block maxima would be *slightly* different under
quantization (question 4).

### Step 6 — why WAND loves BM25: the score has a ceiling

Because tf saturates at (K1+1) and fieldnorm has a minimum, every
term's contribution is bounded for ALL docs:

```
score(t, d) ≤ idf(t) · (K1 + 1)
```

— a static per-term ceiling, computable at index time, refinable
per 128-doc block. The next chapter's entire algorithm (skip every
doc whose summed ceilings can't beat the current top-k) rests on
this monotone bound existing. Learned/neural scorers without such
bounds lose it — which is why neural rerankers run AFTER a
BM25/WAND first stage, never instead of it.

## How to read the paper (with the concepts in hand)

The 2009 monograph is ~90 pages; you need two sections:

- **§2** Background/notation — skim to anchor the probabilistic
  ranking principle (Step 1).
- **§3 — read carefully.** The derivation ladder: §3.2
  Robertson-Spärck Jones weights and the no-relevance-info idf
  (Step 2), §3.3 eliteness and saturation (Step 3), §3.4 length
  normalization (Step 4), §3.5 the assembled BM25. At each rung ask
  the guide's question: what breaks if this rung is dropped?
- **§4.2** — where K1=1.2, b=0.75 come from (grid search over TREC
  collections; "reasonable defaults", not laws).
- The rest (BM25F for fields, relevance feedback) — skim; return
  for M23's per-field weighting if needed.

## Questions (answer in notes.md)

1. Derive the tf-saturation limit: as tf→∞ the weight → K1+1. At
   K1=1.2, what tf reaches 90% of the ceiling (len=avg)? What does
   that say about keyword stuffing?
2. The +0.5s in idf are a smoothing (Jeffreys prior). What happens
   at df=0 and df=N without them?
3. b=0.75: our corpus has uniform lengths 50-150. Predict how much
   scores change b=0.75 → b=0 here vs on a corpus of tweets+books.
4. Lucene's 1-byte fieldnorm: worst-case relative score error vs
   exact lengths? Why is this fine for ranking but would corrupt our
   oracle-equality test?
5. RSJ weights need relevance judgments (§3.2); idf is the
   no-information special case. Where would M23 get click/edge
   feedback to use the full RSJ weight, and is it worth it?

## References

**Papers**
- Robertson, Zaragoza — "The Probabilistic Relevance Framework:
  BM25 and Beyond" (Foundations and Trends in IR 2009) — §3 is the
  derivation ladder; §4.2 the default constants

**Code**
- [tantivy](https://github.com/quickwit-oss/tantivy)
  `src/query/bm25.rs` — K1/B at :8-9, idf at :52, the precomputed
  fieldnorm table at :59
