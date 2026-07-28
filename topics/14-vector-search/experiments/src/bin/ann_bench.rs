//! Provided: the recall@10 vs QPS curve.
//!
//!   cargo run --release --bin ann_bench
//!
//! Brute-force baseline runs first (the QPS floor + ground truth); the
//! index lanes report as stubs until you implement them. Predict in
//! notes.md: recall and QPS at each ef, and where quantized+rescore lands
//! relative to the HNSW curve.

use std::time::Instant;

use vector_experiments::hnsw::{Hnsw, HnswConfig};
use vector_experiments::quant::{self, ScalarQuant};
use vector_experiments::{brute, data, recall};

const N: u32 = 100_000;
const DIM: usize = 128;
const CLUSTERS: usize = 200;
const NUM_QUERIES: u32 = 500;
const K: usize = 10;

/// Run an exercise lane, reporting unimplemented `todo!()`s as a note
/// instead of a crash, so the brute-force baseline always prints.
fn stub_lane(name: &str, f: impl FnOnce()) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]");
    }
}

fn main() {
    let t = Instant::now();
    let d = data::clustered(N, DIM, CLUSTERS, 42);
    let q = data::queries(&d, NUM_QUERIES, 42);
    println!(
        "data: {}x{} ({} MB) + {} queries (gen {:.1} s)",
        N,
        DIM,
        N as usize * DIM * 4 / 1_000_000,
        NUM_QUERIES,
        t.elapsed().as_secs_f64()
    );

    let t = Instant::now();
    let truth: Vec<Vec<u32>> = (0..q.len()).map(|i| brute::top_k(&d, q.get(i), K)).collect();
    let brute_secs = t.elapsed().as_secs_f64();
    println!(
        "brute force: {:.2} s total, {:.0} QPS — recall 1.000 by definition\n",
        brute_secs,
        q.len() as f64 / brute_secs
    );

    // lane 2 (EXERCISE): HNSW — the recall/QPS curve against the oracle above
    stub_lane("HNSW (src/hnsw.rs)", || {
        let t = Instant::now();
        let h = Hnsw::build(&d, HnswConfig::default());
        println!("hnsw build: {:.1} s (m=16, ef_c=128), max_level={}", t.elapsed().as_secs_f64(), h.max_level);

        println!("\n  {:<10} {:>10} {:>12}", "ef", "recall@10", "QPS");
        for ef in [16, 32, 64, 128, 256] {
            let mut total_recall = 0.0;
            let t = Instant::now();
            for qi in 0..q.len() {
                let found = h.search(&d, q.get(qi), K, ef);
                total_recall += recall(&found, &truth[qi as usize]);
            }
            let secs = t.elapsed().as_secs_f64();
            println!(
                "  {ef:<10} {:>10.3} {:>12.0}",
                total_recall / q.len() as f64,
                q.len() as f64 / secs
            );
        }
    });

    // lane 3 (EXERCISE): scalar quantization — 4x smaller codes, then rescore
    stub_lane("scalar quantization (src/quant.rs)", || {
        let t = Instant::now();
        let sq = ScalarQuant::encode(&d);
        println!("\nscalar u8 encode: {:.1} s ({} MB codes)", t.elapsed().as_secs_f64(), sq.codes.len() / 1_000_000);
        for oversample in [1, 2, 4] {
            let mut total_recall = 0.0;
            let t = Instant::now();
            for qi in 0..q.len() {
                let found = quant::search_rescore(&d, &sq, q.get(qi), K, oversample);
                total_recall += recall(&found, &truth[qi as usize]);
            }
            let secs = t.elapsed().as_secs_f64();
            println!(
                "  u8 scan+rescore x{oversample}: recall {:.3}, {:.0} QPS",
                total_recall / q.len() as f64,
                q.len() as f64 / secs
            );
        }
    });

    println!("\nnotes:");
    println!("- record the full curve in notes.md; optional: same data via");
    println!("  qdrant docker + compare its ef curve");
}
