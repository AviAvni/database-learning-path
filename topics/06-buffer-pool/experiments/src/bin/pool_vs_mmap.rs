//! Buffer pool vs mmap on a file 4× larger than the memory budget.
//!
//! Runs after you implement src/buffer_pool.rs. The mmap side works today.
//!
//! Workload: Zipf(0.99) random page reads (read 8 bytes/page so the access,
//! not the memcpy, dominates). Report p50 / p99 / p99.9 / max — the CIDR '22
//! story is in the tail, not the median.
//!
//! Honest-comparison notes:
//! - We can't easily cap the kernel page cache per-process on macOS, so the
//!   mmap side gets the WHOLE page cache — mmap plays with a handicap in its
//!   favor. If your pool still wins the tail, the result is conclusive; if
//!   mmap wins the median, that's expected and worth explaining in notes.
//! - Run twice: cold (right after `purge`, if you dare) and warm.

use buffer_pool_experiments::buffer_pool::{BufferPool, PAGE_SIZE};
use hdrhistogram::Histogram;
use rand::prelude::*;
use rand_distr::Zipf;
use std::io::Write;
use std::time::Instant;

const FILE_PAGES: u64 = 262_144; // 1 GiB file
const POOL_PAGES: usize = 65_536; // 256 MiB budget = 1/4 of file
const OPS: usize = 2_000_000;

fn make_file(path: &std::path::Path) {
    let mut f = std::fs::File::create(path).unwrap();
    let chunk = vec![0x5Au8; PAGE_SIZE * 256];
    for _ in 0..(FILE_PAGES / 256) {
        f.write_all(&chunk).unwrap();
    }
    f.sync_all().unwrap();
}

fn zipf_pages(n: usize) -> Vec<u64> {
    let mut rng = StdRng::seed_from_u64(7);
    let dist = Zipf::new(FILE_PAGES as u64, 0.99).unwrap();
    (0..n)
        .map(|_| (rng.sample(dist) as u64 - 1) % FILE_PAGES)
        .collect()
}

fn report(name: &str, hist: &Histogram<u64>) {
    println!(
        "{:<12} p50 {:>8} ns   p99 {:>8} ns   p99.9 {:>9} ns   max {:>10} ns",
        name,
        hist.value_at_quantile(0.5),
        hist.value_at_quantile(0.99),
        hist.value_at_quantile(0.999),
        hist.max()
    );
}

fn main() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.dat");
    eprintln!("creating {} MiB file …", FILE_PAGES as usize * PAGE_SIZE >> 20);
    make_file(&path);
    let pages = zipf_pages(OPS);

    // -- mmap ---------------------------------------------------------------
    let file = std::fs::File::open(&path).unwrap();
    let map = unsafe { memmap2::Mmap::map(&file).unwrap() };
    let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
    let mut sink = 0u64;
    for &p in &pages {
        let off = p as usize * PAGE_SIZE;
        let t = Instant::now();
        sink = sink.wrapping_add(u64::from_le_bytes(map[off..off + 8].try_into().unwrap()));
        hist.record(t.elapsed().as_nanos() as u64).unwrap();
    }
    report("mmap", &hist);

    // -- buffer pool ----------------------------------------------------------
    let mut pool = BufferPool::open(&path, POOL_PAGES).unwrap();
    let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
    for &p in &pages {
        let t = Instant::now();
        let v = pool
            .with_page(p, |b| u64::from_le_bytes(b[..8].try_into().unwrap()))
            .unwrap();
        hist.record(t.elapsed().as_nanos() as u64).unwrap();
        sink = sink.wrapping_add(v);
    }
    report("pool(CLOCK)", &hist);
    let s = pool.stats();
    println!(
        "pool hit rate: {:.2}%  ({} hits / {} misses)",
        100.0 * s.hits as f64 / (s.hits + s.misses) as f64,
        s.hits,
        s.misses
    );
    std::hint::black_box(sink);
}
