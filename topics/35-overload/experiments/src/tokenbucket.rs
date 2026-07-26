//! STUB — a token bucket, used here as a *retry budget*.
//!
//! The metastable-failures paper's first policy fix: during overload the
//! retry rate must be an absolute, small number — not proportional to the
//! failure rate. A token bucket gives exactly that: retries spend tokens,
//! tokens refill at a fixed rate, and an idle bucket holds at most
//! `burst` tokens (no saving up for a bigger storm).
//!
//! Time is the simulator's virtual clock: `now_ns` is monotonically
//! non-decreasing across calls. Refill is continuous — after `dt` ns the
//! bucket has gained `rate_per_sec * dt / 1e9` tokens (fractional
//! accounting or integer math on ns both work; the tests only observe
//! whole tokens).

pub struct TokenBucket {
    pub rate_per_sec: u64,
    pub burst: u64,
    // add whatever state you need
}

impl TokenBucket {
    /// Starts full (`burst` tokens at t=0).
    pub fn new(rate_per_sec: u64, burst: u64) -> Self {
        TokenBucket { rate_per_sec, burst }
    }

    /// Take one token at virtual time `now_ns`. `true` = allowed.
    /// Must be O(1): this sits on the retry path of every request.
    pub fn try_acquire(&mut self, now_ns: u64) -> bool {
        let _ = now_ns;
        todo!("refill from elapsed time, cap at burst, spend one token")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_deny() {
        let mut tb = TokenBucket::new(10, 5);
        for _ in 0..5 {
            assert!(tb.try_acquire(0));
        }
        assert!(!tb.try_acquire(0)); // bucket empty, no time has passed
    }

    #[test]
    fn steady_rate_enforced() {
        // 10 tokens/s, burst 1: drain the initial token, then hammer the
        // bucket every 1 ms for one second — exactly 10 more get through,
        // no matter that 1000 were asked for.
        let mut tb = TokenBucket::new(10, 1);
        assert!(tb.try_acquire(0));
        let mut allowed = 0;
        for ms in 1..=1000u64 {
            if tb.try_acquire(ms * 1_000_000) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 10);
    }

    #[test]
    fn idle_does_not_accumulate_beyond_burst() {
        // Drain the bucket, go idle for 10 s (10_000 tokens' worth of
        // refill), then spend: only `burst` are there. A bucket that
        // saved them all would wave the whole retry storm through.
        let mut tb = TokenBucket::new(1_000, 4);
        for _ in 0..4 {
            assert!(tb.try_acquire(0));
        }
        assert!(!tb.try_acquire(0));
        let t = 10_000_000_000; // 10 s later
        for _ in 0..4 {
            assert!(tb.try_acquire(t));
        }
        assert!(!tb.try_acquire(t));
    }
}
