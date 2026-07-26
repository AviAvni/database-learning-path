//! STUB — hedged requests (Tail at Scale, "Hedged requests").
//!
//! The paper's move: send the request to one replica; if no answer
//! arrives within a delay (they use the p95 latency), send a second
//! copy to another replica and take whichever answers first. Google's
//! BigTable benchmark: hedging after 10 ms cut p99.9 from 1,800 ms to
//! 74 ms while sending only ~2% more requests — because 95% of
//! requests finish before the hedge ever fires.
//!
//! The model here reuses the fanout leaf (fast uniform 1–10 ms, or a
//! 1,000 ms stall with probability `p_slow`). With p_slow = 0.005 the
//! unhedged p99.9 IS the stall; a hedge at ~10 ms replaces it with
//! "delay + a second draw" — both draws must stall to stay slow, and
//! p_slow² is negligible.
//!
//! Contracts (the tests): a 10 ms hedge cuts p99.9 by ≥10x; the extra
//! request fraction at that delay stays under 10%; a zero-delay hedge
//! degenerates into sending every request twice.

use crate::fanout::leaf_latency;
use rand::Rng;

pub const P_SLOW: f64 = 0.005;

/// One request, optionally hedged.
///
/// Draw the primary's latency. With `hedge_delay = None`, return
/// (primary, 1 request). With `Some(d)`: if the primary finishes
/// within d, the hedge never fires — (primary, 1). Otherwise fire a
/// second draw at time d and take the winner:
/// (min(primary, d + secondary), 2).
pub fn request_with_hedge(rng: &mut impl Rng, hedge_delay: Option<f64>) -> (f64, u32) {
    let primary = leaf_latency(rng, P_SLOW);
    let _ = (primary, hedge_delay);
    todo!("fire the hedge only if primary > delay; winner's latency, request count")
}

/// Run `trials` requests; return (sorted latencies, total requests sent).
pub fn run_trials(rng: &mut impl Rng, trials: usize, hedge_delay: Option<f64>) -> (Vec<f64>, u64) {
    let mut lats = Vec::with_capacity(trials);
    let mut sent = 0u64;
    for _ in 0..trials {
        let (l, r) = request_with_hedge(rng, hedge_delay);
        lats.push(l);
        sent += r as u64;
    }
    lats.sort_by(f64::total_cmp);
    (lats, sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fanout::{percentile, seeded_rng, SLOW_MS};

    /// The paper's headline, in miniature: unhedged p99.9 is a full
    /// stall; hedging at ~p95 of the fast mode cuts it by 10x or more
    /// (BigTable: 1,800 ms -> 74 ms).
    #[test]
    fn a_10ms_hedge_cuts_p999_by_10x() {
        let mut rng = seeded_rng(11);
        let (unhedged, _) = run_trials(&mut rng, 100_000, None);
        let (hedged, _) = run_trials(&mut rng, 100_000, Some(10.0));
        let u = percentile(&unhedged, 0.999);
        let h = percentile(&hedged, 0.999);
        assert!(u >= SLOW_MS, "unhedged p99.9 should be a stall, got {u}");
        assert!(h * 10.0 <= u, "hedged p99.9 {h} not 10x below unhedged {u}");
    }

    /// The hedge is nearly free: it only fires when the primary takes
    /// longer than the delay, so extra load stays in single digits
    /// (the paper: ~5% at a p95 delay; ~2% in the BigTable run).
    #[test]
    fn extra_load_stays_under_10_percent() {
        let mut rng = seeded_rng(13);
        let trials = 100_000usize;
        let (_, sent) = run_trials(&mut rng, trials, Some(10.0));
        let extra = sent as f64 / trials as f64 - 1.0;
        assert!(extra < 0.10, "extra request fraction {extra}");
        assert!(extra > 0.0, "a 10 ms hedge must fire sometimes");
    }

    /// Degenerate case: delay 0 means the hedge always fires — every
    /// request is sent twice. This is why the paper defers the hedge
    /// to the p95 mark instead of racing two copies from the start.
    #[test]
    fn zero_delay_doubles_the_requests() {
        let mut rng = seeded_rng(17);
        let trials = 10_000usize;
        let (_, sent) = run_trials(&mut rng, trials, Some(0.0));
        let extra = sent as f64 / trials as f64 - 1.0;
        assert!(extra > 0.95, "zero-delay hedge should ~double requests, extra {extra}");
    }
}
