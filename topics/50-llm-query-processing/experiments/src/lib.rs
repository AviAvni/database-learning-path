//! Topic 50 — LLM-era query processing, priced.
//!
//! A semantic predicate is an operator whose per-row cost is a model call:
//! dollars and seconds instead of nanoseconds, and an answer that is wrong
//! some of the time. This crate is a SIMULATION of that — no network, no LLM,
//! every model a seeded function with stated parameters (src/model.rs) — so
//! that the classic optimizer ideas can be priced exactly:
//!
//! * lane 1 (provided, `semop_bench`): predicate ordering by rank, proxy →
//!   oracle cascades, a shared answer cache, and LIMIT pushdown, on a
//!   two-predicate filter over 10,000 rows;
//! * lane 2 (exercise, `threshold`): cascade thresholds with a statistical
//!   guarantee, LOTUS-style;
//! * lane 3 (exercise, `join`): a semantic join that does not pay N×M.

pub mod gen;
pub mod join;
pub mod model;
pub mod plan;
pub mod rng;
pub mod threshold;
