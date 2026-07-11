//! Topic 16 experiments: build the test harness, not the system.
//!
//! PROVIDED: `sim_fs` (crash-simulating file), `kv` (WAL-backed KV
//! store with four injectable bugs), `crash_matrix` binary.
//! YOU implement: `dst` (the seeded harness that catches the bugs),
//! `shrink` (delta-debugging minimizer), `tlp` (ternary logic
//! partitioning over a mini row filter).

pub mod dst;
pub mod kv;
pub mod shrink;
pub mod sim_fs;
pub mod tlp;

/// Operations the workload generator can emit.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Put(u32, u32),
    Delete(u32),
    /// Commit buffered ops (kv fsyncs its WAL here — unless buggy).
    Commit,
    /// kill -9: unsynced WAL bytes are lost, last record may tear.
    Crash,
}

/// The oracle: what a correct KV must contain after the same ops.
/// Uncommitted ops are lost on crash — the model tracks both views.
#[derive(Default, Clone)]
pub struct Model {
    committed: std::collections::BTreeMap<u32, u32>,
    pending: Vec<(u32, Option<u32>)>, // key -> Some(put) / None(delete)
}

impl Model {
    pub fn apply(&mut self, op: &Op) {
        match op {
            Op::Put(k, v) => self.pending.push((*k, Some(*v))),
            Op::Delete(k) => self.pending.push((*k, None)),
            Op::Commit => {
                for (k, v) in self.pending.drain(..) {
                    match v {
                        Some(v) => {
                            self.committed.insert(k, v);
                        }
                        None => {
                            self.committed.remove(&k);
                        }
                    }
                }
            }
            Op::Crash => self.pending.clear(),
        }
    }

    /// State a correct store must expose after recovery.
    pub fn expected(&self) -> &std::collections::BTreeMap<u32, u32> {
        &self.committed
    }
}
