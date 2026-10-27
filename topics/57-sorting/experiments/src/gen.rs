//! Seeded workload generators. Every count and checksum reproduces exactly;
//! only timings vary between machines.

use crate::collation::accented_forms;
use crate::{IntStr, Row64, StrRef, StrSet};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub const SEED: u64 = 57;

/// Uniform random 64-bit keys. `payload` is the original position.
pub fn gen_u64(n: usize, seed: u64) -> Vec<Row64> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n).map(|i| Row64 { key: rng.gen(), payload: i as u64 }).collect()
}

/// Which strings the `(int, string)` workload carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrShape {
    /// 8–24 random lowercase letters: strings differ in the first bytes.
    Random,
    /// A fixed 20-byte prefix, then 4–12 random lowercase letters: the first
    /// 20 bytes of every string are identical.
    SharedPrefix20,
}

pub const SHARED_PREFIX: &[u8; 20] = b"warehouse/region-eu/";

pub struct IntStrData {
    pub rows: Vec<IntStr>,
    pub arena: Vec<u8>,
}

/// `n` rows of (int, string). `a` is drawn uniformly from `distinct` values
/// centred on zero (so about half are negative); `distinct == 0` means the
/// whole `i32` range.
pub fn gen_int_str(n: usize, distinct: u32, shape: StrShape, seed: u64) -> IntStrData {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut arena: Vec<u8> = Vec::with_capacity(n * 20);
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let a: i32 = if distinct == 0 {
            rng.gen()
        } else {
            rng.gen_range(0..distinct as i64) as i32 - (distinct / 2) as i32
        };
        let off = arena.len() as u32;
        match shape {
            StrShape::Random => {
                let len = rng.gen_range(8..=24);
                for _ in 0..len {
                    arena.push(b'a' + rng.gen_range(0..26u8));
                }
            }
            StrShape::SharedPrefix20 => {
                arena.extend_from_slice(SHARED_PREFIX);
                let len = rng.gen_range(4..=12);
                for _ in 0..len {
                    arena.push(b'a' + rng.gen_range(0..26u8));
                }
            }
        }
        rows.push(IntStr { a, off, len: arena.len() as u32 - off });
    }
    IntStrData { rows, arena }
}

/// Strings for the collation lane. `n / 4` base words of 6–20 letters
/// are drawn first; each row picks a base uniformly and re-spells it: a, c,
/// e, i, n, o, u are replaced by a random accented form (Latin-1 range, two
/// bytes in UTF-8) with probability 1/6, then every letter is upper-cased
/// with probability 1/4. Rows that picked the same base therefore tie at the
/// first collation level and differ at the second or third — the case that
/// makes a collation expensive.
pub fn gen_collation_strings(n: usize, seed: u64) -> StrSet {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let nbases = (n / 4).max(1);
    let bases: Vec<Vec<u8>> = (0..nbases)
        .map(|_| {
            let len = rng.gen_range(6..=20);
            (0..len).map(|_| b'a' + rng.gen_range(0..26u8)).collect()
        })
        .collect();
    let mut bytes: Vec<u8> = Vec::with_capacity(n * 16);
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let base = &bases[rng.gen_range(0..nbases)];
        let off = bytes.len() as u32;
        let mut buf = [0u8; 4];
        for &b in base {
            let mut c = b as char;
            let forms = accented_forms(c);
            if !forms.is_empty() && rng.gen_range(0..6) == 0 {
                c = forms[rng.gen_range(0..forms.len())];
            }
            if rng.gen_range(0..4) == 0 {
                c = c.to_uppercase().next().unwrap();
            }
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        rows.push(StrRef { off, len: bytes.len() as u32 - off });
    }
    StrSet::new(bytes, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_are_seeded() {
        assert_eq!(gen_u64(1000, 1), gen_u64(1000, 1));
        assert_ne!(gen_u64(1000, 1), gen_u64(1000, 2));
        let a = gen_int_str(500, 100, StrShape::Random, 3);
        let b = gen_int_str(500, 100, StrShape::Random, 3);
        assert_eq!(a.rows, b.rows);
        assert_eq!(a.arena, b.arena);
        let s = gen_collation_strings(400, 5);
        let t = gen_collation_strings(400, 5);
        assert_eq!(s.bytes(), t.bytes());
    }

    #[test]
    fn shared_prefix_rows_share_twenty_bytes() {
        let d = gen_int_str(200, 10, StrShape::SharedPrefix20, 9);
        assert!(d.rows.iter().all(|r| r.string(&d.arena).starts_with(SHARED_PREFIX)));
    }

    #[test]
    fn collation_strings_are_utf8_and_mixed() {
        let s = gen_collation_strings(2000, 5);
        let mut non_ascii = 0;
        let mut upper = 0;
        for r in s.rows() {
            let t = s.get(*r);
            non_ascii += !t.is_ascii() as usize;
            upper += t.chars().any(|c| c.is_uppercase()) as usize;
        }
        assert!(non_ascii > 500 && upper > 1500, "{non_ascii} {upper}");
    }
}
