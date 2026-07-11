//! Provided (runs without stubs): leader→follower log shipping over
//! channels with REAL fsync per policy. Topic 5's fsync ladder,
//! measured as replication lag: the follower's durability policy
//! sets the floor on ack latency the leader observes.
//!
//! Wire: leader thread writes N entries to its own log file (always
//! group-committed every 64) and ships each over an mpsc channel;
//! the follower appends to ITS file under the given fsync policy and
//! acks over a return channel. We measure leader-side ack latency
//! and end-to-end throughput per policy.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::mpsc;
use std::time::Instant;

const N: usize = 2_000;
const ENTRY_BYTES: usize = 128;

#[derive(Clone, Copy, Debug)]
enum Fsync {
    Every(usize),
    Never,
}

fn follower(mut file: File, policy: Fsync, rx: mpsc::Receiver<Vec<u8>>, ack: mpsc::Sender<u64>) {
    let mut since_sync = 0usize;
    let mut seq = 0u64;
    for entry in rx {
        file.write_all(&entry).unwrap();
        since_sync += 1;
        match policy {
            Fsync::Every(k) if since_sync >= k => {
                file.sync_all().unwrap(); // F_FULLFSYNC on macOS
                since_sync = 0;
            }
            _ => {}
        }
        seq += 1;
        ack.send(seq).unwrap();
    }
    if matches!(policy, Fsync::Every(_)) {
        file.sync_all().unwrap();
    }
}

fn run(policy: Fsync, dir: &std::path::Path) -> (f64, f64, f64) {
    let fpath = dir.join(format!("follower-{policy:?}.log"));
    let file = OpenOptions::new().create(true).write(true).truncate(true).open(&fpath).unwrap();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let (ack_tx, ack_rx) = mpsc::channel::<u64>();
    let handle = std::thread::spawn(move || follower(file, policy, rx, ack_tx));

    let mut leader_log =
        OpenOptions::new().create(true).write(true).truncate(true).open(dir.join("leader.log")).unwrap();
    let entry = vec![0xABu8; ENTRY_BYTES];
    let mut ack_lat_us = Vec::with_capacity(N);
    let start = Instant::now();
    for i in 0..N {
        leader_log.write_all(&entry).unwrap();
        if i % 64 == 63 {
            leader_log.sync_all().unwrap();
        }
        let sent = Instant::now();
        tx.send(entry.clone()).unwrap();
        ack_rx.recv().unwrap(); // wait for follower ack (WAIT 1 semantics)
        ack_lat_us.push(sent.elapsed().as_secs_f64() * 1e6);
    }
    let total = start.elapsed().as_secs_f64();
    drop(tx);
    handle.join().unwrap();

    ack_lat_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = ack_lat_us[N / 2];
    let p99 = ack_lat_us[N * 99 / 100];
    (N as f64 / total, p50, p99)
}

fn main() {
    let dir = std::env::temp_dir().join("repl_lag");
    std::fs::create_dir_all(&dir).unwrap();
    println!("{N} entries x {ENTRY_BYTES} B, leader group-commits every 64,");
    println!("follower fsync policy varies; ack = WAIT 1 semantics\n");
    println!("{:<16} {:>12} {:>12} {:>12}", "follower fsync", "entries/s", "ack p50 us", "ack p99 us");
    for policy in [Fsync::Every(1), Fsync::Every(8), Fsync::Every(64), Fsync::Never] {
        let (tput, p50, p99) = run(policy, &dir);
        println!("{:<16} {:>12.0} {:>12.1} {:>12.1}", format!("{policy:?}"), tput, p50, p99);
    }
}
