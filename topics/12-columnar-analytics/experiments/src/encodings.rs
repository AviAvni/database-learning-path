//! YOU implement: the three lightweight encodings.
//!
//! Contract notes (the tests pin these down):
//! - round-trip: decode(encode(v)) == v for all three
//! - sizes are EXACT (tests compute them) — no headers beyond what the
//!   structs carry, no padding games
//! - `Rle::sum` operates ON the encoding: value * run_length, no decode
//!   (SIGMOD '06's whole point — scan_bench measures the payoff)
//! - bit-packing must handle width 0 (all values equal min) and
//!   non-multiple-of-8 widths; values are packed LSB-first into u64
//!   words (like DuckDB/Parquet, easier to SIMD later)

/// Run-length encoding: (value, run_length) pairs.
pub struct Rle {
    pub runs: Vec<(u64, u32)>,
}

impl Rle {
    pub fn encode(values: &[u64]) -> Rle {
        let _ = values;
        todo!()
    }

    pub fn decode(&self) -> Vec<u64> {
        todo!()
    }

    /// Sum WITHOUT decoding: one multiply-add per RUN.
    pub fn sum(&self) -> u64 {
        todo!()
    }

    /// Encoded size in bytes (12 per run: 8 value + 4 count).
    pub fn size_bytes(&self) -> usize {
        self.runs.len() * 12
    }
}

/// Dictionary encoding: distinct values + per-row codes.
/// Codes are u32 here for simplicity (bit-pack them in `DictPacked`
/// if you want the full cascade — optional stretch).
pub struct Dict {
    pub dict: Vec<u64>,   // sorted distinct values
    pub codes: Vec<u32>,  // index into dict, one per row
}

impl Dict {
    pub fn encode(values: &[u64]) -> Dict {
        let _ = values;
        todo!()
    }

    pub fn decode(&self) -> Vec<u64> {
        todo!()
    }

    /// Encoded size in bytes: dict entries * 8 + codes * 4.
    pub fn size_bytes(&self) -> usize {
        self.dict.len() * 8 + self.codes.len() * 4
    }
}

/// Frame-of-reference + bit-packing: store min, pack (value - min) in
/// the minimum number of bits.
pub struct BitPacked {
    pub min: u64,
    pub width: u32,      // bits per value, 0..=64
    pub len: usize,      // number of values
    pub words: Vec<u64>, // packed data, LSB-first
}

impl BitPacked {
    pub fn encode(values: &[u64]) -> BitPacked {
        let _ = values;
        todo!()
    }

    pub fn decode(&self) -> Vec<u64> {
        todo!()
    }

    /// Random access — the fetch_row requirement that shapes real
    /// encoder menus. Must be O(1).
    pub fn get(&self, i: usize) -> u64 {
        let _ = i;
        todo!()
    }

    pub fn size_bytes(&self) -> usize {
        8 + self.words.len() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data;

    #[test]
    fn rle_round_trip_and_size() {
        let v = data::sorted_low_cardinality(100_000, 1);
        let e = Rle::encode(&v);
        assert_eq!(e.decode(), v);
        // runs must be MAXIMAL (adjacent equal values merged)
        for w in e.runs.windows(2) {
            assert_ne!(w[0].0, w[1].0, "adjacent runs with equal value");
        }
        assert!(e.size_bytes() < v.len() * 8 / 50, "sorted data must compress >50x");
    }

    #[test]
    fn rle_sum_without_decode() {
        let v = data::sorted_low_cardinality(50_000, 2);
        let e = Rle::encode(&v);
        assert_eq!(e.sum(), v.iter().sum::<u64>());
    }

    #[test]
    fn dict_round_trip_sorted_dedup() {
        let v = data::shuffled_low_cardinality(100_000, 3);
        let e = Dict::encode(&v);
        assert_eq!(e.decode(), v);
        assert_eq!(e.dict.len(), 64);
        assert!(e.dict.windows(2).all(|w| w[0] < w[1]), "dict must be sorted+deduped");
    }

    #[test]
    fn bitpacked_round_trip_and_width() {
        let v = data::small_range_random(100_000, 4);
        let e = BitPacked::encode(&v);
        assert_eq!(e.decode(), v);
        assert_eq!(e.width, 12, "0..4096 needs exactly 12 bits");
        // 100_000 * 12 bits = 150_000 bytes (+8 header, + last-word slack)
        assert!(e.size_bytes() <= 150_016);
    }

    #[test]
    fn bitpacked_frame_of_reference() {
        // clustered around 1e12: FOR must shrink width to the RANGE
        let v: Vec<u64> = (0..10_000u64).map(|i| 1_000_000_000_000 + (i % 100)).collect();
        let e = BitPacked::encode(&v);
        assert_eq!(e.min, 1_000_000_000_000);
        assert_eq!(e.width, 7, "range 0..100 needs 7 bits");
        assert_eq!(e.decode(), v);
    }

    #[test]
    fn bitpacked_constant_column_is_width_zero() {
        let v = vec![42u64; 1000];
        let e = BitPacked::encode(&v);
        assert_eq!(e.width, 0);
        assert_eq!(e.decode(), v);
        assert_eq!(e.get(500), 42);
    }

    #[test]
    fn bitpacked_random_access() {
        let v = data::small_range_random(10_000, 5);
        let e = BitPacked::encode(&v);
        for &i in &[0, 1, 63, 64, 65, 4095, 9999] {
            assert_eq!(e.get(i), v[i], "get({i})");
        }
    }

    #[test]
    fn empty_and_single() {
        assert_eq!(Rle::encode(&[]).decode(), Vec::<u64>::new());
        assert_eq!(Dict::encode(&[7]).decode(), vec![7]);
        let e = BitPacked::encode(&[7]);
        assert_eq!(e.decode(), vec![7]);
    }
}
