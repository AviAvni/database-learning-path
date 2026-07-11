//! YOU implement: the vectorized engine (X100/DuckDB style).
//!
//! Same query, but next() fills a BATCH of up to BATCH_SIZE rows.
//! Requirements the tests enforce (and the ones that make it FAST):
//!
//! - columnar batches: three parallel arrays, NOT Vec<Row>
//! - the filter produces a SELECTION VECTOR over the scan's batch —
//!   it must NOT copy the data columns (DuckDB SelectionVector)
//! - the aggregation loops over the selection and adds into a flat
//!   `[i64; NUM_GROUPS]` array (k is dense — no hash table needed;
//!   DataFusion's intern-then-flat-arrays shape, with interning free)
//! - inner loops must be simple `for` over slices — give the
//!   autovectorizer a chance
//!
//! Suggested shape (feel free to restructure — only `run` is public API):
//!
//! ```text
//! struct Batch { k: [u32; N], v: [i32; N], f: [u32; N], len: usize }
//! scan_next(&table, &mut pos, &mut batch) -> bool        // fills batch
//! filter(&batch, threshold, &mut sel: Vec<u32>)          // builds sel
//! agg(&batch, &sel, &mut sums)                           // adds via sel
//! ```
//!
//! After it passes: try BATCH_SIZE = 64 and 65536 in exec_bench and
//! record the U-curve (X100's Figure) in notes.md.

use crate::data::Table;

pub const BATCH_SIZE: usize = 1024;

/// Run `SELECT k, SUM(v) WHERE f < threshold GROUP BY k` vectorized.
/// Returns sums indexed by group key.
pub fn run(table: &Table, threshold: u32) -> Vec<i64> {
    let _ = (table, threshold);
    todo!("scan in batches, filter to selection vector, aggregate via selection")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{data::Table, oracle};

    #[test]
    fn vectorized_matches_oracle() {
        let t = Table::generate(100_000, 42);
        assert_eq!(run(&t, 50), oracle(&t, 50));
    }

    #[test]
    fn edge_selectivities() {
        let t = Table::generate(10_000, 7);
        assert_eq!(run(&t, 0), oracle(&t, 0)); // nothing passes
        assert_eq!(run(&t, 100), oracle(&t, 100)); // everything passes
    }

    #[test]
    fn non_multiple_of_batch_size() {
        // partial final batch must not be dropped or double-counted
        let t = Table::generate(BATCH_SIZE * 3 + 17, 9);
        assert_eq!(run(&t, 50), oracle(&t, 50));
    }

    #[test]
    fn tiny_table() {
        let t = Table::generate(3, 1);
        assert_eq!(run(&t, 50), oracle(&t, 50));
    }
}
