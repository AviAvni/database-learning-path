//! STUB 1 — the Gorilla codec (VLDB '15 §4.1): delta-of-delta timestamps
//! + XOR floats. Facebook measured 1.37 bytes/sample on real metrics vs
//! 16 raw. Prometheus ships the same idea in tsdb/chunkenc/xor.go (Append
//! :161, dod buckets :195-208, writeVDelta :226) with different bucket
//! sizes; we use the paper's.
//!
//! Layout this stub must produce (BitWriter is provided in bits.rs):
//!
//!   header: t0 as 64 bits, v0 as 64 bits.
//!   each subsequent sample:
//!     TIMESTAMP  dod = (t - prev_t) - prev_delta   (prev_delta starts 0)
//!       dod == 0                -> '0'
//!       dod in [-63, 64]        -> '10'   + 7-bit dod
//!       dod in [-255, 256]      -> '110'  + 9-bit dod
//!       dod in [-2047, 2048]    -> '1110' + 12-bit dod
//!       else                    -> '1111' + 64-bit dod
//!       The bucket ranges are [-63, 64], [-255, 256], [-2047, 2048]: the
//!       positive end is one larger than two's complement allows, so the
//!       n-bit field holds `dod` itself for dod >= 0 and `dod + 2^n` for
//!       dod < 0, and the reader undoes it with
//!       `if v > 2^(n-1) { v - 2^n } else { v }` (prometheus xor.go does
//!       the same; bits.rs `sign_extend` would read a dod of 64 as -64).
//!     VALUE  xor = v.to_bits() ^ prev_v.to_bits()
//!       xor == 0 -> '0'
//!       else '1', then:
//!         if leading_zeros >= prev_leading && trailing_zeros >= prev_trailing
//!            (and a previous window exists)
//!           -> '0' + the middle (64 - prev_leading - prev_trailing) bits
//!         else
//!           -> '1' + 5-bit leading + 6-bit length + the meaningful bits
//!              (length is 1..=64; store 64 as 0 — the classic wrinkle)
//!              and this becomes the new reuse window
//!
//! Caller guarantees in-order timestamps (head.rs owns out-of-order).

use crate::bits::{BitReader, BitWriter};

pub struct GorillaBlock {
    pub bytes: Vec<u8>,
    pub count: usize,
}

pub struct GorillaEncoder {
    pub w: BitWriter,
    pub count: usize,
    pub prev_t: i64,
    pub prev_delta: i64,
    pub prev_v: u64,
    pub prev_leading: u8,
    pub prev_trailing: u8,
    pub window_set: bool,
}

impl GorillaEncoder {
    pub fn new() -> Self {
        Self {
            w: BitWriter::new(),
            count: 0,
            prev_t: 0,
            prev_delta: 0,
            prev_v: 0,
            prev_leading: 0,
            prev_trailing: 0,
            window_set: false,
        }
    }

    pub fn append(&mut self, _t: i64, _v: f64) {
        todo!("stub: gorilla append (dod buckets + xor float)")
    }

    pub fn finish(self) -> GorillaBlock {
        GorillaBlock { bytes: self.w.finish(), count: self.count }
    }
}

impl Default for GorillaEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode a block back to (t, v) pairs. Must be the exact inverse of the
/// encoder for every f64 bit pattern (NaNs included — compare via to_bits).
pub fn decode(block: &GorillaBlock) -> Vec<(i64, f64)> {
    let mut _r = BitReader::new(&block.bytes);
    todo!("stub: gorilla decode")
}

pub fn encode_all(ts: &[i64], vs: &[f64]) -> GorillaBlock {
    let mut e = GorillaEncoder::new();
    for (&t, &v) in ts.iter().zip(vs) {
        e.append(t, v);
    }
    e.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::*;

    fn roundtrip(ts: &[i64], vs: &[f64]) -> GorillaBlock {
        let b = encode_all(ts, vs);
        let out = decode(&b);
        assert_eq!(out.len(), ts.len());
        for (i, (t, v)) in out.iter().enumerate() {
            assert_eq!(*t, ts[i], "timestamp {i}");
            assert_eq!(v.to_bits(), vs[i].to_bits(), "value {i}");
        }
        b
    }

    #[test]
    fn roundtrip_gauge_with_jitter() {
        let ts = scrape_timestamps(50_000, 1_700_000_000_000, 10_000, 100, 1);
        let vs = gauge_values(50_000, 2);
        roundtrip(&ts, &vs);
    }

    #[test]
    fn roundtrip_survives_full_entropy_values() {
        let ts = scrape_timestamps(10_000, 0, 15_000, 5_000, 3);
        let vs = random_values(10_000, 4);
        roundtrip(&ts, &vs);
    }

    #[test]
    fn roundtrip_bucket_boundaries() {
        // deltas engineered to hit every dod bucket incl. exact edges
        let mut ts = vec![0i64];
        for d in [
            10_000, 10_000, 10_064, 10_001, 9_938, 10_194, 9_745, 12_048, 7_953, 100_000,
            1_i64 << 30,
        ] {
            ts.push(ts.last().unwrap() + d);
        }
        let vs: Vec<f64> = (0..ts.len()).map(|i| i as f64 * 0.1).collect();
        roundtrip(&ts, &vs);
    }

    #[test]
    fn constant_series_costs_under_a_byte_per_sample() {
        let ts = scrape_timestamps(10_000, 0, 10_000, 0, 5);
        let vs = constant_values(10_000);
        let b = roundtrip(&ts, &vs);
        // steady state: 1 bit dod + 1 bit xor = 0.25 B/sample
        assert!(
            b.bytes.len() < 10_000,
            "regular ts + constant value must be ~2 bits/sample, got {} bytes",
            b.bytes.len()
        );
    }

    #[test]
    fn counter_beats_raw_by_3x() {
        // integer-valued doubles: the XOR of neighbours has ~40 trailing
        // zeros, so the reuse window is narrow and cheap
        let ts = scrape_timestamps(50_000, 0, 10_000, 0, 6);
        let vs = counter_values(50_000, 7);
        let b = roundtrip(&ts, &vs);
        let bps = b.bytes.len() as f64 / 50_000.0;
        assert!(bps < 16.0 / 3.0, "counter bytes/sample {bps:.2} not < 5.33");
    }

    #[test]
    fn gauge_from_f64_arithmetic_has_no_free_lunch() {
        // a random walk accumulates rounding error, so its mantissa bits are
        // full-entropy even though consecutive values are numerically close:
        // XOR saves the shared sign/exponent/top bits and nothing else.
        let ts = scrape_timestamps(50_000, 0, 10_000, 0, 6);
        let vs = gauge_values(50_000, 7);
        let b = roundtrip(&ts, &vs);
        let bps = b.bytes.len() as f64 / 50_000.0;
        assert!(bps > 7.0, "gauge bytes/sample {bps:.2}: the mantissa is incompressible");
        assert!(bps < 16.0 / 1.5, "gauge bytes/sample {bps:.2}: should still beat raw by a little");
    }

    #[test]
    fn random_values_hit_the_entropy_floor() {
        // XOR can't compress full-entropy mantissas. 52 mantissa bits plus
        // the shared-exponent saving and a reuse window that sticks open at
        // ~57 meaningful bits: expect ~7.4 B/sample, NOT below ~6.5 and
        // nowhere near the 3x of the counter (the honest lesson: Gorilla wins
        // on REGULARITY, not magic).
        let ts = scrape_timestamps(10_000, 0, 10_000, 0, 8);
        let vs = random_values(10_000, 9);
        let b = roundtrip(&ts, &vs);
        let bps = b.bytes.len() as f64 / 10_000.0;
        assert!(bps > 6.5, "random values can't compress far below raw f64: {bps:.2}");
        assert!(bps < 9.0, "overhead cap: {bps:.2}");
    }
}
