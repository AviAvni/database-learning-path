//! tsdb_bench — compression ratios, decode throughput, the OOO tax, and
//! tag-index selectivity.
//!
//! Lane 1 (provided): raw vs delta+varint baselines — what the clever
//!   codec has to beat, measured.
//! Lane 2 (stub): Gorilla bytes/sample per workload shape + decode Msamples/s.
//! Lane 3 (stub): out-of-order ingestion sweep — the disorder tax priced.
//! Lane 4 (stub): tag index build + selector latency at 100K series.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;
use timeseries_experiments::baseline::*;
use timeseries_experiments::gen::*;
use timeseries_experiments::gorilla;
use timeseries_experiments::head::{Append, Head};
use timeseries_experiments::index::TagIndex;

const N: usize = 1_000_000;

fn main() {
    println!("=== tsdb_bench: {N} samples/series shape, 10s scrape interval ===\n");

    let ts = scrape_timestamps(N, 1_700_000_000_000, 10_000, 100, 42);
    let shapes: Vec<(&str, Vec<f64>)> = vec![
        ("constant", constant_values(N)),
        ("gauge", gauge_values(N, 1)),
        ("counter", counter_values(N, 2)),
        ("random", random_values(N, 3)),
    ];

    // ---- Lane 1 (provided): baselines ---------------------------------
    println!("-- baselines (provided): raw = 16.00 B/sample --");
    for (name, vs) in &shapes {
        let buf = delta_varint_encode(&ts, vs);
        let t0 = Instant::now();
        let (t2, _) = delta_varint_decode(&buf);
        let dt = t0.elapsed();
        assert_eq!(t2.len(), N);
        println!(
            "{name:>9}: delta+varint {:.2} B/sample | decode {:.0} Msamples/s",
            buf.len() as f64 / N as f64,
            N as f64 / dt.as_secs_f64() / 1e6
        );
    }
    println!();

    // ---- Lane 2 (stub): Gorilla ----------------------------------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        println!("-- gorilla codec (stub lane) --");
        for (name, vs) in &shapes {
            let t0 = Instant::now();
            let b = gorilla::encode_all(&ts, vs);
            let enc = t0.elapsed();
            let t0 = Instant::now();
            let out = gorilla::decode(&b);
            let dec = t0.elapsed();
            assert_eq!(out.len(), N);
            println!(
                "{name:>9}: {:.2} B/sample ({:.1}x vs raw) | encode {:.0} / decode {:.0} Msamples/s",
                b.bytes.len() as f64 / N as f64,
                raw_size(N) as f64 / b.bytes.len() as f64,
                N as f64 / enc.as_secs_f64() / 1e6,
                N as f64 / dec.as_secs_f64() / 1e6
            );
        }
    }));
    if r.is_err() {
        println!("gorilla lane: [stub — implement gorilla.rs]");
    }
    println!();

    // ---- Lane 3 (stub): the out-of-order tax ---------------------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        println!("-- out-of-order ingestion tax (stub lane) --");
        let vs = gauge_values(N, 4);
        for p in [0.0, 0.01, 0.10, 0.50] {
            let arrivals = with_out_of_order(&ts, &vs, p, 60_000, 5);
            let mut h = Head::new(120_000);
            let t0 = Instant::now();
            let mut too_old = 0usize;
            for &(t, v) in &arrivals {
                if h.append(t, v) == Append::TooOld {
                    too_old += 1;
                }
            }
            let ingest = t0.elapsed();
            let t0 = Instant::now();
            let flushed = h.flush();
            let flush = t0.elapsed();
            assert!(flushed.windows(2).all(|w| w[0].0 < w[1].0));
            println!(
                "ooo {:>4.0}%: ingest {:.0} Msamples/s | flush {:.1} ms | dropped-too-old {too_old}",
                p * 100.0,
                N as f64 / ingest.as_secs_f64() / 1e6,
                flush.as_secs_f64() * 1e3
            );
        }
    }));
    if r.is_err() {
        println!("ooo lane: [stub — implement head.rs]");
    }
    println!();

    // ---- Lane 4 (stub): tag index --------------------------------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        println!("-- tag index at 100K series (stub lane) --");
        let sets = label_sets(100_000);
        let t0 = Instant::now();
        let mut idx = TagIndex::new();
        for (i, ls) in sets.iter().enumerate() {
            idx.add_series(i as u64, ls);
        }
        let build = t0.elapsed();
        println!(
            "build: {:.0} ms | postings entries: {}",
            build.as_secs_f64() * 1e3,
            idx.postings.len()
        );
        for sel in [
            vec![("job", "job-3"), ("env", "dev")],
            vec![("job", "job-3"), ("env", "prod"), ("region", "r0")],
            vec![("job", "job-7"), ("instance", "i-70007")],
        ] {
            let t0 = Instant::now();
            let mut n = 0;
            for _ in 0..100 {
                n = idx.intersect(&sel).len();
            }
            let dt = t0.elapsed() / 100;
            println!("{sel:?}: {n} series in {:.1} µs", dt.as_secs_f64() * 1e6);
        }
    }));
    if r.is_err() {
        println!("index lane: [stub — implement index.rs]");
    }
}
