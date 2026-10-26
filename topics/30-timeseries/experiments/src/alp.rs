//! ALP-style float codec — PROVIDED. The decimal-to-integer family of float
//! compression (ALP, Afroozeh/Kuffo/Boncz SIGMOD '24), the opposite bet to
//! Gorilla's XOR family: instead of predicting the next *bit pattern*, find
//! the decimal the double came from, store it as an integer, and let
//! frame-of-reference + bit-packing do what they do on integers.
//!
//!   encode  d = round(v * 10^e * 10^-f)        (one (e, f) per vector)
//!   decode  v' = (d * 10^f) * 10^-e            (must equal v bit for bit,
//!                                               else v is an EXCEPTION)
//!
//! This is the scalar, single-level core of the algorithm, mirroring
//! ALP's `encoder.hpp` / DuckDB's `alp.hpp` (see reading-alp-chimp.md for
//! the line anchors). What it deliberately leaves out:
//!   - ALP's first-level sampling per row group (top-k combinations): here
//!     every vector searches all (e, f) pairs over its own 32 equidistant
//!     values. The paper reports the brute-force best improves the ratio by
//!     under 1% over the sampled search (section 4.2), so this costs encode
//!     speed, not ratio.
//!   - ALP_RD, the fallback for "real" doubles (a vector that is mostly
//!     exceptions). Here such a vector is stored raw, like DuckDB's
//!     uncompressed-vector mode (alp_compress.hpp:174).
//!   - SIMD / FastLanes layouts. The bit-unpack below is a plain loop.

pub const VECTOR: usize = 1024;
pub const SAMPLES: usize = 32;
pub const MAX_E: usize = 18;
/// 2^51 + 2^52: adding then subtracting it rounds a double to the nearest
/// integer without a branch (ALP's `fast_double_round`, paper section 3.1).
const MAGIC: f64 = 6_755_399_441_055_744.0;
/// Header bytes per compressed vector, as DuckDB lays it out
/// (alp_compress.hpp:125-140): exponent 1, factor 1, exception count 2,
/// frame of reference 8, bit width 1.
pub const HEADER_BYTES: usize = 13;
/// Per exception: the raw f64 (8 B) and its u16 position in the vector.
pub const EXCEPTION_BYTES: usize = 10;

const EXP10: [f64; MAX_E + 1] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18,
];
const FRAC10: [f64; MAX_E + 1] = [
    1e-0, 1e-1, 1e-2, 1e-3, 1e-4, 1e-5, 1e-6, 1e-7, 1e-8, 1e-9, 1e-10, 1e-11, 1e-12, 1e-13,
    1e-14, 1e-15, 1e-16, 1e-17, 1e-18,
];
const FACT10: [i64; MAX_E + 1] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];

/// v -> integer. Anything that cannot be an i64 (NaN, inf, huge) maps to
/// i64::MAX, which will fail the round trip and become an exception.
#[inline]
fn encode_value(v: f64, e: usize, f: usize) -> i64 {
    let t = v * EXP10[e] * FRAC10[f];
    if !(t.abs() < 9.2e18) {
        return i64::MAX;
    }
    ((t + MAGIC) - MAGIC) as i64
}

#[inline]
fn decode_value(d: i64, e: usize, f: usize) -> f64 {
    (d as f64 * FACT10[f] as f64) * FRAC10[e]
}

fn bits_for(range: u64) -> u8 {
    (64 - range.leading_zeros()) as u8
}

/// One encoded vector of up to 1024 values.
#[derive(Clone, Debug)]
pub enum AlpVector {
    /// Decimal path: ints = FOR + bit-packed, plus patched exceptions.
    Alp {
        e: u8,
        f: u8,
        bit_width: u8,
        base: i64,
        n: usize,
        packed: Vec<u8>,
        exc_vals: Vec<f64>,
        exc_pos: Vec<u16>,
    },
    /// The compressed form would not be smaller than 8 B/value.
    Raw(Vec<f64>),
}

impl AlpVector {
    /// Stored size in bytes, counted the way DuckDB's `RequiredSpace`
    /// does (alp.hpp:93-100): packed ints + exceptions + the header; a raw
    /// vector is 1 sentinel byte + 8 B/value.
    pub fn bytes(&self) -> usize {
        match self {
            AlpVector::Alp { n, bit_width, exc_vals, .. } => {
                (n * *bit_width as usize + 7) / 8 + exc_vals.len() * EXCEPTION_BYTES + HEADER_BYTES
            }
            AlpVector::Raw(v) => 1 + 8 * v.len(),
        }
    }

    pub fn n_exceptions(&self) -> usize {
        match self {
            AlpVector::Alp { exc_vals, .. } => exc_vals.len(),
            AlpVector::Raw(v) => v.len(),
        }
    }
}

/// Pick (e, f) for one vector from 32 equidistant samples: the pair that
/// minimises  n * bits(max - min) + exceptions * 80  (the cost model of
/// encoder.hpp:184-188). Ties go to the larger exponent, then larger factor
/// (the loops run downward and only a strict improvement replaces the
/// incumbent). Pairs with fewer than two successes are skipped
/// (encoder.hpp:182).
pub fn search_ef(vs: &[f64]) -> (usize, usize) {
    let step = (vs.len() / SAMPLES).max(1);
    let sample: Vec<f64> = vs.iter().step_by(step).copied().collect();
    let mut best = (0usize, 0usize);
    let mut best_cost = u64::MAX;
    for e in (0..=MAX_E).rev() {
        for f in (0..=e).rev() {
            let (mut lo, mut hi) = (i64::MAX, i64::MIN);
            let (mut ok, mut exc) = (0u64, 0u64);
            for &v in &sample {
                let d = encode_value(v, e, f);
                if decode_value(d, e, f).to_bits() == v.to_bits() {
                    ok += 1;
                    lo = lo.min(d);
                    hi = hi.max(d);
                } else {
                    exc += 1;
                }
            }
            if ok < 2 {
                continue;
            }
            let cost = sample.len() as u64 * bits_for(hi.wrapping_sub(lo) as u64) as u64 + exc * 80;
            if cost < best_cost {
                best_cost = cost;
                best = (e, f);
            }
        }
    }
    best
}

pub fn encode_vector(vs: &[f64]) -> AlpVector {
    let n = vs.len();
    let (e, f) = search_ef(vs);
    let mut ints = vec![0i64; n];
    let mut exc_pos: Vec<u16> = Vec::new();
    for (i, &v) in vs.iter().enumerate() {
        let d = encode_value(v, e, f);
        ints[i] = d;
        if decode_value(d, e, f).to_bits() != v.to_bits() {
            exc_pos.push(i as u16);
        }
    }
    // Exceptions get a placeholder (the first good integer) so they do not
    // stretch the frame of reference — ALP does the same, encoder.hpp:382-397.
    let placeholder = (0..n)
        .find(|i| exc_pos.binary_search(&(*i as u16)).is_err())
        .map(|i| ints[i])
        .unwrap_or(0);
    let exc_vals: Vec<f64> = exc_pos.iter().map(|&p| vs[p as usize]).collect();
    for &p in &exc_pos {
        ints[p as usize] = placeholder;
    }
    let (lo, hi) = ints.iter().fold((i64::MAX, i64::MIN), |(l, h), &d| (l.min(d), h.max(d)));
    let (lo, hi) = if n == 0 { (0, 0) } else { (lo, hi) };
    let bit_width = bits_for(hi.wrapping_sub(lo) as u64);
    let v = AlpVector::Alp {
        e: e as u8,
        f: f as u8,
        bit_width,
        base: lo,
        n,
        packed: pack(&ints, lo, bit_width),
        exc_vals,
        exc_pos,
    };
    if v.bytes() < 1 + 8 * n {
        v
    } else {
        AlpVector::Raw(vs.to_vec())
    }
}

/// LSB-first bit-packing of (d - base) at `w` bits each, padded by 16 bytes
/// so the unpacker can always load a u128.
fn pack(ints: &[i64], base: i64, w: u8) -> Vec<u8> {
    let w = w as usize;
    let mut out = vec![0u8; (ints.len() * w + 7) / 8 + 16];
    if w == 0 {
        return out;
    }
    for (i, &d) in ints.iter().enumerate() {
        let x = d.wrapping_sub(base) as u64 as u128;
        let bit = i * w;
        let (byte, sh) = (bit / 8, bit % 8);
        let mut cur = [0u8; 16];
        cur.copy_from_slice(&out[byte..byte + 16]);
        let v = u128::from_le_bytes(cur) | (x << sh);
        out[byte..byte + 16].copy_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn decode_vector(v: &AlpVector, out: &mut Vec<f64>) {
    match v {
        AlpVector::Raw(r) => out.extend_from_slice(r),
        AlpVector::Alp { e, f, bit_width, base, n, packed, exc_vals, exc_pos } => {
            let (e, f, w) = (*e as usize, *f as usize, *bit_width as usize);
            let start = out.len();
            let mask: u128 = if w == 64 { u64::MAX as u128 } else { (1u128 << w) - 1 };
            for i in 0..*n {
                let d = if w == 0 {
                    *base
                } else {
                    let bit = i * w;
                    let (byte, sh) = (bit / 8, bit % 8);
                    let mut cur = [0u8; 16];
                    cur.copy_from_slice(&packed[byte..byte + 16]);
                    let x = ((u128::from_le_bytes(cur) >> sh) & mask) as u64;
                    base.wrapping_add(x as i64)
                };
                out.push(decode_value(d, e, f));
            }
            for (val, &p) in exc_vals.iter().zip(exc_pos) {
                out[start + p as usize] = *val;
            }
        }
    }
}

/// A whole column, one `AlpVector` per 1024 values.
pub struct AlpColumn {
    pub vectors: Vec<AlpVector>,
    pub n: usize,
}

pub fn alp_encode(vs: &[f64]) -> AlpColumn {
    AlpColumn { vectors: vs.chunks(VECTOR).map(encode_vector).collect(), n: vs.len() }
}

pub fn alp_decode(c: &AlpColumn) -> Vec<f64> {
    let mut out = Vec::with_capacity(c.n);
    for v in &c.vectors {
        decode_vector(v, &mut out);
    }
    out
}

impl AlpColumn {
    pub fn bytes(&self) -> usize {
        self.vectors.iter().map(|v| v.bytes()).sum()
    }
    pub fn exceptions(&self) -> usize {
        self.vectors.iter().map(|v| v.n_exceptions()).sum()
    }
    pub fn raw_vectors(&self) -> usize {
        self.vectors.iter().filter(|v| matches!(v, AlpVector::Raw(_))).count()
    }
    /// Mean bit width over the vectors that took the decimal path.
    pub fn mean_bit_width(&self) -> f64 {
        let w: Vec<u8> = self
            .vectors
            .iter()
            .filter_map(|v| if let AlpVector::Alp { bit_width, .. } = v { Some(*bit_width) } else { None })
            .collect();
        if w.is_empty() {
            0.0
        } else {
            w.iter().map(|&x| x as f64).sum::<f64>() / w.len() as f64
        }
    }
    /// The (e, f) pair most vectors chose, and how many did.
    pub fn modal_ef(&self) -> Option<((u8, u8), usize)> {
        let mut m = std::collections::BTreeMap::new(); // BTreeMap: deterministic order
        for v in &self.vectors {
            if let AlpVector::Alp { e, f, .. } = v {
                *m.entry((*e, *f)).or_insert(0usize) += 1;
            }
        }
        m.into_iter().max_by_key(|&(k, c)| (c, std::cmp::Reverse(k)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::*;

    fn bitwise_equal(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    }

    #[test]
    fn paper_example_8_0605_needs_e14_f10_or_any_working_pair() {
        // section 2.5: 8.0605 is not exactly representable, and e = 4 alone
        // fails (P_dec gives 8.0605000000000011); the factor-f form fixes it.
        let v = 8.0605f64;
        let d4 = encode_value(v, 4, 0);
        assert_eq!(d4, 80605);
        assert_ne!(decode_value(d4, 4, 0).to_bits(), v.to_bits(), "e=4 must fail");
        let d = encode_value(v, 14, 10);
        assert_eq!(d, 80605);
        assert_eq!(decode_value(d, 14, 10).to_bits(), v.to_bits());
    }

    #[test]
    fn roundtrip_is_bit_exact_including_specials() {
        let n = 5_000;
        let mut vs = decimal_gauge_values(n, 1, 2);
        vs[0] = f64::NAN;
        vs[1] = -0.0;
        vs[2] = f64::INFINITY;
        vs[3] = f64::NEG_INFINITY;
        vs[4] = f64::from_bits(1);
        vs[5] = 1e300;
        vs[6] = -1e300;
        vs[1500] = f64::NAN;
        for data in [vs, constant_values(n), gauge_values(n, 2), counter_values(n, 3), random_values(n, 4)] {
            let c = alp_encode(&data);
            assert!(bitwise_equal(&data, &alp_decode(&c)));
        }
    }

    #[test]
    fn partial_last_vector_roundtrips() {
        for n in [1, 2, 31, 1023, 1024, 1025, 2049] {
            let vs = decimal_gauge_values(n, 9, 3);
            assert!(bitwise_equal(&vs, &alp_decode(&alp_encode(&vs))), "n={n}");
        }
    }

    #[test]
    fn decimals_compress_hard_and_random_does_not() {
        let n = 100_000;
        let per = |vs: &[f64]| alp_encode(vs).bytes() as f64 / n as f64;
        let dec2 = per(&decimal_gauge_values(n, 1, 2));
        let counter = per(&counter_values(n, 2));
        let konst = per(&constant_values(n));
        let random = per(&random_values(n, 3));
        assert!(dec2 < 2.5, "2-decimal values are ~14-bit integers: {dec2}");
        assert!(counter < 3.0, "integer-valued doubles: {counter}");
        assert!(konst < 0.05, "constant -> bit width 0: {konst}");
        assert!(random > 7.9, "full-entropy doubles fall back to raw: {random}");
        // the fallback never costs more than the 1-byte sentinel per vector
        assert!(random < 8.0 + 1.5 / VECTOR as f64,"{random}");
    }

    #[test]
    fn arithmetic_derived_values_are_not_decimals() {
        // a random walk accumulates f64 rounding: 17 significant digits,
        // nothing a power of ten can undo. ALP must say so (mostly
        // exceptions -> raw), not pretend.
        let n = 100_000;
        let c = alp_encode(&gauge_values(n, 5));
        assert!(c.raw_vectors() as f64 > 0.9 * c.vectors.len() as f64, "raw vectors {}", c.raw_vectors());
    }
}
