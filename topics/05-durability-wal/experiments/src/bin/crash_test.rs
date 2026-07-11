//! Crash injection harness — kill -9 a child mid-commit, verify recovery.
//!
//! Protocol:
//!   parent spawns  `crash_test child <wal-path> <ack-path>`
//!   child loop:    append(key) → commit → APPEND key to ack file (after
//!                  fsyncing the WAL, so the ack file is the ground truth of
//!                  what was acknowledged "to the client")
//!   parent:        sleeps a random few ms, SIGKILLs the child, replays the
//!                  WAL, and checks the durability contract:
//!
//!     1. every key in the ack file appears in the replay  (no lost acks)
//!     2. replayed keys are a prefix-consistent set: committed txns are
//!        all-or-nothing (both records of a 2-record txn present or neither)
//!
//!   Run: `cargo run --release --bin crash_test` — 100 rounds.
//!
//! This binary only compiles against YOUR src/wal.rs implementation — it is
//! the acceptance test for the topic ("Done when: 100/100 crash rounds pass").

use durability_experiments::wal::Wal;
use rand::Rng;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

const ROUNDS: usize = 100;

fn child(wal_path: &Path, ack_path: &Path) -> ! {
    let mut wal = Wal::open(wal_path).expect("open wal");
    let mut ack = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ack_path)
        .expect("open ack");

    // txn i writes TWO records (atomicity check) then commits.
    for i in 0u64.. {
        wal.append(i, format!("k{i}:a").as_bytes()).unwrap();
        wal.append(i, format!("k{i}:b").as_bytes()).unwrap();
        wal.commit(i).unwrap();
        // ack AFTER durability — write + fsync the ack line
        ack.write_all(format!("{i}\n").as_bytes()).unwrap();
        ack.sync_data().unwrap();
    }
    unreachable!()
}

fn round(n: usize) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let wal_path = dir.path().join("wal");
    let ack_path = dir.path().join("ack");

    let exe = std::env::current_exe().unwrap();
    let mut kid = Command::new(exe)
        .arg("child")
        .arg(&wal_path)
        .arg(&ack_path)
        .spawn()
        .expect("spawn child");

    let ms = rand::thread_rng().gen_range(5..80);
    std::thread::sleep(std::time::Duration::from_millis(ms));
    unsafe { libc::kill(kid.id() as i32, libc::SIGKILL) };
    kid.wait().unwrap();

    // -- verify ------------------------------------------------------------
    let recs = match Wal::replay(&wal_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("round {n}: replay ERRORED: {e}");
            return false;
        }
    };

    let recovered_txns: HashSet<u64> = recs.iter().map(|r| r.txn_id).collect();

    // 1. no lost acks
    let acked = std::fs::read_to_string(&ack_path).unwrap_or_default();
    for line in acked.lines() {
        let txn: u64 = line.parse().unwrap();
        if !recovered_txns.contains(&txn) {
            eprintln!("round {n}: ACKED txn {txn} missing after recovery — durability violated");
            return false;
        }
    }

    // 2. atomicity: every recovered txn has exactly its 2 records
    for &txn in &recovered_txns {
        let count = recs.iter().filter(|r| r.txn_id == txn).count();
        if count != 2 {
            eprintln!("round {n}: txn {txn} recovered {count}/2 records — torn transaction");
            return false;
        }
    }

    true
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("child") {
        child(Path::new(&args[2]), Path::new(&args[3]));
    }

    let mut passed = 0;
    for n in 1..=ROUNDS {
        if round(n) {
            passed += 1;
        }
        if n % 10 == 0 {
            println!("{n:>3}/{ROUNDS} rounds, {passed} passed");
        }
    }
    println!("\n{passed}/{ROUNDS} passed");
    if passed != ROUNDS {
        std::process::exit(1);
    }
    println!("Durability contract holds. Put this in notes.md.");
}
