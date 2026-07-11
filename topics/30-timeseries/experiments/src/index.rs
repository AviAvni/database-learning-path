//! STUB 3 — the tag inverted index: label selectors -> series ids.
//!
//! Prometheus's MemPostings (tsdb/index/postings.go:60, Add :403) is a
//! map from (label name, value) to a sorted list of series ids — topic
//! 23's inverted index with labels as terms and series as documents.
//! VictoriaMetrics does the same in index_db.go with a
//! tagFilters->metricIDs cache in front (:124). "High cardinality kills
//! TSDBs" means exactly: one unique label value per series turns this map
//! into n_series single-entry postings lists — memory and churn, no
//! selectivity.

use std::collections::HashMap;

pub struct TagIndex {
    /// (label name, label value) -> sorted, deduped series ids.
    pub postings: HashMap<(String, String), Vec<u64>>,
    pub n_series: u64,
}

impl TagIndex {
    pub fn new() -> Self {
        Self { postings: HashMap::new(), n_series: 0 }
    }

    /// Register a series under every one of its labels. Series ids arrive
    /// in increasing order (the caller allocates them sequentially), so a
    /// plain push keeps each postings list sorted — state that invariant,
    /// rely on it, debug_assert it.
    pub fn add_series(&mut self, _id: u64, _labels: &[(String, String)]) {
        todo!("stub: tag index add_series")
    }

    pub fn postings(&self, name: &str, value: &str) -> &[u64] {
        self.postings
            .get(&(name.to_string(), value.to_string()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// All series matching EVERY (name, value) selector: k-way sorted
    /// intersection. Start from the shortest postings list (the rare
    /// label does the selecting — topic 23's WAND intuition, minus the
    /// scoring) and check membership in the rest via galloping/binary
    /// search. An absent selector matches nothing.
    pub fn intersect(&self, _selectors: &[(&str, &str)]) -> Vec<u64> {
        todo!("stub: tag index k-way intersection")
    }
}

impl Default for TagIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::label_sets;

    fn built(n: usize) -> (TagIndex, Vec<Vec<(String, String)>>) {
        let sets = label_sets(n);
        let mut idx = TagIndex::new();
        for (i, ls) in sets.iter().enumerate() {
            idx.add_series(i as u64, ls);
        }
        (idx, sets)
    }

    fn brute(sets: &[Vec<(String, String)>], sel: &[(&str, &str)]) -> Vec<u64> {
        sets.iter()
            .enumerate()
            .filter(|(_, ls)| {
                sel.iter().all(|(n, v)| ls.iter().any(|(ln, lv)| ln == n && lv == v))
            })
            .map(|(i, _)| i as u64)
            .collect()
    }

    #[test]
    fn matches_brute_force() {
        let (idx, sets) = built(1000);
        for sel in [
            vec![("job", "job-3")],
            vec![("job", "job-3"), ("env", "dev")],
            vec![("env", "prod"), ("region", "r1")],
            vec![("job", "job-0"), ("env", "prod"), ("region", "r0")],
        ] {
            assert_eq!(idx.intersect(&sel), brute(&sets, &sel), "{sel:?}");
        }
    }

    #[test]
    fn results_are_sorted() {
        let (idx, _) = built(1000);
        let r = idx.intersect(&[("env", "dev")]);
        assert!(r.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn rare_selector_narrows_hot_one() {
        let (idx, _) = built(10_000);
        // instance is unique per series: hot ∧ rare must yield exactly 0 or 1
        let r = idx.intersect(&[("job", "job-7"), ("instance", "i-77")]);
        assert_eq!(r, vec![77]);
    }

    #[test]
    fn missing_label_matches_nothing() {
        let (idx, _) = built(100);
        assert!(idx.intersect(&[("job", "job-1"), ("nope", "x")]).is_empty());
    }

    #[test]
    fn cardinality_bomb_is_visible() {
        // the unique-per-series label creates one postings entry per series
        let (idx, _) = built(5_000);
        let unique = idx.postings.keys().filter(|(n, _)| n == "instance").count();
        assert_eq!(unique, 5_000, "high cardinality = the index grows with series count");
    }
}
