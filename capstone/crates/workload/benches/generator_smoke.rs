//! M0 smoke bench — how fast can we *generate* ops? The generator must never be
//! the bottleneck when benchmarking the engine, so track its throughput from day one.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use workload::{Generator, WorkloadConfig};

fn bench_generator(c: &mut Criterion) {
    let mut group = c.benchmark_group("workload_generator");
    const OPS: usize = 100_000;
    group.throughput(Throughput::Elements(OPS as u64));
    group.bench_function("generate_100k_ops", |b| {
        b.iter(|| {
            let gen = Generator::new(WorkloadConfig::default());
            let mut count = 0usize;
            for op in gen.take(OPS) {
                black_box(&op);
                count += 1;
            }
            black_box(count)
        })
    });
    group.finish();
}

criterion_group!(benches, bench_generator);
criterion_main!(benches);
