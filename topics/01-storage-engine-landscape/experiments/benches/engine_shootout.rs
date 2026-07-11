//! engine_shootout — fjall (LSM) vs redb (B-tree), db_bench vocabulary:
//! fillseq, fillrandom, readrandom (Zipfian), scan.
//! Space amplification is measured by `cargo run --release` (src/main.rs).

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use engine_shootout::{value_for, Engine, FjallEngine, RedbEngine};
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use rand_distr::{Distribution, Zipf};

const FILL_N: u64 = 100_000;
const READ_N: u64 = 1_000_000;
const PROBES: usize = 1024;
const BATCH: usize = 1000;

fn items(keys: &[u64]) -> Vec<(u64, Vec<u8>)> {
    keys.iter().map(|&k| (k, value_for(k))).collect()
}

fn fill(engine: &mut dyn Engine, items: &[(u64, Vec<u8>)]) {
    for chunk in items.chunks(BATCH) {
        engine.put_batch(chunk);
    }
}

fn bench_fill(c: &mut Criterion) {
    let seq = items(&(0..FILL_N).collect::<Vec<_>>());
    let mut shuffled: Vec<u64> = (0..FILL_N).collect();
    shuffled.shuffle(&mut StdRng::seed_from_u64(42));
    let random = items(&shuffled);

    for (bench_name, data) in [("fillseq", &seq), ("fillrandom", &random)] {
        let mut group = c.benchmark_group(bench_name);
        group.sample_size(10);
        group.throughput(Throughput::Elements(FILL_N));

        group.bench_function("fjall", |b| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let engine = FjallEngine::open(dir.path());
                    (dir, engine)
                },
                |(_dir, mut engine)| fill(&mut engine, data),
                BatchSize::PerIteration,
            )
        });
        group.bench_function("redb", |b| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let engine = RedbEngine::open(dir.path());
                    (dir, engine)
                },
                |(_dir, mut engine)| fill(&mut engine, data),
                BatchSize::PerIteration,
            )
        });
        group.finish();
    }
}

fn bench_read_scan(c: &mut Criterion) {
    let mut keys: Vec<u64> = (0..READ_N).collect();
    keys.shuffle(&mut StdRng::seed_from_u64(42));
    let data = items(&keys);

    let fjall_dir = tempfile::tempdir().unwrap();
    let mut fjall_engine = FjallEngine::open(fjall_dir.path());
    fill(&mut fjall_engine, &data);
    fjall_engine.sync();

    let redb_dir = tempfile::tempdir().unwrap();
    let mut redb_engine = RedbEngine::open(redb_dir.path());
    fill(&mut redb_engine, &data);
    redb_engine.sync();

    // Zipfian probe set (s=0.99, YCSB default — same skew as the M0 generator).
    let zipf = Zipf::new(READ_N, 0.99).unwrap();
    let mut rng = StdRng::seed_from_u64(7);
    let probes: Vec<u64> = (0..PROBES)
        .map(|_| zipf.sample(&mut rng) as u64 - 1)
        .collect();

    let engines: [&dyn Engine; 2] = [&fjall_engine, &redb_engine];

    let mut group = c.benchmark_group("readrandom_zipf");
    group.throughput(Throughput::Elements(PROBES as u64));
    for engine in engines {
        group.bench_function(engine.name(), |b| {
            b.iter(|| {
                let mut hits = 0usize;
                for &k in &probes {
                    if engine.get(criterion::black_box(k)).is_some() {
                        hits += 1;
                    }
                }
                criterion::black_box(hits)
            })
        });
    }
    group.finish();

    // Uniform-random probes for contrast with the Zipfian hot set.
    let uniform: Vec<u64> = (0..PROBES)
        .map(|_| rng.gen_range(0..READ_N))
        .collect();
    let mut group = c.benchmark_group("readrandom_uniform");
    group.throughput(Throughput::Elements(PROBES as u64));
    for engine in engines {
        group.bench_function(engine.name(), |b| {
            b.iter(|| {
                let mut hits = 0usize;
                for &k in &uniform {
                    if engine.get(criterion::black_box(k)).is_some() {
                        hits += 1;
                    }
                }
                criterion::black_box(hits)
            })
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scan");
    group.sample_size(10);
    group.throughput(Throughput::Elements(READ_N));
    for engine in engines {
        group.bench_function(engine.name(), |b| {
            b.iter(|| criterion::black_box(engine.scan_count()))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_fill, bench_read_scan);
criterion_main!(benches);
