//! Experiment 2 — lookup_shootout
//!
//! Point lookups across Vec linear scan, Vec binary search, HashMap, BTreeMap
//! at sizes 1e2 → 1e7. Look for the crossover where linear scan beats hashing.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use std::collections::{BTreeMap, HashMap};

const LOOKUPS: usize = 1024;

fn make_keys(n: usize, rng: &mut StdRng) -> (Vec<u64>, Vec<u64>) {
    let keys: Vec<u64> = (0..n as u64).map(|i| i * 7 + 3).collect();
    let mut probes: Vec<u64> = keys.clone();
    probes.shuffle(rng);
    probes.truncate(LOOKUPS.min(n));
    while probes.len() < LOOKUPS {
        probes.extend_from_within(0..probes.len().min(LOOKUPS - probes.len()));
    }
    (keys, probes)
}

fn bench_lookup_shootout(c: &mut Criterion) {
    let mut group = c.benchmark_group("lookup_shootout");
    let mut rng = StdRng::seed_from_u64(7);

    for &n in &[100usize, 1_000, 10_000, 100_000, 1_000_000, 10_000_000] {
        let (keys, probes) = make_keys(n, &mut rng);
        let sorted = keys.clone(); // keys are generated in sorted order
        let hash: HashMap<u64, u64> = keys.iter().map(|&k| (k, k * 2)).collect();
        let btree: BTreeMap<u64, u64> = keys.iter().map(|&k| (k, k * 2)).collect();

        group.throughput(Throughput::Elements(probes.len() as u64));

        // Linear scan only makes sense at small n — cap it to avoid hour-long runs.
        if n <= 100_000 {
            group.bench_with_input(BenchmarkId::new("vec_linear", n), &n, |b, _| {
                b.iter(|| {
                    let mut hits = 0u64;
                    for &p in &probes {
                        if let Some(&v) = sorted.iter().find(|&&k| k == p) {
                            hits += v;
                        }
                    }
                    black_box(hits)
                })
            });
        }

        group.bench_with_input(BenchmarkId::new("vec_binary_search", n), &n, |b, _| {
            b.iter(|| {
                let mut hits = 0u64;
                for &p in &probes {
                    if let Ok(i) = sorted.binary_search(&p) {
                        hits += sorted[i];
                    }
                }
                black_box(hits)
            })
        });

        group.bench_with_input(BenchmarkId::new("hashmap", n), &n, |b, _| {
            b.iter(|| {
                let mut hits = 0u64;
                for &p in &probes {
                    if let Some(&v) = hash.get(&p) {
                        hits += v;
                    }
                }
                black_box(hits)
            })
        });

        group.bench_with_input(BenchmarkId::new("btreemap", n), &n, |b, _| {
            b.iter(|| {
                let mut hits = 0u64;
                for &p in &probes {
                    if let Some(&v) = btree.get(&p) {
                        hits += v;
                    }
                }
                black_box(hits)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_lookup_shootout);
criterion_main!(benches);
