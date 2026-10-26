//! LANE 2 (exercise, Linux only) — the same random reads through io_uring,
//! with submission batched.
//!
//! Lane 1 bought queue depth with threads: N blocking `pread`s, N syscalls,
//! N kernel threads parked in the block layer. io_uring buys it with two
//! rings in shared memory: you write submission-queue entries (SQEs) into the
//! SQ ring, make *one* `io_uring_enter` call to hand the kernel all of them,
//! and later read completion-queue entries (CQEs) out of the CQ ring without
//! any syscall at all (Jasny et al., VLDB'26, §2.1 and Figure 2).
//!
//! The paper's central finding is that the ring by itself is not the win:
//! "naively replacing traditional I/O interfaces with io_uring does not
//! necessarily yield performance benefits" (abstract); its synchronous
//! io_uring buffer manager matches the POSIX one at 16.5 k tx/s (§3.3). What
//! wins is *architecture*: enough I/Os in flight (queue depth) and enough of
//! them per syscall (batching). This lane makes those two knobs separate
//! parameters so you can measure each one.
//!
//! Implement [`uring_reads`] with the `io-uring` crate (tokio-rs/io-uring,
//! the crate `reading-io-uring-crate.md` walks through):
//!
//!  1. `IoUring::new(entries)` with `entries >= qd`. Allocate `qd` page-aligned
//!     buffers ([`crate::file::AlignedBuf`]) — the file is opened `O_DIRECT`.
//!  2. Keep at most `qd` reads in flight. Each read is an
//!     `opcode::Read::new(fd, buf, 4096).offset(off)` SQE whose `user_data`
//!     names its buffer slot, so a completion tells you which buffer is done.
//!  3. **Batching**, in both directions. Do not push an SQE the moment a slot
//!     frees up: hold new SQEs back until `batch` of them are ready (or the
//!     offsets have run out, or nothing is in flight), then push them all and
//!     make ONE `submit_and_wait(min(batch, in_flight))` call — it submits the
//!     batch *and* sleeps until `batch` completions are ready, which frees
//!     exactly enough slots for the next batch. If you get control back with
//!     fewer than `batch` SQEs ready and reads still in flight, call
//!     `submit_and_wait(min(batch - ready, in_flight))` with an empty SQ: a
//!     pure wait. In steady state that is one `io_uring_enter` per `batch`
//!     reads; `batch == 1` is one per read, the paper's "single-operation
//!     submission" baseline.
//!  4. Reap every available CQE after each call: check `result() == 4096`,
//!     record latency from that slot's submit time, XOR the first 8 bytes of
//!     the buffer into the checksum ([`crate::file::head_u64`]), free the slot.
//!  5. Return a [`Run`] whose `syscalls` counts your `io_uring_enter` calls
//!     (every `submit`/`submit_and_wait` is one).
//!
//! The tests are the specification: same bytes as `pread` (checksum), and at
//! most about one `io_uring_enter` per `batch` reads. There are no reference
//! timings — see notes.md for why this machine could not produce them.

#[cfg(target_os = "linux")]
pub use imp::uring_reads;

#[cfg(target_os = "linux")]
mod imp {
    use crate::stats::Run;
    use std::fs::File;

    /// Read every offset once through io_uring, at most `qd` in flight and
    /// `batch` SQEs per `io_uring_enter` (see the module docs).
    #[allow(unused_variables)] // stub: the arguments are the specification
    pub fn uring_reads(f: &File, offsets: &[u64], qd: usize, batch: usize) -> Run {
        let _ring = io_uring::IoUring::new; // the crate is a dependency on Linux
        todo!("io_uring reads with batched submission (see module docs)")
    }
}

/// Can this process create a ring at all? Returns the OS error if not —
/// Docker's default seccomp profile, `kernel.io_uring_disabled`, and
/// pre-5.1 kernels all refuse `io_uring_setup` (PostgreSQL's
/// `method_io_uring.c:376-379` prints a hint for exactly the EPERM case).
#[cfg(target_os = "linux")]
pub fn probe() -> Result<(), std::io::Error> {
    io_uring::IoUring::new(8).map(|_| ())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::file::{create, open_cold};
    use crate::gen::random_offsets;
    use crate::qd::sync_reads;

    fn setup(name: &str) -> Option<(crate::file::TestFile, std::fs::File)> {
        if let Err(e) = probe() {
            eprintln!("io_uring unavailable ({e}); this test cannot run here");
            return None;
        }
        let tf = create(&std::env::temp_dir().join(name), 1024, 11).unwrap();
        let (f, _) = open_cold(&tf.path).unwrap();
        Some((tf, f))
    }

    #[test]
    fn uring_reads_the_same_bytes_as_pread() {
        let Some((_tf, f)) = setup("dlp45-uring-a.dat") else { return };
        let offs = random_offsets(3, 2000, 1024);
        let want = sync_reads(&f, &offs).checksum;
        for (qd, batch) in [(1, 1), (8, 1), (8, 8), (32, 4), (32, 32)] {
            let r = uring_reads(&f, &offs, qd, batch);
            assert_eq!(r.ops, 2000);
            assert_eq!(r.lat_ns.len(), 2000, "one latency per read");
            assert_eq!(r.checksum, want, "qd {qd} batch {batch} read different bytes");
        }
    }

    #[test]
    fn batching_divides_the_syscalls() {
        let Some((_tf, f)) = setup("dlp45-uring-b.dat") else { return };
        let offs = random_offsets(4, 4096, 1024);
        let one = uring_reads(&f, &offs, 32, 1);
        let many = uring_reads(&f, &offs, 32, 16);
        // every enter submits at least `batch` SQEs, except while draining the
        // last `qd` reads — so enters <= n / batch + qd (+1 for rounding)
        let bound = 4096 / 16 + 32 + 1;
        assert!(
            many.syscalls <= bound,
            "batch 16 made {} io_uring_enter calls for 4096 reads (bound {bound})",
            many.syscalls
        );
        assert!(
            many.syscalls * 4 < one.syscalls,
            "batch 16 ({}) should need far fewer enters than batch 1 ({})",
            many.syscalls,
            one.syscalls
        );
    }
}
