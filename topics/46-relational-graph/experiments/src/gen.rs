//! Seeded, integer-only generators.
//!
//! Everything here iterates in a deterministic order and never reads the
//! iteration order of a `HashMap`/`HashSet` (the bug class fixed in topics 27
//! and 42): edges are produced into a `Vec`, then sorted and deduplicated, so
//! the edge list — and every count derived from it — is identical on every
//! machine and every run.

/// SplitMix64 (Steele, Lea & Flood, OOPSLA'14). One `u64` of state, no
/// floating point, so the stream is bit-identical everywhere.
#[derive(Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        SplitMix64(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (Lemire's multiply-shift; the bias is < n / 2^64).
    pub fn below(&mut self, n: u64) -> u64 {
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }
}

/// A directed edge relation `R(src, dst)`.
pub type Edges = Vec<(u32, u32)>;

/// Parameters of the power-law generator.
#[derive(Clone, Copy, Debug)]
pub struct PowerLaw {
    pub vertices: u32,
    pub edges: u64,
    /// Percent of endpoints copied from an earlier endpoint (preferential
    /// attachment); the rest are uniform. 0 gives an Erdős–Rényi-like graph.
    pub pref_percent: u64,
    pub seed: u64,
}

/// A directed graph with heavy-tailed in- *and* out-degrees.
///
/// Each endpoint of each new edge is, with probability `pref_percent`%, a
/// copy of a uniformly chosen endpoint of an earlier edge, and otherwise a
/// uniform vertex. Copying an endpoint picks a vertex with probability
/// proportional to its current degree — the Yule/Simon "rich get richer"
/// process, which yields a power-law degree tail. Both ends copy from one
/// shared endpoint pool, so a vertex that is a hub for out-edges also tends to
/// be a hub for in-edges: social graphs look like that (celebrities follow and
/// are followed), and it is the case that makes `Σ in(b)·out(b)` — the 2-hop
/// path count — large.
///
/// Self-loops are dropped and duplicate edges collapsed (sort + dedup), so the
/// result is a simple directed graph sorted by `(src, dst)`.
pub fn power_law(p: PowerLaw) -> Edges {
    let mut rng = SplitMix64::new(p.seed);
    let mut pool: Vec<u32> = Vec::with_capacity(2 * p.edges as usize);
    let mut out: Edges = Vec::with_capacity(p.edges as usize);
    let n = p.vertices as u64;
    let pick = |rng: &mut SplitMix64, pool: &Vec<u32>| -> u32 {
        if !pool.is_empty() && rng.below(100) < p.pref_percent {
            pool[rng.below(pool.len() as u64) as usize]
        } else {
            rng.below(n) as u32
        }
    };
    for _ in 0..p.edges {
        let s = pick(&mut rng, &pool);
        let d = pick(&mut rng, &pool);
        pool.push(s);
        pool.push(d);
        if s != d {
            out.push((s, d));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The base relation `R(src, dst)` of Yasser, Dorbani & Mhedhbi, SIGMOD'26,
/// Fig. 1a — ten edges over vertices 0..=8. The paper uses it to show that
/// `R(a1,a2) ⋈ R(a2,a3)` has 13 flat tuples (39 values), 22 values over the
/// f-tree a1–a2–a3 and 15 over a2→{a1, a3} (§2.2.1).
pub fn ffx_fig1() -> Edges {
    vec![
        (0, 2),
        (1, 2),
        (1, 3),
        (1, 4),
        (2, 5),
        (2, 6),
        (2, 7),
        (3, 2),
        (3, 4),
        (4, 8),
    ]
}

/// Edge labels for lane 3: `true` = label `a`, `false` = label `b`. One label
/// in four is `a`. The label is a hash of the edge's endpoints, so it does not
/// depend on the edge's position in any list.
pub fn label_is_a(src: u32, dst: u32, seed: u64) -> bool {
    let mut r = SplitMix64::new(seed ^ ((src as u64) << 32 | dst as u64));
    r.next_u64() % 4 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_is_deterministic_and_simple() {
        let p = PowerLaw { vertices: 1000, edges: 8000, pref_percent: 70, seed: 7 };
        let a = power_law(p);
        let b = power_law(p);
        assert_eq!(a, b);
        assert!(a.windows(2).all(|w| w[0] < w[1]), "sorted and deduplicated");
        assert!(a.iter().all(|&(s, d)| s != d), "no self-loops");
    }

    #[test]
    fn splitmix_known_first_value() {
        // Reference value of SplitMix64 seeded with 0 (Vigna's splitmix64.c).
        assert_eq!(SplitMix64::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
    }
}
