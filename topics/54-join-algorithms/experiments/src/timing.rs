//! Timing helpers (PROVIDED).
//!
//! `Stopwatch` measures the calling thread's **CPU time** — the time it was
//! actually running — rather than wall-clock time. On an idle machine the two
//! are the same; on a busy one (a build running next door) wall time includes
//! the slices the scheduler gave to someone else, and a single-threaded join
//! lane's number would be mostly noise. Multi-threaded lanes use wall time
//! (`std::time::Instant`) because their question *is* elapsed time.
//!
//! `prefer_performance_cores` asks macOS to run the thread at the
//! user-interactive QoS class, which keeps it off the efficiency cores. On
//! other platforms it does nothing.

pub fn prefer_performance_cores() {
    #[cfg(target_os = "macos")]
    // SAFETY: plain libc call with a valid QoS class constant, relative priority 0.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

fn thread_cpu_secs() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes one timespec into the struct we pass.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return 0.0;
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

#[derive(Clone, Copy)]
pub struct Stopwatch(f64);

impl Stopwatch {
    pub fn start() -> Self {
        Stopwatch(thread_cpu_secs())
    }
    pub fn secs(&self) -> f64 {
        thread_cpu_secs() - self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_advances_with_work_and_not_with_sleep() {
        let sw = Stopwatch::start();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(sw.secs() < 0.05, "sleeping is not CPU time");
        let sw = Stopwatch::start();
        let mut x = 0u64;
        for i in 0..50_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        assert!(sw.secs() > 0.0);
    }
}
