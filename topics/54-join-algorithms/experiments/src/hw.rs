//! Hardware probe (PROVIDED): what this machine's memory hierarchy looks like,
//! measured rather than quoted, so every join number can be read against it.
//!
//! * `chase_ns` — dependent-load latency: walk a random single cycle through
//!   a buffer of `bytes`, one node per 128-byte line (the M3's cache line, per
//!   `sysctl hw.cachelinesize`). Each load's address comes from the previous
//!   load, so nothing overlaps; the time per step is the latency of whichever
//!   level of the hierarchy the buffer fits in.
//! * `read_gbps` — sequential read bandwidth with `threads` threads, each
//!   summing its own slice with four independent accumulators.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::hint::black_box;
use crate::timing::{prefer_performance_cores, Stopwatch};
use std::time::Instant;

const NODE_WORDS: usize = 16; // 16 × 8 B = 128 B per node

/// Nanoseconds per dependent load over a working set of `bytes`.
pub fn chase_ns(bytes: usize) -> f64 {
    prefer_performance_cores();
    let nodes = (bytes / (NODE_WORDS * 8)).max(2);
    let mut rng = ChaCha8Rng::seed_from_u64(0xC4A5E);
    // Sattolo's algorithm: a uniformly random permutation that is one cycle.
    let mut perm: Vec<u32> = (0..nodes as u32).collect();
    for i in (1..nodes).rev() {
        let j = rng.gen_range(0..i);
        perm.swap(i, j);
    }
    let mut buf = vec![0u64; nodes * NODE_WORDS];
    for (i, &p) in perm.iter().enumerate() {
        buf[i * NODE_WORDS] = p as u64 * NODE_WORDS as u64;
    }
    let steps = 20_000_000usize.min(nodes * 64).max(2_000_000);
    // Warm: one full lap of up to 2M steps, then time.
    let mut at = 0usize;
    for _ in 0..nodes.min(2_000_000) {
        at = buf[at] as usize;
    }
    let t = Stopwatch::start();
    for _ in 0..steps {
        at = buf[at] as usize;
    }
    let ns = t.secs() * 1e9 / steps as f64;
    black_box(at);
    ns
}

/// Nanoseconds per dependent load when the chain visits `pages` distinct
/// 16 KiB pages (the M3's page size, `sysctl hw.pagesize`), one 128-byte node
/// per page at a random offset inside it. The cache footprint is only
/// `pages × 128 B`, so once the latency rises it is address translation, not
/// data: the number of pages at which it rises is the TLB's reach.
pub fn chase_pages_ns(pages: usize) -> f64 {
    prefer_performance_cores();
    const PAGE_WORDS: usize = 16384 / 8;
    let mut rng = ChaCha8Rng::seed_from_u64(0x7_1B);
    let mut perm: Vec<u32> = (0..pages as u32).collect();
    for i in (1..pages).rev() {
        let j = rng.gen_range(0..i);
        perm.swap(i, j);
    }
    let offs: Vec<usize> = (0..pages).map(|_| rng.gen_range(0..PAGE_WORDS / NODE_WORDS) * NODE_WORDS).collect();
    let mut buf = vec![0u64; pages * PAGE_WORDS];
    for i in 0..pages {
        let next = perm[i] as usize;
        buf[i * PAGE_WORDS + offs[i]] = (next * PAGE_WORDS + offs[next]) as u64;
    }
    let steps = 10_000_000usize.max(pages * 8);
    let mut at = offs[0];
    for _ in 0..pages.min(2_000_000) {
        at = buf[at] as usize;
    }
    let t = Stopwatch::start();
    for _ in 0..steps {
        at = buf[at] as usize;
    }
    let ns = t.secs() * 1e9 / steps as f64;
    black_box(at);
    ns
}

/// Nanoseconds per link of a chain of dependent integer adds: one cycle each,
/// so this is the clock period of whatever core the thread landed on. A run
/// whose figure is noticeably larger than a quiet-machine run's was scheduled on
/// a slower (efficiency) core, and
/// its other numbers are not comparable with that run.
pub fn add_chain_ns() -> f64 {
    prefer_performance_cores();
    let n = 400_000_000u64;
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Stopwatch::start();
        let mut x = 1u64;
        for _ in 0..n / 8 {
            // eight dependent adds in registers (an asm block, so the compiler
            // cannot fold them or spill x to memory between links)
            #[cfg(target_arch = "aarch64")]
            // SAFETY: pure register arithmetic on one operand, no memory access.
            unsafe {
                std::arch::asm!(
                    "add {0}, {0}, #1", "add {0}, {0}, #1", "add {0}, {0}, #1", "add {0}, {0}, #1",
                    "add {0}, {0}, #1", "add {0}, {0}, #1", "add {0}, {0}, #1", "add {0}, {0}, #1",
                    inout(reg) x, options(pure, nomem, nostack)
                );
            }
            #[cfg(not(target_arch = "aarch64"))]
            {
                x = black_box(x).wrapping_add(8);
            }
        }
        black_box(x);
        best = best.min(t.secs());
    }
    best * 1e9 / n as f64
}

/// Aggregate sequential read bandwidth in GB/s (1 GB = 1e9 bytes).
pub fn read_gbps(bytes: usize, threads: usize) -> f64 {
    let words = bytes / 8;
    let data: Vec<u64> = (0..words as u64).collect();
    let per = words / threads;
    let run = || {
        let t = Instant::now();
        let total: u64 = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|k| {
                    let part = &data[k * per..(k + 1) * per];
                    sc.spawn(move || {
                        prefer_performance_cores();
                        sum4(part)
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).fold(0u64, |a, b| a.wrapping_add(b))
        });
        black_box(total);
        t.elapsed().as_secs_f64()
    };
    run(); // warm: fault the pages in
    let best = (0..3).map(|_| run()).fold(f64::MAX, f64::min);
    (per * threads * 8) as f64 / best / 1e9
}

fn sum4(x: &[u64]) -> u64 {
    let mut a = [0u64; 4];
    let mut it = x.chunks_exact(4);
    for c in &mut it {
        a[0] = a[0].wrapping_add(c[0]);
        a[1] = a[1].wrapping_add(c[1]);
        a[2] = a[2].wrapping_add(c[2]);
        a[3] = a[3].wrapping_add(c[3]);
    }
    a[0].wrapping_add(a[1]).wrapping_add(a[2]).wrapping_add(a[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l1_resident_chase_is_faster_than_dram_chase() {
        let small = chase_ns(32 << 10);
        let big = chase_ns(512 << 20);
        assert!(big > 3.0 * small, "small {small} ns, big {big} ns");
    }
}
