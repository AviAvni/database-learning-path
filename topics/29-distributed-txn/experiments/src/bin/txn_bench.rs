//! txn_bench — abort rates vs contention, and atomicity under crashes.
//!
//! Workload: bank transfers over ACCOUNTS keys sharded 2 ways, Zipfian
//! key choice (the contention dial). "Concurrency" is simulated: BATCH
//! transactions all take start timestamps, then prewrite in interleaved
//! order — first locker wins, losers abort (no retry), exactly the
//! optimistic regime TiKV calls percolator/optimistic txns.
//!
//! Lane 1 (provided): measured conflict probability of the workload itself
//!   (does a batch touch the same key twice?) — the enemy, quantified.
//! Lane 2 (stub): Percolator abort rate + committed/s across theta sweep,
//!   with the bank invariant checked.
//! Lane 3 (stub): 2PC with a crash injected at every point, every 100 txns,
//!   + recovery — atomicity survives, and the blocking window is counted.

use distributed_txn_experiments::kv::{Cluster, Zipf};
use distributed_txn_experiments::percolator;
use distributed_txn_experiments::tpc::{CrashPoint, Outcome, TpcCluster};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

const ACCOUNTS: u64 = 100_000;
const BATCH: usize = 8;
const TXNS: usize = 100_000;
const THETAS: [f64; 4] = [0.5, 0.9, 1.1, 1.3];

fn main() {
    println!("=== txn_bench: {TXNS} transfers over {ACCOUNTS} accounts, batches of {BATCH} ===\n");

    // ---- Lane 1 (provided): workload conflict probability -----------------
    println!("-- conflict probability of the workload itself (provided) --");
    for &theta in &THETAS {
        let mut z = Zipf::new(ACCOUNTS as usize, theta, 42);
        let mut conflicted = 0usize;
        let batches = TXNS / BATCH;
        for _ in 0..batches {
            let mut keys: Vec<u64> = Vec::with_capacity(BATCH * 2);
            for _ in 0..BATCH {
                let (a, b) = z.transfer_pair();
                keys.push(a);
                keys.push(b);
            }
            let n = keys.len();
            keys.sort_unstable();
            keys.dedup();
            if keys.len() < n {
                conflicted += 1;
            }
        }
        println!(
            "theta {theta}: {:.1}% of batches contain a key collision",
            conflicted as f64 / batches as f64 * 100.0
        );
    }
    println!();

    // ---- Lane 2 (stub): Percolator abort rate vs contention ---------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        println!("-- percolator: abort rate vs contention (stub lane) --");
        for &theta in &THETAS {
            let mut c = Cluster::new(2);
            // seed balances
            for k in 0..ACCOUNTS {
                let s = c.tso.get_ts();
                let w = c.tso.get_ts();
                c.shard_mut(k).data.insert((k, s), 100);
                c.shard_mut(k).writes.insert((k, w), s);
            }
            let mut z = Zipf::new(ACCOUNTS as usize, theta, 7);
            let (mut committed, mut aborted) = (0u64, 0u64);
            let t0 = Instant::now();
            for _ in 0..TXNS / BATCH {
                // take a batch of transfers, interleave prewrites via run_txn
                // sequentially — but conflicts persist WITHIN the batch since
                // aborted txns' locks are cleaned and committed ones advance ts.
                let transfers: Vec<(u64, u64)> =
                    (0..BATCH).map(|_| z.transfer_pair()).collect();
                for &(a, b) in &transfers {
                    let ts = c.tso.get_ts();
                    let va = percolator::get(&c, a, ts).ok().flatten().unwrap_or(0);
                    let vb = percolator::get(&c, b, ts).ok().flatten().unwrap_or(0);
                    match percolator::run_txn(&mut c, &[(a, va - 1), (b, vb + 1)]) {
                        Ok(_) => committed += 1,
                        Err(_) => aborted += 1,
                    }
                }
            }
            let dt = t0.elapsed();
            let ts = c.tso.get_ts();
            let keys: Vec<u64> = (0..ACCOUNTS).collect();
            let total = c.total_committed(&keys, ts);
            assert_eq!(total, ACCOUNTS as i64 * 100, "bank invariant violated");
            println!(
                "theta {theta}: committed {committed} | aborted {aborted} ({:.2}%) | {:.0} txn/s | invariant OK",
                aborted as f64 / (committed + aborted) as f64 * 100.0,
                committed as f64 / dt.as_secs_f64()
            );
        }
    }));
    if r.is_err() {
        println!("percolator lane: [stub — implement percolator.rs]");
    }
    println!();

    // ---- Lane 3 (stub): 2PC crash storm ------------------------------------
    let r = catch_unwind(AssertUnwindSafe(|| {
        println!("-- 2PC under a crash storm (stub lane) --");
        let mut c = TpcCluster::new(2);
        let n_keys = 10_000u64;
        for k in 0..n_keys {
            let s = c.shard_of(k);
            c.shards[s].committed.insert(k, 100);
        }
        let keys: Vec<u64> = (0..n_keys).collect();
        let crashes = [
            CrashPoint::AfterFirstPrepare,
            CrashPoint::AfterAllPrepares,
            CrashPoint::AfterDecisionLogged,
            CrashPoint::AfterFirstApply,
        ];
        let mut z = Zipf::new(n_keys as usize, 0.9, 13);
        let (mut committed, mut aborted, mut crashed, mut blocked_aborts) = (0u64, 0u64, 0u64, 0u64);
        for i in 0..20_000usize {
            let (a, b) = z.transfer_pair();
            let w = [(a, c.read(a) - 1), (b, c.read(b) + 1)];
            let crash = if i % 100 == 99 { Some(crashes[(i / 100) % 4]) } else { None };
            match c.run_txn(&w, crash) {
                Outcome::Committed => committed += 1,
                Outcome::Aborted => {
                    aborted += 1;
                    if c.locked_keys() > 0 {
                        blocked_aborts += 1; // collided with a crashed txn's locks
                    }
                }
                Outcome::Crashed => {
                    crashed += 1;
                    // leave the wreckage for a while: recover every 4th crash
                    if crashed % 4 == 0 {
                        c.recover();
                    }
                }
            }
        }
        c.recover();
        assert_eq!(c.locked_keys(), 0);
        assert_eq!(c.total(&keys), n_keys as i64 * 100, "2PC atomicity violated");
        println!(
            "20000 txns: committed {committed} | aborted {aborted} (of which {blocked_aborts} hit crash-blocked locks) | crashed {crashed} | invariant OK after recovery"
        );
    }));
    if r.is_err() {
        println!("2PC lane: [stub — implement tpc.rs]");
    }
}
