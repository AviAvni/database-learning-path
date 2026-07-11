//! Posting-list set operations: sorted `Vec<u32>` (PROVIDED oracle)
//! vs a miniature Roaring bitmap (STUB).
//!
//! Roaring (arXiv:1603.06549): partition the u32 space by the high
//! 16 bits; each 64K-value chunk gets a container — a sorted
//! `Vec<u16>` when sparse (≤4096 values), a 1024×u64 bitmap when
//! dense. AND/OR pick a kernel per container pair. CRoaring/roaring-rs
//! add a third "run" container; we skip it.

/// two-pointer intersection — the oracle
pub fn vec_and(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// two-pointer union — the oracle
pub fn vec_or(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// sparse↔dense switch point: 4096 u16s = 8 KiB = the bitmap's size,
/// so a container never exceeds 8 KiB
pub const ARRAY_MAX: usize = 4096;

pub enum Container {
    Array(Vec<u16>),          // sorted, len <= ARRAY_MAX
    Bitmap(Box<[u64; 1024]>), // 65536 bits
}

pub struct Roaring {
    /// (high 16 bits, container), sorted by key
    pub containers: Vec<(u16, Container)>,
}

impl Roaring {
    /// STUB: split by high 16 bits; chunks with > ARRAY_MAX values
    /// become bitmaps, the rest sorted u16 arrays.
    pub fn from_sorted(_vals: &[u32]) -> Self {
        todo!("build containers from a sorted, deduplicated slice")
    }

    /// STUB: intersect matching keys only (galloping over the key
    /// vecs); kernels: array∩array = two-pointer, bitmap∩bitmap =
    /// 1024 u64 ANDs, array∩bitmap = probe each u16 into the bitmap.
    /// Return sorted u32s.
    pub fn and(&self, _other: &Roaring) -> Vec<u32> {
        todo!("per-container-pair AND kernels")
    }

    /// STUB: union of keys; kernels mirror `and` (bitmap∪array =
    /// clone bitmap + set bits). Return sorted u32s.
    pub fn or(&self, _other: &Roaring) -> Vec<u32> {
        todo!("per-container-pair OR kernels")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    fn gen_set(n: usize, universe: u32, seed: u64) -> Vec<u32> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut v: Vec<u32> = (0..n).map(|_| rng.gen_range(0..universe)).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn vec_oracle_sanity() {
        assert_eq!(vec_and(&[1, 3, 5], &[3, 5, 9]), vec![3, 5]);
        assert_eq!(vec_or(&[1, 3], &[2, 3, 9]), vec![1, 2, 3, 9]);
    }

    #[test]
    fn roaring_matches_oracle_sparse() {
        let a = gen_set(2_000, 10_000_000, 1); // ~0.02% dense: array containers
        let b = gen_set(2_000, 10_000_000, 2);
        let (ra, rb) = (Roaring::from_sorted(&a), Roaring::from_sorted(&b));
        assert_eq!(ra.and(&rb), vec_and(&a, &b));
        assert_eq!(ra.or(&rb), vec_or(&a, &b));
    }

    #[test]
    fn roaring_matches_oracle_dense() {
        let a = gen_set(300_000, 1_000_000, 3); // ~30% dense: bitmap containers
        let b = gen_set(300_000, 1_000_000, 4);
        let (ra, rb) = (Roaring::from_sorted(&a), Roaring::from_sorted(&b));
        assert_eq!(ra.and(&rb), vec_and(&a, &b));
        assert_eq!(ra.or(&rb), vec_or(&a, &b));
    }

    #[test]
    fn roaring_matches_oracle_mixed_density() {
        let a = gen_set(200_000, 500_000, 5); // dense → bitmaps
        let b = gen_set(1_000, 500_000, 6); // sparse → arrays
        let (ra, rb) = (Roaring::from_sorted(&a), Roaring::from_sorted(&b));
        assert_eq!(ra.and(&rb), vec_and(&a, &b));
        assert_eq!(ra.or(&rb), vec_or(&a, &b));
        assert!(matches!(ra.containers[0].1, Container::Bitmap(_)));
        assert!(matches!(rb.containers[0].1, Container::Array(_)));
    }
}
