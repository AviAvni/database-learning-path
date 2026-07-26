//! STUB — a bi-temporal edge store (Zep/Graphiti §2.1).
//!
//! Graphiti tracks TWO timelines per fact. T is *event time* — when the
//! fact was true in the world (t_valid, t_invalid). T' is *ingestion
//! time* — when the system learned it (t_created, t_expired). Four
//! timestamps per edge. Nothing is ever deleted: when a new fact
//! contradicts an old one ("Alice works at Acme" vs "Alice works at
//! Beta"), the old edge's t_invalid is set to the new edge's t_valid
//! and its t_expired to now — the edge stays, expired, as an audit
//! trail. Two timelines buy two different questions:
//!
//!   as_of(event=March, ingest=today)  — what WAS true in March?
//!   as_of(event=March, ingest=March)  — what did we KNOW in March?
//!
//! A fact learned late (t_created ≫ t_valid) appears in the first
//! answer and not the second. That distinction — "true then" vs "known
//! then" — is the whole reason for carrying four timestamps.
//!
//! Contracts (the tests): contradiction invalidates without deleting;
//! event-time travel reconstructs any past state; a late-arriving fact
//! is visible to a modern query about the past but invisible to a query
//! replaying what was known at the time.

/// One fact with its two timelines. `rel` identifies the relation
/// (e.g. 0 = WORKS_AT): an entity has one current value per (src, rel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub src: u32,
    pub rel: u32,
    pub dst: u32,
    /// Event time the fact became true.
    pub t_valid: u64,
    /// Event time it stopped being true (None = still true).
    pub t_invalid: Option<u64>,
    /// Ingestion time the system learned the fact.
    pub t_created: u64,
    /// Ingestion time the system learned it was superseded.
    pub t_expired: Option<u64>,
}

#[derive(Default)]
pub struct TemporalStore {
    pub edges: Vec<Edge>,
}

impl TemporalStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest fact (src, rel, dst) valid from `t_valid`, learned at
    /// `t_ingest`. Graphiti's invalidation: any currently-valid edge
    /// with the same (src, rel) but a different dst is contradicted —
    /// set its t_invalid to the NEW edge's t_valid and its t_expired to
    /// `t_ingest`. Never remove an edge. Then append the new edge.
    pub fn ingest(&mut self, src: u32, rel: u32, dst: u32, t_valid: u64, t_ingest: u64) {
        let _ = (src, rel, dst, t_valid, t_ingest);
        todo!("invalidate the contradicted edge, then append")
    }

    /// Edges valid at event time `t_event`, as known at ingestion time
    /// `t_ingest`. An edge is visible iff it was ingested by t_ingest,
    /// was valid at t_event, and was NOT yet known-invalid: an
    /// invalidation only counts if it was learned by t_ingest
    /// (t_expired <= t_ingest) AND it takes effect by t_event
    /// (t_invalid <= t_event).
    pub fn as_of(&self, t_event: u64, t_ingest: u64) -> Vec<&Edge> {
        let _ = (t_event, t_ingest);
        todo!("filter on both timelines")
    }

    /// The current world view: valid now, per the latest knowledge.
    pub fn current(&self) -> Vec<&Edge> {
        todo!("as_of at the end of both timelines")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKS_AT: u32 = 0;
    const ALICE: u32 = 1;
    const ACME: u32 = 10;
    const BETA: u32 = 11;

    #[test]
    fn contradiction_invalidates_without_delete() {
        let mut store = TemporalStore::new();
        store.ingest(ALICE, WORKS_AT, ACME, 100, 100);
        store.ingest(ALICE, WORKS_AT, BETA, 200, 205);

        // Both edges are still in the store — the old one expired.
        assert_eq!(store.edges.len(), 2);
        let old = &store.edges[0];
        assert_eq!(old.t_invalid, Some(200), "invalid from the NEW fact's t_valid");
        assert_eq!(old.t_expired, Some(205), "expired when we LEARNED it");

        // The current view has exactly the new fact.
        let now = store.current();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].dst, BETA);
    }

    #[test]
    fn as_of_event_time_reconstructs_the_past() {
        let mut store = TemporalStore::new();
        store.ingest(ALICE, WORKS_AT, ACME, 100, 100);
        store.ingest(ALICE, WORKS_AT, BETA, 200, 205);

        // "Where did Alice work at event time 150?" — Acme, even though
        // the edge is expired today.
        let then = store.as_of(150, 1_000);
        assert_eq!(then.len(), 1);
        assert_eq!(then[0].dst, ACME);
        // Before she joined: nothing.
        assert!(store.as_of(50, 1_000).is_empty());
    }

    #[test]
    fn ingestion_time_distinguishes_known_from_true() {
        let mut store = TemporalStore::new();
        // A fact about the past, learned late: valid from event time 5,
        // ingested at time 100.
        store.ingest(ALICE, WORKS_AT, ACME, 5, 100);

        // What WAS true at event 10, per today's knowledge: the fact.
        assert_eq!(store.as_of(10, 1_000).len(), 1);
        // What we KNEW at ingestion time 50 about event 10: nothing —
        // the fact hadn't arrived yet.
        assert!(store.as_of(10, 50).is_empty());
    }
}
