//! Does the estimate pick the cheaper join order? (PROVIDED)
//!
//! A k-edge path is a k-way join of its edges, and a left-deep plan for it
//! is an *edge order*: pick a first edge, then repeatedly add the edge to the
//! left or to the right of the interval built so far (any other edge would be
//! a cross product). `Cypher`'s `(a)-[:r0]->(b)-[:r1]->(c)-[:r2]->(d)` has
//! 2^(k−1) = 4 such orders: start at r0 and go right; start at r2 and go
//! left; start at r1 and add r0 then r2; start at r1 and add r2 then r0.
//!
//! Every order materialises one intermediate result per interval it passes
//! through — the walks of the edges `lo..=hi` — so its cost is the sum of
//! those interval counts (the single start edge and the full answer
//! included; the answer is common to all orders). Given a function that
//! returns the count of any sub-walk — the oracle's, or an estimator's — an
//! order has a cost, and the planner picks the cheapest. *Regret* is the true
//! cost of the order picked by the estimates divided by the true cost of the
//! truly cheapest order, so 1.0 is a correct pick.

pub type Interval = (usize, usize);

/// Every left-deep order of a k-edge path, as the sequence of intervals it
/// materialises (first element is the start edge `(s, s)`, last is `(0, k-1)`).
pub fn edge_orders(k: usize) -> Vec<Vec<Interval>> {
    fn rec(k: usize, cur: &mut Vec<Interval>, out: &mut Vec<Vec<Interval>>) {
        let (lo, hi) = *cur.last().unwrap();
        if lo == 0 && hi == k - 1 {
            out.push(cur.clone());
            return;
        }
        if lo > 0 {
            cur.push((lo - 1, hi));
            rec(k, cur, out);
            cur.pop();
        }
        if hi + 1 < k {
            cur.push((lo, hi + 1));
            rec(k, cur, out);
            cur.pop();
        }
    }
    let mut out = Vec::new();
    for s in 0..k {
        rec(k, &mut vec![(s, s)], &mut out);
    }
    out
}

/// Sum of `count(lo, hi)` over the intervals of an order.
pub fn order_cost(order: &[Interval], count: &dyn Fn(usize, usize) -> f64) -> f64 {
    order.iter().map(|&(lo, hi)| count(lo, hi)).sum()
}

/// Index of the cheapest order under `count` (first on ties).
pub fn pick(orders: &[Vec<Interval>], count: &dyn Fn(usize, usize) -> f64) -> usize {
    let mut best = 0;
    let mut best_cost = f64::INFINITY;
    for (i, o) in orders.iter().enumerate() {
        let c = order_cost(o, count);
        if c < best_cost {
            best_cost = c;
            best = i;
        }
    }
    best
}

/// Outcome of planning one pattern with one estimator.
#[derive(Clone, Copy, Debug)]
pub struct Planned {
    pub regret: f64,
    /// The estimator's pick is not (one of) the truly cheapest order(s).
    pub wrong: bool,
}

pub fn plan(
    orders: &[Vec<Interval>],
    truth: &dyn Fn(usize, usize) -> f64,
    estimate: &dyn Fn(usize, usize) -> f64,
) -> Planned {
    let t_best = (0..orders.len()).map(|i| order_cost(&orders[i], truth)).fold(f64::INFINITY, f64::min);
    let picked = order_cost(&orders[pick(orders, estimate)], truth);
    Planned { regret: picked / t_best, wrong: picked > t_best }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_k_edge_path_has_two_to_the_k_minus_one_orders() {
        for k in 2..=5 {
            let o = edge_orders(k);
            assert_eq!(o.len(), 1 << (k - 1), "k = {k}");
            for ord in &o {
                assert_eq!(ord.len(), k);
                assert_eq!(*ord.last().unwrap(), (0, k - 1));
            }
        }
        // k = 3: right from r0, left from r2, and r1 then either side first.
        let o = edge_orders(3);
        assert!(o.contains(&vec![(0, 0), (0, 1), (0, 2)]));
        assert!(o.contains(&vec![(2, 2), (1, 2), (0, 2)]));
        assert!(o.contains(&vec![(1, 1), (0, 1), (0, 2)]));
        assert!(o.contains(&vec![(1, 1), (1, 2), (0, 2)]));
    }

    #[test]
    fn exact_counts_always_pick_a_regret_one_plan() {
        let truth = |lo: usize, hi: usize| ((hi - lo + 1) * 10 + lo) as f64;
        let o = edge_orders(4);
        let p = plan(&o, &truth, &truth);
        assert_eq!(p.regret, 1.0);
        assert!(!p.wrong);
    }

    #[test]
    fn a_misleading_estimate_costs_regret() {
        // truth: the middle edge is the tiny one, so start there; the estimate thinks edge 0 is.
        let truth = |lo: usize, hi: usize| if lo <= 1 && 1 <= hi { 1.0 } else { 100.0 };
        let est = |lo: usize, hi: usize| if lo == 0 { 1.0 } else { 100.0 };
        let o = edge_orders(3);
        let p = plan(&o, &truth, &est);
        assert!(p.wrong && p.regret > 1.0);
    }
}
