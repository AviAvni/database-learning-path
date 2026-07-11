//! STUB 3 — copy-on-write branching: Neon's timeline model in miniature.
//!
//! Neon's storage key insight: the pageserver stores *page versions keyed by
//! (page, LSN)*, so a branch is just a record "child of timeline T as of LSN
//! L" — creating one copies nothing (pageserver/src/tenant.rs:4985
//! branch_timeline_impl). A read at (page, lsn) that finds no version on the
//! child walks to the ancestor, capped at the branch point
//! (timeline.rs:4548, the ancestor walk in get_vectored_reconstruct_data).
//! SlateDB's clone.rs:38 create_clone is the same idea over SST manifests.
//!
//! PROVIDED: the version store, `put`, and O(1) `create_branch`.
//! STUB: `get` — the ancestry-walking visibility rule. That rule IS the
//! feature; everything else is bookkeeping.

use std::collections::HashMap;

pub type BranchId = usize;
pub type PageId = u64;
pub type Lsn = u64;

pub struct Branch {
    /// None for the root branch; otherwise (parent, LSN at branch creation).
    pub parent: Option<(BranchId, Lsn)>,
}

pub struct BranchStore {
    pub branches: Vec<Branch>,
    /// Per (branch, page): versions sorted by ascending LSN (append-only,
    /// LSNs are globally monotonic, so pushes keep it sorted).
    versions: HashMap<(BranchId, PageId), Vec<(Lsn, u64)>>,
    next_lsn: Lsn,
}

pub const ROOT: BranchId = 0;

impl BranchStore {
    pub fn new() -> Self {
        Self {
            branches: vec![Branch { parent: None }],
            versions: HashMap::new(),
            next_lsn: 1,
        }
    }

    /// PROVIDED: write `value` to `page` on `branch`; returns the LSN.
    pub fn put(&mut self, branch: BranchId, page: PageId, value: u64) -> Lsn {
        let lsn = self.next_lsn;
        self.next_lsn += 1;
        self.versions.entry((branch, page)).or_default().push((lsn, value));
        lsn
    }

    /// PROVIDED: O(1) branch creation — records (parent, at_lsn), copies
    /// NOTHING. `at_lsn` may be historical (point-in-time branch).
    pub fn create_branch(&mut self, parent: BranchId, at_lsn: Lsn) -> BranchId {
        assert!(parent < self.branches.len());
        assert!(at_lsn < self.next_lsn);
        self.branches.push(Branch { parent: Some((parent, at_lsn)) });
        self.branches.len() - 1
    }

    pub fn last_lsn(&self) -> Lsn {
        self.next_lsn - 1
    }

    /// PROVIDED (for tests): total stored versions — proves branching is CoW.
    pub fn version_count(&self) -> usize {
        self.versions.values().map(|v| v.len()).sum()
    }

    /// Visibility rule: newest version of `page` visible from `branch` at
    /// `at_lsn`.
    pub fn get(&self, _branch: BranchId, _page: PageId, _at_lsn: Lsn) -> Option<u64> {
        // Recipe: look at versions[(branch, page)]: the last entry with
        // lsn <= at_lsn wins (partition_point on the sorted Vec). If none,
        // recurse (or loop) to the parent with the cap
        // min(at_lsn, branch_point_lsn). Root with no version -> None.
        todo!("stub: branch-visibility read")
    }

    /// Latest visible version on a branch.
    pub fn latest(&self, branch: BranchId, page: PageId) -> Option<u64> {
        self.get(branch, page, Lsn::MAX)
    }
}

impl Default for BranchStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_creation_copies_nothing() {
        // PROVIDED-path test: passes before the stub is implemented.
        let mut s = BranchStore::new();
        for p in 0..1000 {
            s.put(ROOT, p, p);
        }
        let before = s.version_count();
        let at = s.last_lsn();
        for _ in 0..100 {
            s.create_branch(ROOT, at);
        }
        assert_eq!(s.version_count(), before, "branching must not copy versions");
    }

    #[test]
    fn read_your_writes_and_time_travel() {
        let mut s = BranchStore::new();
        let l1 = s.put(ROOT, 7, 100);
        let l2 = s.put(ROOT, 7, 200);
        assert_eq!(s.latest(ROOT, 7), Some(200));
        assert_eq!(s.get(ROOT, 7, l1), Some(100));
        assert_eq!(s.get(ROOT, 7, l2), Some(200));
        assert_eq!(s.get(ROOT, 7, l1 - 1), None, "before first write");
        assert_eq!(s.latest(ROOT, 99), None);
    }

    #[test]
    fn branch_sees_parent_prefix_only() {
        let mut s = BranchStore::new();
        s.put(ROOT, 1, 10);
        let at = s.last_lsn();
        let b = s.create_branch(ROOT, at);
        s.put(ROOT, 1, 20); // parent write AFTER the branch point
        assert_eq!(s.latest(b, 1), Some(10), "child must not see post-branch parent writes");
        assert_eq!(s.latest(ROOT, 1), Some(20));
    }

    #[test]
    fn parent_and_sibling_isolation() {
        let mut s = BranchStore::new();
        s.put(ROOT, 1, 10);
        let at = s.last_lsn();
        let b1 = s.create_branch(ROOT, at);
        let b2 = s.create_branch(ROOT, at);
        s.put(b1, 1, 111);
        assert_eq!(s.latest(ROOT, 1), Some(10), "branch write leaked into parent");
        assert_eq!(s.latest(b2, 1), Some(10), "branch write leaked into sibling");
        assert_eq!(s.latest(b1, 1), Some(111));
    }

    #[test]
    fn historical_branch_point() {
        let mut s = BranchStore::new();
        let l1 = s.put(ROOT, 5, 1);
        s.put(ROOT, 5, 2);
        // branch AT the historical lsn l1 (point-in-time recovery shape)
        let b = s.create_branch(ROOT, l1);
        assert_eq!(s.latest(b, 5), Some(1), "PITR branch must see the old version");
    }

    #[test]
    fn deep_chain_resolves_through_ancestors() {
        let mut s = BranchStore::new();
        s.put(ROOT, 0, 42);
        let mut cur = ROOT;
        for i in 0..100 {
            let at = s.last_lsn();
            cur = s.create_branch(cur, at);
            s.put(cur, (i + 1) as PageId, i as u64);
        }
        // page 0 was written only at the root, 100 hops up.
        assert_eq!(s.latest(cur, 0), Some(42));
        // each intermediate page resolves at its own depth
        assert_eq!(s.latest(cur, 50), Some(49));
        // and the tip page
        assert_eq!(s.latest(cur, 100), Some(99));
    }
}
