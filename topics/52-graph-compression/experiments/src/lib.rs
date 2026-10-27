//! Topic 52 — Graph reordering and compression.
//!
//! Lane 1 (provided): one seeded power-law graph with planted communities,
//! loaded with random vertex ids, then relabelled four ways — random,
//! degree-sorted, BFS, and the generator's own community order (an oracle,
//! not an algorithm). Each labelling is priced in bits per edge (gap
//! + varint, Elias-gamma, LogGap) and in time (a neighbourhood-gather scan
//! and a BFS on the uncompressed CSR, the same scan decoding the varint
//! form, and a simulated cache's miss rate on the gather).
//!
//! Lane 2 (exercise): recursive graph bisection (Dhulipala et al., KDD'16)
//! as a vertex ordering — `bisect.rs`.
//!
//! Lane 3 (exercise): WebGraph-style reference + interval + residual coding
//! (Boldi & Vigna, WWW'04) with a decode-and-scan — `refcode.rs`.

pub mod bisect;
pub mod csr;
pub mod gapcode;
pub mod gen;
pub mod measure;
pub mod order;
pub mod refcode;
