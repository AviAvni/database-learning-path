//! The seeded workload: documents, their ground truth, and what each model
//! would answer about them.
//!
//! Every fact is a pure function of `(seed, doc, predicate)` through
//! [`crate::rng::unit`] — so the oracle gives the same answer to the same
//! question every time (temperature 0), and two plans that ask in different
//! orders see the same world. Rows are indices into the document table; in
//! lanes 1a–1c and 1e row i *is* document i, and lane 1d's agents draw
//! overlapping subsets of rows.

use crate::model::Predicate;
use crate::rng::{normal, unit};

#[derive(Clone, Debug)]
pub struct Workload {
    pub seed: u64,
    pub n_docs: usize,
    pub preds: Vec<Predicate>,
}

// Distinct key spaces for the three per-document draws, so that truth, oracle
// error and proxy noise are independent of each other.
const K_TRUTH: u64 = 100;
const K_ORACLE: u64 = 200;
const K_PROXY: u64 = 300;

impl Workload {
    pub fn new(seed: u64, n_docs: usize, preds: Vec<Predicate>) -> Self {
        Workload { seed, n_docs, preds }
    }

    /// Ground truth: is predicate `p` really true of document `doc`?
    pub fn truth(&self, doc: usize, p: usize) -> bool {
        unit(self.seed, doc as u64, K_TRUTH + p as u64) < self.preds[p].selectivity
    }

    /// What the oracle answers. Wrong with probability 1 − accuracy,
    /// independently per (doc, predicate), and the same on every call.
    pub fn oracle(&self, doc: usize, p: usize) -> bool {
        let wrong = unit(self.seed, doc as u64, K_ORACLE + p as u64) >= self.preds[p].oracle_accuracy;
        self.truth(doc, p) ^ wrong
    }

    /// The proxy's score in (0, 1): sigmoid(±mu + noise). It is a function of
    /// the *truth*, not of the oracle's answer, so the proxy and the oracle err
    /// independently.
    pub fn proxy_score(&self, doc: usize, p: usize) -> f64 {
        let mu = self.preds[p].proxy_mu;
        let centre = if self.truth(doc, p) { mu } else { -mu };
        let z = centre + normal(self.seed, doc as u64, K_PROXY + p as u64);
        1.0 / (1.0 + (-z).exp())
    }

    /// Documents for which every predicate is really true.
    pub fn truth_set(&self, rows: &[usize]) -> Vec<usize> {
        rows.iter()
            .copied()
            .filter(|&d| (0..self.preds.len()).all(|p| self.truth(d, p)))
            .collect()
    }

    /// The gold answer as LOTUS defines it: the oracle on every row.
    pub fn gold_set(&self, rows: &[usize]) -> Vec<usize> {
        rows.iter()
            .copied()
            .filter(|&d| (0..self.preds.len()).all(|p| self.oracle(d, p)))
            .collect()
    }

    /// Fraction of `rows` for which the oracle says yes to `p` (measured, not
    /// the generator's target).
    pub fn oracle_selectivity(&self, rows: &[usize], p: usize) -> f64 {
        rows.iter().filter(|&&d| self.oracle(d, p)).count() as f64 / rows.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{P1, P2};

    #[test]
    fn selectivities_and_accuracies_land_near_their_targets() {
        let w = Workload::new(50, 20_000, vec![P1, P2]);
        let rows: Vec<usize> = (0..w.n_docs).collect();
        for (p, pred) in w.preds.iter().enumerate() {
            let t = rows.iter().filter(|&&d| w.truth(d, p)).count() as f64 / rows.len() as f64;
            assert!((t - pred.selectivity).abs() < 0.01, "{} truth sel {t}", pred.name);
            let acc = rows.iter().filter(|&&d| w.truth(d, p) == w.oracle(d, p)).count() as f64
                / rows.len() as f64;
            assert!((acc - pred.oracle_accuracy).abs() < 0.005, "{} oracle acc {acc}", pred.name);
        }
    }

    #[test]
    fn answers_do_not_depend_on_who_asks_first() {
        let w = Workload::new(50, 100, vec![P1, P2]);
        let a: Vec<bool> = (0..100).map(|d| w.oracle(d, 1)).collect();
        let _ = (0..100).rev().map(|d| w.oracle(d, 0)).count();
        let b: Vec<bool> = (0..100).map(|d| w.oracle(d, 1)).collect();
        assert_eq!(a, b);
    }
}
