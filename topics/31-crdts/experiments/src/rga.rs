//! RGA sequence CRDT — YOUR JOB. The list/text structure underneath
//! collaborative editors. Yjs/yrs (yrs/src/block.rs:1415
//! Item::integrate), automerge, and diamond-types
//! (src/listmerge/merge.rs:142) are all descendants of this idea:
//! address characters by *identity* (a Dot), not by index.
//!
//! Model (simplified RGA):
//! - Every element has an id (Dot) and a parent: the id of the element
//!   it was inserted AFTER (None = head of list).
//! - Deletion is a tombstone flag — the element stays for addressing.
//! - Integration rule: place the new element after its parent, but
//!   skip over any existing siblings with a LARGER id (compare
//!   (counter, replica)). Two users typing at the same spot thus land
//!   in the same deterministic order on every replica — higher id
//!   sits closer to the shared parent.
//! - Ops are delivered as `Insert`/`Delete` values; `apply` must be
//!   idempotent (re-delivery is a no-op) and tolerate any order of
//!   *causally ready* ops (parent already present).
//!
//! This is op-based where the OR-Set was state-based — you get to feel
//! the delivery-precondition difference the README diagrams.

use crate::clock::{Dot, ReplicaId, VClock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op<T: Copy> {
    Insert {
        id: Dot,
        parent: Option<Dot>,
        value: T,
    },
    Delete {
        target: Dot,
    },
}

#[derive(Clone, Debug)]
pub struct Element<T: Copy> {
    pub id: Dot,
    pub value: T,
    pub deleted: bool,
}

#[derive(Clone, Debug)]
pub struct Rga<T: Copy> {
    pub replica: ReplicaId,
    pub clock: VClock,
    /// Elements in document order, tombstones included.
    pub elems: Vec<Element<T>>,
}

impl<T: Copy + PartialEq + std::fmt::Debug> Rga<T> {
    pub fn new(replica: ReplicaId) -> Self {
        Self {
            replica,
            clock: VClock::new(),
            elems: Vec::new(),
        }
    }

    /// Insert at visible position `pos` (0 = front). Finds the parent
    /// (the visible element before `pos`, or None), builds the op,
    /// applies it locally, and returns it for broadcast.
    pub fn insert(&mut self, pos: usize, value: T) -> Op<T> {
        let _ = (pos, value);
        todo!("resolve pos -> parent dot, tick clock, build + apply Insert")
    }

    /// Delete the visible element at `pos`; returns the op.
    pub fn delete(&mut self, pos: usize) -> Op<T> {
        let _ = pos;
        todo!("resolve pos -> target dot, build + apply Delete")
    }

    /// Apply a (possibly remote) op. Must be idempotent. Inserts skip
    /// larger-id siblings after the parent; deletes set the tombstone.
    /// Remember to merge the op's dot into self.clock.
    pub fn apply(&mut self, op: &Op<T>) {
        let _ = op;
        todo!("the integration rule lives here")
    }

    /// Visible content, tombstones filtered out.
    pub fn to_vec(&self) -> Vec<T> {
        self.elems
            .iter()
            .filter(|e| !e.deleted)
            .map(|e| e.value)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::seq::SliceRandom;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn from_str(r: ReplicaId, s: &str) -> (Rga<char>, Vec<Op<char>>) {
        let mut rga = Rga::new(r);
        let mut ops = Vec::new();
        for (i, ch) in s.chars().enumerate() {
            ops.push(rga.insert(i, ch));
        }
        (rga, ops)
    }

    #[test]
    fn local_editing() {
        let (mut rga, _) = from_str(1, "helo");
        rga.insert(3, 'l');
        assert_eq!(rga.to_vec().iter().collect::<String>(), "hello");
        rga.delete(0);
        assert_eq!(rga.to_vec().iter().collect::<String>(), "ello");
    }

    #[test]
    fn concurrent_inserts_at_same_spot_converge() {
        let (a0, ops) = from_str(1, "ac");
        let mut a = a0.clone();
        let mut b = Rga::new(2);
        for op in &ops {
            b.apply(op);
        }

        // Both insert between 'a' and 'c' concurrently.
        let op_a = a.insert(1, 'X');
        let op_b = b.insert(1, 'Y');
        a.apply(&op_b);
        b.apply(&op_a);

        assert_eq!(a.to_vec(), b.to_vec(), "same order on both replicas");
        let s: String = a.to_vec().iter().collect();
        assert!(s == "aXYc" || s == "aYXc");
    }

    #[test]
    fn apply_is_idempotent() {
        let (_, ops) = from_str(1, "abc");
        let mut b = Rga::new(2);
        for op in &ops {
            b.apply(op);
            b.apply(op); // duplicate delivery
        }
        assert_eq!(b.to_vec().iter().collect::<String>(), "abc");
    }

    #[test]
    fn delete_concurrent_with_insert_after_it() {
        let (a0, ops) = from_str(1, "ab");
        let mut a = a0.clone();
        let mut b = Rga::new(2);
        for op in &ops {
            b.apply(op);
        }
        // a deletes 'a'; b concurrently inserts after 'a'. The tombstone
        // must keep serving as b's parent anchor.
        let del = a.delete(0);
        let ins = b.insert(1, 'X');
        a.apply(&ins);
        b.apply(&del);
        assert_eq!(a.to_vec(), b.to_vec());
        assert_eq!(a.to_vec().iter().collect::<String>(), "Xb");
    }

    #[test]
    fn three_replicas_converge_any_causal_delivery_order() {
        let mut rng = ChaCha8Rng::seed_from_u64(31);
        let (base, base_ops) = from_str(1, "xyz");
        let mut a = base.clone();
        let mut b = Rga::new(2);
        let mut c = Rga::new(3);
        for op in &base_ops {
            b.apply(op);
            c.apply(op);
        }
        // Independent concurrent edits on each replica.
        let mut ops = vec![a.insert(1, 'A'), b.insert(1, 'B'), b.delete(0), c.insert(3, 'C')];

        for _ in 0..10 {
            // Concurrent ops from distinct replicas may arrive in any
            // order; shuffle and deliver to fresh copies.
            ops.shuffle(&mut rng);
            let mut fresh = base.clone();
            let mut fresh2 = {
                let mut f = Rga::new(9);
                for op in &base_ops {
                    f.apply(op);
                }
                f
            };
            for op in &ops {
                fresh.apply(op);
            }
            for op in ops.iter().rev() {
                fresh2.apply(op);
            }
            assert_eq!(fresh.to_vec(), fresh2.to_vec());
        }
    }
}
