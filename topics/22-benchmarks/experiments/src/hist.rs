//! Latency recorder: collect µs samples, report percentiles. The
//! p999 column is the whole point — throughput hides tails
//! (coordinated omission is the classic sin; we record per-op
//! service time, which is honest only because the driver is
//! closed-loop with zero think time).

#[derive(Default)]
pub struct Hist {
    samples: Vec<u64>, // µs
}

impl Hist {
    pub fn record(&mut self, us: u64) {
        self.samples.push(us);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn percentile(&mut self, p: f64) -> u64 {
        assert!(!self.samples.is_empty());
        self.samples.sort_unstable();
        // nearest-rank: ceil(p*N) - 1
        let idx = (p * self.samples.len() as f64).ceil() as usize;
        self.samples[idx.clamp(1, self.samples.len()) - 1]
    }

    pub fn report(&mut self) -> (u64, u64, u64, u64) {
        (
            self.percentile(0.50),
            self.percentile(0.95),
            self.percentile(0.99),
            self.percentile(0.999),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut h = Hist::default();
        for i in 1..=1000 {
            h.record(i);
        }
        assert_eq!(h.percentile(0.50), 500);
        assert_eq!(h.percentile(0.99), 990);
    }
}
