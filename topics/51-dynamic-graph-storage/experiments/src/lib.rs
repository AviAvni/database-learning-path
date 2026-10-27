//! Topic 51 — Dynamic graph storage: a CSR that takes writes.
//!
//! Lane 1 (provided): the same seeded power-law graph held four ways — a
//! static CSR, a CSR plus FalkorDB-style delta (`DP`/`DM` side structures and
//! a flush threshold), a `Vec<Vec<u32>>` adjacency list, and a CSR that really
//! splices on every insert — driven by two-hop neighbourhood scans interleaved
//! with edge inserts and deletes.
//!
//! Lane 2 (exercise): Sortledton-style storage — one sorted vector per small
//! neighbourhood, an unrolled list of sorted blocks (50–100% full) per hub.
//!
//! Lane 3 (exercise): a packed-memory-array CSR (Wheatman & Xu's PCSR) — one
//! gapped array for every edge, a sentinel per vertex, density-driven
//! rebalances.

pub mod csr;
pub mod delta;
pub mod gen;
pub mod hash;
pub mod pma;
pub mod segblocks;
pub mod store;
pub mod vecvec;
