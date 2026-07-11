//! Single-threaded skip list — YOUR implementation (this is the topic's build work).
//!
//! Spec (steal from redis t_zset.c, drop what you don't need):
//! - geometric height, p = 0.25, max level 16 (n ≤ 1e7 ⇒ log4(1e7) ≈ 12)
//! - insert, get, ordered iteration; no spans/backward needed (no rank queries)
//! - keys u64, values u64 — keep it simple, the benchmark supplies both
//!
//! Suggested node shape (Box + raw next pointers is fine single-threaded;
//! or Vec<Option<NonNull<Node>>> towers — pick one and note the trade in notes.md):
//!
//! ```text
//! head towers ──► [8]──────►[42]        level 1
//!            ──► [8]─►[17]─►[42]─►[55]  level 0 (all nodes)
//! ```
//!
//! Done when tests below pass and `benches/structures.rs` runs against it.

pub struct SkipList {
    // TODO: your fields
}

impl SkipList {
    pub fn new() -> Self {
        todo!("implement: head node with MAX_LEVEL forward pointers")
    }

    pub fn insert(&mut self, key: u64, value: u64) {
        let _ = (key, value);
        todo!("descend recording update[] per level, splice new node (see reading-redis-skiplist.md §3)")
    }

    pub fn get(&self, key: u64) -> Option<u64> {
        let _ = key;
        todo!("descend: advance while next.key < target, drop a level; check level 0")
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        todo!("walk level 0 — this is the memtable-flush path in miniature");
        #[allow(unreachable_code)]
        std::iter::empty()
    }

    pub fn len(&self) -> usize {
        todo!()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SkipList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::prelude::*;

    #[test]
    fn insert_get_roundtrip() {
        let mut sl = SkipList::new();
        for i in 0..1000u64 {
            sl.insert(i * 7 % 1000, i);
        }
        assert_eq!(sl.get(7), Some(1));
        assert_eq!(sl.get(1001), None);
    }

    #[test]
    fn ordered_iteration() {
        let mut sl = SkipList::new();
        let mut keys: Vec<u64> = (0..10_000).collect();
        keys.shuffle(&mut StdRng::seed_from_u64(42));
        for k in &keys {
            sl.insert(*k, *k * 2);
        }
        let out: Vec<u64> = sl.iter().map(|(k, _)| k).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(out, sorted);
    }

    #[test]
    fn overwrite_updates_value() {
        let mut sl = SkipList::new();
        sl.insert(5, 1);
        sl.insert(5, 2);
        assert_eq!(sl.get(5), Some(2));
        assert_eq!(sl.len(), 1);
    }
}
