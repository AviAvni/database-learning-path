//! Topic 47 — Semi-join reduction in practice.
//!
//! Lane 1 (provided): one seeded chain join — a three-hop graph pattern with
//! a selective predicate at the far end from the biggest relation — run as a
//! left-deep hash-join plan in a good order and a bad order, then again after
//! a full Yannakakis semi-join reduction. The headline is a count of
//! intermediate tuples, which reproduces exactly.
//!
//! Lane 2 (exercise): the same reduction with Bloom filters in place of exact
//! semi-joins ("predicate transfer"), with every false positive accounted for
//! and split into the filter's own and the ones it inherited from upstream.
//!
//! Lane 3 (exercise): no reduction phase at all — each hash join builds a
//! bitmap from its build side and pushes it into the probe subtree, the way
//! SQL Server's batch-mode hash join does, in two pushdown modes.

pub mod bitmap;
pub mod bloom;
pub mod gen;
pub mod hash;
pub mod join;
pub mod rel;
pub mod transfer;
pub mod yannakakis;
