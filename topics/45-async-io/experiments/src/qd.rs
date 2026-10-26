//! LANE 1 (provided) — queue depth, and what it costs to get it.
//!
//! Three ways to read the same seeded random offsets:
//!
//! - [`sync_reads`]: one `pread` at a time. Queue depth 1: the thread waits
//!   out every device latency, so throughput is 1 / latency. This is the
//!   io_uring paper's §3.3 baseline ("Posix" and synchronous "io_uring" both
//!   give 16.5 k tx/s — the interface does not matter when nothing overlaps).
//! - [`thread_reads`]: N threads, each doing blocking `pread`. Queue depth N,
//!   bought with N kernel threads and N syscalls per N I/Os. This is what
//!   PostgreSQL's `io_method=worker` is, and what io_uring's io-wq falls back
//!   to for anything it cannot do asynchronously.
//! - [`warm_syscall_cost`]: the same 4 KiB `pread` against data already in
//!   the page cache, one syscall per page vs one `preadv` per batch of
//!   contiguous pages — the per-call CPU cost that batching amortizes, with
//!   the device taken out of the picture.

use crate::file::{head_u64, pread, AlignedBuf};
use crate::stats::Run;
use crate::PAGE;
use std::fs::File;
use std::hint::black_box;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Queue depth 1: issue, wait, repeat.
pub fn sync_reads(f: &File, offsets: &[u64]) -> Run {
    let fd = f.as_raw_fd();
    let mut buf = AlignedBuf::new(PAGE);
    let mut lat = Vec::with_capacity(offsets.len());
    let mut sum = 0u64;
    let t0 = Instant::now();
    for &off in offsets {
        let t = Instant::now();
        pread(fd, &mut buf, PAGE, black_box(off));
        lat.push(t.elapsed().as_nanos() as u64);
        sum ^= head_u64(buf.as_slice());
    }
    Run {
        ops: offsets.len(),
        secs: t0.elapsed().as_secs_f64(),
        lat_ns: lat,
        syscalls: offsets.len() as u64,
        checksum: sum,
    }
}

/// Queue depth `qd`: `qd` threads pull offsets from a shared cursor and each
/// does a blocking `pread`. Every offset is read exactly once, so the
/// checksum must equal [`sync_reads`]' on the same offsets.
pub fn thread_reads(f: &File, offsets: &[u64], qd: usize, bytes: usize) -> Run {
    let offsets: Arc<Vec<u64>> = Arc::new(offsets.to_vec());
    let cursor = Arc::new(AtomicUsize::new(0));
    let fd = f.as_raw_fd();
    let t0 = Instant::now();
    let handles: Vec<_> = (0..qd)
        .map(|_| {
            let offsets = Arc::clone(&offsets);
            let cursor = Arc::clone(&cursor);
            std::thread::spawn(move || {
                let mut buf = AlignedBuf::new(bytes);
                let mut lat = Vec::with_capacity(offsets.len() / qd + 1);
                let mut sum = 0u64;
                loop {
                    let i = cursor.fetch_add(1, Ordering::Relaxed);
                    if i >= offsets.len() {
                        break;
                    }
                    let t = Instant::now();
                    pread(fd, &mut buf, bytes, black_box(offsets[i]));
                    lat.push(t.elapsed().as_nanos() as u64);
                    sum ^= head_u64(buf.as_slice());
                }
                (lat, sum)
            })
        })
        .collect();
    let mut lat = Vec::with_capacity(offsets.len());
    let mut sum = 0u64;
    for h in handles {
        let (l, s) = h.join().expect("reader thread");
        lat.extend(l);
        sum ^= s;
    }
    Run {
        ops: offsets.len(),
        secs: t0.elapsed().as_secs_f64(),
        lat_ns: lat,
        syscalls: offsets.len() as u64,
        checksum: sum,
    }
}

/// Per-4 KiB CPU cost of reading from the page cache, in nanoseconds, for
/// `batch` pages per syscall. `batch == 1` is `pread`; `batch > 1` is one
/// `preadv` with `batch` iovecs over `batch` contiguous pages.
///
/// The caveat, stated where the code is: `preadv` over contiguous pages is
/// not io_uring batching. It amortizes the syscall entry/exit exactly as an
/// `io_uring_enter` with `batch` SQEs does, but it also does one file-offset
/// lookup instead of `batch`. So it is an upper bound on what syscall batching
/// alone buys, and the guide says so.
pub fn warm_syscall_cost(f: &File, region_pages: u64, batch: usize, pages_total: usize) -> f64 {
    let fd = f.as_raw_fd();
    let mut bufs: Vec<AlignedBuf> = (0..batch).map(|_| AlignedBuf::new(PAGE)).collect();
    let iov: Vec<libc::iovec> = bufs
        .iter_mut()
        .map(|b| libc::iovec {
            iov_base: b.as_mut_ptr() as *mut libc::c_void,
            iov_len: PAGE,
        })
        .collect();
    let calls = pages_total / batch;
    let span = region_pages / batch as u64; // batch-aligned starting pages
    let mut sum = 0u64;
    let t0 = Instant::now();
    for i in 0..calls {
        let off = ((i as u64 * 7919) % span) * (batch * PAGE) as u64;
        let n = if batch == 1 {
            unsafe { libc::pread(fd, iov[0].iov_base, PAGE, black_box(off) as libc::off_t) }
        } else {
            unsafe { libc::preadv(fd, iov.as_ptr(), batch as libc::c_int, black_box(off) as libc::off_t) }
        };
        assert_eq!(n as usize, batch * PAGE, "short warm read");
        sum ^= head_u64(bufs[0].as_slice());
    }
    black_box(sum);
    t0.elapsed().as_nanos() as f64 / (calls * batch) as f64
}

/// Read a region once through a buffered fd so it is resident in the page
/// cache for [`warm_syscall_cost`].
pub fn warm_up(f: &File, region_pages: u64) {
    let fd = f.as_raw_fd();
    let mut buf = AlignedBuf::new(1 << 20);
    let per = (1 << 20) / PAGE as u64;
    let mut p = 0;
    while p < region_pages {
        pread(fd, &mut buf, 1 << 20, p * PAGE as u64);
        p += per;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{create, open_cold};
    use crate::gen::random_offsets;

    #[test]
    fn threads_read_the_same_bytes_as_sync() {
        let path = std::env::temp_dir().join("dlp45-qd-test.dat");
        let tf = create(&path, 256, 7).unwrap();
        let (f, _) = open_cold(&tf.path).unwrap();
        let offs = random_offsets(1, 500, 256);
        let a = sync_reads(&f, &offs);
        let b = thread_reads(&f, &offs, 8, PAGE);
        assert_eq!(a.checksum, b.checksum);
        assert_eq!(a.ops, 500);
        assert_eq!(b.lat_ns.len(), 500);
    }
}
