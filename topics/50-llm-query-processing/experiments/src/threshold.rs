//! LANE 2 (exercise) — learn cascade thresholds that meet a precision and
//! recall target *with probability 1 − δ*, the guarantee LOTUS attaches to its
//! approximate sem_filter (arXiv 2407.11418v3, §3.1, Algorithm 1).
//!
//! The setting. One semantic predicate (P1) over N rows. The cascade of lane
//! 1c accepts a row when the proxy's score is ≥ `hi`, rejects it when the score
//! is ≤ `lo`, and asks the oracle in between. Correctness is measured *against
//! the oracle on every row* (the "gold algorithm"), not against ground truth:
//!
//! ```text
//! recall    = |gold ∩ out| / |gold|
//! precision = |gold ∩ out| / |out|
//! ```
//!
//! where `out` = {score ≥ hi} ∪ {lo < score < hi and the oracle said yes}.
//! The band contributes no errors — every row in it gets the oracle's answer —
//! so recall is lost only below `lo` and precision only at or above `hi`.
//!
//! You choose `lo` and `hi` from a uniform random sample of rows that the
//! oracle has labelled (those labels cost oracle calls; the harness charges
//! them). [`naive_thresholds`] is provided: it picks the thresholds that meet
//! the targets *on the sample*. Lane 2's provided row measures how often that
//! misses the targets on the full table — far more often than δ, because a
//! threshold chosen to just meet a target on a sample of a few hundred rows
//! is, by construction, on the edge.
//!
//! Implement [`learn_thresholds`] so the cascade meets both targets in at
//! least a 1 − δ fraction of trials, while still saving oracle calls. LOTUS's
//! recipe (`lotus/sem_ops/cascade_utils.py:42-144` at the pinned commit; see
//! reading-lotus-source.md) is one way: replace the empirical recall target by
//! an upper confidence bound, and accept a precision threshold only when the
//! *lower* confidence bound of the sample precision above it clears the
//! target, with the failure probability split between the two (δ/2 each) and,
//! for precision, divided again across the candidate thresholds you scan
//! (a union bound). Hoeffding's inequality gives a usable radius:
//! ε = sqrt(ln(1/δ') / (2n)) for a mean of n values in [0, 1].
//!
//! The tests are the specification. `cargo test --release threshold` — the
//! trial loop is 100 workloads of 10,000 rows each.

use crate::gen::Workload;
use crate::model::P1;
use crate::plan::{prf, Band};
use crate::rng::Rng;

/// One sampled row: the proxy's score and the oracle's label.
#[derive(Clone, Copy, Debug)]
pub struct Labelled {
    pub score: f64,
    pub gold: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Targets {
    pub recall: f64,
    pub precision: f64,
    /// Allowed probability of missing either target.
    pub delta: f64,
}

/// Recall and precision of the cascade on a labelled set, for band (lo, hi).
pub fn sample_metrics(s: &[Labelled], lo: f64, hi: f64) -> (f64, f64) {
    let pos = s.iter().filter(|x| x.gold).count();
    let found = s.iter().filter(|x| x.gold && x.score > lo).count();
    let accepted = s.iter().filter(|x| x.score >= hi).count();
    let accepted_true = s.iter().filter(|x| x.score >= hi && x.gold).count();
    let band_true = s.iter().filter(|x| x.gold && x.score > lo && x.score < hi).count();
    let recall = if pos == 0 { 1.0 } else { found as f64 / pos as f64 };
    let out = accepted + band_true;
    let precision = if out == 0 { 1.0 } else { (accepted_true + band_true) as f64 / out as f64 };
    (recall, precision)
}

/// PROVIDED: the thresholds that meet the targets on the sample itself.
/// `lo` = the highest sample score that still keeps sample recall ≥ target;
/// `hi` = the lowest sample score ≥ `lo` that keeps sample precision ≥ target.
pub fn naive_thresholds(sample: &[Labelled], t: Targets) -> Band {
    let mut scores: Vec<f64> = sample.iter().map(|x| x.score).collect();
    scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut lo = 0.0;
    for &c in &scores {
        if sample_metrics(sample, c, 1.0).0 >= t.recall {
            lo = c;
        } else {
            break;
        }
    }
    let mut hi = 1.0;
    for &c in scores.iter().filter(|&&c| c > lo) {
        if sample_metrics(sample, lo, c).1 >= t.precision {
            hi = c;
            break;
        }
    }
    Band { lo, hi }
}

/// LANE 2 — thresholds that meet both targets with probability ≥ 1 − δ.
pub fn learn_thresholds(sample: &[Labelled], t: Targets) -> Band {
    let _ = (sample, t);
    todo!("lane 2: statistically corrected cascade thresholds (see module docs)")
}

/// What one trial produced.
#[derive(Clone, Copy, Debug)]
pub struct Trial {
    pub band: Band,
    pub recall: f64,
    pub precision: f64,
    /// Oracle calls on the sample (threshold learning).
    pub sample_calls: u64,
    /// Oracle calls in the cascade's band over the full table.
    pub band_calls: u64,
}

impl Trial {
    pub fn met(&self, t: Targets) -> bool {
        self.recall >= t.recall && self.precision >= t.precision
    }
}

/// PROVIDED harness: one workload (seed), one uniform sample, one learner.
pub fn trial(seed: u64, n: usize, sample_size: usize, t: Targets, learner: fn(&[Labelled], Targets) -> Band) -> Trial {
    let w = Workload::new(seed, n, vec![P1]);
    let rows: Vec<usize> = (0..n).collect();
    let gold: Vec<usize> = rows.iter().copied().filter(|&d| w.oracle(d, 0)).collect();
    let idx = Rng::new(seed ^ 0x5A17).sample(n, sample_size);
    let sample: Vec<Labelled> = idx
        .iter()
        .map(|&d| Labelled { score: w.proxy_score(d, 0), gold: w.oracle(d, 0) })
        .collect();
    let band = learner(&sample, t);
    let mut out = Vec::new();
    let mut band_calls = 0u64;
    for &d in &rows {
        let s = w.proxy_score(d, 0);
        if s >= band.hi {
            out.push(d);
        } else if s > band.lo {
            band_calls += 1;
            if w.oracle(d, 0) {
                out.push(d);
            }
        }
    }
    let (precision, recall, _) = prf(&out, &gold);
    Trial { band, recall, precision, sample_calls: sample_size as u64, band_calls }
}

/// Summary over `trials` seeds: (fraction of trials that missed a target,
/// mean band calls, mean recall, mean precision).
pub fn sweep(trials: u64, n: usize, sample_size: usize, t: Targets, learner: fn(&[Labelled], Targets) -> Band) -> (f64, f64, f64, f64) {
    let mut missed = 0u64;
    let (mut calls, mut rec, mut prec) = (0.0, 0.0, 0.0);
    for s in 1..=trials {
        let r = trial(1000 + s, n, sample_size, t, learner);
        if !r.met(t) {
            missed += 1;
        }
        calls += r.band_calls as f64;
        rec += r.recall;
        prec += r.precision;
    }
    let k = trials as f64;
    (missed as f64 / k, calls / k, rec / k, prec / k)
}

pub const N: usize = 10_000;
pub const SAMPLE: usize = 300;
pub const TARGETS: Targets = Targets { recall: 0.9, precision: 0.9, delta: 0.2 };

#[cfg(test)]
mod tests {
    use super::*;

    /// Provided, passes on a fresh clone: the uncorrected learner misses a
    /// target in more than δ of the trials. This is the problem lane 2 solves.
    #[test]
    fn naive_thresholds_miss_more_often_than_delta() {
        let (miss, _, _, _) = sweep(100, N, SAMPLE, TARGETS, naive_thresholds);
        assert!(miss > TARGETS.delta, "naive miss rate {miss}");
    }

    /// STUB: the guarantee — at most δ of trials miss either target.
    #[test]
    fn learned_thresholds_meet_targets_with_probability_one_minus_delta() {
        let (miss, _, _, _) = sweep(100, N, SAMPLE, TARGETS, learn_thresholds);
        assert!(miss <= TARGETS.delta, "miss rate {miss} > delta {}", TARGETS.delta);
    }

    /// STUB: and it still saves work — a band that sends every row to the
    /// oracle meets any target trivially and is not an answer.
    #[test]
    fn learned_thresholds_still_skip_most_oracle_calls() {
        let (_, calls, _, _) = sweep(100, N, SAMPLE, TARGETS, learn_thresholds);
        assert!(calls < 0.6 * N as f64, "mean band calls {calls} of {N}");
    }

    /// STUB: thresholds are ordered and inside [0, 1].
    #[test]
    fn learned_band_is_well_formed() {
        for s in 1..=20 {
            let r = trial(1000 + s, N, SAMPLE, TARGETS, learn_thresholds);
            assert!(0.0 <= r.band.lo && r.band.lo <= r.band.hi && r.band.hi <= 1.0, "{:?}", r.band);
        }
    }
}
