//! LANE 1 (provided) — POSIX AIO's batched submission, `lio_listio`.
//!
//! `lio_listio(LIO_WAIT, list, n)` hands the kernel `n` independent reads in
//! ONE system call and returns when all of them are done. On paper that is
//! exactly the io_uring paper's batched submission (§2.1, "multiple I/O
//! requests ... with a single io_uring_enter syscall"). What it is not is
//! batched *completion*: each request's result is collected with its own
//! `aio_return`, which on macOS is a system call per request — so this lane
//! counts both, and the bench prints syscalls per I/O.
//!
//! And submission is only half the story. How many of those `n` reads are in
//! flight at the device at once is decided by whoever *executes* them. On
//! macOS that is a fixed pool of kernel AIO worker threads (`sysctl
//! kern.aiothreads`, 4 on the machine this was measured on), each doing a
//! blocking read. glibc on Linux does the same thing with user-space threads.
//! So the interface accepts 16 reads at once and the device may still see
//! four — the same trap the io_uring paper describes as its io-wq fallback
//! (§2.2, path (2c)) and that PostgreSQL's `pgaio_uring_should_use_async`
//! steers into deliberately for buffered I/O.

use crate::file::{head_u64, AlignedBuf};
use crate::stats::Run;
use crate::PAGE;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::time::Instant;

/// The per-process limit on outstanding POSIX AIO requests on macOS
/// (`kern.aioprocmax`); `lio_listio` with more fails with `EAGAIN`.
pub const MAX_BATCH: usize = 16;

/// Read every offset, `batch` per `lio_listio(LIO_WAIT)` call.
pub fn lio_reads(f: &File, offsets: &[u64], batch: usize) -> Run {
    assert!((1..=MAX_BATCH).contains(&batch));
    let fd = f.as_raw_fd();
    let mut bufs: Vec<AlignedBuf> = (0..batch).map(|_| AlignedBuf::new(PAGE)).collect();
    let mut cbs: Vec<libc::aiocb> = (0..batch).map(|_| unsafe { std::mem::zeroed() }).collect();
    let mut lat = Vec::with_capacity(offsets.len());
    let mut sum = 0u64;
    let mut syscalls = 0u64;
    let t0 = Instant::now();
    for chunk in offsets.chunks(batch) {
        let mut list: Vec<*mut libc::aiocb> = Vec::with_capacity(chunk.len());
        for (i, &off) in chunk.iter().enumerate() {
            let cb = &mut cbs[i];
            *cb = unsafe { std::mem::zeroed() };
            cb.aio_fildes = fd;
            cb.aio_offset = off as libc::off_t;
            cb.aio_buf = bufs[i].as_mut_ptr() as *mut libc::c_void;
            cb.aio_nbytes = PAGE;
            cb.aio_lio_opcode = libc::LIO_READ;
            list.push(cb as *mut libc::aiocb);
        }
        let t = Instant::now();
        let rc = unsafe {
            libc::lio_listio(
                libc::LIO_WAIT,
                list.as_ptr(),
                list.len() as libc::c_int,
                std::ptr::null_mut(),
            )
        };
        syscalls += 1;
        assert_eq!(rc, 0, "lio_listio: {}", std::io::Error::last_os_error());
        let ns = t.elapsed().as_nanos() as u64;
        for (i, cb) in list.iter().enumerate() {
            let n = unsafe { libc::aio_return(*cb) };
            syscalls += 1;
            assert_eq!(n as usize, PAGE, "aio read {i} returned {n}");
            sum ^= head_u64(bufs[i].as_slice());
            lat.push(ns);
        }
    }
    Run {
        ops: offsets.len(),
        secs: t0.elapsed().as_secs_f64(),
        lat_ns: lat,
        syscalls,
        checksum: sum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{create, open_cold};
    use crate::gen::random_offsets;
    use crate::qd::sync_reads;

    #[test]
    fn lio_reads_the_same_bytes_as_pread() {
        let path = std::env::temp_dir().join("dlp45-aio-test.dat");
        let tf = create(&path, 256, 9).unwrap();
        let (f, _) = open_cold(&tf.path).unwrap();
        let offs = random_offsets(2, 160, 256);
        let a = sync_reads(&f, &offs);
        let b = lio_reads(&f, &offs, 16);
        assert_eq!(a.checksum, b.checksum);
        // one submission per 16 reads, one aio_return per read
        assert_eq!(b.syscalls, 10 + 160);
    }
}
