//! STUB 2 — the head block: in-order fast path + a bounded out-of-order
//! window, prometheus-style.
//!
//! Prometheus rejected OOO samples entirely for a decade
//! (storage.ErrOutOfOrderSample, head_append.go:481) and then bolted on a
//! bounded window (OutOfOrderTimeWindow head.go:168, ooo_head.go): a
//! sample older than `max_t` but within the window lands in a *separate*
//! OOO buffer, merged with the in-order data at flush/compaction time.
//! Older than the window -> ErrTooOldSample (head_append.go:688) — the
//! database refuses rather than resorting forever. That refusal is the
//! design: the in-order path stays append-only (Gorilla chunks demand
//! sorted input), and the disorder cost is quarantined + paid once at
//! flush.

pub type Sample = (i64, f64);

#[derive(Debug, PartialEq, Eq)]
pub enum Append {
    /// t > max_t: in-order fast path.
    Ok,
    /// t <= max_t but inside the OOO window: buffered separately.
    OutOfOrder,
    /// t <= max_t - window: REJECTED, not stored.
    TooOld,
}

pub struct Head {
    pub ooo_window_ms: i64,
    pub in_order: Vec<Sample>,
    pub ooo: Vec<Sample>,
    pub max_t: i64,
}

impl Head {
    pub fn new(ooo_window_ms: i64) -> Self {
        Self { ooo_window_ms, in_order: Vec::new(), ooo: Vec::new(), max_t: i64::MIN }
    }

    /// Recipe: t > max_t -> push in_order, bump max_t, Ok.
    /// t > max_t - ooo_window_ms -> push ooo (unsorted!), OutOfOrder.
    /// else -> TooOld, store nothing.
    pub fn append(&mut self, _t: i64, _v: f64) -> Append {
        todo!("stub: head append with OOO window")
    }

    /// Merge in_order + ooo into one sorted run; on duplicate timestamps
    /// the LAST-ARRIVED sample wins. Clears both buffers.
    ///
    /// Recipe: sort ooo (stable, by t), then merge with the already-sorted
    /// in_order run — arrival order within each buffer plus "ooo arrived
    /// after the in-order sample it duplicates" resolves LWW. Cost is
    /// O(k log k + n): the disorder tax is proportional to the DISORDER,
    /// not to the data.
    pub fn flush(&mut self) -> Vec<Sample> {
        todo!("stub: head flush (merge + LWW dedup)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_never_touches_the_ooo_buffer() {
        let mut h = Head::new(60_000);
        for i in 0..1000 {
            assert_eq!(h.append(i * 10_000, i as f64), Append::Ok);
        }
        assert!(h.ooo.is_empty());
        assert_eq!(h.in_order.len(), 1000);
    }

    #[test]
    fn window_boundaries_enforced() {
        let mut h = Head::new(60_000);
        assert_eq!(h.append(1_000_000, 1.0), Append::Ok);
        assert_eq!(h.append(999_000, 2.0), Append::OutOfOrder);
        assert_eq!(h.append(940_001, 3.0), Append::OutOfOrder);
        assert_eq!(h.append(940_000, 4.0), Append::TooOld);
        assert_eq!(h.append(1_000_000, 5.0), Append::OutOfOrder, "duplicate ts is OOO");
        let flushed = h.flush();
        assert!(!flushed.iter().any(|&(_, v)| v == 4.0), "TooOld must not be stored");
    }

    #[test]
    fn flush_is_sorted_and_last_write_wins() {
        let mut h = Head::new(60_000);
        h.append(100, 1.0);
        h.append(200, 2.0);
        h.append(300, 3.0);
        assert_eq!(h.append(250, 9.0), Append::OutOfOrder);
        assert_eq!(h.append(200, 8.0), Append::OutOfOrder); // overwrite
        let out = h.flush();
        assert_eq!(out, vec![(100, 1.0), (200, 8.0), (250, 9.0), (300, 3.0)]);
        assert!(h.in_order.is_empty() && h.ooo.is_empty(), "flush must clear");
    }

    #[test]
    fn flush_result_feeds_gorilla() {
        // the whole point of the quarantine: flush output is sorted, so it
        // can be handed straight to the in-order-only encoder.
        let mut h = Head::new(120_000);
        let ts = crate::gen::scrape_timestamps(5_000, 0, 10_000, 0, 11);
        let vs = crate::gen::gauge_values(5_000, 12);
        for (t, v) in crate::gen::with_out_of_order(&ts, &vs, 0.2, 100_000, 13) {
            assert_ne!(h.append(t, v), Append::TooOld);
        }
        let out = h.flush();
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0), "sorted, deduped");
    }
}
