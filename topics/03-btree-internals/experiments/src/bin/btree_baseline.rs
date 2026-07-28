//! Lane 1 (PROVIDED): the numbers your B+tree is aiming at, measured on a
//! production one — plus the page arithmetic that predicts them.
//!
//!   cargo run --release --bin btree_baseline
//!
//! Nothing here touches your `src/page.rs` or `src/btree.rs`, so it runs on a
//! fresh clone. It exists because the topic's claim — *height is the metric,
//! fanout is the lever* — is checkable before you write a line of B-tree code:
//!
//!   1. the fanout arithmetic, derived from the fixed 4 KiB page format
//!   2. the height ladder: lookup cost vs key count in redb, warm
//!   3. the long-key case: 32-byte keys sharing a 24-byte prefix, which is
//!      what suffix truncation exists to fix — priced on a real B-tree
//!
//! Predict in notes.md BEFORE running: the fanout for each key shape, the
//! height at 1e6 keys, and how much of the lookup ladder you expect to see
//! (all of this is warm — the OS page cache holds the whole file, so you are
//! measuring pointer chasing and in-page search, not the disk).

use std::time::Instant;

use rand::prelude::*;

const TABLE: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("t");
const PAGE_SIZE: usize = 4096;
const HEADER: usize = 8;
const PROBES: usize = 200_000;

/// Cells per leaf and interior fanout for the page format documented in
/// src/page.rs. Arithmetic, not measurement — labelled as such in the output.
fn geometry(key_len: usize, val_len: usize) -> (usize, usize) {
    // leaf cell:     key_len u16 ∥ val_len u16 ∥ key ∥ val   (+ 2 for its ptr)
    let leaf_cell = 2 + 2 + key_len + val_len + 2;
    // interior cell: child u32 ∥ key_len u16 ∥ key           (+ 2 for its ptr)
    let interior_cell = 4 + 2 + key_len + 2;
    let usable = PAGE_SIZE - HEADER;
    (usable / leaf_cell, usable / interior_cell)
}

/// Height of a B+tree holding `n` keys given leaf capacity and fanout —
/// counting levels of page reads a point lookup must do.
fn height(n: u64, leaf_cells: usize, fanout: usize) -> u32 {
    let mut pages = (n as f64 / leaf_cells as f64).ceil().max(1.0);
    let mut h = 1;
    while pages > 1.0 {
        pages = (pages / fanout as f64).ceil();
        h += 1;
    }
    h
}

fn short_key(i: u64) -> [u8; 8] {
    i.to_be_bytes()
}

/// 32 bytes, 24-byte shared prefix — the case that collapses fanout when
/// separators keep the whole key.
fn long_key(i: u64) -> [u8; 32] {
    let mut k = [b'p'; 32];
    k[24..].copy_from_slice(&i.to_be_bytes());
    k
}

fn dir_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Load `n` keys into a fresh redb file, then time random point lookups.
/// Returns (ns per lookup, file bytes).
fn measure_redb(n: u64, long: bool) -> (f64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("redb.db");
    let db = redb::Database::create(&path).unwrap();

    // sorted inserts in one transaction: the cheap path, so the load time
    // here is not what we are measuring
    let tx = db.begin_write().unwrap();
    {
        let mut t = tx.open_table(TABLE).unwrap();
        for i in 0..n {
            let v = i.to_le_bytes();
            if long {
                t.insert(&long_key(i)[..], &v[..]).unwrap();
            } else {
                t.insert(&short_key(i)[..], &v[..]).unwrap();
            }
        }
    }
    tx.commit().unwrap();

    let mut rng = StdRng::seed_from_u64(11);
    let ps: Vec<u64> = (0..PROBES).map(|_| rng.gen_range(0..n)).collect();

    let rtx = db.begin_read().unwrap();
    let t = rtx.open_table(TABLE).unwrap();
    // warm every level of the tree before timing
    for &i in ps.iter().take(1000) {
        let _ = t.get(&short_key(i)[..]).unwrap();
    }

    let start = Instant::now();
    let mut found = 0u64;
    for &i in &ps {
        let hit = if long {
            t.get(&long_key(i)[..]).unwrap()
        } else {
            t.get(&short_key(i)[..]).unwrap()
        };
        found += hit.is_some() as u64;
    }
    let ns = start.elapsed().as_secs_f64() * 1e9 / PROBES as f64;
    assert_eq!(found, PROBES as u64, "every probe key was inserted");
    drop(t);
    drop(rtx);
    (ns, dir_size(&path))
}

fn main() {
    println!("== page arithmetic (from the format in src/page.rs, not measured) ==");
    println!(
        "  {:<26} {:>10} {:>9} {:>16} {:>16}",
        "key shape", "leaf cells", "fanout", "height @ 1e6", "height @ 1e9"
    );
    for (name, kl, vl) in [
        ("8 B key, 8 B value", 8, 8),
        ("32 B key, 8 B value", 32, 8),
        ("8 B key, 100 B value", 8, 100),
    ] {
        let (leaf, fanout) = geometry(kl, vl);
        println!(
            "  {name:<26} {leaf:>10} {fanout:>9} {:>16} {:>16}",
            height(1_000_000, leaf, fanout),
            height(1_000_000_000, leaf, fanout)
        );
    }
    println!("  a 32 B key costs {}x the interior slots of an 8 B key — that ratio,", {
        let (_, f8) = geometry(8, 8);
        let (_, f32b) = geometry(32, 8);
        format!("{:.1}", f8 as f64 / f32b as f64)
    });
    println!("  not the byte count, is what suffix truncation is buying back.\n");

    println!("== the height ladder: redb point lookup vs key count, warm ==");
    println!(
        "  {:<12} {:>12} {:>14} {:>16}",
        "keys", "ns/lookup", "file MB", "height (our fmt)"
    );
    let (leaf, fanout) = geometry(8, 8);
    for n in [10_000u64, 100_000, 1_000_000, 4_000_000] {
        let (ns, bytes) = measure_redb(n, false);
        println!(
            "  {n:<12} {ns:>12.0} {:>14.1} {:>16}",
            bytes as f64 / 1e6,
            height(n, leaf, fanout)
        );
    }

    println!("\n== the long-key case: 32 B keys, 24 B shared prefix, 1e6 keys ==");
    let (ns_short, b_short) = measure_redb(1_000_000, false);
    let (ns_long, b_long) = measure_redb(1_000_000, true);
    println!("  8 B keys : {ns_short:>8.0} ns/lookup   {:>8.1} MB", b_short as f64 / 1e6);
    println!("  32 B keys: {ns_long:>8.0} ns/lookup   {:>8.1} MB", b_long as f64 / 1e6);
    println!(
        "  ratio    : {:>8.2}x slower, {:>7.2}x bigger",
        ns_long / ns_short,
        b_long as f64 / b_short as f64
    );

    println!("\nnotes:");
    println!("- everything above is WARM: the file fits in the page cache, so this is");
    println!("  in-page binary search plus pointer chasing, not disk I/O. Say so when");
    println!("  you record it, or the numbers mean nothing (topic 0's rule).");
    println!("- the last column is the height in OUR page format, so it is a cross-check");
    println!("  on the arithmetic, not a prediction of redb's own layout.");
    println!("- READ THE LADDER CAREFULLY. The tidy version of this topic says cost is a");
    println!("  step function of height: flat while height is constant, jumping when it");
    println!("  grows. That is not what the middle column does — it keeps climbing from");
    println!("  1e6 to 4e6 keys while the height stays put. Height sets how many pages a");
    println!("  lookup TOUCHES; what those touches COST is set by whether the pages are");
    println!("  in CPU cache, and at 270 MB they are not. Two levers, not one — and the");
    println!("  second is the reason topic 6 exists. Write down both numbers.");
    println!("- these are the targets for your own DiskBTree. Record them in notes.md,");
    println!("  then run `cargo bench --bench disk_btree` once src/btree.rs works to");
    println!("  put your tree in the same table.");
}
