//! structures — point lookups, inserts, ordered scan across five structures.
//!
//! Competitors (work out of the box): hashbrown::HashMap, std BTreeMap,
//! crossbeam_skiplist::SkipMap. Yours (todo!() until implemented): SkipList,
//! IncrementalMap — their benches panic at runtime until you implement them,
//! which is the reminder.
//!
//! Zipfian probes (s = 0.99, fixed seed) so the comparison has realistic skew;
//! sizes 1e3 → 1e7 show each structure crossing cache-level cliffs (topic 0).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::prelude::*;
use rand_distr::Zipf;
use std::collections::BTreeMap;
use std::hint::black_box;
use topic02_experiments::{IncrementalMap, SkipList};

const SIZES: &[u64] = &[1_000, 100_000, 10_000_000];
const PROBES: usize = 1024;

fn key(i: u64) -> u64 {
    i.wrapping_mul(0x9E3779B97F4A7C15)
}

fn zipf_probes(n: u64) -> Vec<u64> {
    let mut rng = StdRng::seed_from_u64(7);
    let dist = Zipf::new(n, 0.99).unwrap();
    (0..PROBES).map(|_| key(rng.sample(dist) as u64 - 1)).collect()
}

fn bench_lookups(c: &mut Criterion) {
    let mut g = c.benchmark_group("point_lookup_zipf");
    for &n in SIZES {
        let probes = zipf_probes(n);
        g.throughput(Throughput::Elements(PROBES as u64));

        let hb: hashbrown::HashMap<u64, u64> = (0..n).map(|i| (key(i), i)).collect();
        g.bench_with_input(BenchmarkId::new("hashbrown", n), &probes, |b, p| {
            b.iter(|| p.iter().map(|k| hb.get(k).copied().unwrap_or(0)).sum::<u64>())
        });

        let bt: BTreeMap<u64, u64> = (0..n).map(|i| (key(i), i)).collect();
        g.bench_with_input(BenchmarkId::new("btreemap", n), &probes, |b, p| {
            b.iter(|| p.iter().map(|k| bt.get(k).copied().unwrap_or(0)).sum::<u64>())
        });

        let cb = crossbeam_skiplist::SkipMap::new();
        for i in 0..n {
            cb.insert(key(i), i);
        }
        g.bench_with_input(BenchmarkId::new("crossbeam_skiplist", n), &probes, |b, p| {
            b.iter(|| p.iter().map(|k| cb.get(k).map(|e| *e.value()).unwrap_or(0)).sum::<u64>())
        });

        // yours — panics until implemented
        let mut mine = SkipList::new();
        for i in 0..n {
            mine.insert(key(i), i);
        }
        g.bench_with_input(BenchmarkId::new("my_skiplist", n), &probes, |b, p| {
            b.iter(|| p.iter().map(|k| mine.get(*k).unwrap_or(0)).sum::<u64>())
        });

        let mut imap = IncrementalMap::new();
        for i in 0..n {
            imap.insert(key(i), i);
        }
        g.bench_with_input(BenchmarkId::new("my_incremental_map", n), &probes, |b, p| {
            b.iter(|| p.iter().map(|k| imap.get(*k).unwrap_or(0)).sum::<u64>())
        });
    }
    g.finish();
}

fn bench_inserts(c: &mut Criterion) {
    let mut g = c.benchmark_group("insert");
    let n: u64 = 1_000_000;
    g.throughput(Throughput::Elements(n));
    g.sample_size(10);

    g.bench_function("hashbrown", |b| {
        b.iter(|| {
            let mut m = hashbrown::HashMap::new();
            for i in 0..n {
                m.insert(key(i), i);
            }
            black_box(m.len())
        })
    });
    g.bench_function("btreemap", |b| {
        b.iter(|| {
            let mut m = BTreeMap::new();
            for i in 0..n {
                m.insert(key(i), i);
            }
            black_box(m.len())
        })
    });
    g.bench_function("crossbeam_skiplist", |b| {
        b.iter(|| {
            let m = crossbeam_skiplist::SkipMap::new();
            for i in 0..n {
                m.insert(key(i), i);
            }
            black_box(m.len())
        })
    });
    g.bench_function("my_skiplist", |b| {
        b.iter(|| {
            let mut m = SkipList::new();
            for i in 0..n {
                m.insert(key(i), i);
            }
            black_box(m.len())
        })
    });
    g.bench_function("my_incremental_map", |b| {
        b.iter(|| {
            let mut m = IncrementalMap::new();
            for i in 0..n {
                m.insert(key(i), i);
            }
            black_box(m.len())
        })
    });
    g.finish();
}

fn bench_ordered_scan(c: &mut Criterion) {
    // the memtable-flush path in miniature: full sorted iteration
    let mut g = c.benchmark_group("ordered_scan");
    let n: u64 = 1_000_000;
    g.throughput(Throughput::Elements(n));

    let bt: BTreeMap<u64, u64> = (0..n).map(|i| (key(i), i)).collect();
    g.bench_function("btreemap", |b| {
        b.iter(|| bt.iter().map(|(_, v)| v).sum::<u64>())
    });

    let cb = crossbeam_skiplist::SkipMap::new();
    for i in 0..n {
        cb.insert(key(i), i);
    }
    g.bench_function("crossbeam_skiplist", |b| {
        b.iter(|| cb.iter().map(|e| *e.value()).sum::<u64>())
    });

    let mut mine = SkipList::new();
    for i in 0..n {
        mine.insert(key(i), i);
    }
    g.bench_function("my_skiplist", |b| {
        b.iter(|| mine.iter().map(|(_, v)| v).sum::<u64>())
    });
    g.finish();
}

criterion_group!(benches, bench_lookups, bench_inserts, bench_ordered_scan);
criterion_main!(benches);
