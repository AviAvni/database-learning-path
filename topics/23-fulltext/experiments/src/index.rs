//! Inverted index: term id → posting list, postings sorted by doc id,
//! grouped in 128-posting blocks with per-block max BM25 score — the
//! block-max metadata of SIGIR'11 / tantivy's postings/skip.rs:175
//! (`block_max_score`) + :186 (`last_doc_in_block`).
//!
//! Doc ids are dense (row = doc), so no term dictionary is needed here;
//! tantivy puts an FST in front (termdict/fst_termdict/termdict.rs:46
//! maps term bytes → TermInfo { doc_freq, postings_range }, see
//! postings/term_info.rs:9-13). RediSearch instead chains blocks with
//! varint-encoded doc-id deltas (redisearch_rs/inverted_index
//! index/core.rs:30/:75).

use crate::bm25;
use crate::corpus::Corpus;
use std::collections::HashMap;

pub const BLOCK: usize = 128; // tantivy: COMPRESSION_BLOCK_SIZE (compression/mod.rs:3)

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Posting {
    pub doc: u32,
    pub tf: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct BlockMeta {
    /// last doc id covered by this block (skip pointer)
    pub last_doc: u32,
    /// max BM25 score of any posting in the block
    pub max_score: f32,
}

pub struct PostingList {
    pub postings: Vec<Posting>,
    pub blocks: Vec<BlockMeta>,
    /// global ceiling = max over blocks (WAND's term upper bound)
    pub max_score: f32,
}

pub struct Index {
    /// indexed by term id (term ids are dense — no dictionary needed)
    pub lists: Vec<PostingList>,
    pub doc_len: Vec<u32>,
    pub avg_len: f32,
    pub n_docs: usize,
}

pub fn build_index(corpus: &Corpus) -> Index {
    let mut raw: Vec<Vec<Posting>> = vec![Vec::new(); corpus.vocab];
    let mut doc_len = Vec::with_capacity(corpus.docs.len());
    for (doc_id, doc) in corpus.docs.iter().enumerate() {
        doc_len.push(doc.len() as u32);
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for &t in doc {
            *counts.entry(t).or_insert(0) += 1;
        }
        let mut terms: Vec<(u32, u32)> = counts.into_iter().collect();
        terms.sort_unstable();
        for (t, tf) in terms {
            raw[t as usize].push(Posting { doc: doc_id as u32, tf });
        }
    }
    let n_docs = corpus.docs.len();
    let avg_len = doc_len.iter().map(|&l| l as f64).sum::<f64>() as f32 / n_docs as f32;

    let lists = raw
        .into_iter()
        .map(|postings| {
            let idf = bm25::idf(postings.len(), n_docs);
            let mut blocks = Vec::with_capacity(postings.len().div_ceil(BLOCK));
            let mut max_score = 0.0f32;
            for chunk in postings.chunks(BLOCK) {
                let bm = chunk
                    .iter()
                    .map(|p| bm25::score(p.tf, doc_len[p.doc as usize], avg_len, idf))
                    .fold(0.0f32, f32::max);
                blocks.push(BlockMeta {
                    last_doc: chunk.last().unwrap().doc,
                    max_score: bm,
                });
                max_score = max_score.max(bm);
            }
            PostingList { postings, blocks, max_score }
        })
        .collect();

    Index { lists, doc_len, avg_len, n_docs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::gen_corpus;

    #[test]
    fn postings_sorted_and_blocked() {
        let c = gen_corpus(2000, 5000, 1.0, 3);
        let idx = build_index(&c);
        let list = &idx.lists[0]; // most common term
        assert!(list.postings.len() > BLOCK, "head term spans blocks");
        assert!(list.postings.windows(2).all(|w| w[0].doc < w[1].doc));
        assert_eq!(list.blocks.len(), list.postings.len().div_ceil(BLOCK));
        for (i, b) in list.blocks.iter().enumerate() {
            let chunk = &list.postings[i * BLOCK..((i + 1) * BLOCK).min(list.postings.len())];
            assert_eq!(b.last_doc, chunk.last().unwrap().doc);
            assert!(b.max_score <= list.max_score);
        }
    }

    #[test]
    fn tf_counted() {
        let c = Corpus { docs: vec![vec![1, 1, 2], vec![2]], vocab: 3 };
        let idx = build_index(&c);
        assert_eq!(idx.lists[1].postings, vec![Posting { doc: 0, tf: 2 }]);
        assert_eq!(idx.lists[2].postings.len(), 2);
        assert_eq!(idx.avg_len, 2.0);
    }
}
