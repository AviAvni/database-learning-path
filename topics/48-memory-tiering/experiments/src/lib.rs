//! Topic 48 — Memory tiering: CXL, disaggregated memory and placement.
//!
//! This machine (Apple M3 Pro, unified memory) has no CXL device and no NUMA
//! node, so nothing here measures CXL. What it does have is a tier boundary
//! of its own — on-chip cache in front of DRAM — and the mechanism every
//! tiering policy in the papers depends on is the same at that boundary:
//! a dependent load costs whatever the slowest tier it touches costs, and a
//! placement policy can only move *pages* (or lines) — so what matters is
//! whether the hot objects share them.
//!
//! Lane 1 (provided, measured): a pointer-chase latency ladder across this
//! machine's tiers, and a hot/cold placement experiment — the same seeded
//! access stream over 1 GiB of 64-byte objects, with the hot set packed
//! contiguously or scattered across the address space.
//!
//! Lane 2 (exercise, counted): learn a layout from an access trace — the
//! "address-space engineering" of OBASE (OSDI'26) — and score it on a
//! held-out window.
//!
//! Lane 3 (exercise, MODEL): a two-tier page-placement simulator with a TPP
//! style promotion/demotion policy, priced with published CXL latencies.
//! Every figure from it is a model, not a measurement.

pub mod chase;
pub mod gen;
pub mod layout;
pub mod reorg;
pub mod stats;
pub mod tier;

/// Bytes per object. 64 B is one x86 cache line and half an Apple one; OBASE
/// counts page utilization in 64 B units (§2.2), so one object = one unit.
pub const OBJ_BYTES: usize = 64;

/// Objects in the placement lanes: 2^24 × 64 B = 1 GiB, far beyond this
/// machine's 16 MiB L2 and its system-level cache, so a cold access is a
/// DRAM access.
pub const N_OBJ: u32 = 1 << 24;

/// Page sizes the counts are reported at: 16 KiB is this machine's base page
/// (`sysctl hw.pagesize`); 4 KiB is Linux x86's, the unit TPP and MEMTIS
/// migrate.
pub const PAGE_16K: usize = 16 * 1024;
pub const PAGE_4K: usize = 4 * 1024;

/// Objects per page at each size.
pub const OBJS_PER_16K: u32 = (PAGE_16K / OBJ_BYTES) as u32;
pub const OBJS_PER_4K: u32 = (PAGE_4K / OBJ_BYTES) as u32;

/// This machine's cache line (`sysctl hw.cachelinesize` = 128).
pub const LINE_BYTES: usize = 128;
