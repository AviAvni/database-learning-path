//! Topic 54 — main-memory join algorithms.
//!
//! One question, three answers, one machine. Join R (the build side, unique
//! keys) with S (the probe side, foreign keys into R) when both are already in
//! memory, and count the matches:
//!
//! * **Lane 1 (provided)** — the *no-partitioning* hash join: one shared
//!   open-addressing table, built once, probed once. Plus a hardware probe
//!   (cache-latency curve, memory bandwidth) so every later number can be
//!   read against what this machine can do.
//! * **Lane 2 (exercise)** — the *radix-partitioned* hash join: scatter both
//!   inputs into 2^bits partitions by hash bits (one or two passes), then
//!   join each partition pair with a table small enough to stay in cache.
//! * **Lane 3 (exercise)** — the *sort-merge* join: sort both inputs by key,
//!   then walk them together.

pub mod gen;
pub mod hash;
pub mod hw;
pub mod npj;
pub mod radix;
pub mod sortmerge;
pub mod timing;
pub mod tuple;
