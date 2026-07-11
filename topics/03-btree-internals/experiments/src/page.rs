//! Slotted page — YOUR implementation (build work; format is fixed by the tests).
//!
//! 4096-byte page, SQLite-flavored but simplified (see reading guides):
//!
//! ```text
//! offset 0      1        3        5          7        8
//! ┌──────┬──────────┬─────────┬────────────┬──────┬──────────────┬───┬───────┐
//! │ type │ freeblock│ ncells  │ content    │ frag │ cell ptrs    │...│ cells │
//! │  u8  │ head u16 │  u16    │ start u16  │  u8  │ u16 × ncells │   │  ◄──  │
//! └──────┴──────────┴─────────┴────────────┴──────┴──────────────┴───┴───────┘
//! type: 1 = interior, 2 = leaf
//! cell (leaf):     key_len u16 ∥ val_len u16 ∥ key ∥ val
//! cell (interior): child u32   ∥ key_len u16 ∥ key      (separator)
//! ```
//!
//! Rules (same as SQLite): cell ptr array sorted by key, grows up; cells grow
//! down from the end; deleted cells become freeblocks (u16 next ∥ u16 size,
//! min 4 bytes); insert tries freeblocks first, defragments when fragmented
//! space would fit but no contiguous slot does.

pub const PAGE_SIZE: usize = 4096;
const _HEADER_SIZE: usize = 8;

pub struct Page {
    pub buf: [u8; PAGE_SIZE],
}

impl Page {
    pub fn new_leaf() -> Self {
        todo!("zero buf, type=2, content_start=PAGE_SIZE")
    }

    pub fn new_interior() -> Self {
        todo!()
    }

    pub fn is_leaf(&self) -> bool {
        self.buf[0] == 2
    }

    pub fn ncells(&self) -> u16 {
        u16::from_be_bytes([self.buf[3], self.buf[4]])
    }

    /// Binary search the cell pointer array. Ok(idx) = exact, Err(idx) = insert pos.
    pub fn find(&self, key: &[u8]) -> Result<usize, usize> {
        let _ = key;
        todo!("binary search touching only the ptr array, then one jump per probe")
    }

    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        let _ = key;
        todo!()
    }

    /// Insert (leaf). Returns false if the page is full even after defrag —
    /// caller must split.
    pub fn insert(&mut self, key: &[u8], val: &[u8]) -> bool {
        let _ = (key, val);
        todo!("find slot via freeblock chain or content area; shift ptr array; maybe defragment")
    }

    pub fn delete(&mut self, key: &[u8]) -> bool {
        let _ = key;
        todo!("remove ptr, chain a freeblock (merge adjacent for extra credit)")
    }

    pub fn free_space(&self) -> usize {
        todo!("content_start - ptr_array_end + sum(freeblocks) + frag")
    }

    /// Move upper half of cells into `right`; return the separator key.
    pub fn split_into(&mut self, right: &mut Page) -> Vec<u8> {
        let _ = right;
        todo!("experiment hook: try suffix-truncating the separator here and measure fanout")
    }

    pub fn cells(&self) -> impl Iterator<Item = (&[u8], &[u8])> + '_ {
        todo!("in ptr-array order — used by range scans and split");
        #[allow(unreachable_code)]
        std::iter::empty()
    }

    // interior-page ops
    pub fn interior_insert(&mut self, key: &[u8], child: u32) -> bool {
        let _ = (key, child);
        todo!()
    }

    pub fn child_for(&self, key: &[u8]) -> u32 {
        let _ = key;
        todo!("descend: first separator > key wins; last child is the rightmost")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_delete() {
        let mut p = Page::new_leaf();
        assert!(p.insert(b"bbb", b"2"));
        assert!(p.insert(b"aaa", b"1"));
        assert!(p.insert(b"ccc", b"3"));
        assert_eq!(p.get(b"bbb"), Some(&b"2"[..]));
        assert!(p.delete(b"bbb"));
        assert_eq!(p.get(b"bbb"), None);
        assert_eq!(p.ncells(), 2);
    }

    #[test]
    fn cells_stay_sorted() {
        let mut p = Page::new_leaf();
        for i in [5u8, 1, 9, 3, 7] {
            p.insert(&[i], &[i]);
        }
        let keys: Vec<u8> = p.cells().map(|(k, _)| k[0]).collect();
        assert_eq!(keys, vec![1, 3, 5, 7, 9]);
    }

    #[test]
    fn freeblock_reuse_after_delete() {
        let mut p = Page::new_leaf();
        let big = vec![0u8; 200];
        let mut n = 0;
        while p.insert(format!("key{n:04}").as_bytes(), &big) {
            n += 1;
        }
        // free one slot mid-page; a same-size insert must succeed without split
        assert!(p.delete(b"key0005"));
        assert!(p.insert(b"key9999", &big), "freeblock not reused");
    }

    #[test]
    fn split_produces_valid_separator() {
        let mut left = Page::new_leaf();
        let mut i = 0;
        while left.insert(format!("k{i:05}").as_bytes(), b"v") {
            i += 1;
        }
        let mut right = Page::new_leaf();
        let sep = left.split_into(&mut right);
        let max_left = left.cells().last().unwrap().0.to_vec();
        let min_right = right.cells().next().unwrap().0.to_vec();
        assert!(max_left < sep || max_left[..] == sep[..], "sep must be >= left max");
        assert!(sep[..] <= min_right[..], "sep must be <= right min");
    }
}
