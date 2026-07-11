//! STUB 2 — request hedging (backup requests).
//!
//! Dean & Barroso's "The Tail at Scale" trick: if a request hasn't answered
//! by (say) its p95 latency, fire a duplicate and take whichever finishes
//! first. Quickwit ships exactly this for S3 (TimeoutAndRetryStorage,
//! quickwit-storage/src/timeout_and_retry_storage.rs:37; policy knobs in
//! quickwit-config node_config/mod.rs:608) because S3's tail is fat and
//! retries are cheap relative to a stalled search. AWS's own S3 performance
//! guide recommends aggressive timeouts+retries for latency-sensitive reads.
//!
//! In this simulation a "request" is one latency sample. Hedging = sample a
//! second latency at the hedge deadline; completion time is
//! min(primary, deadline + backup). We never model cancellation — S3 bills
//! the loser anyway; that's the cost side of the trade.

use crate::sim::{BlockStore, LatencyModel};

#[derive(Default)]
pub struct HedgeStats {
    pub requests: u64,
    pub hedged: u64,
}

/// GET `block` with a backup request fired at `hedge_after_micros`.
/// Returns (data, completion_micros). Updates `stats`.
pub fn hedged_get<L: LatencyModel>(
    _store: &mut BlockStore<L>,
    _block: u64,
    _hedge_after_micros: u64,
    _stats: &mut HedgeStats,
) -> (Vec<u8>, u64) {
    // Recipe:
    //   (data, primary) = store.get(block); stats.requests += 1.
    //   primary <= hedge_after   -> done, cost = primary.
    //   else fire the backup: (_, backup) = store.get(block) (discard dup
    //   bytes), stats.hedged += 1, cost = min(primary, hedge_after + backup).
    todo!("stub: hedged GET")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{percentile, Fixed, LocalDisk, S3, BLOCK_SIZE};

    #[test]
    fn fast_primary_never_hedges() {
        let mut store = BlockStore::new(LocalDisk::new(1));
        let mut stats = HedgeStats::default();
        for b in 0..100 {
            let (_, cost) = hedged_get(&mut store, b, 10_000, &mut stats);
            assert!(cost < 10_000);
        }
        assert_eq!(stats.hedged, 0);
        assert_eq!(store.gets, 100, "no extra GETs when nothing is slow");
    }

    #[test]
    fn scripted_hedge_arithmetic() {
        // primary 50ms, backup 1ms, deadline 10ms -> completes at 11ms.
        let mut store = BlockStore::new(Fixed::new(vec![50_000, 1_000]));
        let mut stats = HedgeStats::default();
        let (_, cost) = hedged_get(&mut store, 0, 10_000, &mut stats);
        assert_eq!(cost, 11_000);
        assert_eq!(stats.hedged, 1);
        // primary 50ms, backup 60ms, deadline 10ms -> primary still wins: 50ms.
        let mut store = BlockStore::new(Fixed::new(vec![50_000, 60_000]));
        let (_, cost) = hedged_get(&mut store, 0, 10_000, &mut stats);
        assert_eq!(cost, 50_000);
        // primary 5ms under a 10ms deadline -> untouched, one GET.
        let mut store = BlockStore::new(Fixed::new(vec![5_000]));
        let (_, cost) = hedged_get(&mut store, 0, 10_000, &mut stats);
        assert_eq!(cost, 5_000);
        assert_eq!(store.gets, 1);
    }

    #[test]
    fn p99_improves_with_modest_extra_load() {
        let n = 20_000;
        // Straggler-heavy S3: 5% of GETs are ~8x slower.
        let mut base: Vec<u64> = {
            let mut s3 = S3::with_stragglers(11, 0.05);
            (0..n).map(|_| s3.sample_micros(BLOCK_SIZE)).collect()
        };
        base.sort_unstable();
        let p95 = percentile(&base, 0.95);
        let unhedged_p99 = percentile(&base, 0.99);

        let mut store = BlockStore::new(S3::with_stragglers(11, 0.05));
        let mut stats = HedgeStats::default();
        let mut hedged: Vec<u64> =
            (0..n).map(|b| hedged_get(&mut store, b, p95, &mut stats).1).collect();
        hedged.sort_unstable();
        let hedged_p99 = percentile(&hedged, 0.99);

        assert!(
            hedged_p99 * 2 < unhedged_p99,
            "hedging at p95 should at least halve p99: {hedged_p99} vs {unhedged_p99}"
        );
        let hedge_rate = stats.hedged as f64 / n as f64;
        assert!(hedge_rate < 0.10, "hedge rate {hedge_rate:.3} too high for a p95 deadline");
    }
}
