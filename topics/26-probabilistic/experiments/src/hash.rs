//! Hashing infrastructure shared by every filter/sketch here (provided).
//! splitmix64 is the standard cheap-and-good 64-bit finalizer; all filter
//! math downstream assumes the output bits are uniform and independent.

pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Two independent-ish 32-bit hashes from one 64-bit key (Kirsch-Mitzenmacher
/// double hashing: probe_i = h1 + i*h2 covers k probes from two hashes).
pub fn hash2(key: u64) -> (u32, u32) {
    let h = splitmix64(key);
    (h as u32, (h >> 32) as u32)
}

/// Map a 32-bit hash onto [0, n) without modulo (Lemire fastrange —
/// the trick rocksdb bloom_impl.h:117 uses to pick the cache line).
#[inline]
pub fn fastrange32(h: u32, n: u32) -> u32 {
    ((h as u64 * n as u64) >> 32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_is_deterministic_and_spreads() {
        assert_eq!(splitmix64(0), splitmix64(0));
        assert_ne!(splitmix64(1), splitmix64(2));
        // avalanche smoke test: flipping one input bit flips ~half the output
        let a = splitmix64(0x1234);
        let b = splitmix64(0x1235);
        let flipped = (a ^ b).count_ones();
        assert!((16..=48).contains(&flipped), "flipped {}", flipped);
    }

    #[test]
    fn fastrange_in_bounds_and_covers() {
        let n = 1000u32;
        let mut seen0 = false;
        let mut seen_hi = false;
        for k in 0..100_000u64 {
            let r = fastrange32(hash2(k).0, n);
            assert!(r < n);
            seen0 |= r == 0;
            seen_hi |= r == n - 1;
        }
        assert!(seen0 && seen_hi);
    }
}
