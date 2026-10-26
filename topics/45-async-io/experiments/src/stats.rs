//! Latency summaries. Percentiles are nearest-rank over the full sample, not
//! a histogram, because every lane keeps at most a few hundred thousand
//! samples and an exact answer is cheaper than explaining a bucketing error.

#[derive(Clone, Debug, Default)]
pub struct Run {
    /// I/Os completed.
    pub ops: usize,
    /// Wall-clock seconds for the whole run.
    pub secs: f64,
    /// Per-I/O latency in nanoseconds, from issue to completion as seen by
    /// the caller (for a batch: from the batch's submit to its completion).
    pub lat_ns: Vec<u64>,
    /// Syscalls the run issued, where the lane can count them.
    pub syscalls: u64,
    /// XOR of the first 8 bytes of every page read — two lanes that read the
    /// same offsets must agree, or one of them read the wrong bytes.
    pub checksum: u64,
}

impl Run {
    pub fn iops(&self) -> f64 {
        self.ops as f64 / self.secs
    }
    pub fn mib_s(&self, bytes_per_op: usize) -> f64 {
        self.iops() * bytes_per_op as f64 / (1024.0 * 1024.0)
    }
    pub fn pct_us(&mut self, p: f64) -> f64 {
        pct(&mut self.lat_ns, p) as f64 / 1000.0
    }
    pub fn mean_us(&self) -> f64 {
        if self.lat_ns.is_empty() {
            return 0.0;
        }
        self.lat_ns.iter().sum::<u64>() as f64 / self.lat_ns.len() as f64 / 1000.0
    }
}

/// Nearest-rank percentile; sorts in place.
pub fn pct(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v[rank.clamp(1, v.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let mut v = vec![5, 1, 4, 2, 3];
        assert_eq!(pct(&mut v, 50.0), 3);
        assert_eq!(pct(&mut v, 99.0), 5);
        assert_eq!(pct(&mut v, 20.0), 1);
    }
}
