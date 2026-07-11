//! tier_bench — the disaggregated-storage latency ladder, priced.
//!
//! Lanes 1-2 are PROVIDED (they always print): local NVMe sim vs raw S3 sim.
//! Lanes 3-5 exercise the stubs (LRU cache tier, hedged GETs, CoW branching)
//! and survive unimplemented stubs via catch_unwind.
//!
//! All GET latencies are *simulated* (charged, not slept) — see sim.rs — so
//! the whole bench runs in seconds while the percentiles are honest.

use cloud_native_experiments::branch::{BranchStore, ROOT};
use cloud_native_experiments::cache::TieredReader;
use cloud_native_experiments::hedge::{hedged_get, HedgeStats};
use cloud_native_experiments::sim::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

const N_KEYS: u64 = 4_000_000; // ~23.5K blocks of 4 KiB => ~96 MB "SST" space
const READS: usize = 200_000;
const CACHE_BLOCKS: usize = 3_000; // ~12 MB local tier, ~1/8 of the blocks
const ZIPF_THETA: f64 = 0.99;

fn pctl_line(label: &str, lat: &mut Vec<u64>) {
    lat.sort_unstable();
    let mean = lat.iter().sum::<u64>() / lat.len() as u64;
    println!(
        "{label}: p50 {:.2} ms | p95 {:.2} ms | p99 {:.2} ms | mean {:.2} ms",
        percentile(lat, 0.50) as f64 / 1000.0,
        percentile(lat, 0.95) as f64 / 1000.0,
        percentile(lat, 0.99) as f64 / 1000.0,
        mean as f64 / 1000.0,
    );
}

fn main() {
    println!("=== tier_bench: {N_KEYS} keys, {READS} zipf({ZIPF_THETA}) point reads ===\n");

    // Pre-draw the key stream so every lane sees identical reads.
    let keys: Vec<u64> = {
        let mut z = Zipf::new(N_KEYS as usize, ZIPF_THETA, 42);
        (0..READS).map(|_| z.sample()).collect()
    };

    // ---- Lane 1 (provided): local NVMe tier ------------------------------
    {
        let mut store = BlockStore::new(LocalDisk::new(1));
        let mut lat = Vec::with_capacity(READS);
        for &k in &keys {
            let (data, us) = store.get(block_of(k));
            assert!(lookup_in_block(&data, k).is_some());
            lat.push(us);
        }
        pctl_line("local NVMe (all local)      ", &mut lat);
    }

    // ---- Lane 2 (provided): raw S3, no cache — the enemy ------------------
    let s3_mean;
    {
        let mut store = BlockStore::new(S3::new(2));
        let mut lat = Vec::with_capacity(READS);
        for &k in &keys {
            let (_, us) = store.get(block_of(k));
            lat.push(us);
        }
        s3_mean = lat.iter().sum::<u64>() / lat.len() as u64;
        pctl_line("raw S3 (no cache)           ", &mut lat);
    }

    // ---- Lane 3 (stub): S3 + LRU local cache ------------------------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut t = TieredReader::new(BlockStore::new(S3::new(2)), CACHE_BLOCKS);
        let mut lat = Vec::with_capacity(READS);
        for &k in &keys {
            let (v, us) = t.read(k);
            assert_eq!(v, value_for(k));
            lat.push(us);
        }
        let hit_rate = t.cache.hits as f64 / (t.cache.hits + t.cache.misses) as f64;
        let mean = lat.iter().sum::<u64>() / lat.len() as u64;
        pctl_line("S3 + LRU cache (1/8 blocks) ", &mut lat);
        println!(
            "  hit rate {:.1}% | remote GETs {} | mean speedup vs raw S3: {:.1}x",
            hit_rate * 100.0,
            t.remote.gets,
            s3_mean as f64 / mean as f64
        );
    }));
    if r.is_err() {
        println!("S3 + LRU cache: [stub — implement cache.rs]");
    }

    // ---- Lane 4 (stub): hedged GETs on straggler-heavy S3 ------------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        let n = 50_000;
        let mut base: Vec<u64> = {
            let mut s3 = S3::with_stragglers(9, 0.03);
            (0..n).map(|_| s3.sample_micros(BLOCK_SIZE)).collect()
        };
        base.sort_unstable();
        let p95 = percentile(&base, 0.95);
        let unhedged_p99 = percentile(&base, 0.99);

        let mut store = BlockStore::new(S3::with_stragglers(9, 0.03));
        let mut stats = HedgeStats::default();
        let mut lat: Vec<u64> =
            (0..n).map(|b| hedged_get(&mut store, b as u64, p95, &mut stats).1).collect();
        pctl_line("hedged S3 (backup at p95)   ", &mut lat);
        println!(
            "  unhedged p99 {:.2} ms -> hedged p99 {:.2} ms | hedge rate {:.2}% (extra GETs)",
            unhedged_p99 as f64 / 1000.0,
            percentile(&lat, 0.99) as f64 / 1000.0,
            stats.hedged as f64 / n as f64 * 100.0
        );
    }));
    if r.is_err() {
        println!("hedged S3: [stub — implement hedge.rs]");
    }

    // ---- Lane 5 (stub): CoW branching ladder -------------------------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut s = BranchStore::new();
        for p in 0..100_000u64 {
            s.put(ROOT, p, p);
        }
        let versions_before = s.version_count();

        let t0 = Instant::now();
        let mut tip = ROOT;
        for _ in 0..64 {
            let at = s.last_lsn();
            tip = s.create_branch(tip, at);
            for p in 0..1_000u64 {
                s.put(tip, p, p + 1);
            }
        }
        let build = t0.elapsed();

        let t0 = Instant::now();
        let mut checked = 0u64;
        for p in (0..100_000u64).step_by(5) {
            let v = s.latest(tip, p).unwrap();
            assert!(v == p || v == p + 1);
            checked += 1;
        }
        let read = t0.elapsed();
        println!(
            "CoW branching: 64 nested branches over {versions_before} versions in {:.1} ms; \
             {checked} tip reads (up to 64-hop ancestor walks) at {:.2} µs/read",
            build.as_secs_f64() * 1000.0,
            read.as_secs_f64() * 1e6 / checked as f64
        );
    }));
    if r.is_err() {
        println!("CoW branching: [stub — implement branch.rs]");
    }
}
