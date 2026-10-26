//! Physical plans for `SELECT * FROM reviews WHERE P1(text) AND P2(text)`,
//! where P1 and P2 are semantic predicates — and the meter that prices them.
//!
//! Four plans, each a classic optimizer idea re-priced for an operator that
//! costs dollars and seconds per row and is sometimes wrong:
//!
//! * [`oracle_all`] — the LOTUS "gold algorithm" for a conjunction: ask the
//!   oracle every predicate on every row. No short-circuit.
//! * [`ordered`] — short-circuit evaluation in a given predicate order, so the
//!   second predicate only sees the first one's survivors.
//! * [`rank_order`] — choose that order the way an expensive-predicate
//!   optimizer does: estimate each predicate's selectivity on a labelled
//!   sample, then sort by rank = (selectivity − 1) / cost.
//! * [`cascade`] — per predicate, score every surviving row with the cheap
//!   proxy, accept or reject the confident ones, and send only the uncertain
//!   band to the oracle.
//!
//! None of these changes what the oracle would say about a row; they change
//! *how many times it is asked*. Only the cascade can change the answer, and
//! that is what its F1-vs-gold column measures.

use crate::gen::Workload;
use crate::model::{wave_seconds, Model};
use std::collections::{BTreeSet, HashSet};

/// What a plan spent. Dollars and seconds are MODEL figures (calls × the
/// constants in `model.rs`); the call counts follow from the seed exactly.
#[derive(Clone, Debug, Default)]
pub struct Meter {
    pub oracle_calls: u64,
    pub proxy_calls: u64,
    pub cache_hits: u64,
    pub dollars: f64,
    pub seconds: f64,
}

impl Meter {
    /// One stage: `calls` independent requests to `m`, issued with
    /// `PARALLELISM` in flight. Stages run one after another.
    pub fn stage(&mut self, calls: u64, m: Model, oracle: bool) {
        if oracle {
            self.oracle_calls += calls;
        } else {
            self.proxy_calls += calls;
        }
        self.dollars += calls as f64 * m.dollars;
        self.seconds += wave_seconds(calls, m);
    }
}

/// A shared answer cache keyed by (document, predicate). Only membership is
/// ever queried, so the HashSet's iteration order never matters.
pub type Cache = HashSet<(usize, usize)>;

/// Ask the oracle predicate `p` about `docs` (one stage), consulting the cache.
/// Returns the docs it said yes to, in input order.
pub fn oracle_stage(
    w: &Workload,
    docs: &[usize],
    p: usize,
    m: &mut Meter,
    mut cache: Option<&mut Cache>,
) -> Vec<usize> {
    let mut calls = 0u64;
    for &d in docs {
        match cache.as_deref_mut() {
            Some(c) => {
                if c.insert((d, p)) {
                    calls += 1;
                } else {
                    m.cache_hits += 1;
                }
            }
            None => calls += 1,
        }
    }
    m.stage(calls, w.preds[p].oracle, true);
    docs.iter().copied().filter(|&d| w.oracle(d, p)).collect()
}

/// Plan (a): every predicate on every row, no short-circuit. 2N calls.
pub fn oracle_all(w: &Workload, rows: &[usize], m: &mut Meter) -> Vec<usize> {
    let mut keep: BTreeSet<usize> = rows.iter().copied().collect();
    for p in 0..w.preds.len() {
        let yes: BTreeSet<usize> = oracle_stage(w, rows, p, m, None).into_iter().collect();
        keep = keep.intersection(&yes).copied().collect();
    }
    keep.into_iter().collect()
}

/// Short-circuit evaluation in `order`: predicate k only sees the rows every
/// earlier predicate said yes to.
pub fn ordered(
    w: &Workload,
    rows: &[usize],
    order: &[usize],
    m: &mut Meter,
    mut cache: Option<&mut Cache>,
) -> Vec<usize> {
    let mut surv = rows.to_vec();
    for &p in order {
        surv = oracle_stage(w, &surv, p, m, cache.as_deref_mut());
    }
    surv
}

/// The order an expensive-predicate optimizer would pick, from a labelled
/// sample. Returns (order, estimated selectivity per predicate). The sample's
/// oracle calls are charged to `m` — estimating selectivity is not free when
/// the only way to observe a predicate is to pay for it.
pub fn rank_order(w: &Workload, sample: &[usize], m: &mut Meter) -> (Vec<usize>, Vec<f64>) {
    let mut sel = vec![0.0; w.preds.len()];
    for p in 0..w.preds.len() {
        let yes = oracle_stage(w, sample, p, m, None).len();
        sel[p] = yes as f64 / sample.len() as f64;
    }
    let mut order: Vec<usize> = (0..w.preds.len()).collect();
    // rank = (s − 1) / c, ascending: cheap predicates that discard much go first.
    order.sort_by(|&a, &b| {
        let ra = (sel[a] - 1.0) / w.preds[a].oracle.dollars;
        let rb = (sel[b] - 1.0) / w.preds[b].oracle.dollars;
        ra.partial_cmp(&rb).unwrap().then(a.cmp(&b))
    });
    (order, sel)
}

/// The order a selectivity-only rule picks: most selective first, cost ignored.
pub fn selectivity_order(sel: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sel.len()).collect();
    order.sort_by(|&a, &b| sel[a].partial_cmp(&sel[b]).unwrap().then(a.cmp(&b)));
    order
}

/// The proxy's decision band: accept when score ≥ `hi`, reject when
/// score ≤ `lo`, ask the oracle otherwise.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub lo: f64,
    pub hi: f64,
}

impl Band {
    /// A band symmetric around 0.5 with half-width `h`: h = 0 trusts the proxy
    /// on every row, h = 0.5 sends every row to the oracle (sigmoid scores are
    /// never exactly 0 or 1).
    pub fn symmetric(h: f64) -> Band {
        Band { lo: 0.5 - h, hi: 0.5 + h }
    }
}

/// Plan (c): a proxy → oracle cascade per predicate, in `order`, one band per
/// predicate.
pub fn cascade(w: &Workload, rows: &[usize], order: &[usize], bands: &[Band], m: &mut Meter) -> Vec<usize> {
    let mut surv = rows.to_vec();
    for (k, &p) in order.iter().enumerate() {
        let band = bands[k];
        m.stage(surv.len() as u64, w.preds[p].proxy, false);
        let mut accepted = Vec::new();
        let mut unsure = Vec::new();
        for &d in &surv {
            let s = w.proxy_score(d, p);
            if s >= band.hi {
                accepted.push(d);
            } else if s > band.lo {
                unsure.push(d);
            }
        }
        let confirmed = oracle_stage(w, &unsure, p, m, None);
        let mut next: Vec<usize> = accepted.into_iter().chain(confirmed).collect();
        next.sort_unstable();
        surv = next;
    }
    surv
}

/// LIMIT k over one predicate, the way LOTUS 1.1.3 does it per SemBench §5.3:
/// evaluate every row, then keep the first k.
pub fn limit_after_all(w: &Workload, rows: &[usize], p: usize, k: usize, m: &mut Meter) -> Vec<usize> {
    let mut yes = oracle_stage(w, rows, p, m, None);
    yes.truncate(k);
    yes
}

/// LIMIT k pushed into the scan: issue the oracle in waves of `PARALLELISM`
/// rows and stop after the wave that produced the k-th yes. The last wave is
/// paid for in full — requests already in flight cannot be recalled.
pub fn limit_streaming(w: &Workload, rows: &[usize], p: usize, k: usize, m: &mut Meter) -> Vec<usize> {
    let mut out = Vec::new();
    for wave in rows.chunks(crate::model::PARALLELISM) {
        out.extend(oracle_stage(w, wave, p, m, None));
        if out.len() >= k {
            break;
        }
    }
    out.truncate(k);
    out
}

/// Expected F1 of "the oracle says yes to every predicate" against "every
/// predicate is really true", for independent predicates with independent,
/// symmetric oracle errors. With selectivity s and accuracy a, the oracle says
/// yes with probability s·a + (1 − s)(1 − a); a row is a true positive with
/// probability Π s·a. Precision = Π s·a / Π yes; recall = Π a.
pub fn oracle_f1_closed_form(preds: &[crate::model::Predicate]) -> f64 {
    let yes: f64 = preds
        .iter()
        .map(|p| p.selectivity * p.oracle_accuracy + (1.0 - p.selectivity) * (1.0 - p.oracle_accuracy))
        .product();
    let tp: f64 = preds.iter().map(|p| p.selectivity * p.oracle_accuracy).product();
    let truth: f64 = preds.iter().map(|p| p.selectivity).product();
    let (prec, rec) = (tp / yes, tp / truth);
    2.0 * prec * rec / (prec + rec)
}

/// Precision, recall and F1 of `got` against `want`.
pub fn prf(got: &[usize], want: &[usize]) -> (f64, f64, f64) {
    let want: BTreeSet<usize> = want.iter().copied().collect();
    let tp = got.iter().filter(|d| want.contains(d)).count() as f64;
    let p = if got.is_empty() { 1.0 } else { tp / got.len() as f64 };
    let r = if want.is_empty() { 1.0 } else { tp / want.len() as f64 };
    let f = if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) };
    (p, r, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{P1, P2};

    fn world() -> (Workload, Vec<usize>) {
        let w = Workload::new(50, 5_000, vec![P1, P2]);
        let rows = (0..w.n_docs).collect();
        (w, rows)
    }

    #[test]
    fn every_order_returns_the_gold_set() {
        let (w, rows) = world();
        let gold = w.gold_set(&rows);
        let mut m = Meter::default();
        assert_eq!(oracle_all(&w, &rows, &mut m), gold);
        assert_eq!(ordered(&w, &rows, &[0, 1], &mut Meter::default(), None), gold);
        assert_eq!(ordered(&w, &rows, &[1, 0], &mut Meter::default(), None), gold);
    }

    #[test]
    fn short_circuit_call_count_is_exact() {
        let (w, rows) = world();
        let mut m = Meter::default();
        ordered(&w, &rows, &[1, 0], &mut m, None);
        let s2 = rows.iter().filter(|&&d| w.oracle(d, 1)).count() as u64;
        assert_eq!(m.oracle_calls, rows.len() as u64 + s2);
    }

    #[test]
    fn full_band_is_the_oracle_and_empty_band_is_the_proxy() {
        let (w, rows) = world();
        let gold = w.gold_set(&rows);
        let full = cascade(&w, &rows, &[0, 1], &[Band::symmetric(0.5); 2], &mut Meter::default());
        assert_eq!(full, gold);
        let mut m = Meter::default();
        cascade(&w, &rows, &[0, 1], &[Band::symmetric(0.0); 2], &mut m);
        assert_eq!(m.oracle_calls, 0);
    }

    #[test]
    fn oracle_f1_matches_its_closed_form_on_average() {
        let preds = vec![P1, P2];
        let want = oracle_f1_closed_form(&preds);
        let rows: Vec<usize> = (0..10_000).collect();
        let k = 30u64;
        let mean: f64 = (1..=k)
            .map(|s| {
                let w = Workload::new(1050 + s, 10_000, preds.clone());
                prf(&w.gold_set(&rows), &w.truth_set(&rows)).2
            })
            .sum::<f64>()
            / k as f64;
        assert!((mean - want).abs() < 0.01, "mean {mean} closed form {want}");
    }

    #[test]
    fn cache_never_changes_the_answer() {
        let (w, rows) = world();
        let mut c = Cache::new();
        let a = ordered(&w, &rows, &[0, 1], &mut Meter::default(), Some(&mut c));
        let mut m = Meter::default();
        let b = ordered(&w, &rows, &[0, 1], &mut m, Some(&mut c));
        assert_eq!(a, b);
        assert_eq!(m.oracle_calls, 0, "a repeated query is all cache hits");
    }
}
