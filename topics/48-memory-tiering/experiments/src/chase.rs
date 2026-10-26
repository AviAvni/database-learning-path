//! The two timed loops of lane 1. Both are dependent chains: each access's
//! address is computed from the value the previous access loaded, so the
//! CPU cannot overlap two misses and ns/access is latency, not throughput.
//! (Topic 0's `cache_ladder` once measured its own 8 MB hot path because the
//! chase restarted every iteration; both loops here carry their position
//! across repetitions for the same reason.)

use crate::gen::{self, Chain, SplitMix, Workload};
use crate::layout::Layout;
use crate::OBJ_BYTES;
use std::hint::black_box;
use std::time::Instant;

/// A random cyclic permutation (Sattolo's algorithm): following `next`
/// from any slot visits every slot exactly once before returning, so there
/// is no short cycle for the cache to hold.
pub fn sattolo(len: usize, seed: u64) -> Vec<u32> {
    assert!(len >= 2 && len <= u32::MAX as usize);
    let mut perm: Vec<u32> = (0..len as u32).collect();
    let mut rng = SplitMix(seed);
    for i in (1..len).rev() {
        let j = rng.below(i as u64) as usize; // j < i: Sattolo, not Fisher–Yates
        perm.swap(i, j);
    }
    // perm is a single n-cycle as a permutation; use it directly as next[]
    perm
}

/// Follow `next` for `steps` steps from `start`; return where it ended.
#[inline(never)]
pub fn chase(next: &[u32], start: u32, steps: usize) -> u32 {
    let mut i = start;
    for _ in 0..steps {
        i = next[i as usize];
    }
    i
}

/// ns per dependent load over a working set of `bytes`, median of `reps`
/// timed runs of `steps` each, after one untimed warm-up pass.
pub fn ladder_point(bytes: usize, steps: usize, reps: usize, seed: u64) -> f64 {
    let len = bytes / std::mem::size_of::<u32>();
    let next = sattolo(len, seed);
    let mut pos = 0u32;
    pos = chase(black_box(&next), pos, steps.min(len).max(steps / 4));
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        pos = chase(black_box(&next), black_box(pos), steps);
        let dt = t.elapsed().as_nanos() as f64;
        samples.push(dt / steps as f64);
    }
    black_box(pos);
    crate::stats::median(&mut samples)
}

/// One 64-byte object. Only word 0 is read; the rest is the object's body.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct Obj(pub [u64; OBJ_BYTES / 8]);

/// The placement arena: `n` objects laid out by `layout`, with each object's
/// first word holding `payload(id)` for the id stored there.
pub struct Arena {
    pub mem: Vec<Obj>,
    pub layout: Layout,
}

impl Arena {
    pub fn new(layout: Layout) -> Self {
        let n = layout.mask as usize + 1;
        let mut mem = vec![Obj([0; OBJ_BYTES / 8]); n];
        // slot order: sequential writes, whatever the layout
        for (slot, o) in mem.iter_mut().enumerate() {
            let id = layout.id_at(slot as u32);
            o.0[0] = gen::payload(id);
            o.0[1] = id as u64;
        }
        Arena { mem, layout }
    }

    /// Re-lay the same arena (reuses the allocation).
    pub fn relayout(&mut self, layout: Layout) {
        assert_eq!(layout.mask, self.layout.mask);
        for (slot, o) in self.mem.iter_mut().enumerate() {
            let id = layout.id_at(slot as u32);
            o.0[0] = gen::payload(id);
            o.0[1] = id as u64;
        }
        self.layout = layout;
    }
}

/// The timed loop: `steps` dependent accesses of workload `w` over `arena`,
/// continuing from `chain`. Returns (ns per access, XOR of every payload
/// loaded). The checksum is the proof that both layouts visited the same
/// objects in the same order: it depends only on the id sequence.
#[inline(never)]
pub fn run(arena: &Arena, w: &Workload, chain: &mut Chain, steps: usize) -> (f64, u64) {
    let mem = black_box(&arena.mem[..]);
    let (mult, mask) = (black_box(arena.layout.mult), arena.layout.mask);
    let (mut s, mut loaded) = (chain.s, chain.loaded);
    let mut x = 0u64;
    let t = Instant::now();
    for _ in 0..steps {
        s = gen::step(s, loaded);
        let id = w.pick(s);
        let slot = id.wrapping_mul(mult) & mask;
        // SAFETY-free bounds: slot <= mask < mem.len()
        loaded = mem[slot as usize].0[0];
        x ^= loaded;
    }
    let dt = t.elapsed().as_nanos() as f64;
    chain.s = s;
    chain.loaded = loaded;
    (dt / steps as f64, black_box(x))
}

/// Median-of-`reps` ns/access after `warm` untimed accesses, all from one
/// continuing chain. The checksum covers the timed accesses only.
pub fn measure(arena: &Arena, w: &Workload, seed: u64, warm: usize, steps: usize, reps: usize) -> (f64, u64) {
    let mut chain = Chain::new(seed);
    run(arena, w, &mut chain, warm);
    let mut ns = Vec::with_capacity(reps);
    let mut x = 0u64;
    for _ in 0..reps {
        let (t, c) = run(arena, w, &mut chain, steps);
        ns.push(t);
        x ^= c;
    }
    (crate::stats::median(&mut ns), x)
}

/// [`measure`] for two arenas at once, alternating repetitions (a, b, a,
/// b, …) so that load from other processes lands on both. Returns the two
/// medians, the two checksums, and the median of the per-repetition ratios
/// b / a.
pub fn measure_pair(
    arenas: [&Arena; 2],
    w: &Workload,
    seed: u64,
    warm: usize,
    steps: usize,
    reps: usize,
) -> ([f64; 2], [u64; 2], f64) {
    let mut chains = [Chain::new(seed), Chain::new(seed)];
    for k in 0..2 {
        run(arenas[k], w, &mut chains[k], warm);
    }
    let mut ns: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    let mut ratios = Vec::with_capacity(reps);
    let mut x = [0u64; 2];
    for _ in 0..reps {
        let mut t = [0.0; 2];
        for k in 0..2 {
            let (tk, c) = run(arenas[k], w, &mut chains[k], steps);
            t[k] = tk;
            ns[k].push(tk);
            x[k] ^= c;
        }
        ratios.push(t[1] / t[0]);
    }
    let [mut a, mut b] = ns;
    (
        [crate::stats::median(&mut a), crate::stats::median(&mut b)],
        x,
        crate::stats::median(&mut ratios),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sattolo_is_one_cycle() {
        let next = sattolo(1000, 7);
        let mut i = 0u32;
        let mut seen = vec![false; 1000];
        for _ in 0..1000 {
            assert!(!seen[i as usize]);
            seen[i as usize] = true;
            i = next[i as usize];
        }
        assert_eq!(i, 0, "a single 1000-cycle returns to its start");
    }

    #[test]
    fn layouts_see_the_same_stream() {
        let n = 1 << 12;
        let w = Workload::hotspot(n, 0.01);
        let a = Arena::new(Layout::packed(n));
        let b = Arena::new(Layout::scattered(n));
        let (_, xa) = measure(&a, &w, gen::SEED, 100, 5000, 3);
        let (_, xb) = measure(&b, &w, gen::SEED, 100, 5000, 3);
        assert_eq!(xa, xb, "same ids, same payloads, whatever the layout");
    }
}
