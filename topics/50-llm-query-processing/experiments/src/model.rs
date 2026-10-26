//! The simulated models. EVERY NUMBER IN THIS FILE IS A MODEL, NOT A MEASUREMENT.
//!
//! Nothing in this crate calls an LLM. A "model" here is three numbers — what
//! one call costs in dollars, how long it takes, and how often it is right —
//! plus, for a proxy, a noisy score. The bench reports call *counts* and F1 as
//! its headline because those follow from the seeded workload exactly; dollars
//! and seconds are counts multiplied by the constants below, and are only as
//! true as the constants.
//!
//! Where the constants come from:
//!
//! * Oracle price and latency: round illustrative values, not taken from any
//!   paper. The second predicate costs twice the first because its prompt is
//!   longer — the point is only that predicates differ in cost, which is the
//!   premise of rank ordering (Hellerstein & Stonebraker, SIGMOD'93).
//! * Proxy latency: oracle latency / 17.4. LOTUS (arXiv 2407.11418v3, §5.4)
//!   reports its embedding proxy "17.4× faster than the oracle" for
//!   sem_group_by classification. One task, one pair of models; borrowed as an
//!   order of magnitude, not as a law.
//! * Proxy price: 1/20 of the cheap oracle call. A MODEL.
//! * Oracle accuracy 0.97: a MODEL. LOTUS defines correctness *relative to the
//!   oracle* ("gold algorithm", §2.2), so the bench reports F1 against the
//!   oracle as well as against ground truth.
//! * Parallelism 20: SemBench (PVLDB 19(8), §5.1) sets "the LLM invocation
//!   parallelism to 20" for every system it benchmarks.

/// One kind of model call.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    pub name: &'static str,
    /// MODEL: dollars per call.
    pub dollars: f64,
    /// MODEL: seconds per call, as seen by one in-flight request.
    pub seconds: f64,
}

/// One semantic predicate ("the review mentions a delivery problem").
#[derive(Clone, Copy, Debug)]
pub struct Predicate {
    pub name: &'static str,
    /// MODEL: fraction of documents for which the predicate is really true.
    pub selectivity: f64,
    /// The expensive, accurate model that defines the gold answer.
    pub oracle: Model,
    /// MODEL: probability the oracle's answer equals ground truth.
    pub oracle_accuracy: f64,
    /// The cheap model that returns a score in (0, 1).
    pub proxy: Model,
    /// MODEL: separation of the proxy's logit between true and false
    /// documents. Score = sigmoid(±mu + N(0,1)); at a 0.5 cut the proxy is
    /// right with probability Phi(mu).
    pub proxy_mu: f64,
}

/// MODEL: concurrent requests in flight, as in SemBench §5.1.
pub const PARALLELISM: usize = 20;

pub const ORACLE_CHEAP: Model = Model { name: "oracle (short prompt)", dollars: 0.0010, seconds: 0.80 };
pub const ORACLE_LONG: Model = Model { name: "oracle (long prompt)", dollars: 0.0020, seconds: 1.60 };
pub const PROXY: Model = Model { name: "proxy", dollars: 0.00005, seconds: 0.80 / 17.4 };

/// P1 — common and cheap to ask.
pub const P1: Predicate = Predicate {
    name: "P1 mentions a delivery problem",
    selectivity: 0.30,
    oracle: ORACLE_CHEAP,
    oracle_accuracy: 0.97,
    proxy: PROXY,
    proxy_mu: 1.2,
};

/// P2 — rare, and twice as expensive to ask.
pub const P2: Predicate = Predicate {
    name: "P2 is sarcastic",
    selectivity: 0.10,
    oracle: ORACLE_LONG,
    oracle_accuracy: 0.97,
    proxy: PROXY,
    proxy_mu: 0.9,
};

/// Wall-clock seconds for `calls` independent calls of `m` with `PARALLELISM`
/// in flight: whole waves of `seconds` each.
pub fn wave_seconds(calls: u64, m: Model) -> f64 {
    let waves = calls.div_ceil(PARALLELISM as u64);
    waves as f64 * m.seconds
}
