//! Learner reads — YOUR JOB. TiFlash never votes, only replicates; a
//! consistent read asks the leader for the current commit index, then
//! WAITS until the local replica has applied that far
//! (tiflash KVStore/Read/LearnerRead.cpp:35 doLearnerRead, with a
//! waitIndex timeout at :61). Freshness isn't a config flag — it's a
//! wait, and this module makes you price it.
//!
//! Model: the replica applies the log in batches. `schedule[i] =
//! (apply_time, applied_lsn)` — at `apply_time`, everything up to
//! `applied_lsn` becomes visible. Times and lsns are strictly
//! increasing across the schedule.
//!
//! Contract fixed by the tests below:
//! - `read_wait(schedule, now, read_index)`: how long past `now` a
//!   reader blocks until applied_lsn >= read_index. 0 if some batch at
//!   or before `now` already covers it. None if no batch ever will
//!   (the waitIndexTimeout case).

/// (apply_time, applied_lsn), both strictly increasing.
pub type ApplySchedule = Vec<(u64, u64)>;

pub fn read_wait(schedule: &[(u64, u64)], now: u64, read_index: u64) -> Option<u64> {
    let _ = (schedule, now, read_index);
    todo!("first batch with applied_lsn >= read_index; wait = max(0, time - now)")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Replica applies 100 lsns every 10 ticks: at t=10 lsn 100 visible, ...
    fn schedule() -> ApplySchedule {
        (1..=10).map(|i| (i * 10, i * 100)).collect()
    }

    #[test]
    fn already_applied_reads_are_free() {
        let s = schedule();
        assert_eq!(read_wait(&s, 35, 100), Some(0)); // applied at t=10
        assert_eq!(read_wait(&s, 30, 300), Some(0)); // applied exactly now
    }

    #[test]
    fn fresh_reads_wait_for_the_batch() {
        let s = schedule();
        assert_eq!(read_wait(&s, 35, 301), Some(5)); // batch at t=40
        assert_eq!(read_wait(&s, 0, 1), Some(10)); // nothing applied yet
        assert_eq!(read_wait(&s, 12, 950), Some(88)); // t=100 covers 1000
    }

    #[test]
    fn beyond_the_schedule_is_a_timeout() {
        let s = schedule();
        assert_eq!(read_wait(&s, 0, 1001), None);
    }
}
