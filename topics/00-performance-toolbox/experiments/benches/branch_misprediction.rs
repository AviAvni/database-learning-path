//! Experiment 3 — branch_misprediction
//!
//! Sum elements > threshold over sorted vs shuffled data, branchy vs branchless.
//! Sorted data makes the branch perfectly predictable; shuffled data at a 50%
//! threshold defeats the predictor (~15–20 cycle flush per miss). The branchless
//! version converts control dependence into data dependence and the gap vanishes.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

const N: usize = 1 << 20;
const THRESHOLD: u32 = u32::MAX / 2;

fn sum_branchy(data: &[u32], threshold: u32) -> u64 {
    let mut sum = 0u64;
    for &x in data {
        if x > threshold {
            // black_box can't be speculated, so LLVM can't if-convert or
            // vectorize this loop — without it both versions compile to the
            // same NEON select and the sorted/shuffled gap vanishes entirely.
            sum += black_box(x) as u64;
        }
    }
    sum
}

fn sum_branchless(data: &[u32], threshold: u32) -> u64 {
    let mut sum = 0u64;
    for &x in data {
        let keep = (x > threshold) as u64;
        sum += keep * x as u64;
    }
    sum
}

fn bench_branch_misprediction(c: &mut Criterion) {
    let mut rng = StdRng::seed_from_u64(1234);
    let mut shuffled: Vec<u32> = (0..N).map(|_| rng.gen()).collect();
    let mut sorted = shuffled.clone();
    sorted.sort_unstable();
    shuffled.shuffle(&mut rng);

    let mut group = c.benchmark_group("branch_misprediction");
    group.throughput(Throughput::Elements(N as u64));

    for (name, data) in [("sorted", &sorted), ("shuffled", &shuffled)] {
        group.bench_with_input(BenchmarkId::new("branchy", name), data, |b, d| {
            b.iter(|| sum_branchy(black_box(d), black_box(THRESHOLD)))
        });
        group.bench_with_input(BenchmarkId::new("branchless", name), data, |b, d| {
            b.iter(|| sum_branchless(black_box(d), black_box(THRESHOLD)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_branch_misprediction);
criterion_main!(benches);
