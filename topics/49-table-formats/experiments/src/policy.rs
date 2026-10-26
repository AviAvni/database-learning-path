//! LANE 3 (exercise) — when should the log be checkpointed?
//!
//! Lane 1 showed both halves of the trade on one table: a reader of a
//! Delta-shaped log pays one GET per commit since the last checkpoint, and a
//! checkpoint costs a write of the *whole* live state. Delta's reference
//! clients checkpoint "every 10 transactions" (Delta VLDB'20 §3.1.3); the
//! protocol itself only says writers "are encouraged to checkpoint
//! reasonably frequently" (PROTOCOL.md, "Checkpoints"). Ten is a constant;
//! the right interval depends on how often the table is read and how big it
//! is.
//!
//! Two textbook rules for "pay a lump sum to stop paying a recurring cost"
//! are on offer, and this lane prices both:
//!
//! - **Ski rental** (provided, [`SkiRental`]): renting skis costs a little
//!   per day, buying costs a lot once, and you do not know how many days you
//!   will ski. The classic deterministic rule — buy once the rent you have
//!   paid reaches the price — never pays more than twice what an oracle
//!   would. Here "rent" is what readers pay to replay the tail and "buying"
//!   is a checkpoint. The rule's guarantee assumes each day's rent is small
//!   next to the price; a log tail breaks that assumption, because by the
//!   time a rare reader shows up the tail is long and one read's rent is
//!   already many times the price. Lane 3 measures the damage.
//! - **EOQ** (exercise, [`Eoq`]): the *economic order quantity*, inventory
//!   theory's answer to "how big an order" — the batch size Q that minimises ordering cost plus holding cost
//!   when demand arrives at a steady rate. Per commit, checkpointing every k
//!   commits costs `price / k` in writes plus, at read rate λ, a reader that
//!   finds on average k/2 commits in the tail, `λ · c · k / 2`; the sum is
//!   smallest at `k* = sqrt(2 · price / (λ · c))`. Estimate λ from the reads
//!   seen so far and checkpoint when the tail reaches k*.
//!
use crate::delta::Delta;
use crate::gen::{commits, Workload};
use crate::store::Store;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// The byte-equivalent of one request: 5 ms × 50 MB/s = 250,000 bytes
/// (Delta VLDB'20 §2.3: "at least 5–10 ms of base latency", "roughly
/// 50–100 MB/s"; both at the low end).
pub const REQUEST_BYTES: f64 = 250_000.0;

/// What a policy may look at after each commit / read.
#[derive(Clone, Copy, Debug, Default)]
pub struct LogStats {
    pub version: u64,
    /// commits written since the last checkpoint (the tail a reader replays)
    pub tail_commits: u64,
    /// bytes of those commits
    pub tail_bytes: u64,
    /// bytes of the last checkpoint written (0 if none yet)
    pub last_checkpoint_bytes: u64,
}

pub trait CheckpointPolicy {
    fn name(&self) -> String;
    /// A reader just planned the latest snapshot, replaying the tail.
    fn on_read(&mut self, _s: &LogStats) {}
    /// A commit just landed; return true to checkpoint now.
    fn after_commit(&mut self, s: &LogStats) -> bool;
    /// A checkpoint was just written.
    fn on_checkpoint(&mut self) {}
}

/// Checkpoint every `k` commits (Delta's default is `Fixed(10)`).
pub struct Fixed(pub u64);

impl CheckpointPolicy for Fixed {
    fn name(&self) -> String {
        format!("every {} commits", self.0)
    }
    fn after_commit(&mut self, s: &LogStats) -> bool {
        s.tail_commits >= self.0
    }
}

pub struct Never;

impl CheckpointPolicy for Never {
    fn name(&self) -> String {
        "never".into()
    }
    fn after_commit(&mut self, _s: &LogStats) -> bool {
        false
    }
}

/// Buy when the rent paid since the last checkpoint reaches the price.
///
/// - **rent**: on every read, the reader paid for the tail — `tail_bytes`
///   plus `tail_commits` requests. Accumulate it.
/// - **price**: what a checkpoint *now* would cost to write. For an
///   append-only table the new checkpoint is the old one plus the tail, so
///   estimate it as `last_checkpoint_bytes + tail_bytes` plus two requests
///   (the checkpoint and `_last_checkpoint`).
/// - checkpoint when accumulated rent >= price; reset the rent after.
#[derive(Default)]
pub struct SkiRental {
    rent: f64,
}

impl CheckpointPolicy for SkiRental {
    fn name(&self) -> String {
        "ski rental".into()
    }
    fn on_read(&mut self, s: &LogStats) {
        self.rent += s.tail_bytes as f64 + REQUEST_BYTES * s.tail_commits as f64;
    }
    fn after_commit(&mut self, s: &LogStats) -> bool {
        let price = (s.last_checkpoint_bytes + s.tail_bytes) as f64 + 2.0 * REQUEST_BYTES;
        s.tail_commits > 0 && self.rent >= price
    }
    fn on_checkpoint(&mut self) {
        self.rent = 0.0;
    }
}

/// Checkpoint when the tail reaches the economic order quantity.
///
/// - **λ**: reads per commit so far (`on_read` count / `after_commit`
///   count). With no reads yet there is nothing to save: never checkpoint.
/// - **price**: a checkpoint written now — `last_checkpoint_bytes +
///   tail_bytes` plus two requests (the checkpoint and `_last_checkpoint`),
///   the same estimate [`SkiRental`] uses.
/// - **c**: what one tail commit costs one reader — a request plus the
///   commit's bytes (`tail_bytes / tail_commits` on average).
/// - checkpoint when `tail_commits >= sqrt(2 · price / (λ · c))`.
#[derive(Default)]
pub struct Eoq {
    // your state here
}

impl CheckpointPolicy for Eoq {
    fn name(&self) -> String {
        "EOQ".into()
    }
    #[allow(unused_variables)]
    fn on_read(&mut self, s: &LogStats) {
        todo!("count the read (see the type's docs)")
    }
    #[allow(unused_variables)]
    fn after_commit(&mut self, s: &LogStats) -> bool {
        todo!("estimate λ, price and c; compare the tail with k*")
    }
}

/// What a whole run cost.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SimResult {
    pub checkpoints: u64,
    pub reads: u64,
    pub write_bytes: u64,
    pub read_bytes: u64,
    pub requests: u64,
}

impl SimResult {
    /// Total cost in byte-equivalents.
    pub fn cost(&self) -> f64 {
        (self.write_bytes + self.read_bytes) as f64 + REQUEST_BYTES * self.requests as f64
    }
}

/// `n` seeded commits of one file each; after each commit the policy may
/// checkpoint, and then the reader plans the latest snapshot as many times
/// as `reads_per_commit` has accrued (0.1 = one read every ten commits).
pub fn simulate(policy: &mut dyn CheckpointPolicy, n: usize, reads_per_commit: f64, seed: u64) -> SimResult {
    let w = Workload { commits: n, files_per_commit: 1, commits_per_day: 100 };
    let cs = commits(seed, w);
    let mut store = Store::new();
    let mut d = Delta::new(0);
    let mut st = LogStats::default();
    let mut res = SimResult::default();
    let mut arrivals = ChaCha8Rng::seed_from_u64(seed ^ 0xA11CE);
    for c in &cs {
        let before = store.meter.put_bytes;
        st.version = d.append(&mut store, c);
        st.tail_commits += 1;
        st.tail_bytes += store.meter.put_bytes - before;
        if policy.after_commit(&st) {
            let before = store.meter.put_bytes;
            d.checkpoint(&mut store);
            st.last_checkpoint_bytes = store.meter.put_bytes - before;
            st.tail_commits = 0;
            st.tail_bytes = 0;
            res.checkpoints += 1;
            policy.on_checkpoint();
        }
        // reads arrive at random (seeded): floor(rate) reads, plus one more
        // with probability frac(rate) — so no fixed interval can line its
        // checkpoints up with the reads
        let whole = reads_per_commit.floor();
        let extra = reads_per_commit - whole;
        let k = whole as u64 + u64::from(extra > 0.0 && arrivals.gen::<f64>() < extra);
        for _ in 0..k {
            Delta::plan(&mut store, None, None);
            res.reads += 1;
            policy.on_read(&st);
        }
    }
    let m = store.meter;
    res.write_bytes = m.put_bytes;
    res.read_bytes = m.get_bytes;
    res.requests = m.requests();
    res
}

/// The fixed intervals the bench and tests compare against.
pub const FIXED_GRID: [u64; 10] = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000];

/// Cheapest fixed interval for a workload, and its cost.
pub fn best_fixed(n: usize, reads_per_commit: f64, seed: u64) -> (u64, f64) {
    FIXED_GRID
        .iter()
        .map(|&k| (k, simulate(&mut Fixed(k), n, reads_per_commit, seed).cost()))
        .fold((0, f64::INFINITY), |b, x| if x.1 < b.1 { x } else { b })
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 600;

    #[test]
    fn fixed_ten_matches_the_delta_default() {
        let r = simulate(&mut Fixed(10), 100, 0.0, 49);
        assert_eq!(r.checkpoints, 10);
    }

    /// Provided, and passing on a fresh clone: the textbook rule's
    /// 2-competitive guarantee does not survive lumpy rent.
    #[test]
    fn ski_rental_overpays_when_reads_are_rare() {
        let ski = simulate(&mut SkiRental::default(), N, 0.02, 49).cost();
        let (_, best) = best_fixed(N, 0.02, 49);
        assert!(ski > 1.2 * best, "ski rental {ski:.0} vs best fixed {best:.0}");
    }

    #[test]
    fn eoq_never_checkpoints_an_unread_table() {
        let r = simulate(&mut Eoq::default(), N, 0.0, 49);
        assert_eq!(r.checkpoints, 0, "with nobody reading, a checkpoint is pure cost");
    }

    #[test]
    fn eoq_tracks_the_best_fixed_interval() {
        for rpc in [0.02, 0.2, 2.0] {
            let eoq = simulate(&mut Eoq::default(), N, rpc, 49).cost();
            let (k, best) = best_fixed(N, rpc, 49);
            assert!(
                eoq <= 1.10 * best,
                "reads/commit {rpc}: EOQ {eoq:.0} vs best fixed (k={k}) {best:.0}"
            );
        }
    }

    #[test]
    fn eoq_beats_the_default_on_a_busy_table() {
        let eoq = simulate(&mut Eoq::default(), N, 2.0, 49).cost();
        let ten = simulate(&mut Fixed(10), N, 2.0, 49).cost();
        assert!(eoq * 1.3 < ten, "2 reads/commit: EOQ {eoq:.0} vs every-10 {ten:.0}");
    }

    #[test]
    fn eoq_checkpoints_more_when_read_more() {
        let c: Vec<u64> = [0.02, 0.2, 2.0]
            .iter()
            .map(|&r| simulate(&mut Eoq::default(), N, r, 49).checkpoints)
            .collect();
        assert!(c[0] < c[1] && c[1] < c[2], "checkpoints by read rate: {c:?}");
    }
}
