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

// ---------------------------------------------------------------------
// Lane 1c — the two-tier emulation (MODEL for the slow tier, MEASURED for
// everything else).
//
// This machine has one DRAM tier. To make a placement *time* difference
// visible, an access to a page that is not in the fast set is followed by a
// calibrated busy chain of `spin_iters` dependent multiply-adds. The spin's
// result is folded into the next address through an opaque zero, so the CPU
// cannot overlap it with the next miss: it adds its duration to the critical
// path exactly like a longer load would, without changing the id stream or the
// checksum. What is MODELLED is the extra latency (a published number, see the
// bench); what is MEASURED is the rest of the loop, the bitmap lookup, and the
// time the spin really took (checked against `all-fast + slow_share × extra`).
// ---------------------------------------------------------------------

/// A set of fast-tier pages as a bitmap (65,536 pages of 16 KiB = 8 KiB).
pub struct FastSet {
    bits: Vec<u64>,
    pub pages_in_set: usize,
}

impl FastSet {
    pub fn from_pages(total_pages: usize, pages: &[u32]) -> Self {
        let mut bits = vec![0u64; total_pages.div_ceil(64)];
        for &p in pages {
            bits[p as usize / 64] |= 1 << (p % 64);
        }
        FastSet { bits, pages_in_set: pages.len() }
    }
    #[inline(always)]
    pub fn contains(&self, page: u32) -> bool {
        self.bits[page as usize / 64] >> (page % 64) & 1 == 1
    }
}

/// The `k` most-accessed pages of `window` under `layout` (ties broken by
/// page number, so the set is deterministic): the best any static placement
/// can do for that window.
pub fn top_pages(window: &[u32], layout: &Layout, objs_per_page: u32, k: usize) -> Vec<u32> {
    let pages = (layout.mask as usize + 1) / objs_per_page as usize;
    let mut count = vec![0u32; pages];
    for &id in window {
        count[(layout.slot(id) / objs_per_page) as usize] += 1;
    }
    let mut order: Vec<u32> = (0..pages as u32).collect();
    order.sort_unstable_by(|&a, &b| count[b as usize].cmp(&count[a as usize]).then(a.cmp(&b)));
    order.truncate(k);
    order
}

/// `iters` dependent multiply-adds. Pure latency: each step needs the last.
#[inline(never)]
pub fn spin(mut z: u64, iters: u32) -> u64 {
    for _ in 0..iters {
        z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    }
    z
}

/// ns for one dependent chain of `calls` calls of `spin(.., iters)`, per call:
/// includes the call itself, not just the iterations.
fn spin_call_ns(iters: u32, calls: u32) -> f64 {
    let mut ns = Vec::new();
    let mut z = 1u64;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..calls {
            z = spin(black_box(z), black_box(iters));
        }
        ns.push(t.elapsed().as_nanos() as f64 / calls as f64);
    }
    black_box(z);
    crate::stats::median(&mut ns)
}

/// Pick the iteration count whose whole call (call overhead included) costs
/// `target_ns`, by timing chains of calls and correcting twice. Returns
/// (iterations, ns per iteration, achieved ns per call).
pub fn calibrate_spin(target_ns: f64) -> (u32, f64, f64) {
    let per_iter = spin_call_ns(10_000_000, 1) / 10_000_000.0;
    let mut iters = (target_ns / per_iter).round() as i64;
    let mut got = spin_call_ns(iters as u32, 1_000_000);
    for _ in 0..2 {
        iters = (iters + ((target_ns - got) / per_iter).round() as i64).max(1);
        got = spin_call_ns(iters as u32, 1_000_000);
    }
    (iters as u32, per_iter, got)
}

/// One configuration of the tiered loop.
pub struct TierRun<'a> {
    pub arena: &'a Arena,
    pub fast: &'a FastSet,
    /// 0 = the control: same code, same lookup, no added latency
    pub spin_iters: u32,
}

#[inline(never)]
fn run_tiered(
    r: &TierRun,
    w: &Workload,
    chain: &mut Chain,
    steps: usize,
    objs_per_page: u32,
) -> (f64, u64, u64) {
    let mem = black_box(&r.arena.mem[..]);
    let (mult, mask) = (black_box(r.arena.layout.mult), r.arena.layout.mask);
    let zero = black_box(0u64); // opaque to the optimiser: x & zero is not folded away
    let iters = black_box(r.spin_iters);
    let (mut s, mut loaded) = (chain.s, chain.loaded);
    let (mut x, mut slow) = (0u64, 0u64);
    let t = Instant::now();
    for _ in 0..steps {
        s = gen::step(s, loaded);
        let id = w.pick(s);
        let slot = id.wrapping_mul(mult) & mask;
        loaded = mem[slot as usize].0[0];
        x ^= loaded;
        if !r.fast.contains(slot / objs_per_page) {
            slow += 1;
            // value-preserving, but the next address now waits for the spin
            loaded ^= spin(loaded, iters) & zero;
        }
    }
    let dt = t.elapsed().as_nanos() as f64;
    chain.s = s;
    chain.loaded = loaded;
    (dt / steps as f64, slow, black_box(x))
}

/// Median ns/access, slow-access share and checksum for each run, repetitions
/// alternating between the runs so that load from elsewhere lands on all.
pub fn measure_tiered(
    runs: &[TierRun],
    w: &Workload,
    seed: u64,
    warm: usize,
    steps: usize,
    reps: usize,
    objs_per_page: u32,
) -> Vec<(f64, f64, u64)> {
    let mut chains: Vec<Chain> = runs.iter().map(|_| Chain::new(seed)).collect();
    for (k, r) in runs.iter().enumerate() {
        run_tiered(r, w, &mut chains[k], warm, objs_per_page);
    }
    let mut ns: Vec<Vec<f64>> = runs.iter().map(|_| Vec::new()).collect();
    let mut slow = vec![0u64; runs.len()];
    let mut sums = vec![0u64; runs.len()];
    for _ in 0..reps {
        for (k, r) in runs.iter().enumerate() {
            let (t, sl, c) = run_tiered(r, w, &mut chains[k], steps, objs_per_page);
            ns[k].push(t);
            slow[k] += sl;
            sums[k] ^= c;
        }
    }
    ns.iter_mut()
        .enumerate()
        .map(|(k, v)| (crate::stats::median(v), slow[k] as f64 / (steps * reps) as f64, sums[k]))
        .collect()
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

    #[test]
    fn spin_does_not_change_the_stream() {
        let n = 1 << 12;
        let w = Workload::hotspot(n, 0.05);
        let a = Arena::new(Layout::scattered(n));
        let k = (n / 256) as usize / 8; // eighth of the pages
        let mut c = Chain::new(gen::SEED);
        let win = gen::trace(&w, &mut c, 20_000);
        let fast = FastSet::from_pages(
            (n / 256) as usize,
            &top_pages(&win, &a.layout, 256, k),
        );
        let runs = [
            TierRun { arena: &a, fast: &fast, spin_iters: 0 },
            TierRun { arena: &a, fast: &fast, spin_iters: 50 },
        ];
        let r = measure_tiered(&runs, &w, gen::SEED, 100, 5000, 2, 256);
        assert_eq!(r[0].2, r[1].2, "spin must not change what is loaded");
        assert_eq!(r[0].1, r[1].1, "same accesses are slow with or without the spin");
        assert!(r[0].1 > 0.0 && r[0].1 < 1.0);
    }

    #[test]
    fn top_pages_are_deterministic_and_sorted_by_count() {
        let n = 1 << 12;
        let w = Workload::hotspot(n, 0.05);
        let l = Layout::packed(n);
        let win = gen::trace(&w, &mut Chain::new(gen::SEED), 20_000);
        let a = top_pages(&win, &l, 256, 3);
        assert_eq!(a, top_pages(&win, &l, 256, 3));
        assert_eq!(a.len(), 3);
        assert!(a.iter().all(|&p| p < n / 256));
    }
}
