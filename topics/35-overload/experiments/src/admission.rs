//! STUB — DAGOR-lite: priority admission control driven by queuing time
//! (Zhou et al., "Overload Control for Scaling WeChat Microservices",
//! SoCC'18).
//!
//! The three DAGOR decisions this stub reproduces:
//!  - the overload signal is **average request queuing time** over a
//!    window (arrival → processing start), NOT response time (recursive
//!    along the call path → false positives) and NOT CPU (busy is not
//!    overloaded). WeChat's threshold: 20 ms; window: 1 s or 2000
//!    requests, whichever first. Here the window is a request count so
//!    the tests are deterministic.
//!  - shedding is **by priority, lowest first**: a request with priority
//!    `p` (0 = highest, like WeChat's Login) is admitted iff
//!    `p < cursor`. Priority 0 is never shed (cursor ≥ 1).
//!  - adaptation is **multiplicative down, additive up** on an expected
//!    admit count, not on the cursor directly: on an overloaded window,
//!    next window's expected admits = (1 − α)·N_admitted with α = 0.05;
//!    on a healthy window, expected admits grow by β·N with β = 0.01.
//!    A histogram of requested priorities (prefix sums) converts the
//!    expected count back into the largest cursor that fits — WeChat's
//!    Algorithm 1, minus the user-priority sublevels.

pub const ALPHA: f64 = 0.05;
pub const BETA: f64 = 0.01;

pub struct DagorGate {
    pub queuing_threshold_ns: u64,
    /// Observations per adaptation window.
    pub window: usize,
    /// Priority levels; requests carry 0..levels, 0 highest.
    pub levels: u8,
    // add whatever state you need (cursor, window accumulators,
    // per-priority request histogram, expected admit count, ...)
}

impl DagorGate {
    /// Starts fully open: cursor = levels, everything admitted.
    pub fn new(queuing_threshold_ns: u64, window: usize, levels: u8) -> Self {
        DagorGate { queuing_threshold_ns, window, levels }
    }

    /// Gate a request at arrival. Must be O(1) — this is the fast path
    /// that makes rejection cheaper than the work it replaces.
    pub fn admit(&mut self, priority: u8) -> bool {
        let _ = priority;
        todo!("count the request in the priority histogram; priority < cursor")
    }

    /// An admitted request began processing after `queuing_ns` in queue.
    /// Every `window` observations: compare the window's average queuing
    /// time to the threshold and adapt the cursor.
    pub fn observe(&mut self, queuing_ns: u64) {
        let _ = queuing_ns;
        todo!("accumulate; on window boundary run the DAGOR adaptation")
    }

    /// Current admission cursor: `levels` = admit all, 1 = only the
    /// highest priority. Never 0.
    pub fn cursor(&self) -> u8 {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: u64 = 20_000_000; // 20 ms, WeChat's number
    const FAST: u64 = 1_000_000; // 1 ms queuing: healthy
    const SLOW: u64 = 100_000_000; // 100 ms queuing: overloaded

    /// One round of traffic: 400 requests, priorities 0..4 round-robin,
    /// every admitted request reports `queuing_ns`.
    fn round(g: &mut DagorGate, queuing_ns: u64) {
        for i in 0..400u32 {
            if g.admit((i % 4) as u8) {
                g.observe(queuing_ns);
            }
        }
    }

    #[test]
    fn healthy_admits_everything() {
        let mut g = DagorGate::new(THRESHOLD, 100, 4);
        for _ in 0..10 {
            round(&mut g, FAST);
        }
        assert_eq!(g.cursor(), 4);
        for p in 0..4u8 {
            assert!(g.admit(p));
        }
    }

    #[test]
    fn overload_sheds_lowest_priority_first_and_never_the_highest() {
        let mut g = DagorGate::new(THRESHOLD, 100, 4);
        for _ in 0..50 {
            round(&mut g, SLOW);
        }
        assert!(g.cursor() < 4);
        assert!(g.cursor() >= 1);
        assert!(g.admit(0)); // Login never sheds
        assert!(!g.admit(3)); // lowest priority pays first
    }

    #[test]
    fn cursor_recovers_when_pressure_clears() {
        let mut g = DagorGate::new(THRESHOLD, 100, 4);
        for _ in 0..50 {
            round(&mut g, SLOW);
        }
        assert!(g.cursor() < 4);
        // pressure gone: additive recovery reopens the gate, eventually
        for _ in 0..500 {
            round(&mut g, FAST);
        }
        assert_eq!(g.cursor(), 4);
        assert!(g.admit(3));
    }
}
