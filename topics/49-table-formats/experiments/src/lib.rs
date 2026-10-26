//! Topic 49 — open file & table formats.
//!
//! Lane 1 (provided): the metadata bill of an append-only table fed by many
//! small commits, in two shapes — Iceberg's tree (table metadata -> manifest
//! list -> manifests) and Delta's log (one action file per commit, periodic
//! checkpoints) — on a metered in-memory object store; plus the Parquet
//! footer a reader must decode to find 10 columns in a wide file.
//!
//! Lane 2 (exercise): optimistic concurrent commits on the store's
//! conditional put — conflict detection, rebase and retry, abort.
//!
//! Lane 3 (exercise): a checkpointing policy that decides *when* compacting
//! the log pays for itself, against the fixed every-10-commits default.

pub mod delta;
pub mod footer;
pub mod gen;
pub mod iceberg;
pub mod occ;
pub mod policy;
pub mod store;
