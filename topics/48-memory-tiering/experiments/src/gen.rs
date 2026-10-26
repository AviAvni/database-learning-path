//! The seeded access generator.
//!
//! One access = one object id. The workload is the "Hotspot" shape SIDLE uses
//! for its microbenchmarks (§6.1: "90% of requests concentrate on 5% of
//! contiguous keys"): with probability `p_hot` the id is drawn uniformly from
//! the hot set `[0, hot)`, otherwise uniformly from all `[0, n)`.
//!
//! The generator is a chain: the next draw is seeded by the previous state
//! XOR the payload *loaded from the object just visited*. In the timed loop
//! that payload comes out of memory, so each access genuinely depends on the
//! previous one (the CPU cannot overlap the misses); in the counting passes
//! it is recomputed as `payload(id)`. Because the payload is a function of
//! the object's id, not of where the object lives, the *same id sequence*
//! comes out of every layout — the layouts differ only in addresses.

pub const SEED: u64 = 0x5EED_0048;

/// SplitMix64 finaliser (Steele, Lea & Flood 2014). A bijection on u64.
#[inline(always)]
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The payload stored in an object's first word. Distinct per id, so a value
/// predictor has nothing to latch on to.
#[inline(always)]
pub fn payload(id: u32) -> u64 {
    mix(id as u64 ^ 0xA5A5_0000_0000_0000)
}

/// Hotspot workload: `p_hot` of the accesses go to ids `[0, hot)`.
#[derive(Clone, Copy, Debug)]
pub struct Workload {
    pub n: u32,
    pub hot: u32,
    /// p_hot scaled to 2^32, compared against the high half of the state.
    pub p_hot_u32: u32,
}

impl Workload {
    pub fn new(n: u32, hot: u32, p_hot: f64) -> Self {
        assert!(hot >= 1 && hot <= n);
        let p = (p_hot.clamp(0.0, 1.0) * 4_294_967_296.0).min(4_294_967_295.0);
        Workload { n, hot, p_hot_u32: p as u32 }
    }

    /// The paper-shaped default: 90% of accesses to `hot_frac` of the objects.
    pub fn hotspot(n: u32, hot_frac: f64) -> Self {
        let hot = ((n as f64 * hot_frac).round() as u32).max(1);
        Workload::new(n, hot, 0.9)
    }

    /// Draw the next id from state `s` (already advanced). Uses the high 32
    /// bits for the hot/cold coin and the low 32 for the index, scaled with a
    /// multiply-shift (Lemire's method) — no division in the timed loop.
    #[inline(always)]
    pub fn pick(&self, s: u64) -> u32 {
        let lo = s as u32 as u64;
        if ((s >> 32) as u32) < self.p_hot_u32 {
            ((lo * self.hot as u64) >> 32) as u32
        } else {
            ((lo * self.n as u64) >> 32) as u32
        }
    }
}

/// Advance the chain: next state from the current one and the payload just
/// loaded. One multiply and one xor-shift, not the full SplitMix finaliser:
/// this sits on the dependency chain of every access in the timed loop, and
/// the payload it folds in is already a SplitMix output, so the cheap step
/// is enough to keep the stream well mixed (the tests check the hot share).
#[inline(always)]
pub fn step(s: u64, loaded: u64) -> u64 {
    let z = (s ^ loaded).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z ^ (z >> 29)
}

/// Where a chain is: the generator state and the payload of the object last
/// visited. Copy it to run a second window that continues the same stream.
#[derive(Clone, Copy, Debug)]
pub struct Chain {
    pub s: u64,
    pub loaded: u64,
}

impl Chain {
    pub fn new(seed: u64) -> Self {
        Chain { s: seed, loaded: 0 }
    }

    /// The next id, with the payload recomputed rather than loaded.
    #[inline(always)]
    pub fn next(&mut self, w: &Workload) -> u32 {
        self.s = step(self.s, self.loaded);
        let id = w.pick(self.s);
        self.loaded = payload(id);
        id
    }
}

/// The id sequence of `len` accesses, computed without touching memory.
/// Advances `chain`, so a second call continues the same stream.
pub fn trace(w: &Workload, chain: &mut Chain, len: usize) -> Vec<u32> {
    (0..len).map(|_| chain.next(w)).collect()
}

/// A small seeded RNG for building the latency ladder's permutation.
pub struct SplitMix(pub u64);

impl SplitMix {
    #[inline(always)]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, bound) by multiply-shift (bias < bound / 2^64).
    #[inline(always)]
    pub fn below(&mut self, bound: u64) -> u64 {
        ((self.next_u64() as u128 * bound as u128) >> 64) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_is_deterministic_and_hot_share_is_right() {
        let w = Workload::hotspot(1 << 16, 0.01);
        let a = trace(&w, &mut Chain::new(SEED), 200_000);
        let b = trace(&w, &mut Chain::new(SEED), 200_000);
        assert_eq!(a, b);
        let hot = a.iter().filter(|&&id| id < w.hot).count() as f64 / a.len() as f64;
        // 0.9 directly, plus 0.1 × hot/n from the uniform draws
        let want = 0.9 + 0.1 * w.hot as f64 / w.n as f64;
        assert!((hot - want).abs() < 0.005, "hot share {hot} vs {want}");
        assert!(a.iter().all(|&id| id < w.n));
    }

    #[test]
    fn hot_ids_are_drawn_evenly() {
        let w = Workload::new(1 << 12, 64, 0.9);
        let t = trace(&w, &mut Chain::new(SEED), 400_000);
        let mut c = vec![0u32; 64];
        for &id in &t {
            if id < 64 {
                c[id as usize] += 1;
            }
        }
        let want = 400_000.0 * (0.9 + 0.1 * 64.0 / 4096.0) / 64.0;
        for (id, &k) in c.iter().enumerate() {
            assert!((k as f64 - want).abs() < 0.1 * want, "hot id {id}: {k} draws, want ~{want:.0}");
        }
    }

    #[test]
    fn windows_continue_the_same_chain() {
        let w = Workload::hotspot(1 << 12, 0.05);
        let whole = trace(&w, &mut Chain::new(SEED), 2000);
        let mut c = Chain::new(SEED);
        let first = trace(&w, &mut c, 1000);
        let second = trace(&w, &mut c, 1000);
        assert_eq!(&whole[..1000], &first[..]);
        assert_eq!(&whole[1000..], &second[..]);
    }
}
