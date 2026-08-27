//! Topic 44 — the e-graph as a relational database.
//!
//! Lane 1 (provided): the same pattern matched two ways — the backtracking
//! search every e-matcher shipped before 2022, and the relational one that
//! compiles the pattern to a conjunctive query and runs generic join over the
//! e-graph's tables.
//!
//! Lanes 2 and 3 are the exercises: semi-naive evaluation (`semi_naive`) and a
//! binary-join baseline for the cyclic multi-pattern (`binary_join`).

pub mod backtrack;
pub mod binary_join;
pub mod egraph;
pub mod gen;
pub mod pattern;
pub mod relational;
pub mod semi_naive;
