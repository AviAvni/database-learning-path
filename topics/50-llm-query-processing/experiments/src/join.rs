//! LANE 3 (exercise) — a semantic join without N×M oracle calls.
//!
//! `papers ⋈ datasets ON "the paper {abstract} uses the {dataset}"` — the
//! example LOTUS uses for sem_join (arXiv 2407.11418v3, Figure 3). The gold
//! algorithm is a nested loop: one oracle call per pair, O(N1·N2) (LOTUS §2.3,
//! Table 1). SemBench (PVLDB 19(8), §5.3) measured what that means at a small
//! scale: one MMQA query "required 40,000 LLM calls for join predicate
//! evaluation" — a 200 × 200 cross product — and 160,000 at scale 400 (§5.4).
//!
//! The simulation. `n_left` papers, `n_right` datasets. Each paper really uses
//! one dataset, and a second one with probability 0.3. The oracle answers one
//! pair per call, misses a real match 3% of the time and invents one 0.05% of
//! the time (MODEL). The proxy is an embedding similarity in [0, 1] — matching
//! pairs centre on 0.62, non-matching on 0.30 (MODEL) — and costs one
//! embedding call per *item* (n_left + n_right of them), not per pair, because
//! the pairwise similarities are dot products computed locally. That
//! asymmetry is the whole opportunity: the proxy is linear in the inputs, the
//! oracle quadratic.
//!
//! Provided: the generator, [`nested_loop`] (the gold algorithm, which the
//! bench runs to get the reference), and the cost accounting.
//!
//! Implement [`blocked_join`]: use the similarities to choose a small set of
//! candidate pairs (per-left top-k, a global threshold, or LOTUS's sampled
//! threshold — your choice), ask the oracle only about those, and return the
//! pairs it says yes to. You may spend oracle calls on a sample of pairs to
//! choose the cut; they count. The tests are the specification: precision
//! ≥ 0.95 against the gold (nested-loop) answer; recall ≥ 0.95 against the
//! gold pairs that are real matches ([`gold_true`] — read its comment for why
//! not all of gold); total oracle calls ≤ 10% of n_left × n_right.

use crate::model::{wave_seconds, ORACLE_CHEAP, PROXY};
use crate::plan::Meter;
use crate::rng::{normal, unit};

pub struct JoinWorkload {
    pub seed: u64,
    pub n_left: usize,
    pub n_right: usize,
}

const MATCH_MEAN: f64 = 0.62; // MODEL
const MATCH_SD: f64 = 0.08; // MODEL
const NONMATCH_MEAN: f64 = 0.30; // MODEL
const NONMATCH_SD: f64 = 0.10; // MODEL
const MISS_RATE: f64 = 0.03; // MODEL: oracle false-negative rate
const INVENT_RATE: f64 = 0.0005; // MODEL: oracle false-positive rate

impl JoinWorkload {
    pub fn new(seed: u64, n_left: usize, n_right: usize) -> Self {
        JoinWorkload { seed, n_left, n_right }
    }

    /// Ground truth.
    pub fn truth(&self, i: usize, j: usize) -> bool {
        let first = (unit(self.seed, i as u64, 1) * self.n_right as f64) as usize;
        let has_second = unit(self.seed, i as u64, 2) < 0.3;
        let second = (unit(self.seed, i as u64, 3) * self.n_right as f64) as usize;
        j == first || (has_second && j == second)
    }

    /// The oracle's answer for one pair; pure in (i, j).
    pub fn oracle(&self, i: usize, j: usize) -> bool {
        let key = (i * self.n_right + j) as u64;
        let u = unit(self.seed, key, 10);
        if self.truth(i, j) {
            u >= MISS_RATE
        } else {
            u < INVENT_RATE
        }
    }

    /// The proxy: an embedding similarity, clipped to [0, 1].
    pub fn similarity(&self, i: usize, j: usize) -> f64 {
        let key = (i * self.n_right + j) as u64;
        let z = normal(self.seed, key, 20);
        let s = if self.truth(i, j) { MATCH_MEAN + MATCH_SD * z } else { NONMATCH_MEAN + NONMATCH_SD * z };
        s.clamp(0.0, 1.0)
    }

    pub fn pairs(&self) -> u64 {
        (self.n_left * self.n_right) as u64
    }

    /// Charge the oracle for `calls` pair evaluations (one stage).
    pub fn charge_oracle(&self, m: &mut Meter, calls: u64) {
        m.stage(calls, ORACLE_CHEAP, true);
    }

    /// Charge the embedding model once per item on both sides.
    pub fn charge_embeddings(&self, m: &mut Meter) {
        m.stage((self.n_left + self.n_right) as u64, PROXY, false);
    }
}

/// PROVIDED — the gold algorithm: every pair, one oracle call each. Returns
/// matching (left, right) pairs in row-major order.
pub fn nested_loop(w: &JoinWorkload, m: &mut Meter) -> Vec<(usize, usize)> {
    w.charge_oracle(m, w.pairs());
    let mut out = Vec::new();
    for i in 0..w.n_left {
        for j in 0..w.n_right {
            if w.oracle(i, j) {
                out.push((i, j));
            }
        }
    }
    out
}

/// LANE 3 — a blocked semantic join. Charge every oracle call through
/// `w.charge_oracle` and the embeddings through `w.charge_embeddings`.
/// Return the pairs in row-major order.
pub fn blocked_join(w: &JoinWorkload, m: &mut Meter) -> Vec<(usize, usize)> {
    let _ = (w, m);
    todo!("lane 3: block on similarity, then ask the oracle (see module docs)")
}

/// Precision and recall of `got` against `gold` (both row-major sorted).
pub fn pair_pr(got: &[(usize, usize)], gold: &[(usize, usize)]) -> (f64, f64) {
    let tp = got.iter().filter(|p| gold.binary_search(p).is_ok()).count() as f64;
    let p = if got.is_empty() { 1.0 } else { tp / got.len() as f64 };
    let r = if gold.is_empty() { 1.0 } else { tp / gold.len() as f64 };
    (p, r)
}

/// The gold pairs that are also real matches — what the oracle got right.
/// A gold pair that is *not* a real match is an oracle invention on a pair
/// that is, by construction, dissimilar; no similarity-based blocking will
/// ever propose it, so recall against all of gold is capped below 1.
pub fn gold_true(w: &JoinWorkload, gold: &[(usize, usize)]) -> Vec<(usize, usize)> {
    gold.iter().copied().filter(|&(i, j)| w.truth(i, j)).collect()
}

/// Lane 3's sizes: 300 papers × 200 datasets = 60,000 pairs.
pub const LEFT: usize = 300;
pub const RIGHT: usize = 200;

pub fn simulated_seconds_nested(w: &JoinWorkload) -> f64 {
    wave_seconds(w.pairs(), ORACLE_CHEAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_loop_is_quadratic_and_close_to_truth() {
        let w = JoinWorkload::new(77, LEFT, RIGHT);
        let mut m = Meter::default();
        let gold = nested_loop(&w, &mut m);
        assert_eq!(m.oracle_calls, 60_000);
        let truth: Vec<(usize, usize)> = (0..LEFT)
            .flat_map(|i| (0..RIGHT).map(move |j| (i, j)))
            .filter(|&(i, j)| w.truth(i, j))
            .collect();
        let (p, r) = pair_pr(&gold, &truth);
        assert!(p > 0.85 && r > 0.95, "gold vs truth p {p} r {r}");
    }

    /// STUB: the contract.
    #[test]
    fn blocked_join_matches_gold_with_a_tenth_of_the_calls() {
        for seed in [77u64, 78, 79] {
            let w = JoinWorkload::new(seed, LEFT, RIGHT);
            let gold = nested_loop(&w, &mut Meter::default());
            let mut m = Meter::default();
            let got = blocked_join(&w, &mut m);
            let (p, _) = pair_pr(&got, &gold);
            let (_, r) = pair_pr(&got, &gold_true(&w, &gold));
            assert!(r >= 0.95, "seed {seed}: recall {r}");
            assert!(p >= 0.95, "seed {seed}: precision {p}");
            assert!(m.oracle_calls * 10 <= w.pairs(), "seed {seed}: {} oracle calls", m.oracle_calls);
        }
    }

    /// STUB: embeddings are charged, once per item.
    #[test]
    fn blocked_join_pays_for_embeddings() {
        let w = JoinWorkload::new(77, LEFT, RIGHT);
        let mut m = Meter::default();
        blocked_join(&w, &mut m);
        assert_eq!(m.proxy_calls, (LEFT + RIGHT) as u64);
    }
}
