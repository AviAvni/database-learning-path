//! The file every read lane reads, and the two ways of opening it.
//!
//! The whole lane depends on one thing being true: that a "cold" read really
//! goes to the SSD. If it is served from the OS page cache instead it costs a
//! microsecond or two and every queue-depth figure becomes a memcpy benchmark.
//! So the file is written *around* the cache and opened *around* it again:
//!
//! - macOS: `fcntl(F_NOCACHE, 1)` on the fd before writing and before reading
//!   (no page-cache population), plus `F_RDAHEAD 0` (no read-ahead).
//! - Linux: write normally, `fsync`, `posix_fadvise(DONTNEED)` to drop the
//!   pages, then open the reader with `O_DIRECT` — falling back to a buffered
//!   fd plus `POSIX_FADV_RANDOM` on file systems that refuse `O_DIRECT`
//!   (tmpfs, some overlay mounts), in which case the bench says so.
//!
//! The bench then *checks* the claim instead of trusting it: a cold QD-1 read
//! faster than `COLD_FLOOR_US` is reported as a cache hit and the lane is
//! flagged invalid (see `bin/io_bench.rs`).

use crate::PAGE;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// A cold random 4 KiB read below this is not an SSD read. NVMe flash reads
/// are tens of microseconds; a page-cache hit is one or two.
pub const COLD_FLOOR_US: f64 = 8.0;

/// A page-aligned heap buffer, as `O_DIRECT` requires.
pub struct AlignedBuf {
    ptr: *mut u8,
    layout: Layout,
}

unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len.max(PAGE), PAGE).expect("layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "allocation of {len} bytes failed");
        AlignedBuf { ptr, layout }
    }
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.layout.size()) }
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
    pub fn len(&self) -> usize {
        self.layout.size()
    }
    pub fn is_empty(&self) -> bool {
        self.layout.size() == 0
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

/// Where the test file goes: `$DLP_IO_FILE` if set (point it at the device
/// you want to measure), else the system temp directory.
pub fn test_path() -> PathBuf {
    std::env::var_os("DLP_IO_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("dlp-topic45.dat"))
}

/// Removes the file when dropped, so an interrupted run does not leave a GiB
/// behind on the next successful one.
pub struct TestFile {
    pub path: PathBuf,
    pub pages: u64,
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(target_os = "macos")]
fn no_cache(f: &File) -> io::Result<()> {
    let fd = f.as_raw_fd();
    if unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_RDAHEAD, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn no_cache(_f: &File) -> io::Result<()> {
    Ok(())
}

/// Write `pages` pages of seeded bytes, bypassing the page cache where the OS
/// allows it, and make them durable.
pub fn create(path: &Path, pages: u64, seed: u64) -> io::Result<TestFile> {
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(path)?;
    no_cache(&f)?;
    const CHUNK: usize = 1 << 20;
    let mut buf = AlignedBuf::new(CHUNK);
    let total = pages * PAGE as u64;
    let mut written = 0u64;
    let mut i = 0u64;
    while written < total {
        crate::gen::fill_bytes(seed ^ i, buf.as_mut_slice());
        let n = CHUNK.min((total - written) as usize);
        f.write_all(&buf.as_slice()[..n])?;
        written += n as u64;
        i += 1;
    }
    f.sync_all()?;
    #[cfg(target_os = "linux")]
    unsafe {
        libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
    }
    Ok(TestFile {
        path: path.to_path_buf(),
        pages,
    })
}

/// How a cold fd ended up being opened — reported by the bench, because a
/// fallback changes what the numbers mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColdMode {
    /// macOS `F_NOCACHE` + `F_RDAHEAD 0`
    NoCache,
    /// Linux `O_DIRECT`
    Direct,
    /// Linux, `O_DIRECT` refused: buffered + `POSIX_FADV_RANDOM`
    BufferedFallback,
}

impl ColdMode {
    pub fn describe(self) -> &'static str {
        match self {
            ColdMode::NoCache => "F_NOCACHE + F_RDAHEAD=0 (macOS)",
            ColdMode::Direct => "O_DIRECT (Linux)",
            ColdMode::BufferedFallback => "buffered + FADV_RANDOM (O_DIRECT refused)",
        }
    }
}

/// Open for reads that go to the device, not the page cache.
pub fn open_cold(path: &Path) -> io::Result<(File, ColdMode)> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .open(path)
        {
            Ok(f) => return Ok((f, ColdMode::Direct)),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                let f = File::open(path)?;
                unsafe {
                    libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_RANDOM);
                    libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
                }
                return Ok((f, ColdMode::BufferedFallback));
            }
            Err(e) => return Err(e),
        }
    }
    #[allow(unreachable_code)]
    {
        let f = File::open(path)?;
        no_cache(&f)?;
        Ok((f, ColdMode::NoCache))
    }
}

/// Open an ordinary buffered fd — reads may be served by the page cache.
pub fn open_warm(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// One `pread` of `buf.len()` bytes at `off`; panics on a short read, since
/// every offset handed to it is inside the file.
#[inline]
pub fn pread(fd: i32, buf: &mut AlignedBuf, len: usize, off: u64) {
    let n = unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut libc::c_void, len, off as libc::off_t) };
    assert_eq!(
        n as usize,
        len,
        "pread at {off} returned {n}: {}",
        io::Error::last_os_error()
    );
}

/// The first 8 bytes of a buffer as a u64 — the per-page checksum term.
#[inline]
pub fn head_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}
