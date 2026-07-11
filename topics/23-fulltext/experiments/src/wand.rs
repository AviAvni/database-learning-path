//! Block-max WAND (Ding & Suel, SIGIR'11) — top-k retrieval that
//! skips most of the postings by proving, with score upper bounds,
//! that a doc CANNOT enter the current top-k.
//!
//! ## STUB — implement `wand_topk`
//!
//! Recipe (document-at-a-time, one cursor per query term):
//!
//! 1. Cursor = (term's PostingList, position, its idf, its list
//!    max_score). Threshold θ = k-th best score so far (min-heap of
//!    size k; θ = -inf until the heap is full).
//! 2. Sort cursors by current doc id. Find the PIVOT: the first
//!    cursor index p such that Σ max_score of cursors[0..=p] > θ
//!    (tantivy: block_wand_union.rs:8-24 `find_pivot_doc`). If no
//!    such p, every remaining doc is provably below θ — DONE.
//! 3. Let pivot_doc = cursors[p].doc.
//!    - Block-max refinement: tighten the bound using each cursor's
//!      CURRENT block max_score (skip to the block containing
//!      pivot_doc first — blocks[i].last_doc >= pivot_doc). If the
//!      refined sum <= θ, the whole region up to
//!      min(block.last_doc) is dead: advance the cursor with the
//!      largest list max_score past that doc, count the skipped
//!      postings in `postings_skipped`, and loop to 2.
//!    - If cursors[0..p] all sit AT pivot_doc: fully score pivot_doc
//!      (sum bm25::score over cursors at it), docs_scored += 1, push
//!      into the heap (pop min if len > k), advance those cursors,
//!      loop.
//!    - Else: advance a cursor with doc < pivot_doc (pick the one
//!      with the largest max_score — cheapest information gain) up
//!      to pivot_doc via its block index, loop.
//! 4. Return top-k sorted like the oracle: score desc, doc asc.
//!
//! Advancing "via its block index" means: binary-search/linear-scan
//! `blocks` for the first block with last_doc >= target, then scan
//! only inside that block — that's the skip-list read pattern of
//! tantivy's postings/skip.rs:93 `SkipReader`.
//!
//! Contract: identical results to `bm25::oracle_topk`, while
//! `docs_scored` (fully-evaluated docs) is a small fraction of the
//! oracle's postings walk on rare∧common queries.

use crate::index::Index;

#[derive(Debug, Default, Clone, Copy)]
pub struct WandStats {
    /// docs fully scored (oracle counterpart: total postings touched)
    pub docs_scored: usize,
    /// postings jumped over via block skip pointers
    pub postings_skipped: usize,
}

pub fn wand_topk(_index: &Index, _query: &[u32], _k: usize) -> (Vec<(f32, u32)>, WandStats) {
    todo!("block-max WAND: pivot + block-max refinement + skip via BlockMeta")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::oracle_topk;
    use crate::corpus::gen_corpus;
    use crate::index::build_index;

    fn check(query: &[u32], k: usize) -> (usize, usize) {
        let c = gen_corpus(20_000, 20_000, 1.0, 11);
        let idx = build_index(&c);
        let (oracle, oracle_work) = oracle_topk(&idx, query, k);
        let (wand, stats) = wand_topk(&idx, query, k);
        assert_eq!(oracle.len(), wand.len());
        for (o, w) in oracle.iter().zip(&wand) {
            assert!(
                (o.0 - w.0).abs() < 1e-3,
                "score mismatch: oracle {o:?} vs wand {w:?}"
            );
        }
        (oracle_work, stats.docs_scored)
    }

    #[test]
    fn matches_oracle_common_terms() {
        check(&[0, 1, 5], 10);
    }

    #[test]
    fn matches_oracle_rare_and_common() {
        check(&[2, 9000, 15_000], 10);
    }

    #[test]
    fn skips_work_on_rare_and_common() {
        // one rare high-idf term dominates: WAND should prove most
        // common-term postings irrelevant and never score them
        let (oracle_work, wand_work) = check(&[0, 12_000], 10);
        assert!(
            wand_work * 4 < oracle_work,
            "wand scored {wand_work} docs vs oracle {oracle_work} postings — no skipping?"
        );
    }
}
