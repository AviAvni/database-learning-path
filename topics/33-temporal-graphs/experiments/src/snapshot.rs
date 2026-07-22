//! Anchor + delta AT TIME store — YOUR JOB. AeonG's (VLDB 2024) storage
//! bet in miniature: keep the event log (deltas), and every `every`
//! events materialize a full adjacency snapshot (anchor). AT TIME t =
//! copy the nearest anchor at-or-before t, replay only the deltas
//! between anchor and t. Checkpoint spacing is the knob: dense anchors
//! = fast reads, fat storage; sparse = thin storage, long replays.
//! Same trade as topic 5's checkpoint-vs-redo and M30's time-travel.
//!
//! Contract fixed by the tests below:
//! - `new(n, every)`: empty store over n nodes, anchor every `every`
//!   events (an anchor reflecting events[..k] for each k that's a
//!   multiple of `every`, built as events arrive).
//! - `append(e)`: events arrive in non-decreasing t order.
//! - `at_time(t)`: adjacency after applying all events with e.t <= t —
//!   bit-identical to events::replay_at_time — touching only ONE anchor
//!   plus the deltas after it.
//! - `anchor_count()` and `replay_len(t)` (# deltas replayed by
//!   at_time(t)): the bench prints both; that's the price list.

use crate::events::Event;
use std::collections::BTreeSet;

pub type Adjacency = Vec<BTreeSet<u32>>;

pub struct AnchorDeltaStore {
    pub n: u32,
    pub every: usize,
    pub events: Vec<Event>,
    pub anchors: Vec<(usize, Adjacency)>, // (events consumed, state at that point)
}

impl AnchorDeltaStore {
    pub fn new(n: u32, every: usize) -> Self {
        assert!(every > 0);
        Self { n, every, events: Vec::new(), anchors: Vec::new() }
    }

    pub fn append(&mut self, e: Event) {
        let _ = e;
        todo!("push the delta; materialize an anchor at every `every` events")
    }

    pub fn at_time(&self, t: u64) -> Adjacency {
        let _ = t;
        todo!("nearest anchor at-or-before t, then replay deltas up to t")
    }

    pub fn anchor_count(&self) -> usize {
        self.anchors.len()
    }

    /// How many deltas at_time(t) replays — the read cost the bench plots.
    pub fn replay_len(&self, t: u64) -> usize {
        let _ = t;
        todo!("events between the chosen anchor and t")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{gen_events, replay_at_time};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn store_from(events: &[Event], n: u32, every: usize) -> AnchorDeltaStore {
        let mut s = AnchorDeltaStore::new(n, every);
        for &e in events {
            s.append(e);
        }
        s
    }

    #[test]
    fn matches_full_replay_at_every_probe() {
        let mut rng = ChaCha8Rng::seed_from_u64(11);
        let events = gen_events(&mut rng, 100, 5_000, 10_000);
        let s = store_from(&events, 100, 512);
        for t in [0u64, 1, 999, 5_000, 9_999, 20_000] {
            assert_eq!(s.at_time(t), replay_at_time(&events, 100, t), "t={t}");
        }
    }

    #[test]
    fn anchor_spacing_bounds_replay() {
        let mut rng = ChaCha8Rng::seed_from_u64(12);
        let events = gen_events(&mut rng, 100, 5_000, 10_000);
        let s = store_from(&events, 100, 256);
        assert_eq!(s.anchor_count(), events.len() / 256);
        for t in (0..10_000).step_by(777) {
            assert!(s.replay_len(t) < 256, "replay never crosses an anchor");
        }
    }

    #[test]
    fn dense_vs_sparse_is_the_whole_trade() {
        let mut rng = ChaCha8Rng::seed_from_u64(13);
        let events = gen_events(&mut rng, 50, 2_000, 4_000);
        let dense = store_from(&events, 50, 64);
        let sparse = store_from(&events, 50, 1_024);
        assert!(dense.anchor_count() > sparse.anchor_count());
        let t = 3_500;
        assert!(dense.replay_len(t) <= sparse.replay_len(t));
        assert_eq!(dense.at_time(t), sparse.at_time(t));
    }
}
