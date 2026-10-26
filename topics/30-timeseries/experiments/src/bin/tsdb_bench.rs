//! tsdb_bench — compression ratios, decode throughput, the OOO tax, and
//! tag-index selectivity.
//!
//! Lane 1 (provided): raw vs delta+varint timestamps, raw vs XOR-bytes
//!   values — what the clever codec has to beat, measured.
//! Lane F (provided): ALP-style decimal-to-integer vs XOR float codecs on
//!   decimal-origin vs arithmetic-origin vs random doubles.
//! Lane 2 (stub): Gorilla bytes/sample per workload shape + decode Msamples/s.
//! Lane 3 (stub): out-of-order ingestion sweep — the disorder tax priced.
//! Lane 4 (stub): tag index build + selector latency at 100K series.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;
use timeseries_experiments::alp::*;
use timeseries_experiments::baseline::*;
use timeseries_experiments::gen::*;
use timeseries_experiments::gorilla;
use timeseries_experiments::head::{Append, Head};
use timeseries_experiments::index::TagIndex;

const N: usize = 1_000_000;

fn main() {
    // An unimplemented exercise lane is an expected state, not a crash: every
    // stub lane below is wrapped in catch_unwind, so drop the default panic
    // hook to keep their todo!() traces out of the measured output.
    std::panic::set_hook(Box::new(|_| {}));
    println!("=== tsdb_bench: {N} samples/series shape, 10s scrape interval ===\n");

    let ts = scrape_timestamps(N, 1_700_000_000_000, 10_000, 100, 42);
    let shapes: Vec<(&str, Vec<f64>)> = vec![
        ("constant", constant_values(N)),
        ("gauge", gauge_values(N, 1)),
        ("counter", counter_values(N, 2)),
        ("random", random_values(N, 3)),
    ];

    // ---- Lane 1 (provided): baselines ---------------------------------
    // Both columns of the sample are encoded: timestamps as zigzag-varint
    // deltas, values either raw (8 B) or XOR-with-previous with leading zero
    // bytes dropped. The split is printed because the shapes only differ in
    // the value column.
    println!("-- baselines (provided): raw = 16.00 B/sample --");
    let mut ts_buf = Vec::new();
    encode_timestamps(&mut ts_buf, &ts);
    let ts_bps = ts_buf.len() as f64 / N as f64;
    println!("timestamps (all shapes): delta+varint {ts_bps:.2} B/sample");
    for (name, vs) in &shapes {
        let raw_buf = delta_varint_encode(&ts, vs);
        let buf = delta_xor_encode(&ts, vs);
        let t0 = Instant::now();
        let (t2, v2) = delta_xor_decode(&buf);
        let dt = t0.elapsed();
        assert_eq!(t2.len(), N);
        assert!(v2.iter().zip(vs).all(|(a, b)| a.to_bits() == b.to_bits()));
        println!(
            "{name:>9}: values raw {:.2} | values xor-bytes {:.2} B | total raw-values {:.2} / xor-bytes {:.2} B/sample | decode {:.0} Msamples/s",
            (raw_buf.len() - ts_buf.len()) as f64 / N as f64,
            (buf.len() - ts_buf.len()) as f64 / N as f64,
            raw_buf.len() as f64 / N as f64,
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

    // ---- Lane F (provided): decimal-to-integer vs XOR float codecs -----
    // Value stream only, B/value (raw = 8.00). Gorilla column is the stub's
    // total minus the 1 bit/sample its timestamps cost when perfectly regular.
    println!("-- float value codecs (provided; gorilla column is the stub) --");
    let regular = scrape_timestamps(N, 1_700_000_000_000, 10_000, 0, 1);
    let fshapes: Vec<(&str, Vec<f64>)> = vec![
        ("decimal 2dp", decimal_gauge_values(N, 11, 2)),
        ("decimal 6dp", decimal_gauge_values(N, 11, 6)),
        ("counter", counter_values(N, 2)),
        ("constant", constant_values(N)),
        ("walk (f64 arith)", gauge_values(N, 1)),
        ("random doubles", random_values(N, 3)),
    ];
    println!(
        "{:>17} {:>9} {:>9} {:>9} | {:>6} {:>5} {:>8} {:>9} | {:>11} {:>11}",
        "shape", "xor-bytes", "gorilla", "ALP", "exc%", "bits", "raw-vecs", "modal e,f", "xor dec M/s", "ALP dec M/s"
    );
    for (name, vs) in &fshapes {
        let xb = xor_bytes_encode(vs);
        let t0 = Instant::now();
        let back = xor_bytes_decode(&xb, N);
        let xdec = t0.elapsed();
        assert!(back.iter().zip(vs).all(|(a, b)| a.to_bits() == b.to_bits()));
        let col = alp_encode(vs);
        let t0 = Instant::now();
        let back = alp_decode(&col);
        let adec = t0.elapsed();
        assert!(back.iter().zip(vs).all(|(a, b)| a.to_bits() == b.to_bits()));
        let g = catch_unwind(AssertUnwindSafe(|| gorilla::encode_all(&regular, vs).bytes.len()));
        let gcol = match g {
            Ok(len) => format!("{:.2}", (len as f64 - N as f64 / 8.0) / N as f64),
            Err(_) => "[stub]".to_string(),
        };
        let ef = col.modal_ef().map(|((e, f), _)| format!("{e},{f}")).unwrap_or("-".into());
        println!(
            "{name:>17} {:>9.2} {:>9} {:>9.2} | {:>5.1}% {:>5.1} {:>4}/{:<3} {:>9} | {:>11.0} {:>11.0}",
            xb.len() as f64 / N as f64,
            gcol,
            col.bytes() as f64 / N as f64,
            100.0 * col.exceptions() as f64 / N as f64,
            col.mean_bit_width(),
            col.raw_vectors(),
            col.vectors.len(),
            ef,
            N as f64 / xdec.as_secs_f64() / 1e6,
            N as f64 / adec.as_secs_f64() / 1e6
        );
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
