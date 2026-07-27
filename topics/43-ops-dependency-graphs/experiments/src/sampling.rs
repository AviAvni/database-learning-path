//! Dapper's sampling economics: what you keep, and what it costs you.
//!
//! Tracing every request is unaffordable and unnecessary. Dapper
//! measured the cost of not sampling on a web-search cluster:
//!
//! ```text
//!   sampling      avg latency     avg throughput
//!   1/1              +16.3%           −1.48%
//!   1/2               +9.40%          −0.73%
//!   1/4               +6.38%          −0.30%
//!   1/8               +4.12%          −0.23%
//!   1/16              +2.12%          −0.08%
//!   1/1024            −0.20%          −0.06%     (inside experimental error)
//! ```
//!
//! So they sampled 1 in 1024, and justified it with an argument about
//! *what you are looking for*: "for high-throughput services, aggressive
//! sampling does not hinder most important analyses. If a notable
//! execution pattern surfaces once in such systems, it will surface
//! thousands of times." And the caveat in the same breath: "Services
//! with lower volume — perhaps dozens rather than tens of thousands of
//! requests per second — can afford to trace every request."
//!
//! That is two different claims about two different questions, and this
//! module measures the gap between them:
//!
//! * **What is the dependency graph?** An aggregate question. Every edge
//!   is exercised constantly, so a tiny sample recovers nearly all of
//!   them — coupon collecting on a distribution with a fat head.
//! * **What happened on the slow path?** A rare-event question. If a
//!   pattern occurs in 1 request in 10,000 and you sample 1 in 1,024,
//!   you see it once per 10 million requests.
//!
//! One more Dapper detail that matters: sampling decisions are made
//! **per trace, not per span** — the collector hashes the trace id to a
//! scalar `z ∈ [0,1]` and keeps the whole trace if `z` is below the
//! coefficient. Sampling spans independently would shred every trace
//! into disconnected fragments and destroy exactly the causal structure
//! you are paying to collect.

use crate::services::Trace;
use rand_chacha::ChaCha8Rng;
use std::collections::HashSet;

/// **Trace-level sampling (STUB).**
///
/// Keep each *whole* trace with probability `rate`. Deterministic given
/// the rng, so two runs at the same seed sample the same traces.
pub fn sample(rng: &mut ChaCha8Rng, traces: &[Trace], rate: f64) -> Vec<Trace> {
    let _ = (rng, traces, rate);
    todo!(
        "keep each trace with probability `rate`, WHOLE. Do not decide per span - Dapper hashes the trace id precisely so a sampled trace is complete, and a shredded trace has no causal structure left to analyse."
    )
}

/// **Edge recovery (STUB).** What fraction of the true dependency edges
/// appear in the sampled traces?
pub fn edge_recall(sampled: &[Trace], truth: &HashSet<(u32, u32)>) -> f64 {
    let _ = (sampled, truth);
    todo!(
        "collect the distinct edges present in the sampled traces, intersect with `truth`, divide by |truth|. This is the aggregate question, and it should saturate at a startlingly low sampling rate."
    )
}

/// **Rare-path recovery (STUB).** What fraction of the *distinct call
/// paths* that occur at most `max_occurrences` times in the full
/// workload survive sampling?
///
/// This is the number that does not saturate, and the reason
/// low-traffic services cannot use a 1/1024 rate.
pub fn rare_path_recall(
    full: &[Trace],
    sampled: &[Trace],
    max_occurrences: usize,
) -> f64 {
    let _ = (full, sampled, max_occurrences);
    todo!(
        "find the distinct paths in `full` occurring at most max_occurrences times, then report the fraction of them appearing at least once in `sampled`. Unlike edge recall, expect this to fall roughly linearly with the rate - which is the whole reason one sampling rate cannot serve both questions."
    )
}

/// Mean latency over a trace set. Sampling leaves this **unbiased** —
/// the estimate is right on average — while the variance grows as 1/rate.
/// Knowing which of your metrics are unbiased under sampling and which
/// are not is the difference between a usable dashboard and a lie.
pub fn mean_latency_us(traces: &[Trace]) -> f64 {
    if traces.is_empty() {
        return 0.0;
    }
    traces.iter().map(|t| t.latency_us as f64).sum::<f64>() / traces.len() as f64
}

/// The p99 latency, which sampling does *not* estimate as comfortably —
/// at low rates the tail is made of a handful of traces.
pub fn p99_latency_us(traces: &[Trace]) -> u64 {
    if traces.is_empty() {
        return 0;
    }
    let mut v: Vec<u64> = traces.iter().map(|t| t.latency_us).collect();
    v.sort_unstable();
    v[(v.len() as f64 * 0.99) as usize % v.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{
        all_edges, run_workload, seeded_rng, topology, TopologyConfig,
    };

    fn workload() -> (crate::services::Topology, Vec<Trace>) {
        let cfg = TopologyConfig::default();
        let mut rng = seeded_rng(5);
        let t = topology(&mut rng, &cfg);
        let w = run_workload(&mut rng, &t, &cfg);
        (t, w.traces)
    }

    #[test]
    fn sampling_keeps_whole_traces() {
        let (_, traces) = workload();
        let mut rng = seeded_rng(11);
        let s = sample(&mut rng, &traces, 0.05);
        assert!(!s.is_empty());
        // Every sampled trace must be intact: a path and the edges that
        // path implies, not a fragment of either.
        for tr in &s {
            assert!(!tr.path.is_empty());
            assert!(traces.iter().any(|o| o.path == tr.path && o.edges == tr.edges));
        }
        let frac = s.len() as f64 / traces.len() as f64;
        assert!((frac - 0.05).abs() < 0.01, "sampled {frac} of traces");
    }

    #[test]
    fn the_dependency_graph_survives_aggressive_sampling() {
        // The aggregate question. Every edge is exercised on a large
        // fraction of requests, so a tiny sample finds essentially all
        // of them — which is why Dapper can afford 1/1024.
        let (t, traces) = workload();
        // Measured against what FULL tracing sees, which is the honest
        // comparison: an edge no request ever exercises is not something
        // sampling lost.
        let truth: HashSet<(u32, u32)> =
            traces.iter().flat_map(|t| t.edges.iter().copied()).collect();
        let _ = all_edges(&t);
        let mut rng = seeded_rng(11);
        let s = sample(&mut rng, &traces, 1.0 / 64.0);
        let r = edge_recall(&s, &truth);
        assert!(r > 0.95, "1/64 sampling recovered only {r} of the edges");
    }

    #[test]
    fn rare_paths_do_not_survive_it() {
        // The rare-event question, and the reason the same sampling rate
        // cannot serve both.
        let (_, traces) = workload();
        let mut rng = seeded_rng(11);
        let s = sample(&mut rng, &traces, 1.0 / 64.0);
        let rare = rare_path_recall(&traces, &s, 2);
        assert!(
            rare < 0.25,
            "rare-path recall was {rare} — the workload has no rare paths to lose"
        );
    }

    #[test]
    fn mean_latency_is_unbiased_under_sampling() {
        let (_, traces) = workload();
        let full = mean_latency_us(&traces);
        let mut err = 0.0;
        for seed in 0..8u64 {
            let mut rng = seeded_rng(seed);
            let s = sample(&mut rng, &traces, 0.02);
            err += (mean_latency_us(&s) - full).abs() / full;
        }
        assert!(err / 8.0 < 0.05, "mean latency error {:.3}", err / 8.0);
    }
}
