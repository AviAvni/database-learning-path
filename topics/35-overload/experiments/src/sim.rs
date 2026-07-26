//! PROVIDED — a deterministic queueing simulator on a virtual clock.
//!
//! One server, open-loop arrivals, a client timeout, and retries: the
//! minimal system that exhibits a metastable failure (Bronson et al.,
//! HotOS'21, Fig 2). No real time passes and nothing is random — every
//! number the simulator produces is exact and reproducible.
//!
//! The crucial mechanics, in order:
//!  - arrivals are OPEN-LOOP: requests arrive on schedule whether or not
//!    the server is keeping up (topic 34's honest protocol);
//!  - the client abandons a request after `timeout_ns`, but the server
//!    doesn't know and does the work anyway — abandoned work still burns
//!    capacity (work amplification);
//!  - each abandoned attempt spawns a retry arriving at
//!    `arrival + timeout_ns`, up to `max_retries` per original request —
//!    the feedback loop that sustains the failure;
//!  - a one-shot outage (`outage_start_ns`, `outage_ns`) is the trigger:
//!    the server starts no new work inside that window.
//!
//! With one retry, the *hidden capacity* is capacity/2: any sustained
//! load above it can be tipped into a permanent goodput-zero state by a
//! large-enough trigger, because the retry storm alone exceeds capacity.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Clone, Copy)]
pub struct SimConfig {
    /// Server work per request, ns. Capacity = 1e9 / service_ns QPS.
    pub service_ns: u64,
    /// Original (non-retry) arrivals per second, evenly spaced.
    pub load_qps: u64,
    /// Client abandons after this long; the server finishes anyway.
    pub timeout_ns: u64,
    /// Retries per original request (retry i+1 only if retry i timed out).
    pub max_retries: u32,
    /// Trigger: server starts no new work in [outage_start, outage_start+outage).
    pub outage_start_ns: u64,
    pub outage_ns: u64,
    /// Original arrivals stop here; queued work still drains and is counted.
    pub duration_ns: u64,
    /// Request i gets priority i % levels (0 = highest, DAGOR-style).
    pub priority_levels: u8,
}

impl SimConfig {
    pub fn capacity_qps(&self) -> u64 {
        1_000_000_000 / self.service_ns
    }
}

/// Hooks for overload-control policies. The default is "do nothing" —
/// admit everything, retry freely — which is exactly how most systems
/// ship.
pub trait Policy {
    /// Arrival-time gate. `false` = fast reject: no server work, no
    /// retry, counted as rejected (the client gets an error in µs
    /// instead of a timeout in seconds).
    fn admit(&mut self, now_ns: u64, priority: u8) -> bool {
        let _ = (now_ns, priority);
        true
    }
    /// May this timed-out attempt spawn a retry? (Retry-budget hook.)
    fn allow_retry(&mut self, now_ns: u64) -> bool {
        let _ = now_ns;
        true
    }
    /// A request began processing after waiting `queuing_ns` — DAGOR's
    /// overload signal, observed at processing start.
    fn observe_queuing(&mut self, now_ns: u64, queuing_ns: u64) {
        let _ = (now_ns, queuing_ns);
    }
}

/// No overload control at all.
pub struct NoPolicy;
impl Policy for NoPolicy {}

#[derive(Default, Clone, Copy)]
pub struct Window {
    /// Arrivals (originals + retries) in this second.
    pub offered: u64,
    /// Fast-rejected by the policy in this second.
    pub rejected: u64,
    /// Requests that completed within the client timeout in this second.
    pub goodput: u64,
}

pub struct Report {
    /// One entry per second of `duration_ns`.
    pub windows: Vec<Window>,
    /// (successes, attempts) per priority level.
    pub per_priority: Vec<(u64, u64)>,
    /// Latencies of successful attempts, for percentile analysis.
    pub success_latencies: Vec<u64>,
}

pub fn run(cfg: SimConfig, policy: &mut impl Policy) -> Report {
    let interval = 1_000_000_000 / cfg.load_qps;
    let n_windows = (cfg.duration_ns / 1_000_000_000) as usize;
    let mut windows = vec![Window::default(); n_windows];
    let levels = cfg.priority_levels.max(1) as u64;
    let mut per_priority = vec![(0u64, 0u64); levels as usize];
    let mut success_latencies = Vec::new();

    // (arrival, seq, retries_left, priority), min-heap by arrival: the
    // server is FIFO in arrival order.
    let mut heap: BinaryHeap<Reverse<(u64, u64, u32, u8)>> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut t = 0u64;
    while t < cfg.duration_ns {
        heap.push(Reverse((t, seq, cfg.max_retries, (seq % levels) as u8)));
        seq += 1;
        t += interval;
    }

    let outage_end = cfg.outage_start_ns + cfg.outage_ns;
    let mut server_free = 0u64;
    while let Some(Reverse((arrival, _, retries_left, prio))) = heap.pop() {
        let w = (arrival / 1_000_000_000) as usize;
        if w < n_windows {
            windows[w].offered += 1;
        }
        per_priority[prio as usize].1 += 1;

        if !policy.admit(arrival, prio) {
            if w < n_windows {
                windows[w].rejected += 1;
            }
            continue;
        }

        let mut start = arrival.max(server_free);
        if start >= cfg.outage_start_ns && start < outage_end {
            start = outage_end;
        }
        policy.observe_queuing(start, start - arrival);
        let done = start + cfg.service_ns;
        server_free = done;

        let latency = done - arrival;
        if latency <= cfg.timeout_ns {
            let w = (done / 1_000_000_000) as usize;
            if w < n_windows {
                windows[w].goodput += 1;
            }
            per_priority[prio as usize].0 += 1;
            success_latencies.push(latency);
        } else if retries_left > 0 && policy.allow_retry(arrival + cfg.timeout_ns) {
            // The client gave up at arrival + timeout and resent. Note the
            // ORIGINAL is still ahead of the retry in the queue, burning
            // service time nobody will use.
            heap.push(Reverse((arrival + cfg.timeout_ns, seq, retries_left - 1, prio)));
            seq += 1;
        }
    }

    Report { windows, per_priority, success_latencies }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capacity 1000 QPS, load 800 (vulnerable: above the hidden capacity
    /// of 500), 50 ms timeout, one retry, a 1 s outage at t=2 s.
    const VULNERABLE: SimConfig = SimConfig {
        service_ns: 1_000_000,
        load_qps: 800,
        timeout_ns: 50_000_000,
        max_retries: 1,
        outage_start_ns: 2_000_000_000,
        outage_ns: 1_000_000_000,
        duration_ns: 10_000_000_000,
        priority_levels: 1,
    };

    #[test]
    fn vulnerable_state_is_fine_without_a_trigger() {
        // 800 QPS on a 1000 QPS server, no outage: every window is
        // perfect. The vulnerability is invisible — this is why systems
        // run there on purpose.
        let cfg = SimConfig { outage_ns: 0, ..VULNERABLE };
        let r = run(cfg, &mut NoPolicy);
        assert!(r.windows.iter().all(|w| w.goodput == 800));
    }

    #[test]
    fn trigger_above_hidden_capacity_collapses_permanently() {
        let r = run(VULNERABLE, &mut NoPolicy);
        // healthy before the trigger…
        assert_eq!(r.windows[0].goodput, 800);
        assert_eq!(r.windows[1].goodput, 800);
        // …zero goodput long after the 1 s outage ended: the retry storm
        // (800 originals + 800 retries = 1600 QPS on a 1000 QPS server)
        // sustains the failure without the trigger.
        assert_eq!(r.windows[8].goodput, 0);
        assert_eq!(r.windows[9].goodput, 0);
        // work amplification, exactly 2x: every original from the outage
        // on times out and is retried once.
        assert_eq!(r.windows[9].offered, 1600);
    }

    #[test]
    fn trigger_below_hidden_capacity_heals() {
        // Same trigger, load 400 < hidden capacity 500: the storm peaks
        // at 800 QPS < 1000, so the backlog drains and goodput returns.
        let cfg = SimConfig { load_qps: 400, ..VULNERABLE };
        let r = run(cfg, &mut NoPolicy);
        assert_eq!(r.windows[1].goodput, 400);
        assert_eq!(r.windows[3].goodput, 0); // still draining the backlog
        assert_eq!(r.windows[9].goodput, 400); // fully healed
    }
}
