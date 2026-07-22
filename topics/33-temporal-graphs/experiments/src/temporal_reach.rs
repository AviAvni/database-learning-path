//! One-pass earliest-arrival — YOUR JOB. Wu et al. (VLDB 2014, the
//! topic's read-first paper) show the four minimum temporal paths all
//! fall to single-scan algorithms over the time-ordered contact stream.
//! This is the earliest-arrival one: because contacts arrive sorted by
//! departure time, a contact can never improve an arrival that an
//! EARLIER contact already set — so one relaxation per contact suffices
//! where the oracle loops to fixpoint.
//!
//! Contract fixed by the tests below:
//! - `earliest_arrival(contacts, n, src, t_start)`: contacts are sorted
//!   by t ascending (gen_contacts guarantees it). Return the same arr
//!   vector as events::earliest_arrival_oracle — INF for temporally
//!   unreachable — in ONE pass over the slice.
//! - Relax rule: if arr[c.u] <= c.t and c.t + c.lambda < arr[c.v],
//!   update arr[c.v]. The sort order is what makes once-through correct;
//!   convince yourself why before writing it (question 3 in the README).

use crate::events::{Contact, INF};

pub fn earliest_arrival(contacts: &[Contact], n: u32, src: u32, t_start: u64) -> Vec<u64> {
    let _ = (contacts, n, src, t_start);
    let _ = INF;
    todo!("one pass over the time-sorted stream — no fixpoint loop")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{earliest_arrival_oracle, gen_contacts};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn matches_oracle_on_random_streams() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        for (n, m) in [(50u32, 400usize), (200, 3_000), (1_000, 20_000)] {
            let cs = gen_contacts(&mut rng, n, m, 1_000);
            for src in [0u32, n / 2, n - 1] {
                assert_eq!(
                    earliest_arrival(&cs, n, src, 0),
                    earliest_arrival_oracle(&cs, n, src, 0),
                    "n={n} m={m} src={src}"
                );
            }
        }
    }

    #[test]
    fn respects_start_time() {
        let cs = vec![
            Contact { u: 0, v: 1, t: 2, lambda: 1 },
            Contact { u: 0, v: 2, t: 9, lambda: 1 },
        ];
        // departing no earlier than t=5: the t=2 contact is already gone
        let arr = earliest_arrival(&cs, 3, 0, 5);
        assert_eq!(arr, vec![5, INF, 10]);
    }

    #[test]
    fn zero_lambda_chains_within_one_instant() {
        // t=3 arrival can board a t=3 departure (non-strict, matches oracle)
        let cs = vec![
            Contact { u: 0, v: 1, t: 3, lambda: 0 },
            Contact { u: 1, v: 2, t: 3, lambda: 0 },
        ];
        assert_eq!(earliest_arrival(&cs, 3, 0, 0), vec![0, 3, 3]);
    }
}
