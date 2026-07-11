//! PROVIDED infrastructure — simulated storage tiers with *virtual* latency.
//!
//! Every read returns `(bytes, simulated_micros)`. Latency is charged, not
//! slept, so a 200K-op benchmark against "S3" finishes in milliseconds of
//! wall time while producing honest p50/p99 *distributions*. Everything is
//! deterministic under a seed, so lanes are comparable run-to-run.
//!
//! Latency numbers are calibrated to public measurements:
//!   local NVMe   ~100 µs to first byte, ~4 GB/s
//!   S3 GET       ~10-20 ms to first byte (lognormal), ~80 MB/s per stream,
//!                a small probability of ~8x stragglers (the tail hedging kills)

use rand::prelude::*;
use rand_chacha::ChaCha8Rng;

pub const BLOCK_SIZE: usize = 4096;
pub const ENTRY_SIZE: usize = 24; // 8-byte key + 16-byte value
pub const ENTRIES_PER_BLOCK: u64 = (BLOCK_SIZE / ENTRY_SIZE) as u64; // 170

/// Deterministic value for a key — no giant in-memory table needed.
pub fn value_for(key: u64) -> [u8; 16] {
    let mut x = key.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    let a = x ^ (x >> 31);
    let b = a.wrapping_mul(0xd6e8feb86659fd93);
    let mut v = [0u8; 16];
    v[..8].copy_from_slice(&a.to_le_bytes());
    v[8..].copy_from_slice(&b.to_le_bytes());
    v
}

/// Which block holds `key`.
pub fn block_of(key: u64) -> u64 {
    key / ENTRIES_PER_BLOCK
}

/// Materialize the bytes of a block: ENTRIES_PER_BLOCK sorted (key, value)
/// entries. This stands in for an SST data block (topic 3/4 built the real
/// format; here the block is just the unit of caching and fetch).
pub fn block_bytes(block: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(BLOCK_SIZE);
    let first = block * ENTRIES_PER_BLOCK;
    for key in first..first + ENTRIES_PER_BLOCK {
        out.extend_from_slice(&key.to_le_bytes());
        out.extend_from_slice(&value_for(key));
    }
    out
}

/// Binary search a block's entries for `key`.
pub fn lookup_in_block(block_data: &[u8], key: u64) -> Option<[u8; 16]> {
    let n = block_data.len() / ENTRY_SIZE;
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let off = mid * ENTRY_SIZE;
        let k = u64::from_le_bytes(block_data[off..off + 8].try_into().unwrap());
        if k < key {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo < n {
        let off = lo * ENTRY_SIZE;
        let k = u64::from_le_bytes(block_data[off..off + 8].try_into().unwrap());
        if k == key {
            return Some(block_data[off + 8..off + 24].try_into().unwrap());
        }
    }
    None
}

/// A latency model samples the simulated cost of one GET.
pub trait LatencyModel {
    fn sample_micros(&mut self, bytes: usize) -> u64;
}

/// Local NVMe-ish tier: ~90-120 µs first byte + 4 GB/s transfer.
pub struct LocalDisk {
    rng: ChaCha8Rng,
}

impl LocalDisk {
    pub fn new(seed: u64) -> Self {
        Self { rng: ChaCha8Rng::seed_from_u64(seed) }
    }
}

impl LatencyModel for LocalDisk {
    fn sample_micros(&mut self, bytes: usize) -> u64 {
        let first_byte = 90 + self.rng.gen_range(0..30);
        let transfer = bytes as u64 / 4096; // 4 GB/s ~= 4096 B/µs
        first_byte + transfer
    }
}

/// S3-ish tier: lognormal first byte (median ~14 ms), 80 MB/s transfer,
/// `straggler_prob` chance of an ~8x straggler (dominates p99).
pub struct S3 {
    rng: ChaCha8Rng,
    pub straggler_prob: f64,
}

impl S3 {
    pub fn new(seed: u64) -> Self {
        Self { rng: ChaCha8Rng::seed_from_u64(seed), straggler_prob: 0.02 }
    }
    pub fn with_stragglers(seed: u64, straggler_prob: f64) -> Self {
        Self { rng: ChaCha8Rng::seed_from_u64(seed), straggler_prob }
    }
}

impl LatencyModel for S3 {
    fn sample_micros(&mut self, bytes: usize) -> u64 {
        // lognormal: exp(N(ln 14000, 0.35))
        let z = normal_sample(&mut self.rng);
        let mut first_byte = (14_000.0f64.ln() + 0.35 * z).exp();
        if self.rng.gen_bool(self.straggler_prob) {
            first_byte *= 8.0;
        }
        let transfer = bytes as f64 / 80.0; // 80 MB/s ~= 80 B/µs
        (first_byte + transfer) as u64
    }
}

/// Scripted latencies for deterministic contract tests (cycles).
pub struct Fixed {
    script: Vec<u64>,
    at: usize,
}

impl Fixed {
    pub fn new(script: Vec<u64>) -> Self {
        assert!(!script.is_empty());
        Self { script, at: 0 }
    }
}

impl LatencyModel for Fixed {
    fn sample_micros(&mut self, _bytes: usize) -> u64 {
        let v = self.script[self.at % self.script.len()];
        self.at += 1;
        v
    }
}

fn normal_sample(rng: &mut ChaCha8Rng) -> f64 {
    // Box-Muller
    let u1: f64 = rng.gen_range(f64::EPSILON..1.0);
    let u2: f64 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// A block-addressed remote store: GET = materialize block + charge latency.
pub struct BlockStore<L: LatencyModel> {
    pub latency: L,
    pub gets: u64,
    pub bytes_fetched: u64,
}

impl<L: LatencyModel> BlockStore<L> {
    pub fn new(latency: L) -> Self {
        Self { latency, gets: 0, bytes_fetched: 0 }
    }

    pub fn get(&mut self, block: u64) -> (Vec<u8>, u64) {
        self.gets += 1;
        let data = block_bytes(block);
        self.bytes_fetched += data.len() as u64;
        let micros = self.latency.sample_micros(data.len());
        (data, micros)
    }
}

/// p in [0,1]; input must be sorted ascending.
pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    assert!(!sorted.is_empty());
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// Zipfian sampler over [0, n) via precomputed CDF + binary search.
pub struct Zipf {
    cdf: Vec<f64>,
    rng: ChaCha8Rng,
}

impl Zipf {
    pub fn new(n: usize, theta: f64, seed: u64) -> Self {
        let mut cdf = Vec::with_capacity(n);
        let mut acc = 0.0f64;
        for i in 0..n {
            acc += 1.0 / ((i + 1) as f64).powf(theta);
            cdf.push(acc);
        }
        let total = acc;
        for v in cdf.iter_mut() {
            *v /= total;
        }
        Self { cdf, rng: ChaCha8Rng::seed_from_u64(seed) }
    }

    pub fn sample(&mut self) -> u64 {
        let u: f64 = self.rng.gen();
        self.cdf.partition_point(|&c| c < u) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_layout_roundtrip() {
        let block = 123;
        let data = block_bytes(block);
        assert_eq!(data.len(), ENTRIES_PER_BLOCK as usize * ENTRY_SIZE);
        let first = block * ENTRIES_PER_BLOCK;
        for key in first..first + ENTRIES_PER_BLOCK {
            assert_eq!(lookup_in_block(&data, key), Some(value_for(key)));
            assert_eq!(block_of(key), block);
        }
        assert_eq!(lookup_in_block(&data, first + ENTRIES_PER_BLOCK), None);
    }

    #[test]
    fn s3_latency_shape_and_determinism() {
        let mut a: Vec<u64> = {
            let mut s3 = S3::new(7);
            (0..20_000).map(|_| s3.sample_micros(BLOCK_SIZE)).collect()
        };
        let b: Vec<u64> = {
            let mut s3 = S3::new(7);
            (0..20_000).map(|_| s3.sample_micros(BLOCK_SIZE)).collect()
        };
        assert_eq!(a, b, "same seed must give same latencies");
        a.sort_unstable();
        let p50 = percentile(&a, 0.50);
        let p99 = percentile(&a, 0.99);
        assert!((10_000..20_000).contains(&p50), "p50 {p50} outside 10-20ms");
        assert!(p99 > 3 * p50, "stragglers should stretch the tail (p99 {p99})");
    }

    #[test]
    fn local_disk_is_two_orders_faster() {
        let mut local = LocalDisk::new(7);
        let mut s3 = S3::new(7);
        let l: u64 = (0..1000).map(|_| local.sample_micros(BLOCK_SIZE)).sum();
        let s: u64 = (0..1000).map(|_| s3.sample_micros(BLOCK_SIZE)).sum();
        assert!(s > 50 * l, "S3 {s} vs local {l}: ladder collapsed");
    }

    #[test]
    fn zipf_is_skewed() {
        let n = 100_000;
        let mut z = Zipf::new(n, 0.99, 42);
        let mut hot = 0u64;
        let samples = 50_000;
        for _ in 0..samples {
            if z.sample() < (n / 100) as u64 {
                hot += 1;
            }
        }
        // top 1% of keys should draw well over 20% of accesses at theta=.99
        assert!(hot as f64 / samples as f64 > 0.2, "hot share {hot}/{samples}");
    }

    #[test]
    fn percentile_endpoints() {
        let v = vec![1, 2, 3, 4, 5];
        assert_eq!(percentile(&v, 0.0), 1);
        assert_eq!(percentile(&v, 0.5), 3);
        assert_eq!(percentile(&v, 1.0), 5);
    }
}
