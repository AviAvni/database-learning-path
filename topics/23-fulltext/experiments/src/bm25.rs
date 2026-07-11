//! BM25 (the Lucene/tantivy variant): idf uses the +1 inside ln so it
//! never goes negative; tf normalization saturates at (K1+1)·idf, which
//! is exactly the property block-max WAND exploits — every posting has
//! a finite, precomputable score ceiling.
//!
//! Anchors: tantivy query/bm25.rs:8-9 (K1=1.2, B=0.75), :52 (idf),
//! :59 (tf-norm with fieldnorm/average_fieldnorm).

use crate::index::Index;
use std::collections::HashMap;

pub const K1: f32 = 1.2;
pub const B: f32 = 0.75;

pub fn idf(doc_freq: usize, n_docs: usize) -> f32 {
    (1.0 + (n_docs as f32 - doc_freq as f32 + 0.5) / (doc_freq as f32 + 0.5)).ln()
}

pub fn tf_norm(tf: u32, doc_len: u32, avg_len: f32) -> f32 {
    let tf = tf as f32;
    tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * doc_len as f32 / avg_len))
}

pub fn score(tf: u32, doc_len: u32, avg_len: f32, idf: f32) -> f32 {
    idf * tf_norm(tf, doc_len, avg_len)
}

/// Exhaustive term-at-a-time oracle: walk EVERY posting of every query
/// term, accumulate scores in a hash map, sort, truncate. Returns
/// (top-k as (score, doc) desc, postings touched). This is what WAND
/// must beat while returning the same answer.
pub fn oracle_topk(index: &Index, query: &[u32], k: usize) -> (Vec<(f32, u32)>, usize) {
    let mut acc: HashMap<u32, f32> = HashMap::new();
    let mut docs_scored = 0usize;
    for &t in query {
        let list = &index.lists[t as usize];
        let idf = idf(list.postings.len(), index.n_docs);
        for p in &list.postings {
            *acc.entry(p.doc).or_insert(0.0) +=
                score(p.tf, index.doc_len[p.doc as usize], index.avg_len, idf);
        }
        docs_scored += list.postings.len();
    }
    let mut v: Vec<(f32, u32)> = acc.into_iter().map(|(d, s)| (s, d)).collect();
    v.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
    v.truncate(k);
    (v, docs_scored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idf_monotone_in_rarity() {
        assert!(idf(10, 100_000) > idf(10_000, 100_000));
        assert!(idf(99_999, 100_000) > 0.0, "the +1 keeps idf positive");
    }

    #[test]
    fn tf_saturates() {
        let one = tf_norm(1, 100, 100.0);
        let ten = tf_norm(10, 100, 100.0);
        let hundred = tf_norm(100, 100, 100.0);
        assert!(ten > one && hundred > ten);
        assert!(hundred < K1 + 1.0, "ceiling is K1+1");
        assert!(hundred - ten < ten - one, "diminishing returns");
    }

    #[test]
    fn long_docs_penalized() {
        assert!(tf_norm(3, 50, 100.0) > tf_norm(3, 200, 100.0));
    }
}
