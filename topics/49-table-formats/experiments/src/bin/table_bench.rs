//! table_bench — what a table format costs before the first data byte.
//!
//! cargo run --release --bin table_bench
//!
//! Lane 1 (provided): 1,000 small commits to an append-only table, in an
//! Iceberg-shaped tree and a Delta-shaped log on a metered object store.
//!   1a — metadata objects, bytes and dependent rounds a planner pays,
//!        as the table grows, for a one-day query, and for time travel;
//!   1b — the same after maintenance, and what the commits wrote;
//!   1c — the Parquet footer a reader decodes to find 10 of C columns.
//! Lane 2 (exercise): concurrent writers on a conditional put.
//! Lane 3 (exercise): a checkpoint policy that adapts to the read rate.

use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use table_formats_experiments::{
    delta::{Delta, DEFAULT_CHECKPOINT_INTERVAL},
    footer,
    gen::{commits, Workload, SEED},
    iceberg::{Iceberg, Plan},
    occ, policy,
    store::Store,
};

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        if msg.contains("not yet implemented") {
            STUBBED.store(true, Ordering::Relaxed);
        } else {
            eprintln!("{msg}");
        }
    }));
}

const N: usize = 1000;
/// Lane 3's table: the size its tests use.
const L3_N: usize = 600;
const PER_DAY: usize = 100;
/// Java Iceberg's `commit.manifest.min-count-to-merge` default
/// (core/src/main/java/org/apache/iceberg/TableProperties.java:119).
const MERGE_MIN: usize = 100;
/// Delta VLDB'20 §2.3: "Each read operation usually incurs at least 5–10 ms
/// of base latency". The floor below multiplies dependent rounds by the low
/// end; LIST calls are slower still ("tens to hundreds of milliseconds",
/// §2.1), so it is a lower bound, not an estimate.
const CITED_GET_FLOOR_MS: u64 = 5;

fn main() {
    quiet_stubs();
    println!("=== table_bench ===\n");
    lane1a_1b();
    lane1c();
    lane2();
    lane3();
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/occ.rs and src/policy.rs (Eoq) to unlock the lanes marked STUB]");
    }
}

fn row(label: &str, p: &Plan) {
    println!(
        "   {:<34} {:>7} {:>11} {:>6} {:>7} {:>8} {:>9}",
        label,
        p.objects,
        p.meter.get_bytes,
        p.meter.lists,
        p.meter.rounds,
        p.meter.rounds * CITED_GET_FLOOR_MS,
        p.files.len()
    );
}

fn header() {
    println!(
        "   {:<34} {:>7} {:>11} {:>6} {:>7} {:>8} {:>9}",
        "", "GETs", "bytes read", "LISTs", "rounds", "floor ms", "files"
    );
}

/// Local, in-memory CPU time to plan (JSON parse + reconcile): median of 7.
fn cpu_us(mut f: impl FnMut() -> Plan) -> f64 {
    let mut ts: Vec<f64> = (0..7)
        .map(|_| {
            let t = Instant::now();
            black_box(f());
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ts[3]
}

fn lane1a_1b() {
    let w = Workload { commits: N, files_per_commit: 1, commits_per_day: PER_DAY };
    let cs = commits(SEED, w);
    let truth = |n: usize| {
        let mut v: Vec<String> = cs[..n].iter().flatten().map(|f| f.path.clone()).collect();
        v.sort();
        v
    };
    println!("-- lane 1a: planning a scan after n small commits --");
    println!(
        "   workload: {N} commits x 1 data file, {PER_DAY} commits per day partition, seed {SEED}"
    );
    println!("   Iceberg = fast append (1 manifest per commit); Delta = 1 log entry per commit");
    println!("   floor ms = dependent rounds x {CITED_GET_FLOOR_MS} ms (Delta VLDB'20 §2.3 GET base latency, low end)\n");

    let mut ice_s = Store::new();
    let mut ice = Iceberg::new();
    let mut icm_s = Store::new();
    let mut icm = Iceberg::with_merge_append(MERGE_MIN);
    let mut d0_s = Store::new();
    let mut d0 = Delta::new(0);
    let mut d10_s = Store::new();
    let mut d10 = Delta::new(DEFAULT_CHECKPOINT_INTERVAL);
    let mut ice_ids = Vec::new();
    let mut mid_ice = None;
    let mut ok = true;
    header();
    for (i, c) in cs.iter().enumerate() {
        ice_ids.push(ice.append(&mut ice_s, c));
        icm.append(&mut icm_s, c);
        d0.append(&mut d0_s, c);
        d10.append(&mut d10_s, c);
        let n = i + 1;
        if n == N / 2 {
            mid_ice = Some(*ice_ids.last().unwrap());
        }
        if [10, 100, 1000].contains(&n) {
            println!("   after {n} commits:");
            let a = Iceberg::plan(&mut ice_s, None, None);
            let m = Iceberg::plan(&mut icm_s, None, None);
            let b = Delta::plan(&mut d0_s, None, None);
            let c = Delta::plan(&mut d10_s, None, None);
            row("Iceberg, fast append", &a);
            row("Iceberg, merge append (min 100)", &m);
            row("Delta, no checkpoints", &b);
            row("Delta, checkpoint every 10", &c);
            ok &= a.files == truth(n) && m.files == truth(n) && b.files == truth(n) && c.files == truth(n);
        }
    }
    println!("\n   at {N} commits, a query for one day (day 3 of 10):");
    let day = 3;
    let a = Iceberg::plan(&mut ice_s, None, Some(day));
    let m = Iceberg::plan(&mut icm_s, None, Some(day));
    let c = Delta::plan(&mut d10_s, None, Some(day));
    row("Iceberg, fast append (pruned)", &a);
    row("Iceberg, merge append (pruned)", &m);
    row("Delta, checkpoint every 10", &c);
    ok &= a.files == c.files && m.files == c.files && a.files.len() == PER_DAY;

    println!("\n   at {N} commits, time travel to version {}:", N / 2);
    let a = Iceberg::plan(&mut ice_s, mid_ice, None);
    let b = Delta::plan(&mut d0_s, Some(N as u64 / 2 - 1), None);
    let c = Delta::plan(&mut d10_s, Some(N as u64 / 2 - 1), None);
    row("Iceberg (snapshot in metadata)", &a);
    row("Delta, no checkpoints", &b);
    row("Delta, checkpoint every 10", &c);
    ok &= a.files == truth(N / 2) && b.files == truth(N / 2) && c.files == truth(N / 2);

    // local CPU to plan, before maintenance
    let cpu_ice = cpu_us(|| Iceberg::plan(&mut ice_s, None, None));
    let cpu_d0 = cpu_us(|| Delta::plan(&mut d0_s, None, None));
    let cpu_d10 = cpu_us(|| Delta::plan(&mut d10_s, None, None));

    println!("\n-- lane 1b: what the {N} commits wrote, and what maintenance buys --\n");
    println!(
        "   {:<34} {:>8} {:>13} {:>13} {:>13}",
        "metadata written by the commits", "PUTs", "bytes", "max commit", "objects now"
    );
    // Delta's _last_checkpoint and checkpoints are metadata; data files are
    // never written by this model, so every PUT here is metadata.
    let last_ice = ice_max_commit_bytes(&cs, None);
    let last_icm = ice_max_commit_bytes(&cs, Some(MERGE_MIN));
    let last_d10 = d10_last_commit_bytes(&cs);
    for (label, s, last) in [
        ("Iceberg, fast append", &ice_s, last_ice),
        ("Iceberg, merge append (min 100)", &icm_s, last_icm),
        ("Delta, no checkpoints", &d0_s, d0_last_commit_bytes(&cs)),
        ("Delta, checkpoint every 10", &d10_s, last_d10),
    ] {
        let prefix = if label.starts_with("Iceberg") { Iceberg::prefix() } else { Delta::prefix() };
        println!(
            "   {:<34} {:>8} {:>13} {:>13} {:>13}",
            label,
            s.meter.puts,
            s.meter.put_bytes,
            last,
            s.object_count(prefix)
        );
    }

    println!("\n   where the Iceberg bytes are (objects stored after {N} commits, by kind):");
    for (label, s) in [("fast append", &ice_s), ("merge append", &icm_s)] {
        println!(
            "   {:<14} table metadata {:>11} | manifest lists {:>11} | manifests {:>9}",
            label,
            s.bytes_with_suffix("ice/", ".metadata.json"),
            s.bytes_with_suffix("ice/", ".manifest-list"),
            s.bytes_with_suffix("ice/", ".manifest"),
        );
    }

    println!("\n   after maintenance at {N} commits:");
    header();
    ice.rewrite_manifests(&mut ice_s);
    let a = Iceberg::plan(&mut ice_s, None, None);
    row("Iceberg + rewrite manifests", &a);
    ice.expire_snapshots(&mut ice_s, 1);
    let a2 = Iceberg::plan(&mut ice_s, None, None);
    row("  + expire snapshots (keep 1)", &a2);
    d10.checkpoint(&mut d10_s);
    let c = Delta::plan(&mut d10_s, None, None);
    row("Delta + checkpoint at latest", &c);
    ok &= a.files == truth(N) && a2.files == truth(N) && c.files == truth(N);
    let still = Iceberg::snapshot_exists(&mut ice_s, mid_ice.unwrap());
    println!(
        "   Iceberg time travel to version {} after expiry: {}",
        N / 2,
        if still { "still there" } else { "gone (the snapshot was expired)" }
    );

    println!("\n   local CPU to plan the latest scan (in-memory store, JSON parse + replay, median of 7):");
    println!("   Iceberg {cpu_ice:.0} µs | Delta no checkpoints {cpu_d0:.0} µs | Delta every 10 {cpu_d10:.0} µs");
    let cpu_after = cpu_us(|| Iceberg::plan(&mut ice_s, None, None));
    let cpu_dafter = cpu_us(|| Delta::plan(&mut d10_s, None, None));
    println!("   after maintenance: Iceberg {cpu_after:.0} µs | Delta {cpu_dafter:.0} µs");
    println!(
        "\n   sanity: every planner returned exactly the committed files: {}",
        if ok { "yes" } else { "NO — BUG" }
    );
    assert!(ok, "a planner disagreed with the ground truth");
    println!();
}

/// The most metadata bytes any one commit wrote (replaying the workload and
/// diffing the meter around each commit).
fn ice_max_commit_bytes(cs: &[Vec<table_formats_experiments::gen::DataFile>], merge: Option<usize>) -> u64 {
    let mut s = Store::new();
    let mut t = match merge {
        Some(k) => Iceberg::with_merge_append(k),
        None => Iceberg::new(),
    };
    let mut max = 0;
    for c in cs {
        let before = s.meter.put_bytes;
        t.append(&mut s, c);
        max = max.max(s.meter.put_bytes - before);
    }
    max
}

fn d10_last_commit_bytes(cs: &[Vec<table_formats_experiments::gen::DataFile>]) -> u64 {
    // the largest commit is a checkpointing one: report the max over commits
    let mut s = Store::new();
    let mut d = Delta::new(DEFAULT_CHECKPOINT_INTERVAL);
    let mut max = 0;
    for c in cs {
        let before = s.meter.put_bytes;
        d.append(&mut s, c);
        max = max.max(s.meter.put_bytes - before);
    }
    max
}

fn d0_last_commit_bytes(cs: &[Vec<table_formats_experiments::gen::DataFile>]) -> u64 {
    let mut s = Store::new();
    let mut d = Delta::new(0);
    let mut max = 0;
    for c in cs {
        let before = s.meter.put_bytes;
        d.append(&mut s, c);
        max = max.max(s.meter.put_bytes - before);
    }
    max
}

fn lane1c() {
    println!("-- lane 1c: the Parquet footer a reader decodes to find 10 columns --");
    println!("   one row group, 1,024 rows of required float, parquet crate 59.1.0; decode times are");
    println!("   local CPU (footer already in memory), median of 9\n");
    println!(
        "   {:>7} {:>12} {:>9} {:>10} {:>11} {:>14} {:>12}",
        "columns", "footer bytes", "B/column", "full µs", "skip-stats", "+cached schema", "10-col share"
    );
    // 10 columns is the control: the footer of a file holding only the columns
    // the query projects, so the wide rows read as a ratio against it.
    let mut narrow = (0usize, 0.0f64);
    let mut wide = (0usize, 0.0f64);
    for cols in [10usize, 100, 1_000, 10_000, 50_000] {
        let file = footer::write_wide_file(cols, 1024, SEED);
        let ft = footer::footer_bytes(&file).to_vec();
        drop(file);
        let keep = footer::projection(cols, 10, SEED);
        let opts = footer::options(&keep, None);
        let schema = parquet::file::metadata::ParquetMetaDataReader::decode_schema(&ft).unwrap();
        let opts_s = footer::options(&keep, Some(schema));
        let time = |mode: footer::Decode, o: &parquet::file::metadata::ParquetMetaDataOptions| {
            let mut ts: Vec<f64> = (0..9)
                .map(|_| {
                    let t = Instant::now();
                    black_box(footer::decode(black_box(&ft), mode, o));
                    t.elapsed().as_secs_f64() * 1e6
                })
                .collect();
            ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
            ts[4]
        };
        let full = time(footer::Decode::Full, &opts);
        let skip = time(footer::Decode::SkipStats, &opts);
        let cached = time(footer::Decode::SkipStatsCachedSchema, &opts_s);
        let (n, with) = footer::decode(&ft, footer::Decode::SkipStats, &opts);
        assert_eq!((n, with), (cols, 10));
        if cols == 10 {
            narrow = (ft.len(), full);
        }
        if cols == 50_000 {
            wide = (ft.len(), full);
        }
        println!(
            "   {:>7} {:>12} {:>9.1} {:>10.0} {:>11.0} {:>14.0} {:>11.2}%",
            cols,
            ft.len(),
            ft.len() as f64 / cols as f64,
            full,
            skip,
            cached,
            1000.0 / cols as f64
        );
    }
    println!("   (10-col share = the fraction of column-chunk entries a 10-column projection needs)");
    println!(
        "   control: the 10-column file the query actually needs has a {} B footer, {:.1} µs to decode;",
        narrow.0, narrow.1
    );
    println!(
        "            the 50,000-column footer is {:.0}x the bytes and {:.0}x the full-decode time of that control\n",
        wide.0 as f64 / narrow.0 as f64,
        wide.1 / narrow.1
    );
}

fn lane2() {
    println!("-- lane 2: concurrent writers on put-if-absent (exercise) --");
    println!(
        "   {:<32} {:>9} {:>9} {:>8} {:>8} {:>10} {:>9}",
        "writers (append/compact/delete)", "committed", "conflicts", "aborted", "skipped", "confl/cmt", "val reads"
    );
    for (a, c, d) in [(1, 0, 0), (2, 0, 0), (4, 0, 0), (8, 0, 0), (16, 0, 0), (0, 2, 0), (4, 2, 0), (4, 2, 2)] {
        let cfg = occ::OccConfig { appenders: a, compactors: c, deleters: d, ops_per_writer: 25, seed: SEED };
        let label = format!("{a} / {c} / {d}");
        match catch_unwind(AssertUnwindSafe(|| occ::run_occ(cfg))) {
            Ok((r, mut s)) => {
                let audit = occ::audit_log(&mut s);
                println!(
                    "   {:<32} {:>9} {:>9} {:>8} {:>8} {:>10.2} {:>9} {}",
                    label,
                    r.committed,
                    r.conflicts,
                    r.aborted,
                    r.skipped,
                    r.conflicts as f64 / r.committed.max(1) as f64,
                    r.validation_reads,
                    if audit.is_ok() { "" } else { "AUDIT FAILED" }
                );
            }
            Err(_) => println!("   {label:<32}    STUB"),
        }
    }
    println!();
}

fn lane3() {
    println!("-- lane 3: when to checkpoint (exercise) --");
    println!("   cost = bytes moved + {:.0} B per request (5 ms x 50 MB/s, Delta VLDB'20 §2.3 low ends); {L3_N} commits", policy::REQUEST_BYTES);
    println!("   (MB-eq = millions of byte-equivalents; best fixed searched over k in {:?})\n", policy::FIXED_GRID);
    println!(
        "   {:<13} {:>10} {:>10} {:>16} {:>17} {:>17}",
        "reads/commit", "never", "every 10", "best fixed", "ski rental", "EOQ"
    );
    for rpc in [0.02, 0.2, 2.0] {
        let never = policy::simulate(&mut policy::Never, L3_N, rpc, SEED).cost();
        let ten = policy::simulate(&mut policy::Fixed(10), L3_N, rpc, SEED).cost();
        let (k, best) = policy::best_fixed(L3_N, rpc, SEED);
        let ski = policy::simulate(&mut policy::SkiRental::default(), L3_N, rpc, SEED);
        let eoq = match catch_unwind(AssertUnwindSafe(|| {
            policy::simulate(&mut policy::Eoq::default(), L3_N, rpc, SEED)
        })) {
            Ok(r) => format!("{:.1} ({} cp)", r.cost() / 1e6, r.checkpoints),
            Err(_) => "STUB".into(),
        };
        println!(
            "   {:<13} {:>10.1} {:>10.1} {:>16} {:>17} {:>17}",
            rpc,
            never / 1e6,
            ten / 1e6,
            format!("{:.1} (k={k})", best / 1e6),
            format!("{:.1} ({} cp)", ski.cost() / 1e6, ski.checkpoints),
            eoq
        );
    }
}
