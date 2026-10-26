//! LANE 3 (exercise) — out-of-place writes, and the factor the in-place
//! engine cannot touch.
//!
//! "How to Write to SSDs" (Lee, Ziegler & Leis, VLDB'26) argues that the
//! in-place engine of [`crate::inplace`] pays twice: DB WAF = 2.00 for the
//! doublewrite buffer, times an SSD WAF it inherits from the device's greedy
//! GC (2.36 on their PM9A3, Table 1; whatever `io_bench` lane 3 prints on
//! this model). Writing out of place removes the doublewrite buffer — the old
//! version stays valid until the new one is durable — and, more importantly,
//! hands *placement* to the database (§2, "Out-of-place writes are essential
//! for minimizing total WAF").
//!
//! Implement [`run_out_of_place`]: a database of `pages` pages (same `pages`
//! as the in-place lane, so both hold the same data) that never overwrites a
//! page where it lies.
//!
//!  1. Divide the device's LBAs into **zones** of `oc.zone_pages` consecutive
//!     LBAs. Keep a PID -> LBA map and an LBA -> PID reverse map (the paper's
//!     `PID2OffsetTable` and `StorageSpace`, §6.2), and a valid-page count per
//!     zone.
//!  2. A page write *appends* to an open zone (round-robin over
//!     `oc.open_zones` open zones), writes that LBA to the [`Ssd`], marks the
//!     page's previous LBA invalid in the reverse map, and updates both maps.
//!     No doublewrite: one device write per user write, plus GC.
//!  3. **DB garbage collection**, greedy (§4.1): whenever fewer than
//!     `oc.open_zones + 1` zones are free, pick the full, non-open zone with
//!     the fewest valid pages, re-append each of its valid pages (that is DB
//!     write amplification), and return the zone to the free list. Do *not*
//!     TRIM — the paper's engine does not, and the point of the exercise is
//!     that the SSD's view of what is valid is the stale one.
//!  4. Load: append every page once (pid 0..pages, in order) before the
//!     stream. Then run `stream[..warm]` uncounted and `stream[warm..]`
//!     counted, exactly as [`crate::inplace::run_inplace`] does, and return
//!     the counted window's [`Waf`] plus the GC victims' valid pages.
//!
//! The tests below are the specification. Three of them are the paper's
//! claims restated for this model:
//!
//! - with one open zone exactly one erase block long, every block the SSD
//!   ever erases is fully invalid, so **SSD WAF = 1** (§5.3, Figure 6);
//! - with zones half an erase block long, two zones with different
//!   deathtimes share each block and SSD WAF climbs well above 1 (Figure 7);
//! - greedy DB GC's amplification is 1 / (1 − v) where v is the victims'
//!   valid fraction (§4.1, "75% ... yielding a WAF of 1/(1 − 0.75) = 4").
//!
//! Reference numbers (from a reference implementation, in notes.md) are what
//! `io_bench` lane 3 should print once this is done.

use crate::ftl::{Ssd, SsdConfig, Waf};

#[derive(Clone, Copy, Debug)]
pub struct OopConfig {
    /// LBAs per zone — the database's garbage-collection unit
    pub zone_pages: u32,
    /// zones appended to concurrently (round-robin)
    pub open_zones: u32,
}

/// What DB GC found in its victims over the counted window.
#[derive(Clone, Copy, Debug, Default)]
pub struct GcStats {
    pub victims: u64,
    /// valid pages summed over all victims
    pub victim_valid: u64,
}

impl GcStats {
    /// Mean valid fraction of a victim zone.
    pub fn valid_ratio(&self, zone_pages: u32) -> f64 {
        self.victim_valid as f64 / (self.victims as f64 * zone_pages as f64)
    }
}

/// Same contract as [`crate::inplace::run_inplace`]: the database holds
/// `pages = lbas * fill - DWB_SLOTS` pages (so both lanes store the same
/// data), `stream[..warm]` is uncounted, and the returned [`Waf`] and
/// [`GcStats`] cover `stream[warm..]` only.
#[allow(unused_variables)] // stub: the arguments are the specification
pub fn run_out_of_place(
    cfg: SsdConfig,
    fill: f64,
    oc: OopConfig,
    stream: &[u32],
    warm: usize,
) -> (Waf, GcStats) {
    let _ssd: fn(SsdConfig) -> Ssd = Ssd::new; // you will need one
    todo!("out-of-place writes with zones and greedy DB GC (see module docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::page_stream;
    use crate::inplace::{run_inplace, DWB_SLOTS};

    /// A small device so the tests run in a debug build: 200 addressable
    /// erase blocks of 64 pages, plus 20 over-provisioned.
    fn cfg() -> SsdConfig {
        SsdConfig {
            pages_per_block: 64,
            logical_blocks: 200,
            op_blocks: 20,
        }
    }

    fn stream(theta: f64) -> (Vec<u32>, usize) {
        let c = cfg();
        let pages = (c.lbas() as f64 * 0.9) as u32 - DWB_SLOTS;
        let s = page_stream(45, 3 * c.lbas() as usize, pages, theta);
        let warm = s.len() / 2;
        (s, warm)
    }

    #[test]
    fn aligned_single_zone_has_ssd_waf_one() {
        let (s, warm) = stream(0.0);
        let oc = OopConfig { zone_pages: 64, open_zones: 1 };
        let (w, _) = run_out_of_place(cfg(), 0.9, oc, &s, warm);
        assert!(
            (w.ssd_waf() - 1.0).abs() < 1e-3,
            "zone == erase block, one stream: every victim block should be empty, got SSD WAF {:.3}",
            w.ssd_waf()
        );
        assert!(w.db_waf() > 1.0, "DB GC must relocate something at 90% fill");
    }

    #[test]
    fn half_block_zones_amplify_at_the_ssd() {
        let (s, warm) = stream(0.0);
        let oc = OopConfig { zone_pages: 32, open_zones: 1 };
        let (w, _) = run_out_of_place(cfg(), 0.9, oc, &s, warm);
        assert!(
            w.ssd_waf() > 1.5,
            "two zones with different deathtimes share each block; SSD WAF {:.3} is too low",
            w.ssd_waf()
        );
    }

    #[test]
    fn greedy_db_waf_is_one_over_one_minus_valid_ratio() {
        for theta in [0.0, 0.8] {
            let (s, warm) = stream(theta);
            let oc = OopConfig { zone_pages: 64, open_zones: 1 };
            let (w, g) = run_out_of_place(cfg(), 0.9, oc, &s, warm);
            let v = g.valid_ratio(64);
            let predicted = 1.0 / (1.0 - v);
            assert!(
                (w.db_waf() / predicted - 1.0).abs() < 0.03,
                "theta {theta}: DB WAF {:.3} but victims were {:.3} valid, predicting {:.3}",
                w.db_waf(),
                v,
                predicted
            );
        }
    }

    #[test]
    fn aligned_out_of_place_beats_in_place_on_total_waf() {
        let (s, warm) = stream(0.0);
        let inplace = run_inplace(cfg(), 0.9, &s, warm);
        let oc = OopConfig { zone_pages: 64, open_zones: 1 };
        let (oop, _) = run_out_of_place(cfg(), 0.9, oc, &s, warm);
        assert!(
            oop.total() < inplace.total(),
            "out-of-place total WAF {:.3} should beat in-place {:.3}",
            oop.total(),
            inplace.total()
        );
        // and the same user writes were counted on both sides
        assert_eq!(oop.user, inplace.user);
    }
}
