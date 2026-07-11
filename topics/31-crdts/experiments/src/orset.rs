//! Add-wins OR-Set — YOUR JOB. The set that makes remove-then-re-add
//! and concurrent add|remove behave the way users expect (Shapiro
//! SSS'11 §3.3.5). This is the exact structure M31 uses for graph
//! nodes and edges, so get the semantics right here first.
//!
//! Mechanism: every add tags the element with a fresh Dot (from
//! clock.rs — automerge calls these OpIds). A remove collects the tags
//! it has *observed* for that element and ships only those. An element
//! is present iff it has at least one live tag. A concurrent add's tag
//! wasn't observed by the remove, so it survives: add wins.
//!
//! Contract fixed by the tests below:
//! - `add(elem)` ticks self.clock for self.replica, inserts the dot
//!   into elems[elem].
//! - `remove(elem)` moves every currently-live dot for elem into
//!   `tombstones` (and drops the elem entry if empty).
//! - `contains`/`elements` see only elems entries with ≥1 live dot.
//! - `merge`: union both sides' tombstones; for each element, union
//!   live dots from both sides minus anything tombstoned; also merge
//!   the clocks. (Keeping tombstones forever is deliberate — §"garbage"
//!   in the README covers why compaction needs causal stability.)

use crate::clock::{Dot, ReplicaId, VClock};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub struct OrSet<E: std::hash::Hash + Eq + Clone> {
    pub replica: ReplicaId,
    pub clock: VClock,
    pub elems: HashMap<E, HashSet<Dot>>,
    pub tombstones: HashSet<Dot>,
}

impl<E: std::hash::Hash + Eq + Clone> OrSet<E> {
    pub fn new(replica: ReplicaId) -> Self {
        Self {
            replica,
            clock: VClock::new(),
            elems: HashMap::new(),
            tombstones: HashSet::new(),
        }
    }

    /// Tag `elem` with a fresh dot. Re-adding an existing element is
    /// fine — it just gains another tag.
    pub fn add(&mut self, elem: E) {
        let _ = elem;
        todo!("tick clock, insert dot into elems[elem]")
    }

    /// Remove by tombstoning every dot we've *seen* for `elem`.
    /// Removing an absent element is a no-op.
    pub fn remove(&mut self, elem: &E) {
        let _ = elem;
        todo!("move live dots into tombstones, drop empty entry")
    }

    pub fn contains(&self, elem: &E) -> bool {
        let _ = elem;
        todo!("any live dot for elem?")
    }

    pub fn elements(&self) -> HashSet<E> {
        todo!("all elems with >=1 live dot")
    }

    pub fn merge(&mut self, other: &OrSet<E>) {
        let _ = other;
        todo!("union tombstones, union live dots minus tombstones, merge clocks")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::seq::SliceRandom;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn add_remove_locally() {
        let mut s = OrSet::new(1);
        s.add("x");
        assert!(s.contains(&"x"));
        s.remove(&"x");
        assert!(!s.contains(&"x"));
        s.add("x"); // re-add after remove must work
        assert!(s.contains(&"x"));
    }

    #[test]
    fn concurrent_add_beats_remove() {
        let mut a = OrSet::new(1);
        a.add("x");
        let mut b = a.clone();
        b.replica = 2;

        // Concurrently: a removes x, b re-adds x (fresh tag a never saw).
        a.remove(&"x");
        b.add("x");

        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert!(ab.contains(&"x"), "add wins");
        assert!(ba.contains(&"x"), "add wins in either merge order");
        assert_eq!(ab.elements(), ba.elements());
    }

    #[test]
    fn remove_covers_all_observed_tags() {
        let mut a = OrSet::new(1);
        a.add("x");
        let mut b = OrSet::new(2);
        b.add("x"); // second, independent tag for x
        a.merge(&b);
        // a has now OBSERVED both tags, so its remove kills both.
        a.remove(&"x");
        let mut m = b.clone();
        m.merge(&a);
        assert!(!m.contains(&"x"));
    }

    #[test]
    fn converges_under_any_merge_order() {
        let mut rng = ChaCha8Rng::seed_from_u64(31);
        let mut a = OrSet::new(1);
        let mut b = OrSet::new(2);
        let mut c = OrSet::new(3);
        a.add("p");
        a.add("q");
        b.add("q");
        b.remove(&"q"); // only kills b's own tag; a's q survives
        c.add("r");
        c.add("p");

        let replicas = [a, b, c];
        let mut results: Vec<HashSet<&str>> = Vec::new();
        for _ in 0..20 {
            let mut order: Vec<usize> = (0..3).collect();
            order.shuffle(&mut rng);
            let mut acc = replicas[order[0]].clone();
            acc.merge(&replicas[order[1]]);
            acc.merge(&replicas[order[2]]);
            results.push(acc.elements());
        }
        let first = results[0].clone();
        assert!(results.iter().all(|r| *r == first));
        assert!(first.contains("p") && first.contains("q") && first.contains("r"));
    }
}
