//! A PGM-style static learned index: piecewise-linear approximation of the
//! key -> position function with a hard error bound epsilon, so a lookup is
//! "predict, then binary-search a 2*eps+2 window". Reference:
//! PGM-index pgm_index.hpp:67 (search via segment, PGM_SUB/ADD_EPS at
//! :32-33 bound the window) and piecewise_linear_model.hpp:45 (the optimal
//! streaming PLA). Our stub uses the simpler shrinking-cone greedy PLA —
//! same guarantee, more segments than optimal, O(n) build.

pub struct Segment {
    pub first_key: u64,
    pub slope: f64,
    pub intercept: f64, // predicted pos for key k: slope * (k - first_key) + intercept
}

pub struct LearnedIndex {
    pub segments: Vec<Segment>,
    pub epsilon: usize,
    pub n: usize,
}

impl LearnedIndex {
    /// STUB — shrinking-cone greedy PLA over sorted, deduped keys:
    /// open a segment at (k0, pos0) with slope cone (lo, hi) = (0, inf);
    /// for each next point (k, pos), the segment can keep it iff some
    /// slope in the cone predicts pos within eps — narrow the cone to
    ///   lo = max(lo, (pos - eps - pos0) / (k - k0))
    ///   hi = min(hi, (pos + eps - pos0) / (k - k0))
    /// and close the segment (emit slope = (lo+hi)/2) when the cone
    /// empties, starting a fresh one at (k, pos).
    pub fn build(_keys: &[u64], _epsilon: usize) -> LearnedIndex {
        todo!("greedy shrinking-cone segmentation")
    }

    /// STUB — binary-search segments by first_key (or hold a small
    /// fanout array), predict pos, clamp, return the window
    /// [pos - eps, pos + eps + 2) intersected with [0, n).
    pub fn search_window(&self, _key: u64) -> (usize, usize) {
        todo!("predict then widen by epsilon")
    }

    /// Provided — the full lookup: predicted window + binary search in it.
    pub fn lookup(&self, keys: &[u64], key: u64) -> Option<usize> {
        let (lo, hi) = self.search_window(key);
        keys[lo..hi].binary_search(&key).ok().map(|i| lo + i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    fn sorted_keys(n: usize, seed: u64) -> Vec<u64> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut keys: Vec<u64> = (0..n).map(|_| rng.gen()).collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    #[test]
    fn every_key_found_and_window_respects_epsilon() {
        let keys = sorted_keys(200_000, 42);
        let eps = 64;
        let idx = LearnedIndex::build(&keys, eps);
        for (i, &k) in keys.iter().enumerate().step_by(37) {
            let (lo, hi) = idx.search_window(k);
            assert!(hi - lo <= 2 * eps + 2, "window {} too wide", hi - lo);
            assert!(lo <= i && i < hi, "true pos {} outside [{}, {})", i, lo, hi);
            assert_eq!(idx.lookup(&keys, k), Some(i));
        }
    }

    #[test]
    fn absent_keys_return_none() {
        let keys: Vec<u64> = (0..100_000).map(|i| i * 2).collect();
        let idx = LearnedIndex::build(&keys, 32);
        for k in (0..100_000u64).step_by(101) {
            assert_eq!(idx.lookup(&keys, k * 2 + 1), None);
        }
    }

    // Uniform-random keys are globally near-linear: the whole point of
    // learned indexes is that segments << n. The greedy cone gets well
    // under n/eps on uniform data.
    #[test]
    fn uniform_data_compresses_hard() {
        let keys = sorted_keys(1_000_000, 7);
        let idx = LearnedIndex::build(&keys, 64);
        assert!(
            idx.segments.len() < keys.len() / 500,
            "{} segments for {} keys",
            idx.segments.len(),
            keys.len()
        );
    }

    // A hard distribution: exponentially spaced keys break linearity —
    // MORE segments, but the epsilon guarantee must still hold.
    #[test]
    fn epsilon_holds_on_hostile_distribution() {
        let mut keys: Vec<u64> = (0..64u32).map(|i| 1u64 << (i % 63)).collect();
        keys.extend((0..10_000u64).map(|i| i * i * 31));
        keys.sort_unstable();
        keys.dedup();
        let idx = LearnedIndex::build(&keys, 16);
        for (i, &k) in keys.iter().enumerate() {
            let (lo, hi) = idx.search_window(k);
            assert!(lo <= i && i < hi);
            assert!(hi - lo <= 34);
        }
    }
}
