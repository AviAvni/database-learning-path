//! PROVIDED — a synthetic microservice dependency graph, a fault to
//! localize, and the traces you would actually have to work with.
//!
//! The shape is the one every service architecture converges on: a few
//! front ends, several tiers of internal services, a handful of shared
//! infrastructure leaves (a cache, a database, an auth service) that
//! almost everything depends on. Requests enter at a front end and fan
//! out along the dependency edges.
//!
//! One service is broken. Because failure propagates *up* the dependency
//! edges — if the thing you called failed, you failed — the symptom
//! appears at every service on every path from the fault to a front end.
//! That is the alert storm, and it is why "which service has the most
//! errors?" is the wrong question: the answer is almost always the front
//! end, which is the service furthest from the cause.
//!
//! ```text
//!      frontend-0   frontend-1        <- alerts loudest (most traffic)
//!          │  ╲       ╱   │
//!        svc-3  svc-7   svc-4         <- alerts
//!          ╲      │      ╱
//!            ╲    │    ╱
//!              cache-1                <- ACTUALLY BROKEN, alerts least
//! ```
//!
//! Sherlock (SIGCOMM'07) calls the middle state **troubled** rather than
//! down — "servers or links continue to function but users perceive poor
//! performance" — and models every node as a three-tuple
//! `(P_up, P_troubled, P_down)`. The generator here uses the same idea:
//! a fault makes a *fraction* of calls fail, and that fraction attenuates
//! as it propagates, so the loudest alert and the true cause are not the
//! same node.

use rand::Rng;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::collections::{HashMap, HashSet};

pub fn seeded_rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

#[derive(Clone, Copy, Debug)]
pub struct TopologyConfig {
    pub n_frontends: usize,
    /// Services per intermediate tier, outermost first.
    pub tier_sizes: [usize; 3],
    /// Shared infrastructure leaves (cache, db, auth) that many
    /// services depend on — the nodes whose failure hurts most.
    pub n_infra: usize,
    /// Dependency edges from each service to the tier below.
    pub fanout: usize,
    /// Probability an intermediate service also calls an infra leaf.
    pub infra_edge_prob: f64,
    /// Requests generated per measurement window.
    pub n_requests: usize,
    /// Fraction of calls into the broken service that come back SLOW.
    /// Note: slow, not failed. This is a gray failure — the component's
    /// own error rate stays at baseline while everyone who depends on it
    /// suffers, which is precisely what defeats per-node ranking.
    pub fault_severity: f64,
    /// Probability that a caller gives up on a slow callee and reports
    /// an error of its own. This is where the errors in the storm are
    /// actually generated — one hop ABOVE the cause.
    pub timeout_prob: f64,
    /// How much of a callee's failure rate a caller inherits. Below 1.0
    /// because retries, fallbacks and caches absorb some of it — which
    /// is exactly why the signal attenuates away from the cause.
    pub propagation: f64,
    /// Baseline failure rate everywhere, so the graph is not noiseless.
    pub baseline_error: f64,
}

impl Default for TopologyConfig {
    fn default() -> Self {
        TopologyConfig {
            n_frontends: 4,
            tier_sizes: [10, 16, 20],
            n_infra: 5,
            fanout: 3,
            infra_edge_prob: 0.45,
            n_requests: 40_000,
            fault_severity: 0.55,
            timeout_prob: 0.7,
            propagation: 0.8,
            baseline_error: 0.004,
        }
    }
}

pub struct Topology {
    pub n_services: usize,
    /// caller → callees.
    pub deps: Vec<Vec<u32>>,
    /// callee → callers (failure propagates along these).
    pub rdeps: Vec<Vec<u32>>,
    pub frontends: Vec<u32>,
    pub infra: Vec<u32>,
    /// GROUND TRUTH: the one service that is actually broken.
    pub root_cause: u32,
    pub names: Vec<String>,
}

impl Topology {
    pub fn is_frontend(&self, s: u32) -> bool {
        self.frontends.contains(&s)
    }
    pub fn name(&self, s: u32) -> &str {
        &self.names[s as usize]
    }
}

pub fn topology(rng: &mut ChaCha8Rng, cfg: &TopologyConfig) -> Topology {
    let mut names = Vec::new();
    let mut tiers: Vec<Vec<u32>> = Vec::new();

    let mut push_tier = |n: usize, label: &str, names: &mut Vec<String>| -> Vec<u32> {
        (0..n)
            .map(|i| {
                let id = names.len() as u32;
                names.push(format!("{label}-{i}"));
                id
            })
            .collect()
    };

    let frontends = push_tier(cfg.n_frontends, "frontend", &mut names);
    tiers.push(frontends.clone());
    for (t, &n) in cfg.tier_sizes.iter().enumerate() {
        let tier = push_tier(n, &format!("svc{}", t + 1), &mut names);
        tiers.push(tier);
    }
    let infra = push_tier(cfg.n_infra, "infra", &mut names);
    tiers.push(infra.clone());

    let n_services = names.len();
    let mut deps: Vec<Vec<u32>> = vec![Vec::new(); n_services];
    let mut seen: HashSet<(u32, u32)> = HashSet::new();

    // Each tier calls the next one down.
    for t in 0..tiers.len() - 1 {
        for &caller in &tiers[t] {
            let below = &tiers[t + 1];
            for _ in 0..cfg.fanout.min(below.len()) {
                let callee = below[rng.gen_range(0..below.len())];
                if seen.insert((caller, callee)) {
                    deps[caller as usize].push(callee);
                }
            }
        }
    }
    // And many services also reach straight into shared infrastructure.
    for t in 1..tiers.len() - 1 {
        for &caller in &tiers[t] {
            if rng.gen::<f64>() < cfg.infra_edge_prob {
                let callee = infra[rng.gen_range(0..infra.len())];
                if seen.insert((caller, callee)) {
                    deps[caller as usize].push(callee);
                }
            }
        }
    }

    let mut rdeps: Vec<Vec<u32>> = vec![Vec::new(); n_services];
    for (caller, cs) in deps.iter().enumerate() {
        for &callee in cs {
            rdeps[callee as usize].push(caller as u32);
        }
    }

    // The fault goes on the infra leaf with the most callers — the
    // realistic worst case, and the one that produces the widest storm.
    let root_cause = *infra
        .iter()
        .max_by_key(|&&i| rdeps[i as usize].len())
        .unwrap();

    Topology {
        n_services,
        deps,
        rdeps,
        frontends,
        infra,
        root_cause,
        names,
    }
}

/// One recorded request: the services it touched, whether it failed, and
/// where. This is a Dapper trace, minus the timing detail.
#[derive(Clone, Debug)]
pub struct Trace {
    /// Services visited, in call order (the spans).
    pub path: Vec<u32>,
    /// Edges exercised — what a dependency-graph reconstruction sees.
    pub edges: Vec<(u32, u32)>,
    pub failed: bool,
    /// Latency in microseconds.
    pub latency_us: u64,
}

pub struct Workload {
    pub traces: Vec<Trace>,
    /// Per-service: (calls, failures).
    pub calls: Vec<u64>,
    pub failures: Vec<u64>,
}

impl Workload {
    /// The number every dashboard shows.
    pub fn error_rate(&self, s: u32) -> f64 {
        let c = self.calls[s as usize];
        if c == 0 {
            0.0
        } else {
            self.failures[s as usize] as f64 / c as f64
        }
    }
    /// Services whose error rate is above the alerting threshold.
    pub fn alerting(&self, threshold: f64) -> Vec<u32> {
        (0..self.calls.len() as u32)
            .filter(|&s| self.calls[s as usize] > 0 && self.error_rate(s) > threshold)
            .collect()
    }
}

/// Generate a window of traffic against a topology with one broken
/// service. Failure propagates from callee to caller with probability
/// `propagation`, which is why the loudest alert is not the cause.
pub fn run_workload(rng: &mut ChaCha8Rng, t: &Topology, cfg: &TopologyConfig) -> Workload {
    let mut traces = Vec::with_capacity(cfg.n_requests);
    let mut calls = vec![0u64; t.n_services];
    let mut failures = vec![0u64; t.n_services];

    for _ in 0..cfg.n_requests {
        let entry = t.frontends[rng.gen_range(0..t.frontends.len())];
        let mut path = Vec::new();
        let mut edges = Vec::new();
        let mut latency = 0u64;

        // Depth-first descent, recording spans; returns whether this
        // subtree failed.
        fn walk(
            rng: &mut ChaCha8Rng,
            t: &Topology,
            cfg: &TopologyConfig,
            s: u32,
            depth: usize,
            path: &mut Vec<u32>,
            edges: &mut Vec<(u32, u32)>,
            calls: &mut [u64],
            failures: &mut [u64],
            latency: &mut u64,
        ) -> (bool, bool) {
            path.push(s);
            calls[s as usize] += 1;
            *latency += rng.gen_range(200..900);

            // The gray failure: the broken service is SLOW, and its own
            // error rate never leaves the baseline.
            let mut failed = rng.gen::<f64>() < cfg.baseline_error;
            let mut slow = false;
            if s == t.root_cause && rng.gen::<f64>() < cfg.fault_severity {
                slow = true;
                *latency += rng.gen_range(8_000..25_000);
            }

            if depth < 6 {
                for &callee in &t.deps[s as usize] {
                    if rng.gen::<f64>() < 0.75 {
                        edges.push((s, callee));
                        let (child_failed, child_slow) = walk(
                            rng, t, cfg, callee, depth + 1, path, edges, calls, failures, latency,
                        );
                        // A failing dependency propagates. A SLOW one
                        // makes the caller time out and report an error
                        // of its own — so the errors appear one hop
                        // above the thing that is actually broken.
                        if child_failed && rng.gen::<f64>() < cfg.propagation {
                            failed = true;
                        }
                        if child_slow {
                            slow = true;
                            if rng.gen::<f64>() < cfg.timeout_prob {
                                failed = true;
                            }
                        }
                    }
                }
            }
            if failed {
                failures[s as usize] += 1;
            }
            (failed, slow)
        }

        let (failed, _slow) = walk(
            rng,
            t,
            cfg,
            entry,
            0,
            &mut path,
            &mut edges,
            &mut calls,
            &mut failures,
            &mut latency,
        );
        traces.push(Trace {
            path,
            edges,
            failed,
            latency_us: latency,
        });
    }

    Workload {
        traces,
        calls,
        failures,
    }
}

/// Baseline 1: rank by absolute failure count. This is what an
/// error-count dashboard sorted descending gives you.
pub fn rank_by_failures(w: &Workload) -> Vec<(u32, f64)> {
    let mut v: Vec<(u32, f64)> = (0..w.calls.len() as u32)
        .map(|s| (s, w.failures[s as usize] as f64))
        .collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    v
}

/// Baseline 2: rank by error *rate*. Better, but still a per-node score
/// that ignores the graph.
pub fn rank_by_error_rate(w: &Workload) -> Vec<(u32, f64)> {
    let mut v: Vec<(u32, f64)> = (0..w.calls.len() as u32)
        .map(|s| (s, w.error_rate(s)))
        .collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    v
}

/// Where the true root cause sits in a ranking. 1 is best.
pub fn rank_of(ranking: &[(u32, f64)], target: u32) -> usize {
    ranking.iter().position(|&(s, _)| s == target).unwrap_or(usize::MAX) + 1
}

/// Per-service correlation between "this service was on the path" and
/// "the request failed". This is the signal a MonitorRank-style walker
/// weights its edges by, and it is deliberately the *weak* signal an
/// operator actually has: a request-level outcome plus a trace, with no
/// per-hop attribution of blame.
pub fn failure_correlation(t: &Topology, w: &Workload) -> Vec<f64> {
    let mut both = vec![0f64; t.n_services];
    let mut svc = vec![0f64; t.n_services];
    let mut fe_failed = 0f64;
    let n = w.traces.len() as f64;
    for tr in &w.traces {
        let touched: HashSet<u32> = tr.path.iter().copied().collect();
        if tr.failed {
            fe_failed += 1.0;
        }
        for &s in &touched {
            // A service is implicated on this request if it was on the
            // path; we have no per-span status, only the request outcome
            // — which is exactly the observability an operator has.
            svc[s as usize] += 1.0;
            if tr.failed {
                both[s as usize] += 1.0;
            }
        }
    }
    (0..t.n_services)
        .map(|s| {
            let (a, b) = (svc[s] / n, fe_failed / n);
            if a <= 0.0 || a >= 1.0 || b <= 0.0 || b >= 1.0 {
                return 0.0;
            }
            let cov = both[s] / n - a * b;
            let denom = (a * (1.0 - a) * b * (1.0 - b)).sqrt();
            if denom <= 0.0 {
                0.0
            } else {
                (cov / denom).clamp(-1.0, 1.0)
            }
        })
        .collect()
}

/// For each (frontend, service): the fraction of that frontend's
/// requests whose trace touched the service. This is the observable
/// Ferret scores against — Sherlock's agents measure client-side
/// response times per service, not per-hop blame.
pub fn participation(t: &Topology, w: &Workload) -> Vec<Vec<f64>> {
    let mut counts = vec![vec![0f64; t.n_services]; t.frontends.len()];
    let mut totals = vec![0f64; t.frontends.len()];
    let fe_index: HashMap<u32, usize> =
        t.frontends.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    for tr in &w.traces {
        let Some(&fi) = tr.path.first().and_then(|f| fe_index.get(f)) else {
            continue;
        };
        totals[fi] += 1.0;
        let touched: HashSet<u32> = tr.path.iter().copied().collect();
        for s in touched {
            counts[fi][s as usize] += 1.0;
        }
    }
    for (fi, row) in counts.iter_mut().enumerate() {
        if totals[fi] > 0.0 {
            for v in row.iter_mut() {
                *v /= totals[fi];
            }
        }
    }
    counts
}

/// Ground-truth dependency edges *reachable from a front end*. Edges
/// hanging off a service no request ever enters are real in the config
/// and invisible in production — which is itself something a
/// dependency-graph tool should tell you.
pub fn all_edges(t: &Topology) -> HashSet<(u32, u32)> {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = t.frontends.clone();
    for &f in &t.frontends {
        seen.insert(f);
    }
    let mut out = HashSet::new();
    while let Some(v) = stack.pop() {
        for &c in &t.deps[v as usize] {
            out.insert((v, c));
            if seen.insert(c) {
                stack.push(c);
            }
        }
    }
    out
}

/// Every edge in the configuration, reachable or not.
pub fn configured_edges(t: &Topology) -> HashSet<(u32, u32)> {
    let mut s = HashSet::new();
    for (caller, cs) in t.deps.iter().enumerate() {
        for &callee in cs {
            s.insert((caller as u32, callee));
        }
    }
    s
}

/// Distinct call paths observed in a workload — the thing that is much
/// harder to recover from samples than the edge set.
pub fn distinct_paths(traces: &[Trace]) -> HashMap<Vec<u32>, usize> {
    let mut m: HashMap<Vec<u32>, usize> = HashMap::new();
    for t in traces {
        *m.entry(t.path.clone()).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_storm_is_much_larger_than_the_cause() {
        let mut rng = seeded_rng(1);
        let cfg = TopologyConfig::default();
        let t = topology(&mut rng, &cfg);
        let w = run_workload(&mut rng, &t, &cfg);
        let alerting = w.alerting(0.05);
        assert!(
            alerting.len() > 10,
            "only {} services alerting — no storm to localize",
            alerting.len()
        );
        // And the service that is actually broken is NOT among them.
        assert!(
            !alerting.contains(&t.root_cause),
            "a gray failure must not trip its own alert"
        );
    }

    #[test]
    fn the_broken_service_looks_healthy() {
        // The gray failure, measured: the component that is actually
        // broken has an error rate at the baseline, because it is slow
        // rather than failing. Its own dashboard is green.
        let mut rng = seeded_rng(1);
        let cfg = TopologyConfig::default();
        let t = topology(&mut rng, &cfg);
        let w = run_workload(&mut rng, &t, &cfg);
        assert!(
            w.error_rate(t.root_cause) < 3.0 * cfg.baseline_error,
            "root cause error rate {} is not baseline-quiet",
            w.error_rate(t.root_cause)
        );
        let callers_rate: f64 = t.rdeps[t.root_cause as usize]
            .iter()
            .map(|&c| w.error_rate(c))
            .sum::<f64>()
            / t.rdeps[t.root_cause as usize].len() as f64;
        assert!(
            callers_rate > 10.0 * w.error_rate(t.root_cause),
            "callers at {callers_rate} vs cause at {}",
            w.error_rate(t.root_cause)
        );
    }

    #[test]
    fn the_loudest_alert_is_not_the_cause() {
        // The whole reason this topic exists. Ranking by failure count
        // puts the front end first, because it is on every path.
        let mut rng = seeded_rng(1);
        let cfg = TopologyConfig::default();
        let t = topology(&mut rng, &cfg);
        let w = run_workload(&mut rng, &t, &cfg);
        // Both per-node rankings bury the cause in the bottom half,
        // and error-rate ranking puts the front ends — the services
        // furthest from the fault — at the very top.
        let by_count = rank_by_failures(&w);
        let by_rate = rank_by_error_rate(&w);
        assert!(t.is_frontend(by_rate[0].0), "got {}", t.name(by_rate[0].0));
        assert!(
            rank_of(&by_count, t.root_cause) > t.n_services / 2,
            "root cause ranked {} of {} by failure count",
            rank_of(&by_count, t.root_cause),
            t.n_services
        );
        assert!(
            rank_of(&by_rate, t.root_cause) > t.n_services / 2,
            "root cause ranked {} of {} by error rate",
            rank_of(&by_rate, t.root_cause),
            t.n_services
        );
        // And every infra leaf looks identical from per-node stats, so
        // no amount of dashboard sorting can separate them.
        let rates: Vec<f64> = t.infra.iter().map(|&i| w.error_rate(i)).collect();
        let spread = rates.iter().cloned().fold(0.0f64, f64::max)
            - rates.iter().cloned().fold(1.0f64, f64::min);
        assert!(spread < 0.01, "infra error rates differ by {spread}");
    }

    #[test]
    fn traces_reconstruct_the_dependency_graph() {
        let mut rng = seeded_rng(2);
        let cfg = TopologyConfig::default();
        let t = topology(&mut rng, &cfg);
        let w = run_workload(&mut rng, &t, &cfg);
        let observed: HashSet<(u32, u32)> =
            w.traces.iter().flat_map(|tr| tr.edges.iter().copied()).collect();
        let truth = all_edges(&t);
        // Every observed edge must be real, and full tracing must reach
        // most of the graph. It cannot reach all of it: a service with no
        // caller is never exercised, which is itself a finding an
        // operator would want.
        assert!(observed.is_subset(&truth));
        assert_eq!(
            observed.len(),
            truth.len(),
            "full tracing must see every reachable edge"
        );
        assert!(
            configured_edges(&t).len() > truth.len(),
            "the generator should contain some unreachable configuration"
        );
    }
}
