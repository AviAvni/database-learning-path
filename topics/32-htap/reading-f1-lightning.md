# F1 Lightning: HTAP without touching OLTP

This chapter closes the topic's design space with two documents: F1
Lightning, where analytics is bolted onto an untouchable OLTP system
entirely from the outside, and the Özcan survey, which organizes every
architecture you've met along one axis — how many copies, how coupled.
Before the papers, this chapter builds Lightning's design step by step —
the constraint, the changelog feed, the replica, and the safe-timestamp
trick that replaces waiting — and ends with every corner of the README's
trilemma priced.

## The problem in one sentence

Google's OLTP databases (Spanner, F1 DB) already exist, serve
revenue-critical traffic, and may not be modified or slowed by a single
microsecond — so analytics must be added *entirely from the outside*,
and the price of touching nothing is that "fresh" degrades from a
bounded Raft wait to seconds of pipeline lag.

## The concepts, step by step

### Step 1 — the constraint: the OLTP system is a black box

Every other design in this topic changed the primary — HANA restructured
its storage, HyPer forked its process, TiDB put a learner inside its
consensus group. Lightning's constraint forbids all of that: no OLTP
code changes, no extra replica in the quorum, not even an assumption
that there's only *one* OLTP engine (it must serve Spanner and F1 DB
behind one interface). Whatever feeds the analytical side must use
interfaces the OLTP systems already expose. The only such interface that
carries every write is the changelog.

### Step 2 — CDC: the changelog is the coupling

CDC (change data capture) means subscribing to the stream of committed
changes a database already produces — topic 27's changelog, promoted to
an architecture. Lightning's ingest service, **Changepump**, consumes
per-shard change streams and turns them into one usable feed:

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

The subtlety is ordering: changes arrive per shard, but analytics needs
transactionally consistent snapshots across shards — so Changepump must
reassemble a cross-shard order from commit timestamps. Spanner can
supply globally meaningful ones because of TrueTime (topic 29); that's
the hidden dependency of the whole design (question 3).

### Step 3 — the replica is delta+main again

Lightning servers apply the change stream into columnar storage
organized — once more — as delta+main: changes append into
write-optimized deltas, background merges fold them into read-optimized
main, reads merge the two. Same fold as HANA, DeltaTree, and your
`replica.rs`; the only new twist is schema: because Lightning serves
*multiple* OLTP engines, changes are translated into Lightning's own
neutral row format and its own MVCC versioning — it cannot reuse any one
engine's version format (question 4). The fourth appearance of this
diagram in one topic is the point: whatever feeds the replica, the
replica's storage problem has exactly one known shape.

### Step 4 — the safe timestamp: never wait, serve stale-but-consistent

Each Lightning replica tracks its **safe timestamp** — the maximum
commit timestamp up to which it has applied *everything* (no gaps). A
query is served at a single timestamp at-or-below the minimum safe
timestamp of every replica it touches: consistent by construction, and
**the read never blocks** — the opposite trade from `doLearnerRead`,
which waits for the replica to catch up to *now*. Lightning reads are
stale by the CDC lag (seconds) but return immediately; the safe
timestamp is your `applied_lsn`, and your `freshness_is_visible` test is
this idea productionized.

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

The `min()` is load-bearing: a multi-shard query needs *one* snapshot
all touched replicas can serve, so the laggiest replica sets the
timestamp. And the refusal branch is the honesty contract: if a caller
demands freshness the pipeline can't deliver, say no — never serve a
lie.

### Step 5 — decoupling as a feature, priced

Now place Lightning on the trilemma. Isolation: total — analytics
cannot slow OLTP even in principle, because it shares nothing, and an
OLTP leader failover just pauses the change stream (analytics keeps
serving, staleness grows) rather than breaking reads (question 2).
Cost: a full extra copy plus the pipeline. Freshness: the sacrifice —
seconds of CDC lag, unbounded during pipeline hiccups, versus TiFlash's
bounded learner wait. That's the exact opposite corner from HANA
(perfectly fresh, poorly isolated), with TiFlash between them. Two ideas
to steal for M32: the safe timestamp *is* `applied_lsn`, and
refuse-rather-than-lie is the router's contract (question 6).

### Step 6 — the survey: one axis to organize everything

Özcan et al. classify every HTAP architecture by *how many copies, how
coupled*:

| | single copy | separate copies |
|---|---|---|
| single engine | HANA delta+main | HyPer fork (logical single) |
| separate engines | pg_duckdb-style offload (same files) | TiFlash (learner), Lightning (CDC) |

Every cell trades the same three currencies — freshness, isolation, cost
(README trilemma). Lane 1 measured why the top-left cell is hard; lanes
2–3 price the right column's two currencies (scan speedup vs lsn lag,
wait distribution). With Steps 1–5 in hand the table reads as a design
procedure: pick the coupling you can afford, and the freshness mechanism
(merge-on-read, re-fork, learner wait, safe timestamp) follows.

## How to read the papers (with the concepts in hand)

- **F1 Lightning (VLDB 2020)**: read §3–4 — §3 for Changepump (Step 2:
  find the ordering guarantee it enforces and what it costs to enforce
  it), §4 for the safe timestamp (Step 4: check the real routing rule
  against the sketch above, especially multi-shard `min()` and what
  happens on refusal). Skim the schema-translation material with Step 3's
  question in mind: what does engine-neutrality rule out?
- **Özcan et al. survey (SIGMOD 2017 tutorial)**: skim for the map, not
  the details — place every system you've met this topic into the Step 6
  table, then check your placements against theirs.

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

## Done when

You can fill in the survey's copies-vs-coupling table from memory, name
each cell's freshness mechanism, and state Lightning's two stealable
ideas — safe timestamp as `applied_lsn`, and refuse-rather-than-lie —
in M32's vocabulary.

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
