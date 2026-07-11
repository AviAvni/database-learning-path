//! Provided: the tuple-at-a-time Volcano engine. The honest 1990 baseline.
//!
//! Deliberately faithful to the model's costs:
//! - operators compose via `Box<dyn Operator>` — every next() is a
//!   virtual call (postgres: `node->ExecProcNode(node)`)
//! - predicates and aggregates walk per-tuple
//! - the tuple leaves registers between operators
//!
//! Don't optimize this. It exists to be beaten.

use crate::data::Table;
use crate::NUM_GROUPS;

#[derive(Clone, Copy)]
pub struct Row {
    pub k: u32,
    pub v: i32,
    pub f: u32,
}

pub trait Operator {
    fn next(&mut self) -> Option<Row>;
}

pub struct Scan<'a> {
    table: &'a Table,
    pos: usize,
}

impl<'a> Scan<'a> {
    pub fn new(table: &'a Table) -> Self {
        Scan { table, pos: 0 }
    }
}

impl Operator for Scan<'_> {
    fn next(&mut self) -> Option<Row> {
        if self.pos >= self.table.len() {
            return None;
        }
        let i = self.pos;
        self.pos += 1;
        Some(Row {
            k: self.table.k[i],
            v: self.table.v[i],
            f: self.table.f[i],
        })
    }
}

pub struct FilterOp<'a> {
    input: Box<dyn Operator + 'a>,
    threshold: u32,
}

impl<'a> FilterOp<'a> {
    pub fn new(input: Box<dyn Operator + 'a>, threshold: u32) -> Self {
        FilterOp { input, threshold }
    }
}

impl Operator for FilterOp<'_> {
    fn next(&mut self) -> Option<Row> {
        loop {
            let row = self.input.next()?;
            if row.f < self.threshold {
                return Some(row);
            }
        }
    }
}

/// Blocking group-by-sum: drains its input on first call, then emits.
pub struct AggOp<'a> {
    input: Box<dyn Operator + 'a>,
    sums: Vec<i64>,
    emitted: usize,
    done_building: bool,
}

impl<'a> AggOp<'a> {
    pub fn new(input: Box<dyn Operator + 'a>) -> Self {
        AggOp {
            input,
            sums: vec![0; NUM_GROUPS],
            emitted: 0,
            done_building: false,
        }
    }
}

impl Operator for AggOp<'_> {
    fn next(&mut self) -> Option<Row> {
        if !self.done_building {
            while let Some(row) = self.input.next() {
                self.sums[row.k as usize] += row.v as i64;
            }
            self.done_building = true;
        }
        if self.emitted >= NUM_GROUPS {
            return None;
        }
        let k = self.emitted;
        self.emitted += 1;
        Some(Row {
            k: k as u32,
            v: 0,
            f: 0,
        })
    }
}

/// Run the full query through the operator tree, return sums per group.
pub fn run(table: &Table, threshold: u32) -> Vec<i64> {
    // black_box: LLVM happily devirtualizes this statically-known chain and
    // fuses the loops — turning our 1990 baseline into a compiled engine.
    // Real engines build trees at runtime from plans; keep the calls honest.
    let scan: Box<dyn Operator> = std::hint::black_box(Box::new(Scan::new(table)));
    let filter: Box<dyn Operator> = std::hint::black_box(Box::new(FilterOp::new(scan, threshold)));
    let mut agg = AggOp::new(filter);
    // Drive the root to completion (a real engine's executor loop).
    while agg.next().is_some() {}
    agg.sums
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{data::Table, oracle};

    #[test]
    fn volcano_matches_oracle() {
        let t = Table::generate(100_000, 42);
        assert_eq!(run(&t, 50), oracle(&t, 50));
        assert_eq!(run(&t, 0), oracle(&t, 0));
        assert_eq!(run(&t, 100), oracle(&t, 100));
    }
}
