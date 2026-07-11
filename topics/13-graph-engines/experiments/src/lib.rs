pub mod adj_list;
pub mod csr;
pub mod data;
pub mod matrix;

/// Stamp-based visited set: `seen[v] == stamp` means visited in the
/// CURRENT query. Bump the stamp instead of clearing 4 MB per query —
/// the reusable-state lesson from topic 0's cache_ladder bug.
pub fn new_seen(num_nodes: u32) -> Vec<u32> {
    vec![0; num_nodes as usize]
}
