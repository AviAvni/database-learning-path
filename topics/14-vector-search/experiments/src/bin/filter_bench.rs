//! Provided: the over-fetch factor of a filtered nearest-neighbour query,
//! measured on exact answers (no index involved, so it runs today).
//!
//!   cargo run --release --bin filter_bench
//!
//! For a query q and a filter, the over-fetch factor alpha(q; K) is the
//! number of vector-ranked candidates that must be examined before K of them
//! satisfy the filter (Definition 3.1 of Lim et al., PVLDB 19(10), 2026 —
//! the quantity the README's Step 5 builds on). Here it is read straight off
//! the exact ranking: the 1-based rank of the K-th matching vector.
//!
//! Three filters, each at selectivity s in {50%, 10%, 1%, 0.1%}:
//!   independent  a random subset of s*n rows, the same for every query
//!   aligned      the s*n rows whose attribute is closest to the query's own
//!                attribute; the attribute is a fixed random projection of
//!                the vector, so it is correlated with geometry (a "price
//!                range" that tracks the embedding)
//!   anti-aligned the same window, centred half the attribute range away
//!
//! Alongside alpha it prints K/s (what selectivity alone predicts) and the
//! paper's Eq. 5 estimator, so the three can be compared on one table.

use std::time::Instant;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use vector_experiments::data;
use vector_experiments::distance::dist::l2_sq;

const N: u32 = 100_000;
const DIM: usize = 128;
const CLUSTERS: usize = 200;
const NUM_QUERIES: u32 = 500;
const K: usize = 10;
const SELECTIVITIES: [f64; 4] = [0.5, 0.1, 0.01, 0.001];

/// One attribute column, stored as each row's rank (0..n) so that a window of
/// w consecutive ranks is exactly selectivity w/n.
struct Attr {
    rank: Vec<u32>,
}

fn rank_of(values: &[f32]) -> Attr {
    let mut order: Vec<u32> = (0..values.len() as u32).collect();
    order.sort_by(|&a, &b| values[a as usize].partial_cmp(&values[b as usize]).unwrap().then(a.cmp(&b)));
    let mut rank = vec![0u32; values.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i as usize] = r as u32;
    }
    Attr { rank }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    let t = Instant::now();
    let (d, _clusters) = data::clustered_labeled(N, DIM, CLUSTERS, 42);
    let (q, source) = data::queries_with_source(&d, NUM_QUERIES, 42);
    let n = N as usize;

    // independent attribute: a seeded random permutation of 0..n
    let mut rng = StdRng::seed_from_u64(7);
    let mut perm: Vec<u32> = (0..N).collect();
    perm.shuffle(&mut rng);
    let independent = Attr { rank: perm };

    // geometric attribute: projection on a fixed random unit direction
    let mut dir: Vec<f32> = (0..DIM).map(|_| rng.gen_range(-1.0..1.0f32)).collect();
    let norm = dir.iter().map(|x| x * x).sum::<f32>().sqrt();
    dir.iter_mut().for_each(|x| *x /= norm);
    let proj: Vec<f32> = (0..N).map(|i| d.get(i).iter().zip(&dir).map(|(a, b)| a * b).sum()).collect();
    let geometric = rank_of(&proj);

    println!(
        "data: {}x{} in {} clusters, {} queries, K={} (gen {:.1} s)",
        N,
        DIM,
        CLUSTERS,
        NUM_QUERIES,
        K,
        t.elapsed().as_secs_f64()
    );

    // exact distance-ordered ids per query, computed once and reused by every filter
    let t = Instant::now();
    let orders: Vec<Vec<(f32, u32)>> = (0..q.len())
        .map(|qi| {
            let mut v: Vec<(f32, u32)> = (0..N).map(|i| (l2_sq(d.get(i), q.get(qi)), i)).collect();
            v.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
            v
        })
        .collect();
    println!("exact rankings: {:.1} s\n", t.elapsed().as_secs_f64());

    println!(
        "{:<13} {:>7} {:>8} | {:>8} {:>8} {:>8} {:>9} | {:>9} {:>9}",
        "filter", "s", "|Vf|", "alpha", "median", "p99", "alpha/(K/s)", "K/s", "Eq.5 mean"
    );
    for mode in ["independent", "aligned", "anti-aligned"] {
        for &s in &SELECTIVITIES {
            let w = ((s * n as f64).round() as usize).max(1);
            let mut alphas = Vec::with_capacity(q.len() as usize);
            let mut hats = Vec::with_capacity(q.len() as usize);
            for qi in 0..q.len() as usize {
                // window of attribute ranks [lo, lo+w) for this query
                let lo = match mode {
                    "independent" => 0,
                    "aligned" => {
                        let r = geometric.rank[source[qi] as usize] as usize;
                        r.saturating_sub(w / 2).min(n - w)
                    }
                    _ => {
                        let r = (geometric.rank[source[qi] as usize] as usize + n / 2) % n;
                        r.saturating_sub(w / 2).min(n - w)
                    }
                };
                let attr = if mode == "independent" { &independent } else { &geometric };
                let (lo32, hi32) = (lo as u32, (lo + w) as u32);
                let mut seen = 0usize;
                let mut kth_match_dist = 0.0f32;
                let mut alpha = 0usize;
                for (pos, &(dist, id)) in orders[qi].iter().enumerate() {
                    let r = attr.rank[id as usize];
                    if r >= lo32 && r < hi32 {
                        seen += 1;
                        if seen == K {
                            alpha = pos + 1;
                            kth_match_dist = dist;
                            break;
                        }
                    }
                }
                alphas.push(alpha as f64);
                // Eq. 5: |V0|/|Vf| * d(q, K-NN | Vf) / d(q, K-NN | V0) * K, with d = L2 (sqrt of l2_sq)
                let d0 = orders[qi][K - 1].0.sqrt() as f64;
                let df = kth_match_dist.sqrt() as f64;
                hats.push(n as f64 / w as f64 * df / d0 * K as f64);
            }
            let mean = alphas.iter().sum::<f64>() / alphas.len() as f64;
            let hat = hats.iter().sum::<f64>() / hats.len() as f64;
            let mut sorted = alphas.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let ks = K as f64 / s;
            println!(
                "{:<13} {:>6.1}% {:>8} | {:>8.1} {:>8.0} {:>8.0} {:>9.2} | {:>9.0} {:>9.1}",
                mode,
                s * 100.0,
                w,
                mean,
                percentile(&sorted, 0.5),
                percentile(&sorted, 0.99),
                mean / ks,
                ks,
                hat
            );
        }
        println!();
    }

    println!("notes:");
    println!("- alpha = rank of the K-th matching vector in the exact ordering; post-filtering");
    println!("  an ANN search must fetch at least this many candidates to return K results.");
    println!("- independent: alpha tracks K/s. aligned / anti-aligned: same s, very different alpha —");
    println!("  selectivity alone does not order filtered queries by difficulty.");
}
