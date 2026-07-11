//! STUB 2 — Percolator: transactions over a KV store, coordinator-free.
//!
//! The OSDI '10 protocol TiKV reimplements (tikv/src/storage/txn/ — see
//! reading-percolator-tikv.md). The trick that removes 2PC's blocking
//! window: the transaction's fate is decided by ONE atomic write — the
//! commit of the PRIMARY key. Every secondary lock points at the primary,
//! so any reader who trips over a crashed transaction's lock can decide its
//! fate by looking at the primary:
//!
//!   primary lock still there            -> txn unfinished: roll BACK
//!   primary gone + write record exists  -> txn committed: roll FORWARD
//!   primary gone + no write record      -> txn rolled back: clean up
//!
//! Snapshot isolation, two timestamps per txn (start_ts, commit_ts), both
//! from the TSO. All state lives in kv.rs's three column families.

use crate::kv::{Cluster, Key, Ts};

#[derive(Debug, PartialEq, Eq)]
pub enum TxnError {
    /// Key is locked by another transaction (readers/writers must not wait
    /// blindly — see resolve_lock).
    Locked { key: Key, primary: Key, start_ts: Ts },
    /// A committed write newer than our start_ts exists (WW conflict).
    Conflict,
}

/// Snapshot read of `key` at `ts`.
///
/// Recipe: if a lock on `key` has lock.start_ts <= ts -> Locked (the writer
/// may have committed below our snapshot; we cannot ignore it). Otherwise
/// return the newest committed value at or below ts
/// (cluster.read_committed).
pub fn get(_cluster: &Cluster, _key: Key, _ts: Ts) -> Result<Option<i64>, TxnError> {
    todo!("stub: percolator snapshot get")
}

/// Prewrite all of a transaction's writes. `writes[0]` is the PRIMARY.
///
/// Recipe, per (key, value):
///   any lock on key (any ts)            -> Locked  (WW: first locker wins)
///   newer_write_exists(key, start_ts)   -> Conflict (committed after we began)
///   else: data[(key, start_ts)] = value; lock[key] = { primary, start_ts }.
/// On error, remove the locks THIS prewrite already placed (clean abort).
pub fn prewrite(
    _cluster: &mut Cluster,
    _writes: &[(Key, i64)],
    _start_ts: Ts,
) -> Result<(), TxnError> {
    todo!("stub: percolator prewrite")
}

/// Commit phase 1: the decisive atomic step — commit the PRIMARY only.
///
/// Recipe: verify lock[primary] is ours (start_ts matches; a resolver may
/// have rolled us back while we stalled -> Conflict). Then write
/// writes[(primary, commit_ts)] = start_ts and remove the primary lock.
pub fn commit_primary(
    _cluster: &mut Cluster,
    _primary: Key,
    _start_ts: Ts,
    _commit_ts: Ts,
) -> Result<(), TxnError> {
    todo!("stub: percolator primary commit")
}

/// Commit phase 2: secondaries — safe to do lazily/async; a crash here is
/// harmless because resolve_lock can always roll forward.
///
/// Recipe: for each secondary key still holding our lock: write record at
/// commit_ts, drop the lock. (Missing lock = already resolved; skip.)
pub fn commit_secondaries(
    _cluster: &mut Cluster,
    _secondaries: &[Key],
    _start_ts: Ts,
    _commit_ts: Ts,
) {
    todo!("stub: percolator secondary commit")
}

/// A reader hit `key` locked by txn (primary, start_ts): decide that txn's
/// fate from the primary and clean this key up.
///
/// Recipe:
///   lock[primary] still held with start_ts -> txn dead (this simulation
///     only resolves crashed txns): remove BOTH locks + their data rows
///     (roll back), including the primary's.
///   else find (commit_ts) in writes[(primary, *)] with start_ts value ->
///     roll FORWARD: write record for key at that commit_ts, drop its lock.
///   else -> primary was rolled back: drop key's lock + data row.
pub fn resolve_lock(_cluster: &mut Cluster, _key: Key, _primary: Key, _start_ts: Ts) {
    todo!("stub: percolator lock resolution")
}

/// Convenience: full happy-path transaction (prewrite + both commit phases).
pub fn run_txn(cluster: &mut Cluster, writes: &[(Key, i64)]) -> Result<Ts, TxnError> {
    let start_ts = cluster.tso.get_ts();
    prewrite(cluster, writes, start_ts)?;
    let commit_ts = cluster.tso.get_ts();
    let primary = writes[0].0;
    commit_primary(cluster, primary, start_ts, commit_ts)?;
    let secondaries: Vec<Key> = writes[1..].iter().map(|&(k, _)| k).collect();
    commit_secondaries(cluster, &secondaries, start_ts, commit_ts);
    Ok(commit_ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Cluster {
        let mut c = Cluster::new(2);
        for k in 0..4u64 {
            let start = c.tso.get_ts();
            let commit = c.tso.get_ts();
            c.shard_mut(k).data.insert((k, start), 100);
            c.shard_mut(k).writes.insert((k, commit), start);
        }
        c
    }

    #[test]
    fn snapshot_reads_are_repeatable() {
        let mut c = seeded();
        let snap = c.tso.get_ts();
        assert_eq!(get(&c, 0, snap), Ok(Some(100)));
        run_txn(&mut c, &[(0, 999)]).unwrap();
        assert_eq!(get(&c, 0, snap), Ok(Some(100)), "snapshot must not move");
        let later = c.tso.get_ts();
        assert_eq!(get(&c, 0, later), Ok(Some(999)));
    }

    #[test]
    fn atomic_cross_shard_transfer() {
        let mut c = seeded();
        // keys 0 (shard 0) and 1 (shard 1)
        run_txn(&mut c, &[(0, 70), (1, 130)]).unwrap();
        let ts = c.tso.get_ts();
        assert_eq!(get(&c, 0, ts), Ok(Some(70)));
        assert_eq!(get(&c, 1, ts), Ok(Some(130)));
        assert_eq!(c.lock_count(), 0);
    }

    #[test]
    fn first_locker_wins_and_newer_commit_conflicts() {
        let mut c = seeded();
        let ts_a = c.tso.get_ts();
        let ts_b = c.tso.get_ts();
        prewrite(&mut c, &[(0, 1)], ts_a).unwrap();
        // B trips on A's lock
        assert!(matches!(prewrite(&mut c, &[(0, 2)], ts_b), Err(TxnError::Locked { .. })));
        // B's partial locks must not leak: (3 is free, 0 is locked)
        assert!(matches!(prewrite(&mut c, &[(3, 9), (0, 2)], ts_b), Err(TxnError::Locked { .. })));
        assert!(c.shard(3).locks.get(&3).is_none(), "failed prewrite leaked a lock");
        // A commits at ts > B's start: B's late prewrite now hits Conflict
        let commit_a = c.tso.get_ts();
        commit_primary(&mut c, 0, ts_a, commit_a).unwrap();
        assert_eq!(prewrite(&mut c, &[(0, 2)], ts_b), Err(TxnError::Conflict));
    }

    #[test]
    fn crash_after_primary_commit_rolls_forward() {
        let mut c = seeded();
        let start = c.tso.get_ts();
        prewrite(&mut c, &[(0, 70), (1, 130)], start).unwrap();
        let commit = c.tso.get_ts();
        commit_primary(&mut c, 0, start, commit).unwrap();
        // CRASH before commit_secondaries. A reader hits key 1's lock:
        let ts = c.tso.get_ts();
        let err = get(&c, 1, ts).unwrap_err();
        let TxnError::Locked { key, primary, start_ts } = err else { panic!() };
        resolve_lock(&mut c, key, primary, start_ts);
        assert_eq!(get(&c, 1, ts), Ok(Some(130)), "primary committed => roll forward");
        assert_eq!(c.lock_count(), 0);
    }

    #[test]
    fn crash_before_primary_commit_rolls_back() {
        let mut c = seeded();
        let start = c.tso.get_ts();
        prewrite(&mut c, &[(0, 70), (1, 130)], start).unwrap();
        // CRASH before commit_primary. A reader hits key 1's lock:
        let ts = c.tso.get_ts();
        let TxnError::Locked { key, primary, start_ts } = get(&c, 1, ts).unwrap_err() else {
            panic!()
        };
        resolve_lock(&mut c, key, primary, start_ts);
        assert_eq!(get(&c, 1, ts), Ok(Some(100)), "primary unfinished => roll back");
        assert_eq!(get(&c, 0, ts), Ok(Some(100)), "primary itself must be rolled back too");
        assert_eq!(c.lock_count(), 0);
        // total conserved
        assert_eq!(c.total_committed(&[0, 1, 2, 3], ts), 400);
    }
}
