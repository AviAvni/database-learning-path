//! fsync ladder — PROVIDED, runs now.
//!
//! Measures the latency of each rung on YOUR disk:
//!   write() only | fdatasync | fsync | F_FULLFSYNC (macOS)
//!
//! Every durability design decision in this topic is downstream of these
//! numbers. Predict them in notes.md BEFORE running.

use hdrhistogram::Histogram;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::time::Instant;

const ROUNDS: usize = 500;
const RECORD: &[u8] = &[0xABu8; 256]; // one small commit record

#[derive(Clone, Copy)]
enum Sync {
    None,
    // macOS's libc crate doesn't expose fdatasync; fsync is the closest rung
    #[cfg(not(target_os = "macos"))]
    Fdatasync,
    Fsync,
    #[cfg(target_os = "macos")]
    FullFsync,
}

impl Sync {
    fn name(self) -> &'static str {
        match self {
            Sync::None => "write() only",
            #[cfg(not(target_os = "macos"))]
            Sync::Fdatasync => "fdatasync",
            Sync::Fsync => "fsync",
            #[cfg(target_os = "macos")]
            Sync::FullFsync => "F_FULLFSYNC",
        }
    }

    fn apply(self, fd: i32) {
        let rc = match self {
            Sync::None => 0,
            #[cfg(not(target_os = "macos"))]
            Sync::Fdatasync => unsafe { libc::fdatasync(fd) },
            Sync::Fsync => unsafe { libc::fsync(fd) },
            #[cfg(target_os = "macos")]
            Sync::FullFsync => unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) },
        };
        assert!(rc >= 0, "{} failed: {}", self.name(), std::io::Error::last_os_error());
    }
}

fn measure(sync: Sync) -> Histogram<u64> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ladder.log");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("open");
    let fd = file.as_raw_fd();

    let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
    // warmup
    for _ in 0..20 {
        file.write_all(RECORD).unwrap();
        sync.apply(fd);
    }
    for _ in 0..ROUNDS {
        let t = Instant::now();
        file.write_all(RECORD).unwrap();
        sync.apply(fd);
        hist.record(t.elapsed().as_nanos() as u64).unwrap();
    }
    hist
}

fn fmt_ns(ns: u64) -> String {
    if ns >= 1_000_000 {
        format!("{:>8.2} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:>8.2} µs", ns as f64 / 1e3)
    } else {
        format!("{:>8} ns", ns)
    }
}

fn main() {
    let rungs: &[Sync] = &[
        Sync::None,
        #[cfg(not(target_os = "macos"))]
        Sync::Fdatasync,
        Sync::Fsync,
        #[cfg(target_os = "macos")]
        Sync::FullFsync,
    ];

    println!(
        "{:<14} {:>11} {:>11} {:>11} {:>11}  implied max commits/s (1 fsync/commit)",
        "rung", "p50", "p99", "p99.9", "max"
    );
    for &sync in rungs {
        let h = measure(sync);
        let p50 = h.value_at_quantile(0.5);
        println!(
            "{:<14} {} {} {} {}  {:>10.0}",
            sync.name(),
            fmt_ns(p50),
            fmt_ns(h.value_at_quantile(0.99)),
            fmt_ns(h.value_at_quantile(0.999)),
            fmt_ns(h.max()),
            1e9 / p50 as f64,
        );
    }

    println!("\nGroup commit exists because of the last column.");
    println!("Copy this table into notes.md next to your predictions.");
}
