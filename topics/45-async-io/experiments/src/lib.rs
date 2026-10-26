//! Topic 45 — Async I/O & modern SSDs.
//!
//! Lane 1 (provided): the cost model the io_uring paper argues about,
//! measured with what every OS has — synchronous `pread`, a pool of threads
//! each doing `pread` (queue depth N), POSIX AIO's batched `lio_listio`, and
//! the per-call CPU cost of a syscall against warm page cache.
//!
//! Lane 2 (exercise, Linux only): the same reads through io_uring, with the
//! submission batched — the thing the paper says matters, not the interface.
//!
//! Lane 3 (exercise): out-of-place writes on a simulated flash translation
//! layer — total write amplification = DB WAF x SSD WAF, as in "How to Write
//! to SSDs" (VLDB'26).

pub mod aio;
pub mod file;
pub mod ftl;
pub mod gen;
pub mod inplace;
pub mod oop;
pub mod qd;
pub mod stats;
pub mod uring;

/// The page size every lane reads and writes. 4 KiB because that is the
/// NVMe logical block size the papers use and the unit `O_DIRECT` aligns to.
pub const PAGE: usize = 4096;
