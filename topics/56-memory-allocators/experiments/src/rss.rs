//! Resident memory, measured honestly on macOS and Linux.
//!
//! macOS has no `/proc`. Two numbers matter and they disagree:
//! * `resident` — pages currently mapped into the process (`ri_resident_size`).
//!   libmalloc releases free pages with `MADV_FREE_REUSABLE`, which the kernel
//!   may keep counted here until there is memory pressure.
//! * `footprint` — `ri_phys_footprint`, the number Activity Monitor and the
//!   jetsam killer use. It excludes reusable pages and includes compressed ones.
//!
//! Every phase of the bench runs in a fresh child process so a baseline can be
//! subtracted and an earlier phase cannot leave pages behind.

#[derive(Clone, Copy, Debug, Default)]
pub struct Mem {
    pub resident: u64,
    pub footprint: u64,
}

#[cfg(target_os = "macos")]
pub fn now() -> Mem {
    // SAFETY: proc_pid_rusage fills the buffer we pass; rusage_info_v0 is the
    // layout RUSAGE_INFO_V0 describes.
    unsafe {
        let mut info: libc::rusage_info_v0 = std::mem::zeroed();
        let rc = libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V0,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        if rc != 0 {
            return Mem::default();
        }
        Mem { resident: info.ri_resident_size, footprint: info.ri_phys_footprint }
    }
}

#[cfg(target_os = "linux")]
pub fn now() -> Mem {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    // SAFETY: sysconf has no preconditions.
    let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    // Linux has no separate footprint; RSS is the closest figure.
    Mem { resident: pages * ps, footprint: pages * ps }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn now() -> Mem {
    Mem::default()
}

/// Peak resident size of the process so far (`ru_maxrss`: bytes on macOS, KiB on Linux).
pub fn peak() -> u64 {
    // SAFETY: getrusage fills the struct we pass.
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut ru);
        let v = ru.ru_maxrss as u64;
        if cfg!(target_os = "macos") {
            v
        } else {
            v * 1024
        }
    }
}

pub fn mib(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}
