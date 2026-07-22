//! Temporal edge stream + oracles — PROVIDED. A temporal graph is a
//! stream of contacts (u, v, t, λ): "u could reach v by departing at t
//! and arriving at t+λ". Two provided oracles anchor everything else:
//! a static-BFS reachability (what you get if you throw the timestamps
//! away) and a fixpoint earliest-arrival (slow, obviously correct — the
//! ground truth your one-pass temporal_reach.rs must match).

use rand::Rng;

pub const INF: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub u: u32,
    pub v: u32,
    pub t: u64,      // departure time
    pub lambda: u64, // traversal duration: arrive at t + lambda
}

/// Random contact stream: `m` contacts over `n` nodes, departure times
/// uniform in [0, horizon), unit traversal time. Returned sorted by t —
/// the arrival order a streaming algorithm sees.
pub fn gen_contacts<R: Rng>(rng: &mut R, n: u32, m: usize, horizon: u64) -> Vec<Contact> {
    let mut cs: Vec<Contact> = (0..m)
        .map(|_| Contact {
            u: rng.gen_range(0..n),
            v: rng.gen_range(0..n),
            t: rng.gen_range(0..horizon),
            lambda: 1,
        })
        .filter(|c| c.u != c.v)
        .collect();
    cs.sort_by_key(|c| c.t);
    cs
}

/// Static condensation: forget every timestamp, BFS the leftover digraph.
/// Over-reports — a static path u→w→v may need the w→v contact to leave
/// BEFORE the u→w contact arrives. Lane 1 measures the damage.
pub fn static_reachable(contacts: &[Contact], n: u32, src: u32) -> Vec<bool> {
    let mut adj = vec![Vec::new(); n as usize];
    for c in contacts {
        adj[c.u as usize].push(c.v);
    }
    let mut seen = vec![false; n as usize];
    let mut queue = std::collections::VecDeque::from([src]);
    seen[src as usize] = true;
    while let Some(u) = queue.pop_front() {
        for &v in &adj[u as usize] {
            if !seen[v as usize] {
                seen[v as usize] = true;
                queue.push_back(v);
            }
        }
    }
    seen
}

/// Earliest-arrival ground truth by fixpoint: relax every contact until
/// nothing changes (Bellman-Ford shape — O(V·E) worst case, no ordering
/// assumptions, no cleverness). arr[v] = earliest time v is reachable
/// departing src no earlier than t_start; INF = temporally unreachable.
pub fn earliest_arrival_oracle(contacts: &[Contact], n: u32, src: u32, t_start: u64) -> Vec<u64> {
    let mut arr = vec![INF; n as usize];
    arr[src as usize] = t_start;
    loop {
        let mut changed = false;
        for c in contacts {
            if arr[c.u as usize] <= c.t && c.t + c.lambda < arr[c.v as usize] {
                arr[c.v as usize] = c.t + c.lambda;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    arr
}

/// Graph-mutation event for the AT TIME store (snapshot.rs): an edge
/// (u, v) appears or disappears at time t. Sorted-by-t append-only log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub t: u64,
    pub u: u32,
    pub v: u32,
    pub add: bool,
}

/// Naive AT TIME oracle: replay the whole log from t=0. Correct, and
/// exactly as slow as its position in the log — the baseline anchors
/// must beat.
pub fn replay_at_time(events: &[Event], n: u32, t: u64) -> Vec<std::collections::BTreeSet<u32>> {
    let mut adj = vec![std::collections::BTreeSet::new(); n as usize];
    for e in events.iter().take_while(|e| e.t <= t) {
        if e.add {
            adj[e.u as usize].insert(e.v);
        } else {
            adj[e.u as usize].remove(&e.v);
        }
    }
    adj
}

/// Random add/remove event log, sorted by t: removes only edges that are
/// currently present, so the log is always well-formed.
pub fn gen_events<R: Rng>(rng: &mut R, n: u32, m: usize, horizon: u64) -> Vec<Event> {
    let mut live: Vec<(u32, u32)> = Vec::new();
    let mut ts: Vec<u64> = (0..m).map(|_| rng.gen_range(0..horizon)).collect();
    ts.sort_unstable();
    let mut events = Vec::with_capacity(m);
    for t in ts {
        let remove = !live.is_empty() && rng.gen_bool(0.3);
        if remove {
            let (u, v) = live.swap_remove(rng.gen_range(0..live.len()));
            events.push(Event { t, u, v, add: false });
        } else {
            let (u, v) = (rng.gen_range(0..n), rng.gen_range(0..n));
            if u != v && !live.contains(&(u, v)) {
                live.push((u, v));
                events.push(Event { t, u, v, add: true });
            }
        }
    }
    events
}

/// Nearest-rank percentile over unsorted ns samples.
pub fn percentile(samples: &mut [u64], p: f64) -> u64 {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    let rank = ((p / 100.0) * samples.len() as f64).ceil() as usize;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_overreports_temporal() {
        // v --(t=5)--> w --(t=1)--> x: statically x is reachable from v,
        // temporally it is not (the w->x contact departed before we arrived).
        let cs = vec![
            Contact { u: 0, v: 1, t: 5, lambda: 1 },
            Contact { u: 1, v: 2, t: 1, lambda: 1 },
        ];
        assert!(static_reachable(&cs, 3, 0)[2]);
        assert_eq!(earliest_arrival_oracle(&cs, 3, 0, 0)[2], INF);
    }

    #[test]
    fn oracle_respects_time() {
        let cs = vec![
            Contact { u: 0, v: 1, t: 2, lambda: 1 }, // arrive at 1 at t=3
            Contact { u: 1, v: 2, t: 3, lambda: 2 }, // departs exactly at arrival: ok
        ];
        let arr = earliest_arrival_oracle(&cs, 3, 0, 0);
        assert_eq!(arr, vec![0, 3, 5]);
    }

    #[test]
    fn replay_applies_removes() {
        let events = vec![
            Event { t: 1, u: 0, v: 1, add: true },
            Event { t: 2, u: 0, v: 2, add: true },
            Event { t: 3, u: 0, v: 1, add: false },
        ];
        assert_eq!(replay_at_time(&events, 3, 2)[0].len(), 2);
        assert_eq!(replay_at_time(&events, 3, 3)[0].len(), 1);
    }
}
