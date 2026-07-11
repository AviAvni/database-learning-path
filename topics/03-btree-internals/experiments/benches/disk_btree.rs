//! disk_btree — your slotted-page B+tree vs redb, plus the prefix-truncation
//! experiment.
//!
//! Honesty rules (topic 0/1): both engines warm (OS page cache holds everything
//! at 1M keys — you are NOT benching the disk, note it); fixed seed; predict
//! before running (notes.md table).
//!
//! Prefix-truncation experiment: 32-byte keys sharing a 24-byte prefix are the
//! adversarial case for full-key separators (fanout collapses). After the
//! baseline run, implement suffix truncation in Page::split_into and re-run —
//! report fanout (via DiskBTree::height + page count) and lookup delta.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::prelude::*;
use std::hint::black_box;
use topic03_experiments::DiskBTree;

const N: u64 = 1_000_000;
const PROBES: usize = 1024;
const TABLE: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("t");

fn short_key(i: u64) -> [u8; 8] {
    i.wrapping_mul(0x9E3779B97F4A7C15).to_be_bytes()
}

/// 32 bytes, 24-byte shared prefix — the truncation stress case.
fn long_key(i: u64) -> [u8; 32] {
    let mut k = [b'p'; 32];
    k[24..].copy_from_slice(&i.wrapping_mul(0x9E3779B97F4A7C15).to_be_bytes());
    k
}

fn probes(n: u64) -> Vec<u64> {
    let mut rng = StdRng::seed_from_u64(11);
    (0..PROBES).map(|_| rng.gen_range(0..n)).collect()
}

fn bench_point_lookup(c: &mut Criterion) {
    let mut g = c.benchmark_group("point_lookup_1m");
    g.throughput(Throughput::Elements(PROBES as u64));
    let ps = probes(N);

    for (name, keyfn) in [("short8", short_key as fn(u64) -> [u8; 8])] {
        let dir = tempfile::tempdir().unwrap();
        let mut mine = DiskBTree::create(&dir.path().join("mine.db")).unwrap();
        for i in 0..N {
            mine.insert(&keyfn(i), &i.to_le_bytes()).unwrap();
        }
        g.bench_with_input(BenchmarkId::new("my_btree", name), &ps, |b, ps| {
            b.iter(|| {
                for &i in ps {
                    black_box(mine.get(&keyfn(i)).unwrap());
                }
            })
        });

        let rdb = redb::Database::create(dir.path().join("redb.db")).unwrap();
        let tx = rdb.begin_write().unwrap();
        {
            let mut t = tx.open_table(TABLE).unwrap();
            for i in 0..N {
                t.insert(&keyfn(i)[..], &i.to_le_bytes()[..]).unwrap();
            }
        }
        tx.commit().unwrap();
        let rtx = rdb.begin_read().unwrap();
        let t = rtx.open_table(TABLE).unwrap();
        g.bench_with_input(BenchmarkId::new("redb", name), &ps, |b, ps| {
            b.iter(|| {
                for &i in ps {
                    black_box(t.get(&keyfn(i)[..]).unwrap());
                }
            })
        });
    }
    g.finish();
}

fn bench_range_scan(c: &mut Criterion) {
    let mut g = c.benchmark_group("range_scan_1k_of_1m");
    g.throughput(Throughput::Elements(1000));

    let dir = tempfile::tempdir().unwrap();
    let mut mine = DiskBTree::create(&dir.path().join("mine.db")).unwrap();
    for i in 0..N {
        mine.insert(&i.to_be_bytes(), &i.to_le_bytes()).unwrap();
    }
    g.bench_function("my_btree", |b| {
        b.iter(|| {
            black_box(
                mine.scan(&500_000u64.to_be_bytes(), &501_000u64.to_be_bytes())
                    .unwrap()
                    .len(),
            )
        })
    });

    let rdb = redb::Database::create(dir.path().join("redb.db")).unwrap();
    let tx = rdb.begin_write().unwrap();
    {
        let mut t = tx.open_table(TABLE).unwrap();
        for i in 0..N {
            t.insert(&i.to_be_bytes()[..], &i.to_le_bytes()[..]).unwrap();
        }
    }
    tx.commit().unwrap();
    let rtx = rdb.begin_read().unwrap();
    let t = rtx.open_table(TABLE).unwrap();
    g.bench_function("redb", |b| {
        b.iter(|| {
            let lo = 500_000u64.to_be_bytes();
            let hi = 501_000u64.to_be_bytes();
            black_box(t.range(&lo[..]..&hi[..]).unwrap().count())
        })
    });
    g.finish();
}

fn bench_long_keys(c: &mut Criterion) {
    // the truncation experiment: run before AND after implementing suffix
    // truncation in Page::split_into; record height + file size in notes.md
    let mut g = c.benchmark_group("long_key_lookup_1m");
    g.throughput(Throughput::Elements(PROBES as u64));
    let ps = probes(N);

    let dir = tempfile::tempdir().unwrap();
    let mut mine = DiskBTree::create(&dir.path().join("mine.db")).unwrap();
    for i in 0..N {
        mine.insert(&long_key(i), &i.to_le_bytes()).unwrap();
    }
    eprintln!("long-key tree height: {:?}", mine.height());
    g.bench_with_input(BenchmarkId::new("my_btree", "shared_prefix_32"), &ps, |b, ps| {
        b.iter(|| {
            for &i in ps {
                black_box(mine.get(&long_key(i)).unwrap());
            }
        })
    });
    g.finish();
}

criterion_group!(benches, bench_point_lookup, bench_range_scan, bench_long_keys);
criterion_main!(benches);
