//! Topic 57 — Sorting at scale: normalized keys, radix sort and collation.
//!
//! Lane 1 (provided): comparison sorts over row structs — `u64` keys,
//! `(int, string)` keys, and strings under a case- and accent-insensitive
//! collation that is evaluated inside the comparator.
//!
//! Lane 2 (exercise): the same `u64` and `(int, string)` sorts as byte-wise
//! radix sorts over a memcmp-able *normalized key* (`normalize`, `radix`).
//!
//! Lane 3 (exercise): the collation workload again, but the collation is
//! evaluated once per string into a binary sort key (`collation::sort_key`)
//! and the sort compares bytes.

pub mod baseline;
pub mod collation;
pub mod gen;
pub mod normalize;
pub mod radix;

/// A row of the `u64` workload: the sort key and a payload that identifies the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row64 {
    pub key: u64,
    pub payload: u64,
}

/// A row of the `(int, string)` workload. The string lives out of line in an
/// arena (`off`, `len` index into it), as DuckDB's `string_t` heap does for
/// long strings. `off` is unique per row, so it doubles as a row id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntStr {
    pub a: i32,
    pub off: u32,
    pub len: u32,
}

impl IntStr {
    #[inline]
    pub fn string<'a>(&self, arena: &'a [u8]) -> &'a [u8] {
        &arena[self.off as usize..(self.off + self.len) as usize]
    }
}

/// A reference to one string of a [`StrSet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrRef {
    pub off: u32,
    pub len: u32,
}

/// A set of UTF-8 strings stored back to back, for the collation lane.
///
/// The fields are private so that [`StrSet::get`] can skip UTF-8 validation:
/// [`StrSet::new`] validates every row once, and nothing can change the bytes
/// or the rows afterwards. (Validating on every `get` would put a pass over
/// both strings inside every comparison and make the comparator look slower
/// than it is.)
#[derive(Clone, Debug)]
pub struct StrSet {
    bytes: Vec<u8>,
    rows: Vec<StrRef>,
}

impl StrSet {
    /// Build a set; panics if a row falls outside `bytes` or is not valid UTF-8.
    pub fn new(bytes: Vec<u8>, rows: Vec<StrRef>) -> StrSet {
        for r in &rows {
            let s = &bytes[r.off as usize..(r.off + r.len) as usize];
            assert!(std::str::from_utf8(s).is_ok(), "row at offset {} is not valid UTF-8", r.off);
        }
        StrSet { bytes, rows }
    }

    pub fn rows(&self) -> &[StrRef] {
        &self.rows
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The string a row refers to.
    #[inline]
    pub fn get(&self, r: StrRef) -> &str {
        let s = &self.bytes[r.off as usize..(r.off + r.len) as usize];
        // SAFETY: `new` validated every row of this set as UTF-8, `r` came from
        // `rows()`, and the fields are private, so the bytes never change.
        unsafe { std::str::from_utf8_unchecked(s) }
    }
}

/// One 64-bit mix step used by the order-independent checksums.
#[inline]
pub fn mix(a: u64, b: u64) -> u64 {
    let mut x = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_add(0xD6E8_FEB8_6659_FD93);
    x ^= x >> 32;
    x = x.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    x ^ (x >> 29)
}

/// CPU time consumed by the calling thread, in nanoseconds.
///
/// The benches time *thread CPU time*, not wall-clock time. A sort here is
/// single-threaded, so on a quiet machine the two agree; on a busy one wall
/// time includes the slices the scheduler gave to other processes and CPU time
/// does not. (Cache and memory-bandwidth contention from other processes still
/// leak in, which is why each load average is recorded next to its numbers.)
pub fn cpu_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes one timespec into the pointer we pass.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
