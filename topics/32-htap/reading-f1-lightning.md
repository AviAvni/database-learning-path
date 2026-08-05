# F1 Lightning: HTAP without touching OLTP

This chapter closes the topic's design space with two documents: F1
Lightning, where analytics is bolted onto an untouchable OLTP system
entirely from the outside, and the Özcan survey, which organizes every
architecture you've met along one axis — how many copies, how coupled.
Before the papers, this chapter builds Lightning's design step by step —
the constraint, the changelog feed, the replica, and the safe-timestamp
trick that replaces waiting — and ends with every corner of the README's
trilemma priced.

The two documents are Yang et al., *F1 Lightning: HTAP as a Service*
(VLDB 2020) — section numbers below cite that PDF — and Özcan, Tian &
Tözün, *Hybrid Transactional/Analytical Processing: A Survey* (SIGMOD
2017). The survey PDF is paywalled; this chapter cites its *classification
axis* (coupling × number of copies) and treats the specific placement of
each system into that grid as its own synthesis, not a quotation.

## The problem in one sentence

Google's OLTP databases (Spanner, F1 DB) already exist, serve
revenue-critical traffic, and may not be modified or slowed by a single
microsecond — so analytics must be added *entirely from the outside*,
and the price of touching nothing is that "fresh" degrades from a
bounded Raft wait to the replication delay of an outside change-data-capture
pipeline (Figure 4; F1 Query reads at the *oldest* safe timestamp
advertised across data centers, §7.1).

## The concepts, step by step

### Step 1 — the constraint: the OLTP system is a black box

> **In:** a revenue-critical OLTP system (Spanner, F1 DB) that may not be
> modified or slowed, and more than one such engine. **Out:** the single
> constraint that shapes every later step — analytics may only use interfaces
> the OLTP systems *already* expose, and the one that carries every write is
> the changelog.

Every other design in this topic changed the primary — HANA restructured
its storage, HyPer forked its process, TiDB put a learner inside its
consensus group. Lightning's constraint forbids all of that: no OLTP
code changes, no extra replica in the quorum, not even an assumption
that there's only *one* OLTP engine (it must serve Spanner and F1 DB
behind one interface). Whatever feeds the analytical side must use
interfaces the OLTP systems already expose. The only such interface that
carries every write is the changelog.

### Step 2 — CDC: the changelog is the coupling

> **In:** the black-box constraint from Step 1 — the changelog is the only
> interface that carries every write. **Out:** Changepump, the change-tailing
> service (§4.8) that turns per-shard change streams into one
> transactionally-ordered feed, and the hidden dependency it rests on
> (globally-meaningful commit timestamps).

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
reassemble a cross-shard order from commit timestamps. It exposes "a
unified interface across different transactional [systems]" (§4.8), with a
per-source **adapter** and per-partition **subscriptions** that carry a
start timestamp (§4.8.1), and it "is responsible for maintaining
transactional consistency … and emits checkpoints that advance the
[safe] timestamp" (§4.8). Spanner can supply globally meaningful commit
timestamps because of TrueTime (topic 29); that's the hidden dependency of
the whole design (question 3).

### Step 3 — the replica is delta+main again

> **In:** Changepump's ordered change feed (Step 2). **Out:** Lightning's
> storage — the same delta+main fold met three times already — plus the one
> genuinely new demand of serving *many* OLTP engines: an engine-neutral
> *schema*, not a new version format.

Lightning servers apply the change stream into columnar storage
organized — once more — as delta+main: changes append into
write-optimized deltas, background merges fold them into read-optimized
main, reads merge the two. Same fold as HANA, DeltaTree, and your
`replica.rs`. The new twist is *not* versioning: both F1 DB and Spanner
"support multi-version concurrency control using timestamps, and every
change committed to Lightning **retains its original commit timestamp**"
(§3), so the commit timestamp is the shared version currency — Lightning
does not invent one. Lightning "guarantees that reads at a specific
timestamp will produce results identical to reads against the OLTP
database at the same timestamp" (§3), which *requires* every source to
expose timestamp-MVCC plus a CDC/log-shipping interface. What multiple
engines force instead is an engine-neutral **schema**: Lightning's
two-level schema (§4.6) maps each OLTP schema into a logical Lightning
schema and then one or more physical (file-format) schemas, so the
storage layer is independent of any one engine's types (question 4). The
fourth appearance of the delta+main diagram in one topic is the point:
whatever feeds the replica, the replica's storage problem has exactly one
known shape.

### Step 4 — the safe timestamp: never wait, serve stale-but-consistent

> **In:** a replica applying Changepump's feed (Step 3), always some way
> behind the OLTP present. **Out:** the read rule that turns that lag into a
> *consistent* answer without ever blocking — pin the read to a safe
> timestamp — and the honesty question of what to do when the safe timestamp
> is too old.

Each Lightning replica tracks its **safe timestamp** — the maximum
commit timestamp up to which it has applied *everything* with no gaps
(§4.1: "the maximum safe timestamp indicates that Lightning has ingested
all changes up to that timestamp"). A query is served at a single
timestamp at-or-below the minimum safe timestamp of every replica it
touches: consistent by construction, and **the read never blocks** — the
opposite trade from `doLearnerRead` (`LearnerRead.cpp:35`), which waits
for the replica to catch up to *now*. Lightning reads are stale by the
pipeline's safe-timestamp replication delay (§7.1, Figure 4) but return
immediately, and the queryable window is bounded — "typically ten hours"
in production (§4.1). The safe timestamp is your `applied_lsn`, and your
`freshness_is_visible` test is this idea productionized.

```rust
// ILLUSTRATION — this is M32's router design, not quoted from Lightning
// (which is closed-source). The contrast is doLearnerRead (LearnerRead.cpp:35),
// which WAITS; your never-wait analogue is learner.rs:22 (read_wait) returning
// a safe timestamp instead of blocking. Lightning's own answer to "too stale"
// is table-level failover to the OLTP database (§4.9.3), not a refusal.
fn route_analytical(q: &Query, replicas: &[Replica]) -> Result<Plan, Refuse> {
    let safe_ts = q.touched_shards(replicas)
        .map(|r| r.applied_ts())        // max commit ts each has FULLY applied
        .min()                          // all shards must serve ONE snapshot
        .ok_or(Refuse::NoReplica)?;
    if let Some(bound) = q.freshness_bound {
        if safe_ts < bound {
            return Err(Refuse::TooStale);   // M32: refuse rather than lie.
        }                                   // Lightning: fail over to OLTP (§4.9.3)
    }
    Ok(Plan::scan_at(safe_ts))          // consistent, zero wait: the opposite
}                                       // trade from TiFlash's learner read
```

The `min()` is load-bearing: a multi-shard query needs *one* snapshot
all touched replicas can serve, so the laggiest replica sets the
timestamp — and F1 Query runs "at the oldest safe timestamp advertised
across all data centers" for the same reason (§7.1). The refusal branch
is *this repo's* M32 synthesis of the honesty contract; Lightning itself
handles "too stale" with table-level failover back to the OLTP database
under a configurable staleness threshold (§4.9.3), because it "prefers
data availability over data freshness" (§7.1).

### Step 5 — decoupling as a feature, priced

> **In:** the fully external design of Steps 1–4. **Out:** its coordinates on
> the freshness / isolation / cost triangle — total isolation and a full
> extra copy, freshness traded away — and the two ideas M32 steals from it.

Now place Lightning on the trilemma. Isolation: total — analytics
cannot slow OLTP even in principle, because it shares nothing, and an
OLTP leader failover just pauses the change stream (analytics keeps
serving, staleness grows) rather than breaking reads (question 2).
Cost: a full extra copy plus the pipeline. Freshness: the sacrifice —
the safe-timestamp replication delay (§7.1, Figure 4), bounded by the
§4.9.3 table-level failover threshold (past it, reads fail over to the
OLTP database), versus TiFlash's bounded learner wait. That's the exact
opposite corner from HANA (perfectly fresh, poorly isolated), with
TiFlash between them. Two ideas to steal for M32: the safe timestamp *is*
`applied_lsn`, and refuse-rather-than-lie is the router's contract
(question 6).

### Step 6 — the survey: one axis to organize everything

> **In:** every architecture met this topic — HANA, HyPer, TiFlash,
> Lightning. **Out:** the survey's organizing axis (how many copies × how
> coupled) and, as this guide's own synthesis, where each system lands on it.

Özcan et al. classify HTAP architectures by *how many copies, how
coupled*. The grid below places the systems this topic covered into that
axis — the placement is this guide's synthesis, not a figure lifted from
the survey:

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

- **F1 Lightning (VLDB 2020)**: read §3–4 — §3 for the consistency story
  (Step 3: note that Lightning *retains* each change's original commit
  timestamp), §4.8 for Changepump (Step 2: find the ordering guarantee it
  enforces and what it costs), §4.1 for the safe timestamp and the
  ~10-hour queryable window (Step 4: check the real routing rule against
  the sketch, especially multi-shard `min()`), and §4.9.3 for what really
  happens when data is too stale (table-level failover). Read §4.6 with
  Step 3's question in mind: engine-neutrality lives in the *schema*, not
  the version format.
- **Özcan et al. survey (SIGMOD 2017)**: use it for the classification
  axis (copies × coupling) — place every system you've met this topic into
  the Step 6 grid; the placements there are this guide's synthesis, so the
  survey is where you check them.

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
   translation layer. What does that force to be engine-neutral, and what
   does it *reuse* from each source (hint: does Lightning invent a new
   version format, or retain each change's original commit timestamp — §3
   — and translate only the schema, §4.6)?
5. Place pg_duckdb-style offload (OLAP engine reading the OLTP engine's
   files/snapshots in-process) on the trilemma. Which corner does it
   nail, which does it give up, and for what budget is it the right
   answer?
6. **M32 mapping**: M32 feeds a replica from M27's changelog — that's
   Lightning's shape, not TiFlash's. Adopt the safe-timestamp idea:
   what exactly does the M32 router advertise per replica, and when does
   it *refuse* a query instead of serving stale?

## Done when

Answer each before unfolding it.

- [ ] You can state Lightning's constraint and the only interface it can use.
  <details><summary>Answer</summary>

  The OLTP systems (Spanner, F1 DB) may not be modified or slowed, and there
  is more than one of them (Step 1). So analytics must use interfaces they
  already expose; the only one carrying every write is the changelog, tailed
  by Changepump (§4.8) — "log shipping inside the OLTP engine, which in
  general cannot [be changed]" (§2) is exactly what it works around.

  </details>

- [ ] You can say what multi-engine support forces — and, crucially, what it does *not*.
  <details><summary>Answer</summary>

  It forces an engine-neutral **schema**: the two-level logical/physical
  schema (§4.6). It does *not* force a new version format — "every change
  committed to Lightning retains its original commit timestamp" (§3), because
  both F1 DB and Spanner are timestamp-MVCC. The version currency is shared;
  only the schema/format is translated. (An earlier draft of this guide had
  this backwards.)

  </details>

- [ ] You can explain the safe timestamp and why the read never blocks.
  <details><summary>Answer</summary>

  A replica's safe timestamp is the max commit timestamp it has applied with
  no gaps (§4.1). A query reads at the `min()` safe timestamp across the
  replicas it touches — consistent by construction, immediate, and stale by
  the replication delay (§7.1, Figure 4), inside a ~10-hour queryable window
  (§4.1). This is the opposite trade from `doLearnerRead` (`LearnerRead.cpp:35`),
  which waits for *now*.

  </details>

- [ ] You can say what Lightning does when the safe timestamp is too stale.
  <details><summary>Answer</summary>

  Lightning itself does **table-level failover** back to the OLTP database
  under a configurable staleness threshold (§4.9.3), because it "prefers data
  availability over data freshness" (§7.1). The `Refuse::TooStale` branch in
  the sketch is *this repo's* M32 synthesis (refuse-rather-than-lie), not a
  Lightning mechanism — worth keeping straight.

  </details>

- [ ] You can fill in the copies-vs-coupling grid and name each cell's freshness mechanism.
  <details><summary>Answer</summary>

  Single-copy/single-engine → HANA (merge-on-read); separate-copy/single-engine
  → HyPer (re-fork); single-copy/separate-engine → file-level offload; and
  separate-copy/separate-engine → TiFlash (learner wait) and Lightning (safe
  timestamp). Each trades freshness/isolation/cost differently; the grid's
  axis is the survey's, the placements are this guide's synthesis (Step 6).

  </details>

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
