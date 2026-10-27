//! Lane 1b (PROVIDED): how much of Bf-Tree's mini-page advantage is arithmetic,
//! and how much depends on where the heat sits.
//!
//!   cargo run --release --bin heat_scatter_model
//!
//! Bf-Tree (Hao & Chandramouli, PVLDB 2024, `reading-bftree.md`) caches *records*
//! in variable-size mini-pages instead of whole 4 KiB pages. The paper's setup
//! (§6.1) is 200 M records of 32 B, a 2 GB cache, Zipf skew 0.9; its latency
//! experiment (§6.5) reports "almost 75%" of reads served from cache for
//! Bf-Tree against "around 50%" for the page-caching systems. This binary asks
//! what an **oracle** (a cache that always holds the most popular things that
//! fit) would achieve under that workload, for three layouts:
//!
//!   - a RECORD cache, 41 B per record (mini-pages that grew to the 2048 B class)
//!   - a RECORD cache, 136 B per record (every mini-page holds one record: the
//!     128 B class plus the 8 B circular-buffer header)
//!   - a PAGE cache (4 KiB + 8 B per page, ~71 records per page at 70% fill),
//!     with the heat either SCATTERED (popular keys spread over random pages,
//!     as hashed keys would be) or CLUSTERED (popular keys adjacent)
//!
//! The constants 41 / 136 / 71 are derived from the Bf-Tree source at commit
//! ca61307 (see the guide, Step 3); nothing here links against it. Part 1 sums
//! the Zipf pmf exactly at the paper's N = 200 M. Part 2 simulates page
//! placement at N = 20 M (1/10 scale, same cache fraction), seeded; every
//! printed figure reproduces exactly on every run. There is nothing to time.
//!
//! Predict in notes.md BEFORE running: the oracle hit ratio of the scattered
//! page cache. Does it land near the paper's "around 50%"?

use rand::prelude::*;

const S: f64 = 0.9; // Zipf skew, paper §6.1
const N_PAPER: u64 = 200_000_000; // records, paper §6.1
const REC_BYTES: u64 = 32; // 16 B key + 16 B value, paper §6.1
const CACHE_BYTES: u64 = 1 << 31; // 2 GiB; the code needs a power of two (circular_buffer/mod.rs:416)
const PAGE_BYTES: u64 = 4096;
const LEAF_HEADER: u64 = 32; // LeafNode, leaf_node.rs:222
const KV_META: u64 = 8; // per-record metadata, nodes/mod.rs:20
const CB_META: u64 = 8; // circular-buffer allocation header, circular_buffer/mod.rs:22

fn pmf_unnorm(rank: u64) -> f64 {
    (-S * (rank as f64).ln()).exp()
}

/// Sum of the first `k` terms of the unnormalised pmf, exact.
fn prefix(k: u64) -> f64 {
    (1..=k).map(pmf_unnorm).sum()
}

fn main() {
    let cell = REC_BYTES + KV_META; // 40 B per cached record
    let cells_per_page = (PAGE_BYTES - LEAF_HEADER) / cell; // 101
    let per_page = (cells_per_page as f64 * 0.7).round() as u64; // ~71 at 70% fill

    // Record footprints, from the mini-page size classes (reading-bftree.md, Step 3).
    let lone = 128 + CB_META; // one 32 B record lands in the 128 B class
    let full = (2048 + CB_META) as f64 / ((2048 - LEAF_HEADER) / cell) as f64; // 50 records in 2048 B
    let page_foot = PAGE_BYTES + CB_META;

    println!("Bf-Tree cache arithmetic (derived constants)");
    println!(
        "  cell {cell} B; {cells_per_page} cells fit a 4 KiB leaf, {per_page} at 70% fill"
    );
    println!("  record cache footprint: {lone} B lone / {full:.1} B in a full 2048 B mini-page");
    println!("  page   cache footprint: {page_foot} B per 4 KiB page ({} B per record)", page_foot / per_page);

    // ---- Part 1: exact, at the paper's N ----
    let total = prefix(N_PAPER);
    println!("\nPart 1 — Zipf({S}) over N = {N_PAPER} records, exact sums");
    // smallest k whose prefix reaches 80% of the mass
    let mut acc = 0.0;
    let mut k80 = 0u64;
    for r in 1..=N_PAPER {
        acc += pmf_unnorm(r);
        if acc >= 0.8 * total {
            k80 = r;
            break;
        }
    }
    println!(
        "  80% of requests reach the top {:.1}% of records (paper §6.1 parenthetical: 33%)",
        100.0 * k80 as f64 / N_PAPER as f64
    );
    let data = N_PAPER * REC_BYTES;
    println!(
        "  raw data {:.2} GiB; a 2 GiB cache is {:.1}% of it",
        data as f64 / (1u64 << 30) as f64,
        100.0 * CACHE_BYTES as f64 / data as f64
    );
    for (name, foot) in [("record cache, full mini-pages", full), ("record cache, lone-record mini-pages", lone as f64)] {
        let k = (CACHE_BYTES as f64 / foot) as u64;
        println!(
            "  oracle {name}: holds {:.1}% of records, hit ratio {:.3}",
            100.0 * k as f64 / N_PAPER as f64,
            prefix(k) / total
        );
    }

    // ---- Part 2: page placement, simulated at 1/10 scale ----
    let n = N_PAPER / 10;
    let cache = CACHE_BYTES / 10;
    let pages = n.div_ceil(per_page) as usize;
    let cache_pages = (cache / page_foot) as usize;
    let total_n = prefix(n);
    println!(
        "\nPart 2 — page cache at 1/10 scale: N = {n}, {pages} pages, cache holds {cache_pages} pages ({:.1}% of records)",
        100.0 * (cache_pages as u64 * per_page) as f64 / n as f64
    );

    // CLUSTERED: popularity rank order == key order, so page p holds ranks p*per_page..
    let clustered: f64 = (1..=cache_pages as u64 * per_page).map(pmf_unnorm).sum::<f64>() / total_n;

    // SCATTERED: rank -> page by a seeded random permutation of positions.
    let mut pos: Vec<u32> = (0..n as u32).collect();
    pos.shuffle(&mut StdRng::seed_from_u64(1));
    let mut mass = vec![0.0f64; pages];
    for (i, &p) in pos.iter().enumerate() {
        mass[(p as u64 / per_page) as usize] += pmf_unnorm(i as u64 + 1);
    }
    mass.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let scattered: f64 = mass[..cache_pages].iter().sum::<f64>() / total_n;

    let rec_full = prefix((cache as f64 / full) as u64) / total_n;
    let rec_lone = prefix(cache / lone) / total_n;
    println!("  oracle PAGE cache, heat CLUSTERED : {clustered:.3}");
    println!("  oracle PAGE cache, heat SCATTERED : {scattered:.3}");
    println!("  oracle RECORD cache, full mini-pages  : {rec_full:.3}");
    println!("  oracle RECORD cache, lone-record pages: {rec_lone:.3}");
    println!("  (paper §6.5, measured on real systems: ~0.50 page-caching, 'almost 0.75' Bf-Tree)");
}
