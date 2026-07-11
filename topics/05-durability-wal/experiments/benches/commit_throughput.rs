//! Commit throughput: fsync-per-commit vs group commit vs everysec-style.
//!
//! Run AFTER implementing src/wal.rs. Plot commits/s against the durability
//! window of each policy in notes.md:
//!
//!   per-commit   window = 0            (every ack is durable)
//!   group N      window = 0            (acks wait for the group fsync)
//!   everysec     window = up to batch  (acks BEFORE fsync — redis contract)
//!
//! Predicted shape: per-commit ≈ 1/fsync_latency; group-N approaches
//! N/fsync_latency until write bandwidth takes over; everysec ≈ group but
//! with the ack moved before the flush (same throughput, different contract —
//! the interesting column is the *window*, not the rate).

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use durability_experiments::wal::Wal;

const PAYLOAD: &[u8] = &[0x42u8; 128]; // one graph mutation, roughly

fn bench_commit(c: &mut Criterion) {
    let mut g = c.benchmark_group("commit_throughput");
    g.sample_size(10);

    // fsync per commit — the honest baseline
    g.throughput(Throughput::Elements(1));
    g.bench_function("fsync_per_commit", |b| {
        let dir = tempfile::tempdir().unwrap();
        let mut wal = Wal::open(&dir.path().join("wal")).unwrap();
        let mut txn = 0u64;
        b.iter(|| {
            wal.append(txn, PAYLOAD).unwrap();
            wal.commit(txn).unwrap();
            txn += 1;
        });
    });

    // group commit at batch sizes 8 / 64 / 512
    for batch in [8u64, 64, 512] {
        g.throughput(Throughput::Elements(batch));
        g.bench_function(format!("group_commit_{batch}"), |b| {
            let dir = tempfile::tempdir().unwrap();
            let mut wal = Wal::open(&dir.path().join("wal")).unwrap();
            let mut next = 0u64;
            b.iter_batched(
                || {
                    let ids: Vec<u64> = (next..next + batch).collect();
                    next += batch;
                    ids
                },
                |ids| {
                    for &t in &ids {
                        wal.append(t, PAYLOAD).unwrap();
                    }
                    wal.commit_many(&ids).unwrap();
                },
                BatchSize::SmallInput,
            );
        });
    }

    g.finish();
}

criterion_group!(benches, bench_commit);
criterion_main!(benches);
