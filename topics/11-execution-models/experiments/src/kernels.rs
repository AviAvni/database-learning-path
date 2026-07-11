//! YOU implement: the fused single-pass kernel — what a compiled engine
//! (HyPer/Typer) would emit for this exact pipeline.
//!
//! One loop over the table, no operators, no batches: filter is a
//! branch-free select, the sum goes straight into the flat group array.
//! The row never leaves registers.
//!
//! Requirements:
//! - ONE pass over the columns (no separate filter materialization)
//! - branch-free filter: turn `f < threshold` into a 0/1 (or 0/-1 mask)
//!   multiply/AND instead of an `if` — you measured why in topic 0
//!   (branch_misprediction: 8.1x on shuffled data)
//! - stretch: split the loop into N independent accumulator arrays and
//!   merge at the end (ILP — polars float_sum's multi-accumulator trick;
//!   measure whether it helps when the destination is a 64-slot array
//!   with random access)
//!
//! After it passes, LOOK AT THE ASM (`cargo asm` or objdump): did the
//! autovectorizer emit NEON? Record in notes.md.

use crate::data::Table;

/// Fused scan+filter+group-sum. Returns sums indexed by group key.
pub fn run(table: &Table, threshold: u32) -> Vec<i64> {
    let _ = (table, threshold);
    todo!("one branchless pass: sums[k[i]] += v[i] * (f[i] < threshold) as i64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{data::Table, oracle};

    #[test]
    fn kernel_matches_oracle() {
        let t = Table::generate(100_000, 42);
        assert_eq!(run(&t, 50), oracle(&t, 50));
        assert_eq!(run(&t, 0), oracle(&t, 0));
        assert_eq!(run(&t, 100), oracle(&t, 100));
    }

    #[test]
    fn negative_values_survive_the_mask_trick() {
        // if you use a multiply-by-mask, sign extension must be right
        let t = Table::generate(10_000, 3);
        assert_eq!(run(&t, 37), oracle(&t, 37));
    }
}
