# BM25: a derivation, not folklore

BM25 looks like folklore (two magic constants, a weird fraction) but
it's a derivation: rank documents by P(relevant|doc)/P(irrelevant|doc)
under increasingly honest assumptions. Robertson & Zaragoza's 2009
monograph is the two inventors showing their work, 30 years after
Robertson-Spärck Jones — and every piece of the formula answers
"what breaks if I drop it".

## The derivation ladder

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

Every piece answers "what breaks if I drop it":
- no saturation → a doc repeating `quick` 500× beats a doc with
  `quick fox` (spam magnet);
- no length norm → encyclopedic docs win everything;
- full length norm (b=1) → long docs can never win, even legitimately
  comprehensive ones.

## Mapped to code (tantivy `query/bm25.rs`)

The whole scorer is one multiply-add per posting once idf and the
length-norm are precomputed:

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

Lucene quantizes doc length to a u8 (lossy!) so the whole
length-normalization term is a 256-entry lookup — scoring is one
multiply-add per posting. Our `bm25.rs` keeps exact lengths; the
experiments' block maxima would be *slightly* different under
quantization (question 4).

## Why WAND loves BM25

tf saturates at (K1+1) and fieldnorm ≥ some minimum, so
score(t, d) ≤ idf(t)·(K1+1) for ALL docs — a static per-term ceiling,
refinable per block. Learned scorers without monotone bounds lose
this (that's why neural rerankers run AFTER a BM25/WAND first stage).

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
