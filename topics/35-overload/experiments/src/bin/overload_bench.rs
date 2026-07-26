use std::panic::{catch_unwind, AssertUnwindSafe};

use overload_experiments::admission::DagorGate;
use overload_experiments::sim::{run, NoPolicy, Policy, Report, SimConfig};
use overload_experiments::tokenbucket::TokenBucket;

/// Fig 2 of the metastable-failures paper (HotOS'21), on our simulator:
/// a 300 QPS server, clients that time out at 1 s and retry once.
const FIG2: SimConfig = SimConfig {
    service_ns: 3_333_333, // capacity ~300 QPS
    load_qps: 280,
    timeout_ns: 1_000_000_000,
    max_retries: 1,
    outage_start_ns: 30_000_000_000, // 10 s outage at t=30 s
    outage_ns: 10_000_000_000,
    duration_ns: 200_000_000_000,
    priority_levels: 1,
};

/// First second at/after the outage where goodput is back to the full
/// offered load and stays there to the end.
fn heal_time_s(r: &Report, load: u64) -> Option<usize> {
    let start = 30;
    (start..r.windows.len())
        .find(|&i| r.windows[i..].iter().all(|w| w.goodput == load))
}

/// Lane 1 (PROVIDED): the metastable failure. Same server, same 10 s
/// trigger; the only difference is whether load exceeds the hidden
/// capacity (capacity / 2 with one retry = 150 QPS).
fn lane1_metastable() {
    println!("== lane 1: metastable failure — one trigger, two loads ==");
    println!(
        "   capacity {} QPS, timeout 1 s, 1 retry, 10 s outage at t=30 s",
        FIG2.capacity_qps()
    );
    let hi = run(FIG2, &mut NoPolicy);
    let lo = run(SimConfig { load_qps: 140, ..FIG2 }, &mut NoPolicy);
    println!("           ── load 280 QPS ──   ── load 140 QPS ──");
    println!("   t(s)    offered  goodput      offered  goodput");
    for t in [0, 20, 29, 35, 45, 60, 90, 120, 160, 199] {
        let (h, l) = (hi.windows[t], lo.windows[t]);
        println!(
            "   {t:>4}    {:>7}  {:>7}      {:>7}  {:>7}",
            h.offered, h.goodput, l.offered, l.goodput
        );
    }
    match heal_time_s(&hi, 280) {
        Some(t) => println!("   280 QPS heals at t={t} s"),
        None => println!("   280 QPS: goodput never recovers — the outage ended at t=40 s"),
    }
    match heal_time_s(&lo, 140) {
        Some(t) => println!("   140 QPS heals at t={t} s"),
        None => println!("   140 QPS: never heals"),
    }
    println!();
}

struct RetryBudget(TokenBucket);
impl Policy for RetryBudget {
    fn allow_retry(&mut self, now_ns: u64) -> bool {
        self.0.try_acquire(now_ns)
    }
}

/// Lane 2 (needs tokenbucket.rs): break the sustaining loop. The retry
/// budget must fit inside the headroom (capacity − load = 20 QPS) or the
/// system still never drains.
fn lane2_retry_budget() {
    println!("== lane 2: retry budget on the 280 QPS scenario ==");
    let cfg = SimConfig { duration_ns: 800_000_000_000, ..FIG2 };
    println!("   headroom = 300 − 280 = 20 QPS; budgets straddle it:");
    for budget in [15u64, 25u64] {
        let r = run(cfg, &mut RetryBudget(TokenBucket::new(budget, budget)));
        match heal_time_s(&r, 280) {
            Some(t) => println!(
                "   budget {budget:>2} QPS: heals at t={t} s (drain rate {} req/s)",
                20i64 - budget as i64
            ),
            None => println!(
                "   budget {budget:>2} QPS: NEVER heals — 280 + {budget} > 300, still overloaded"
            ),
        }
    }
    println!();
}

struct Dagor(DagorGate);
impl Policy for Dagor {
    fn admit(&mut self, _now_ns: u64, priority: u8) -> bool {
        self.0.admit(priority)
    }
    fn observe_queuing(&mut self, _now_ns: u64, queuing_ns: u64) {
        self.0.observe(queuing_ns)
    }
}

/// Lane 3 (needs admission.rs): sustained 2x overload, 4 priority
/// levels. Without admission control FIFO + timeouts starve everyone;
/// with it, goodput ~= capacity and the highest priorities never notice.
fn lane3_admission() {
    println!("== lane 3: DAGOR-lite under sustained 2x overload ==");
    let cfg = SimConfig {
        load_qps: 600,
        max_retries: 0,
        outage_ns: 0,
        duration_ns: 300_000_000_000,
        priority_levels: 4,
        ..FIG2
    };
    println!("   600 QPS offered, 300 QPS capacity, 4 priorities, no retries");
    for (name, r) in [
        ("no control ", run(cfg, &mut NoPolicy)),
        (
            "DAGOR-lite ",
            run(cfg, &mut Dagor(DagorGate::new(20_000_000, 500, 4))),
        ),
    ] {
        // steady state: second half of the run
        let half = r.windows.len() / 2;
        let good: u64 = r.windows[half..].iter().map(|w| w.goodput).sum();
        let pct = |(s, a): (u64, u64)| 100.0 * s as f64 / a.max(1) as f64;
        let mut lat = r.success_latencies.clone();
        lat.sort_unstable();
        let p99 = if lat.is_empty() {
            0
        } else {
            lat[(lat.len() * 99 / 100).min(lat.len() - 1)]
        };
        println!(
            "   {name} goodput {:>3} QPS | success by prio: {:>5.1}% {:>5.1}% {:>5.1}% {:>5.1}% | p99 of admitted {:.1} ms",
            good / (r.windows.len() - half) as u64,
            pct(r.per_priority[0]),
            pct(r.per_priority[1]),
            pct(r.per_priority[2]),
            pct(r.per_priority[3]),
            p99 as f64 / 1e6,
        );
    }
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_metastable();
    stub_lane("lane 2", lane2_retry_budget);
    stub_lane("lane 3", lane3_admission);
}
