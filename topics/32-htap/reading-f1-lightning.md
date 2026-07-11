# F1 Lightning: HTAP without touching OLTP

This chapter closes the topic's design space with two documents: F1
Lightning, where analytics is bolted onto an untouchable OLTP system
entirely from the outside, and the Özcan survey, which organizes every
architecture you've met along one axis — how many copies, how coupled.
Between them sits the trilemma the README opened with, now with every
corner priced.

## Lightning: HTAP without touching the OLTP system

Google's constraint: the OLTP databases (Spanner, F1 DB) *already exist*
and cannot be modified or slowed. So the analytical side is bolted on
entirely from the outside, fed by CDC:

```
  Spanner/F1 (OLTP, untouched)
        │ change data capture (changelog — topic 27)
        ▼
  Changepump ──► Lightning servers: apply changes into columnar
        │         delta+main (LSM-ish; deltas merged in background —
        │         the same fold as reading-tiflash-deltatree.md)
        ▼
  F1 Query ──► routes analytical plans to Lightning replicas,
               each read pinned to a *safe timestamp* — the max
               commit ts the replica has fully applied
```

Two ideas to steal:

1. **The safe timestamp is `applied_lsn`.** Lightning serves reads only
   at-or-below the timestamp it has completely applied — your
   `freshness_is_visible` test, productionized. Reads never wait
   (contrast `doLearnerRead`); instead they're *served stale but
   consistent*, and the query layer picks a timestamp all touched
   replicas can serve.
2. **Decoupling as a feature.** No OLTP code changes, no learner in the
   quorum, works over multiple OLTP systems. Payment: freshness is
   seconds (CDC lag), not a bounded Raft wait — the opposite corner of
   the trilemma from HANA.

The safe-timestamp routing rule, which replaces `doLearnerRead`'s wait:

```rust
// Lightning never waits: reads are stale-but-consistent at a SAFE TIMESTAMP
fn route_analytical(q: &Query, replicas: &[Replica]) -> Result<Plan, Refuse> {
    let safe_ts = q.touched_shards(replicas)
        .map(|r| r.applied_ts())        // max commit ts each has FULLY applied
        .min()                          // all shards must serve ONE snapshot
        .ok_or(Refuse::NoReplica)?;
    if let Some(bound) = q.freshness_bound {
        if safe_ts < bound {
            return Err(Refuse::TooStale);   // refuse rather than lie —
        }                                   // the router's honesty contract
    }
    Ok(Plan::scan_at(safe_ts))          // consistent, zero wait: the opposite
}                                       // trade from TiFlash's learner read
```

## The survey: one axis to organize everything

Özcan et al. classify by *how many copies, how coupled*:

| | single copy | separate copies |
|---|---|---|
| single engine | HANA delta+main | HyPer fork (logical single) |
| separate engines | pg_duckdb-style offload (same files) | TiFlash (learner), Lightning (CDC) |

Every cell trades the same three currencies — freshness, isolation, cost
(README trilemma). Lane 1 measured why the top-left cell is hard; lanes
2–3 price the right column's two currencies (scan speedup vs lsn lag,
wait distribution).

## Questions

1. Lightning reads never block on freshness; TiFlash learner reads do.
   Rewrite `read_wait`'s contract for the Lightning model: what does it
   return instead of a wait, and which test of yours becomes the
   important one?
2. CDC lag is seconds; learner apply lag is the lane-2 gap table. What
   *failure* behaviors differ — what happens to each design's analytics
   when the OLTP leader fails over?
3. Lightning must reconstruct transactional consistency from a change
   stream (changes arrive per-shard). What ordering guarantee must
   Changepump enforce, and which topic 27 concept is that? Which topic 29
   concept gives Spanner the timestamps that make it possible?
4. "HTAP as a service" supports multiple OLTP engines behind one
   translation layer. What does that force the delta schema to look like,
   and what does it rule out (hint: can Lightning use the OLTP engine's
   own MVCC versions)?
5. Place pg_duckdb-style offload (OLAP engine reading the OLTP engine's
   files/snapshots in-process) on the trilemma. Which corner does it
   nail, which does it give up, and for what budget is it the right
   answer?
6. **M32 mapping**: M32 feeds a replica from M27's changelog — that's
   Lightning's shape, not TiFlash's. Adopt the safe-timestamp idea:
   what exactly does the M32 router advertise per replica, and when does
   it *refuse* a query instead of serving stale?

## References

**Papers**
- Yang et al. — "F1 Lightning: HTAP as a Service" (VLDB 2020) — §3-4
  for Changepump and the safe timestamp
- Özcan, Tian, Tözün — "Hybrid Transactional/Analytical Processing: A
  Survey" (SIGMOD 2017, tutorial) — the copies-vs-coupling
  classification; skim for the map, not the details

**Code**
- Paper-only chapter — Lightning is not open source; the closest
  readable relative is the CDC pipeline of topic 27 and TiFlash's
  learner in [reading-tidb-htap.md](reading-tidb-htap.md)
