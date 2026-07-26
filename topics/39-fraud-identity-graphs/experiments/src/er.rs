//! STUB — Fellegi-Sunter record linkage in miniature (entity resolution
//! for identity graphs / Customer 360).
//!
//! The model (Fellegi & Sunter 1969, as surveyed in Winkler 2006): for
//! a pair of records, compute an agreement pattern gamma over the
//! matching fields, then score it by the likelihood ratio
//! R = P(gamma | M) / P(gamma | U). Under conditional independence
//! (naive Bayes), log2 R decomposes into a sum over fields: an agreeing
//! field contributes log2(m_i / u_i), a disagreeing one
//! log2((1 - m_i) / (1 - u_i)), where m_i = P(agree | match) and
//! u_i = P(agree | nonmatch). Pairs above a threshold are matches.
//!
//! Nobody scores all n^2 pairs: *blocking* only compares pairs that
//! agree on some key, with multiple passes (here: same last name OR
//! same date of birth) so one typo cannot hide a duplicate. And nobody
//! hand-labels m and u. This crate splits the estimation the way splink
//! does in production: u from *random* record pairs (almost all of
//! which are nonmatches — splink's estimate_u_using_random_sampling),
//! then one EM *training session per blocking pass* (Winkler 1988's EM,
//! restricted to p and m with u held fixed) — crucially, each session
//! EXCLUDES the field it blocked on. Every candidate agrees on the
//! blocking key by construction, so that field carries no signal and a
//! fixed-u EM that includes it degenerates (everything looks like a
//! match). m for the blocked field comes from the other pass; fields
//! estimated by both sessions are averaged, as splink does.
//!
//! Contracts (the tests): sampled u and EM-fitted m land close to their
//! labeled empirical values; match weights separate matches from
//! nonmatches by many bits; blocking cuts comparisons >= 20x while the
//! linked clusters keep pair precision >= 0.95 and recall >= 0.9.

use rand::Rng;
use std::collections::HashMap;

pub const FIELDS: usize = 5;
/// Value-pool sizes: first name, last name, date of birth (10 years of
/// days), city, phone. Uniform draws — see the exercises for what
/// Zipf-distributed names break.
pub const POOLS: [u32; FIELDS] = [200, 500, 3650, 200, 2000];
/// Per-field typo rate: a corrupted field is redrawn from its pool.
pub const TYPO: [f64; FIELDS] = [0.10, 0.07, 0.03, 0.12, 0.05];

#[derive(Clone, Copy, Debug)]
pub struct Record {
    pub entity: usize,
    pub fields: [u32; FIELDS],
}

/// `n_entities` true people, `dups` records each; every field of every
/// record is independently corrupted (redrawn) with its TYPO rate.
pub fn generate_records(rng: &mut impl Rng, n_entities: usize, dups: usize) -> Vec<Record> {
    let mut records = Vec::with_capacity(n_entities * dups);
    for entity in 0..n_entities {
        let truth: [u32; FIELDS] = std::array::from_fn(|f| rng.gen_range(0..POOLS[f]));
        for _ in 0..dups {
            let fields = std::array::from_fn(|f| {
                if rng.gen::<f64>() < TYPO[f] {
                    rng.gen_range(0..POOLS[f])
                } else {
                    truth[f]
                }
            });
            records.push(Record { entity, fields });
        }
    }
    records
}

pub fn agreement(a: &Record, b: &Record) -> [bool; FIELDS] {
    std::array::from_fn(|f| a.fields[f] == b.fields[f])
}

/// One blocking pass: all pairs agreeing on `key_field`, sorted.
pub fn block_pairs(records: &[Record], key_field: usize) -> Vec<(usize, usize)> {
    let mut buckets: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        buckets.entry(r.fields[key_field]).or_default().push(i);
    }
    let mut pairs = Vec::new();
    for bucket in buckets.values() {
        for (k, &i) in bucket.iter().enumerate() {
            for &j in &bucket[k + 1..] {
                pairs.push((i, j));
            }
        }
    }
    pairs.sort_unstable();
    pairs
}

/// Two blocking passes — same last name (field 1), same dob (field 2) —
/// unioned and deduplicated. A duplicate pair is only lost if BOTH keys
/// were corrupted (Winkler's multi-pass blocking).
pub fn candidate_pairs(records: &[Record]) -> Vec<(usize, usize)> {
    let mut pairs: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for key_field in [1usize, 2] {
        pairs.extend(block_pairs(records, key_field));
    }
    let mut v: Vec<(usize, usize)> = pairs.into_iter().collect();
    v.sort_unstable();
    v
}

pub fn naive_pair_count(n: usize) -> usize {
    n * (n - 1) / 2
}

#[derive(Debug, Clone)]
pub struct FsParams {
    /// P(M) among blocked pairs (mean of the EM sessions' proportions).
    pub p: f64,
    /// m_i = P(field i agrees | match).
    pub m: [f64; FIELDS],
    /// u_i = P(field i agrees | nonmatch).
    pub u: [f64; FIELDS],
}

/// splink's estimate_u_using_random_sampling, in one function: sample
/// `samples` random record pairs (i != j) and return the per-field
/// agreement rate. With duplicates a vanishing fraction of the n^2
/// cross product, random pairs are nonmatches to within noise, so this
/// IS u — no labels needed.
pub fn estimate_u_random(rng: &mut impl Rng, records: &[Record], samples: usize) -> [f64; FIELDS] {
    let _ = (rng, records, samples);
    todo!("per-field agreement rate over random (i != j) pairs, floored at 1e-6")
}

/// Expectation-maximisation over the unlabeled patterns of ONE blocking
/// pass, with u held fixed and the blocked field masked out — splink's
/// EM training session shape (Winkler 1988, restricted to p and m).
/// Every candidate agrees on the blocking key by construction, so that
/// field carries no signal here; including it degenerates the fit.
/// Only fields with `active[f] == true` enter the likelihood; inactive
/// entries of the returned m are NaN. Start from p = 0.2, m_i = 0.9.
/// E-step: posterior match weight per pattern
///   w = p * prod_i m_i^g_i (1-m_i)^(1-g_i)
///       / (that + (1-p) * prod_i u_i^g_i (1-u_i)^(1-g_i)),
/// products over active i. M-step: p = mean w; m_i = w-weighted
/// agreement rate. Aggregate the 2^FIELDS distinct patterns for speed.
pub fn em_m(
    patterns: &[[bool; FIELDS]],
    u: &[f64; FIELDS],
    active: [bool; FIELDS],
    iters: usize,
) -> (f64, [f64; FIELDS]) {
    let _ = (patterns, u, active, iters);
    todo!("EM over the aggregated agreement patterns, active fields only, u fixed")
}

/// The Fellegi-Sunter match weight in bits:
/// sum over fields of log2(m_i/u_i) if agreeing, log2((1-m_i)/(1-u_i))
/// if not.
pub fn match_weight(pat: &[bool; FIELDS], fs: &FsParams) -> f64 {
    let _ = (pat, fs);
    todo!("sum the per-field log2 likelihood ratios")
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            let root = self.find(self.parent[x]);
            self.parent[x] = root;
        }
        self.parent[x]
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// The full pipeline: sample u, run one EM session per blocking pass
/// (each excluding its own blocking field, m averaged where both
/// sessions estimate it — splink's estimate_parameters_using_
/// expectation_maximisation workflow), score the unioned candidate
/// pairs, merge pairs above `threshold` bits with union-find.
/// Returns (cluster id per record, fitted params). The reported p is
/// the mean of the sessions' match proportions.
pub fn link(rng: &mut impl Rng, records: &[Record], threshold: f64) -> (Vec<usize>, FsParams) {
    let u = estimate_u_random(rng, records, 200_000);

    let mut m_sum = [0.0f64; FIELDS];
    let mut m_cnt = [0.0f64; FIELDS];
    let mut p_sum = 0.0;
    for key_field in [1usize, 2] {
        let pairs = block_pairs(records, key_field);
        let patterns: Vec<[bool; FIELDS]> = pairs
            .iter()
            .map(|&(i, j)| agreement(&records[i], &records[j]))
            .collect();
        let mut active = [true; FIELDS];
        active[key_field] = false;
        let (p, m) = em_m(&patterns, &u, active, 50);
        p_sum += p;
        for f in 0..FIELDS {
            if active[f] {
                m_sum[f] += m[f];
                m_cnt[f] += 1.0;
            }
        }
    }
    let fs = FsParams {
        p: p_sum / 2.0,
        m: std::array::from_fn(|f| m_sum[f] / m_cnt[f]),
        u,
    };

    let pairs = candidate_pairs(records);
    let mut uf = UnionFind::new(records.len());
    for &(i, j) in &pairs {
        let pat = agreement(&records[i], &records[j]);
        if match_weight(&pat, &fs) > threshold {
            uf.union(i, j);
        }
    }
    let clusters = (0..records.len()).map(|i| uf.find(i)).collect();
    (clusters, fs)
}

/// Pairwise precision/recall of a clustering against entity ground
/// truth (computed per predicted cluster — no n^2 scan).
pub fn pair_precision_recall(records: &[Record], clusters: &[usize]) -> (f64, f64) {
    let mut by_cluster: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in clusters.iter().enumerate() {
        by_cluster.entry(c).or_default().push(i);
    }
    let mut predicted = 0usize;
    let mut correct = 0usize;
    for members in by_cluster.values() {
        predicted += members.len() * (members.len() - 1) / 2;
        let mut by_entity: HashMap<usize, usize> = HashMap::new();
        for &i in members {
            *by_entity.entry(records[i].entity).or_default() += 1;
        }
        for &c in by_entity.values() {
            correct += c * (c - 1) / 2;
        }
    }
    let mut truth = 0usize;
    let mut by_entity: HashMap<usize, usize> = HashMap::new();
    for r in records {
        *by_entity.entry(r.entity).or_default() += 1;
    }
    for &c in by_entity.values() {
        truth += c * (c - 1) / 2;
    }
    (
        correct as f64 / predicted.max(1) as f64,
        correct as f64 / truth.max(1) as f64,
    )
}

/// Labeled empirical m (on the given pairs) — what EM is supposed to
/// recover without the labels. Also returns the match fraction.
pub fn empirical_m(records: &[Record], pairs: &[(usize, usize)]) -> (f64, [f64; FIELDS]) {
    let mut m_num = [0.0f64; FIELDS];
    let (mut n_m, mut n_all) = (0.0f64, 0.0f64);
    for &(i, j) in pairs {
        n_all += 1.0;
        if records[i].entity != records[j].entity {
            continue;
        }
        n_m += 1.0;
        let pat = agreement(&records[i], &records[j]);
        for f in 0..FIELDS {
            if pat[f] {
                m_num[f] += 1.0;
            }
        }
    }
    (n_m / n_all, std::array::from_fn(|f| m_num[f] / n_m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_graph::seeded_rng;

    #[test]
    fn sampled_u_and_em_m_recover_the_labeled_truth() {
        let mut rng = seeded_rng(20);
        let records = generate_records(&mut rng, 2_000, 3);

        // u: random pairs are nonmatches; agreement rate ~ 1/pool.
        let u = estimate_u_random(&mut rng, &records, 200_000);
        for f in 0..FIELDS {
            let expect = 1.0 / POOLS[f] as f64;
            assert!(
                (u[f] - expect).abs() < 0.005 + expect,
                "u[{f}] = {} vs ~{expect}",
                u[f]
            );
        }

        // m and p, one EM session per pass (blocked field masked),
        // against the labeled answer over the same pairs.
        for key_field in [1usize, 2] {
            let pairs = block_pairs(&records, key_field);
            let patterns: Vec<[bool; FIELDS]> = pairs
                .iter()
                .map(|&(i, j)| agreement(&records[i], &records[j]))
                .collect();
            let mut active = [true; FIELDS];
            active[key_field] = false;
            let (p, m) = em_m(&patterns, &u, active, 50);
            let (p_true, m_true) = empirical_m(&records, &pairs);
            assert!(
                (p - p_true).abs() < 0.05,
                "pass on field {key_field}: p {p} vs {p_true}"
            );
            for f in 0..FIELDS {
                if f == key_field {
                    assert!(m[f].is_nan(), "blocked field must be masked");
                    continue;
                }
                assert!(
                    (m[f] - m_true[f]).abs() < 0.05,
                    "pass on field {key_field}: m[{f}] {} vs {}",
                    m[f],
                    m_true[f]
                );
            }
        }
    }

    #[test]
    fn match_weights_separate_by_many_bits() {
        let mut rng = seeded_rng(21);
        let records = generate_records(&mut rng, 2_000, 3);
        let (_, fs) = link(&mut rng, &records, 12.0);
        let pairs = candidate_pairs(&records);
        let (mut m_sum, mut m_n, mut u_sum, mut u_n) = (0.0, 0.0, 0.0, 0.0);
        for &(i, j) in &pairs {
            let pat = agreement(&records[i], &records[j]);
            let w = match_weight(&pat, &fs);
            if records[i].entity == records[j].entity {
                m_sum += w;
                m_n += 1.0;
            } else {
                u_sum += w;
                u_n += 1.0;
            }
        }
        let gap = m_sum / m_n - u_sum / u_n;
        assert!(gap > 20.0, "mean match-weight gap only {gap} bits");
    }

    #[test]
    fn blocking_saves_comparisons_and_linkage_stays_accurate() {
        let mut rng = seeded_rng(22);
        let records = generate_records(&mut rng, 2_000, 3);
        let pairs = candidate_pairs(&records);
        let naive = naive_pair_count(records.len());
        assert!(
            pairs.len() * 20 <= naive,
            "blocking only cut {naive} to {}",
            pairs.len()
        );
        let (clusters, _) = link(&mut rng, &records, 12.0);
        let (precision, recall) = pair_precision_recall(&records, &clusters);
        assert!(precision >= 0.95, "precision {precision}");
        assert!(recall >= 0.9, "recall {recall}");
    }
}
