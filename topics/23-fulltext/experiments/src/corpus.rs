//! Deterministic synthetic corpus: term ids drawn from a Zipfian
//! vocabulary (rank-1 term is the "the" of the corpus), doc lengths
//! uniform in 50..=150 tokens. Term ids ARE ranks, so `t0` is the
//! most common term and `t49999` the rarest — handy for picking
//! query terms with known document frequency.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub struct Corpus {
    /// each doc is a bag of term ids (order kept, but nothing uses it)
    pub docs: Vec<Vec<u32>>,
    pub vocab: usize,
}

pub fn gen_corpus(n_docs: usize, vocab: usize, theta: f64, seed: u64) -> Corpus {
    let mut cdf = Vec::with_capacity(vocab);
    let mut acc = 0.0f64;
    for i in 0..vocab {
        acc += 1.0 / ((i + 1) as f64).powf(theta);
        cdf.push(acc);
    }
    let total = acc;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let docs = (0..n_docs)
        .map(|_| {
            let len = rng.gen_range(50..=150usize);
            (0..len)
                .map(|_| {
                    let u: f64 = rng.gen::<f64>() * total;
                    cdf.partition_point(|&c| c < u) as u32
                })
                .collect()
        })
        .collect();
    Corpus { docs, vocab }
}

/// The text-analysis pipeline, minimal edition: lowercase + split on
/// non-alphanumeric. Real engines chain filters (tantivy's
/// `TextAnalyzer`: tokenizer → lowercase → stemmer → stopwords).
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_basics() {
        assert_eq!(
            tokenize("The QUICK, brown-fox... jumps!"),
            vec!["the", "quick", "brown", "fox", "jumps"]
        );
    }

    #[test]
    fn corpus_deterministic_and_skewed() {
        let c1 = gen_corpus(1000, 10_000, 1.0, 7);
        let c2 = gen_corpus(1000, 10_000, 1.0, 7);
        assert_eq!(c1.docs, c2.docs);
        let mut counts = vec![0usize; 10_000];
        for d in &c1.docs {
            for &t in d {
                counts[t as usize] += 1;
            }
        }
        assert!(counts[0] > counts[99] * 10, "zipf head should dominate");
    }
}
