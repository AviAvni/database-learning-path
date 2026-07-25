//! STUB — a slow log, redis-shaped (`slowlog.c`).
//!
//! The contract, straight from redis: a command whose duration is >= the
//! threshold gets an entry in a fixed-capacity ring (oldest evicted);
//! `threshold < 0` disables logging entirely (slowlog.c:104); entry ids
//! are monotonically increasing and are NOT reused after a reset, so a
//! monitoring system polling `get()` can dedupe by id across resets.

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: u64,
    pub cmd: String,
    pub duration_ns: u64,
}

pub struct SlowLog {
    pub threshold_ns: i64,
    pub max_len: usize,
    // add whatever state you need
}

impl SlowLog {
    pub fn new(threshold_ns: i64, max_len: usize) -> Self {
        SlowLog { threshold_ns, max_len }
    }

    /// Log `cmd` if `duration_ns` clears the threshold. Must be O(1) and
    /// allocation-free on the fast path (below threshold / disabled) —
    /// this runs after EVERY command, like slowlogPushEntryIfNeeded.
    pub fn add(&mut self, cmd: &str, duration_ns: u64) {
        let _ = (cmd, duration_ns);
        todo!("threshold check, ring push, trim to max_len")
    }

    /// Newest first (SLOWLOG GET order), at most `n` entries.
    pub fn get(&self, n: usize) -> Vec<Entry> {
        let _ = n;
        todo!()
    }

    /// Drop all entries. Ids must keep increasing afterwards.
    pub fn reset(&mut self) {
        todo!()
    }

    pub fn len(&self) -> usize {
        todo!()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_gates_and_negative_disables() {
        let mut sl = SlowLog::new(10_000, 128);
        sl.add("GRAPH.QUERY fast", 9_999);
        sl.add("GRAPH.QUERY slow", 10_000); // >= threshold logs (redis semantics)
        assert_eq!(sl.len(), 1);
        assert_eq!(sl.get(10)[0].cmd, "GRAPH.QUERY slow");

        let mut off = SlowLog::new(-1, 128);
        off.add("anything", u64::MAX);
        assert!(off.is_empty());
    }

    #[test]
    fn ring_caps_and_returns_newest_first() {
        let mut sl = SlowLog::new(0, 3);
        for i in 0..10u64 {
            sl.add(&format!("cmd{i}"), 100 + i);
        }
        assert_eq!(sl.len(), 3); // oldest 7 evicted
        let got = sl.get(10);
        assert_eq!(
            got.iter().map(|e| e.cmd.as_str()).collect::<Vec<_>>(),
            vec!["cmd9", "cmd8", "cmd7"]
        );
        assert_eq!(sl.get(2).len(), 2);
    }

    #[test]
    fn ids_are_monotonic_and_survive_reset() {
        let mut sl = SlowLog::new(0, 8);
        sl.add("a", 1);
        sl.add("b", 1);
        let ids: Vec<u64> = sl.get(10).iter().map(|e| e.id).collect();
        assert!(ids[0] > ids[1]); // newest first ⇒ larger id first
        sl.reset();
        assert!(sl.is_empty());
        sl.add("c", 1);
        assert!(sl.get(1)[0].id > ids[0]); // never reused
    }
}
