//! LANE 3 (provided half) — the in-place baseline.
//!
//! A database page `pid` lives at a fixed address forever (LBA = pid, the
//! "PID as an offset" of the paper's Figure 2b). Every eviction overwrites it
//! there. To survive a torn write the engine first copies the page to a
//! **doublewrite buffer** (DWB) — a small fixed region written round-robin —
//! and only then overwrites the home location (§2, "DB WAF: User writes
//! transformed by the DBMS"). So DB WAF is exactly 2.00 by construction, the
//! same 2.00 the paper's Table 1 reports for in-place LeanStore; what this
//! lane measures is the *other* factor, the SSD's, which the in-place engine
//! has no way to influence.

use crate::ftl::{Ssd, SsdConfig, Waf};

/// Doublewrite-buffer slots (pages). 256 = one erase block in `lane3()`.
pub const DWB_SLOTS: u32 = 256;

pub struct InPlaceDb {
    pub ssd: Ssd,
    pub pages: u32,
    dwb_next: u32,
    pub user: u64,
    pub db: u64,
}

impl InPlaceDb {
    /// A database of `pages` pages on a fresh device, loaded sequentially.
    pub fn load(cfg: SsdConfig, pages: u32) -> Self {
        assert!(pages + DWB_SLOTS <= cfg.lbas(), "database does not fit");
        let mut ssd = Ssd::new(cfg);
        for pid in 0..pages {
            ssd.write(pid);
        }
        InPlaceDb {
            ssd,
            pages,
            dwb_next: 0,
            user: 0,
            db: 0,
        }
    }

    /// Persist one dirty page: DWB copy, then the home location.
    pub fn write_page(&mut self, pid: u32) {
        let dwb = self.pages + self.dwb_next;
        self.dwb_next = (self.dwb_next + 1) % DWB_SLOTS;
        self.ssd.write(dwb);
        self.ssd.write(pid);
        self.user += 1;
        self.db += 2;
    }
}

/// Run `stream` (page ids) against an in-place engine holding `fill` of the
/// device's addressable space. The first `warm` writes bring the device to
/// steady state and are not counted.
pub fn run_inplace(cfg: SsdConfig, fill: f64, stream: &[u32], warm: usize) -> Waf {
    let pages = (cfg.lbas() as f64 * fill) as u32 - DWB_SLOTS;
    let mut db = InPlaceDb::load(cfg, pages);
    for &p in &stream[..warm] {
        db.write_page(p % pages);
    }
    let (u0, d0) = (db.user, db.db);
    let (_, f0) = db.ssd.counters();
    for &p in &stream[warm..] {
        db.write_page(p % pages);
    }
    let (_, f1) = db.ssd.counters();
    Waf {
        user: db.user - u0,
        db: db.db - d0,
        flash: f1 - f0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_waf_is_two_by_construction() {
        let cfg = SsdConfig {
            pages_per_block: 16,
            logical_blocks: 64,
            op_blocks: 8,
        };
        let s = crate::gen::page_stream(5, 20_000, 1 << 20, 0.0);
        let w = run_inplace(cfg, 0.9, &s, 5_000);
        assert!((w.db_waf() - 2.0).abs() < 1e-12);
        assert!(w.ssd_waf() > 1.0);
        assert!((w.total() - w.db_waf() * w.ssd_waf()).abs() < 1e-9);
    }
}
