//! LANE 3 (provided half) — a page-mapped flash translation layer.
//!
//! This is a *model* of an SSD, not an SSD: the numbers it produces are write
//! counts under a stated policy, and they are only as good as the policy. It
//! is here because the one quantity "How to Write to SSDs" (VLDB'26) is about —
//! how many flash pages the device programs per page the host writes — cannot
//! be read off a laptop's SSD (no OCP log on consumer drives, which is exactly
//! the paper's §7.5 complaint about CNS devices), but it can be counted exactly
//! in a model whose rules match the paper's §2 description:
//!
//! - writes go to flash *pages* and are always out of place: the device
//!   appends to the current *active block* and remaps the logical block
//!   address (LBA) to the new physical page; the old copy becomes invalid;
//! - space is reclaimed only by erasing a whole *erase block*, so a block that
//!   still holds valid pages must have them copied elsewhere first — those
//!   copies are the device's write amplification;
//! - victims are chosen **greedily** (fewest valid pages), which the paper
//!   notes is optimal for uniform random writes and what its prior study found
//!   commodity SSDs effectively do (§4.1, §10.1);
//! - the device keeps a fixed amount of over-provisioned (OP) space the host
//!   can never address.
//!
//! One stream, one active block: host writes and GC relocations are appended
//! to the same block, as on a drive without streams or FDP. That is the
//! "multiplexing" the paper's §5.4 is about.

use std::collections::VecDeque;

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub struct SsdConfig {
    /// flash pages per erase block
    pub pages_per_block: u32,
    /// erase blocks the host can address
    pub logical_blocks: u32,
    /// extra erase blocks only the device can use (over-provisioning)
    pub op_blocks: u32,
}

impl SsdConfig {
    /// The configuration every lane-3 figure uses: 1,000 MiB addressable in
    /// 1 MiB erase blocks of 256 x 4 KiB pages, plus 10% over-provisioning.
    pub const fn lane3() -> Self {
        SsdConfig {
            pages_per_block: 256,
            logical_blocks: 1000,
            op_blocks: 100,
        }
    }
    pub fn lbas(&self) -> u32 {
        self.pages_per_block * self.logical_blocks
    }
}

pub struct Ssd {
    pub cfg: SsdConfig,
    l2p: Vec<u32>,
    p2l: Vec<u32>,
    valid: Vec<u32>,
    free: VecDeque<u32>,
    is_free: Vec<bool>,
    active: u32,
    wp: u32,
    /// pages the host asked to write
    pub host_writes: u64,
    /// pages programmed into flash: host writes + GC relocations
    pub flash_writes: u64,
    /// garbage collections run, and the valid pages they found in victims
    pub gcs: u64,
    pub gc_valid: u64,
}

/// Keep at least this many erased blocks in reserve; GC runs below it.
const GC_RESERVE: usize = 2;

impl Ssd {
    pub fn new(cfg: SsdConfig) -> Self {
        let blocks = cfg.logical_blocks + cfg.op_blocks;
        let mut free: VecDeque<u32> = (0..blocks).collect();
        let active = free.pop_front().unwrap();
        let mut is_free = vec![true; blocks as usize];
        is_free[active as usize] = false;
        Ssd {
            cfg,
            l2p: vec![NONE; cfg.lbas() as usize],
            p2l: vec![NONE; (blocks * cfg.pages_per_block) as usize],
            valid: vec![0; blocks as usize],
            free,
            is_free,
            active,
            wp: 0,
            host_writes: 0,
            flash_writes: 0,
            gcs: 0,
            gc_valid: 0,
        }
    }

    /// The host writes one page at `lba`.
    pub fn write(&mut self, lba: u32) {
        assert!(lba < self.cfg.lbas(), "lba {lba} out of range");
        self.host_writes += 1;
        self.program(lba);
        while self.free.len() < GC_RESERVE {
            self.gc();
        }
    }

    /// The host tells the device an LBA no longer holds data (TRIM/discard).
    pub fn trim(&mut self, lba: u32) {
        self.invalidate(lba);
    }

    fn invalidate(&mut self, lba: u32) {
        let old = self.l2p[lba as usize];
        if old != NONE {
            self.p2l[old as usize] = NONE;
            self.valid[(old / self.cfg.pages_per_block) as usize] -= 1;
            self.l2p[lba as usize] = NONE;
        }
    }

    fn program(&mut self, lba: u32) {
        self.invalidate(lba);
        if self.wp == self.cfg.pages_per_block {
            self.active = self.free.pop_front().expect("device out of erased blocks");
            self.is_free[self.active as usize] = false;
            self.wp = 0;
        }
        let ppa = self.active * self.cfg.pages_per_block + self.wp;
        self.wp += 1;
        self.l2p[lba as usize] = ppa;
        self.p2l[ppa as usize] = lba;
        self.valid[self.active as usize] += 1;
        self.flash_writes += 1;
    }

    /// Greedy: erase the full block with the fewest valid pages, relocating
    /// those pages into the active block first.
    fn gc(&mut self) {
        let ppb = self.cfg.pages_per_block;
        let victim = (0..self.valid.len() as u32)
            .filter(|&b| b != self.active && !self.is_free[b as usize])
            .min_by_key(|&b| self.valid[b as usize])
            .expect("no GC victim");
        self.gcs += 1;
        self.gc_valid += self.valid[victim as usize] as u64;
        for p in victim * ppb..(victim + 1) * ppb {
            let lba = self.p2l[p as usize];
            if lba != NONE {
                self.program(lba); // a relocation: flash write, no host write
            }
        }
        debug_assert_eq!(self.valid[victim as usize], 0);
        self.is_free[victim as usize] = true;
        self.free.push_back(victim);
    }

    /// Snapshot of the two counters, for measuring over a window.
    pub fn counters(&self) -> (u64, u64) {
        (self.host_writes, self.flash_writes)
    }
}

/// Write amplification over a measurement window, at both layers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Waf {
    /// pages the workload modified (one per user update)
    pub user: u64,
    /// pages the database wrote to the device (user + doublewrite + DB GC)
    pub db: u64,
    /// pages the device programmed (db + device GC relocations)
    pub flash: u64,
}

impl Waf {
    pub fn db_waf(&self) -> f64 {
        self.db as f64 / self.user as f64
    }
    pub fn ssd_waf(&self) -> f64 {
        self.flash as f64 / self.db as f64
    }
    /// The paper's cross-layer metric: Total WAF = DB WAF x SSD WAF (§2).
    pub fn total(&self) -> f64 {
        self.flash as f64 / self.user as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> SsdConfig {
        SsdConfig {
            pages_per_block: 16,
            logical_blocks: 32,
            op_blocks: 4,
        }
    }

    #[test]
    fn sequential_overwrites_never_relocate() {
        // rewriting the whole LBA space in order invalidates whole blocks, so
        // every victim is empty and the device writes exactly what it is given
        let mut ssd = Ssd::new(small());
        for _round in 0..5 {
            for lba in 0..ssd.cfg.lbas() {
                ssd.write(lba);
            }
        }
        assert_eq!(ssd.flash_writes, ssd.host_writes);
        assert!(ssd.gcs > 0);
        assert_eq!(ssd.gc_valid, 0);
    }

    #[test]
    fn random_overwrites_amplify() {
        let mut ssd = Ssd::new(small());
        for lba in 0..ssd.cfg.lbas() {
            ssd.write(lba);
        }
        let s = crate::gen::page_stream(1, 20_000, ssd.cfg.lbas(), 0.0);
        let (h0, f0) = ssd.counters();
        for lba in s {
            ssd.write(lba);
        }
        let (h1, f1) = ssd.counters();
        let waf = (f1 - f0) as f64 / (h1 - h0) as f64;
        assert!(waf > 2.0, "100% full with 12.5% OP should amplify, got {waf}");
    }

    #[test]
    fn mapping_is_consistent() {
        let mut ssd = Ssd::new(small());
        let s = crate::gen::page_stream(3, 5_000, ssd.cfg.lbas(), 0.9);
        for lba in s {
            ssd.write(lba);
        }
        let total_valid: u32 = ssd.valid.iter().sum();
        let mapped = ssd.l2p.iter().filter(|&&p| p != NONE).count() as u32;
        assert_eq!(total_valid, mapped);
        for (lba, &ppa) in ssd.l2p.iter().enumerate() {
            if ppa != NONE {
                assert_eq!(ssd.p2l[ppa as usize], lba as u32);
            }
        }
    }
}
