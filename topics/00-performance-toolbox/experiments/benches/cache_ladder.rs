//! Experiment 1 — cache_ladder
//!
//! Pointer-chase through a random cyclic permutation of varying working-set sizes.
//! Random order defeats the prefetcher, so ns/access ≈ the latency of whichever
//! cache level the working set fits in. Expect plateaus at L1 / L2 / SLC / DRAM.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

/// Build a random cyclic permutation (Sattolo's algorithm) so a chase visits
/// every slot exactly once per cycle — no short cycles hiding in cache.
fn make_chain(len: usize, rng: &mut StdRng) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    order.shuffle(rng);
    let mut chain = vec![0usize; len];
    for w in order.windows(2) {
        chain[w[0]] = w[1];
    }
    chain[order[len - 1]] = order[0];
    chain
}

fn chase(chain: &[usize], start: usize, steps: usize) -> usize {
    let mut idx = start;
    for _ in 0..steps {
        idx = chain[idx];
    }
    idx
}

fn bench_cache_ladder(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_ladder");
    group.sample_size(10);
    let mut rng = StdRng::seed_from_u64(42);

    // Working-set sizes in bytes: 16KB → 512MB (8 bytes per usize slot).
    let sizes_kb: &[usize] = &[
        16, 64, 128, 512, 1024, 4 * 1024, 8 * 1024, 16 * 1024, 32 * 1024, 64 * 1024, 128 * 1024,
        256 * 1024, 512 * 1024,
    ];
    let steps = 1 << 16;

    for &kb in sizes_kb {
        let len = kb * 1024 / std::mem::size_of::<usize>();
        let chain = make_chain(len, &mut rng);
        group.throughput(Throughput::Elements(steps as u64));
        group.bench_with_input(BenchmarkId::from_parameter(format!("{kb}KB")), &chain, |b, chain| {
            // Carry the position across iterations: restarting at 0 every iter
            // re-walks the same `steps` slots, which stay cached — at 512MB that
            // silently measures an ~8MB hot path instead of DRAM.
            let mut idx = 0usize;
            b.iter(|| {
                idx = chase(black_box(chain), idx, steps);
                black_box(idx)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_cache_ladder);
criterion_main!(benches);
