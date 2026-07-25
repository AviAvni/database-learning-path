//! PROVIDED — a service with rare stalls on a virtual clock, measured two
//! ways. No real sleeping: everything is arithmetic on nanosecond counters,
//! so the coordinated-omission demonstration is deterministic and instant.

/// A service that takes `service_ns` per op, except every `stall_every`-th
/// op (i > 0), which takes `stall_ns` — a GC pause / compaction / fork
/// checkpoint in miniature.
#[derive(Clone, Copy)]
pub struct StallModel {
    pub service_ns: u64,
    pub stall_every: u64,
    pub stall_ns: u64,
}

impl StallModel {
    pub fn service_time(&self, i: u64) -> u64 {
        if i > 0 && i % self.stall_every == 0 {
            self.stall_ns
        } else {
            self.service_ns
        }
    }
}

/// Closed-loop measurement: the client sends the next request only after
/// the previous response arrives, and records completion − send. This is
/// what a naive `for { start = now(); op(); record(now() - start) }` bench
/// loop measures — each stall is seen by exactly ONE sample, and the
/// requests that WOULD have been sent during the stall simply never exist.
pub fn closed_loop(n: u64, m: StallModel) -> Vec<u64> {
    (0..n).map(|i| m.service_time(i)).collect()
}

/// Open-loop measurement: requests arrive on a fixed schedule
/// (`interval_ns` apart) whether or not the server is ready — like real
/// clients do. Latency is completion − INTENDED send time, so every
/// request queued behind a stall is charged its full wait.
pub fn open_loop(n: u64, m: StallModel, interval_ns: u64) -> Vec<u64> {
    let mut lat = Vec::with_capacity(n as usize);
    let mut server_free = 0u64;
    for i in 0..n {
        let intended = i * interval_ns;
        let start = intended.max(server_free);
        let done = start + m.service_time(i);
        server_free = done;
        lat.push(done - intended);
    }
    lat
}

/// Nearest-rank percentile. Sorts in place.
pub fn percentile(samples: &mut [u64], p: f64) -> u64 {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    let rank = ((p / 100.0) * samples.len() as f64).ceil() as usize;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: StallModel = StallModel {
        service_ns: 1_000,
        stall_every: 100,
        stall_ns: 1_000_000,
    };

    #[test]
    fn closed_loop_sees_only_service_times() {
        let lat = closed_loop(1_000, M);
        // exactly 9 stalls (i = 100, 200, ..., 900); everything else 1 µs
        assert_eq!(lat.iter().filter(|&&l| l == 1_000_000).count(), 9);
        assert_eq!(lat.iter().filter(|&&l| l == 1_000).count(), 991);
        // so even p99 of the naive numbers is the happy-path 1 µs
        let mut lat = lat;
        assert_eq!(percentile(&mut lat, 99.0), 1_000);
    }

    #[test]
    fn open_loop_charges_the_queue() {
        // arrivals every 10 µs; a 1 ms stall at i=100 delays the NEXT
        // ~100 arrivals, tapering as the backlog drains
        let lat = open_loop(1_000, M, 10_000);
        assert_eq!(lat[100], 1_000_000); // the stalled op itself
        // op 101 intended at 1_010_000, server busy until 2_000_000,
        // service 1_000: latency = 2_001_000 − 1_010_000
        assert_eq!(lat[101], 991_000);
        // far from any stall the queue is empty again
        assert_eq!(lat[50], 1_000);
        assert!(lat.iter().filter(|&&l| l > 100_000).count() > 90);
    }

    #[test]
    fn percentile_nearest_rank() {
        let mut v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&mut v, 50.0), 50);
        assert_eq!(percentile(&mut v, 99.0), 99);
        assert_eq!(percentile(&mut v, 100.0), 100);
    }
}
