# Topic 31 — CRDTs & Multi-Master Replication

The last topic, and the mirror image of topic 15 (Raft). Consensus says:
*agree on an order, then apply*. CRDTs say: *design the data so order
doesn't matter, then never coordinate*. Both give you replicas that agree;
they pay for it in opposite currencies — latency vs. semantics.

## The shape of the problem

```
  consensus (topic 15)                CRDTs (this topic)
  ────────────────────                ──────────────────
  write ──► leader ──► quorum ──► ok  write ──► local apply ──► ok
            │ 1 RTT minimum                     │ 0 RTT
            ▼                                   ▼
  one total order, one truth          gossip later; merge() must make
  unavailable in minority partition   ANY delivery order converge
                                      available under ANY partition
```

Strong Eventual Consistency (SEC): replicas that have *received the same
set of updates* are in the *same state* — no matter the order received.
That's a theorem you get for free if state forms a **join semilattice**
(merge = least upper bound: associative, commutative, idempotent).

```mermaid
graph TD
    subgraph "join semilattice: merge is the join"
        A["A: {x:5}"] --> AB["A⊔B: {x:5, y:7}"]
        B["B: {y:7}"] --> AB
        AB --> ABC["A⊔B⊔C — same no matter the path"]
        C["C: {x:2}"] --> AC["A⊔C: {x:5}"]
        AC --> ABC
    end
```

## Two delivery models

| | state-based (CvRDT) | op-based (CmRDT) |
|---|---|---|
| ship | whole state (or delta) | individual operations |
| network needs | nothing — any gossip, any dupes | causal delivery, exactly-once (or idempotent ops) |
| merge | join of semilattice | apply op; concurrent ops must commute |
| in this crate | `counter.rs`, `orset.rs`, `lww.rs`, `graph.rs` | `rga.rs` (Insert/Delete ops) |
| in the wild | Riak, Redis Enterprise CRDTs | Yjs, automerge, loro updates |

## The zoo you build (src/)

| file | CRDT | the one idea | status |
|---|---|---|---|
| `clock.rs` | VClock + Dot | (replica, counter) names every event; pointwise-max join; `partial_cmp → None` **defines** "concurrent" | PROVIDED |
| `lww.rs` | LWW register/map | total order by (ts, replica) — converges by *discarding* concurrent writes | PROVIDED |
| `counter.rs` | G/PN-Counter | per-replica slots + pointwise max; PN = two G-Counters because signed max isn't a join | stub |
| `orset.rs` | add-wins OR-Set | adds tag fresh dots; remove kills only *observed* dots → concurrent add survives | stub |
| `rga.rs` | RGA sequence | identity (Dot) not index; insert-after-parent; skip larger-id siblings; tombstones | stub |
| `graph.rs` | graph CRDT | OR-Set nodes + OR-Set edges + LWW props; dangling edges **hidden, not deleted** | stub |

### LWW's lie, measured (bench lane 1, provided — runs today)

Two replicas, 20K writes each, LWW map, varying keyspace and sync interval.
The lane counts two things. **Overwritten** is every local value a merge
replaced, which is what a naive merge counter reports. **Lost** is the subset
replaced by a write that never saw it: replica B's write overwrote replica A's
value without B having received it. Each write carries `(replica, seq,
how many of the other replica's writes the writer had received)`, so "saw it"
is a comparison, not a guess.

```
    keys   sync_every  overwritten%  lost (concurrent)%   model%
      10            1       94.98%               5.00%    5.00%
      10          100        5.00%               5.00%    5.00%
    1000          100       88.34%               4.50%    4.53%
  100000        10000       12.45%               4.64%    4.53%
```

The first row is the number this topic used to headline, and it is a
measurement of the wrong thing. With per-write sync on 10 keys, 94.98% of
writes replace a value *that the writer had already received*: an ordinary
sequential update, which any register does and which loses nothing. The writes
that no replica remembers are **5.00%**: one in 20, exactly the chance
(1/10) that both replicas picked the same key in the same window, times one
loser per colliding pair out of two writes.

The model column derives that. In a window of *s* writes per replica over *K*
keys, a key is written by one replica with probability pA = 1 − (1 − 1/K)^s,
by both with probability pA², and each doubly-written key loses one write, out
of 2*s* writes: lost = K·pA² / (2*s*). Worked at K = 10, s = 1: pA = 0.1,
10 × 0.01 / 2 = 0.05. At K = 1000, s = 100: pA = 1 − 0.999^100 = 0.09522,
1000 × 0.009067 / 200 = 4.53%, against 4.50% measured.

The sweep below is the decision-useful form (measured lost% with the model in
brackets, 5,000 writes per replica):

```
    keys     sync_every=1    sync_every=10   sync_every=100  sync_every=1000
      10     5.21% (5.00%)    21.09% (21.21%)     5.00% (5.00%)     0.50% (0.50%)
     100     0.45% (0.50%)     4.54% (4.57%)    20.18% (20.10%)     4.99% (5.00%)
    1000     0.03% (0.05%)     0.43% (0.50%)     4.27% (4.53%)    20.27% (19.99%)
   10000     0.01% (0.00%)     0.04% (0.05%)     0.39% (0.50%)     4.58% (4.53%)
```

Loss is a function of the ratio *s*/*K*, not of either alone: it peaks near
20% where the sync window is about as long as the key space (*s* ≈ *K*) and
falls on both sides. Short windows leave nothing to collide with; long windows
let a replica's own later writes supersede its earlier ones before they are
ever exposed to the other side, and those self-supersessions are not losses.
So the answer to "how bad is LWW" is "about 5% at one write per key per
window, up to about 20% when the window equals the key space, ~0 when sync is
much faster than the key space", and never the 95% the unclassified counter
printed. Two caveats: 20% is for two replicas with uniform keys; more
replicas multiply the collision pairs, and skew raises the doubly-written key
count. And "lost" here means a write nobody saw before replacing it, not that
the application cared: the OR-Set and counters exist to make those writes
survive instead.

## Sequence CRDTs: where the real engineering is

```
insert 'X' after 'a' (parent = a's dot):

  a ──► c              a ──► X ──► c        concurrent 'Y' same parent:
        integrate:           tombstone ok:   a ──► Y ──► X ──► c
        walk after a,        deleted elems   (larger (counter,replica)
        skip larger-id       still anchor    sits closer to parent —
        siblings             children        both replicas agree)
```

Interleaving is the dragon: naive RGA can interleave two users' typed
words character-by-character. Fugue (Loro's algorithm) fixes this with
left+right origins — see `reading-sequence-crdts.md`.

## Code reading (all cloned under ~/repos)

| repo | anchor | what to see |
|---|---|---|
| automerge | `rust/automerge/src/clock.rs:109`, `:145` | our clock.rs, industrial: `covers()`, the partial order |
| automerge | `rust/automerge/src/op_set2/op.rs:52` | `succ` — deletion as *successor ops*, not flags |
| yrs (Yjs) | `yrs/src/block.rs:160`, `:1302`, `:1415` | ID = our Dot; Item = our Element; `integrate()` = the rule you implement in rga.rs |
| diamond-types | `src/listmerge/merge.rs:142`, `yjsspan.rs:16-17` | same integrate, but ops in a run-length time DAG; `NOT_INSERTED_YET` spans |
| loro | `crates/loro-internal/src/{dag,diff_calc,handler}` | Fugue + fractional_index + generic-btree crates |
| cr-sqlite | `core/rs/core/src/local_writes/after_update.rs:65-123`, `mod.rs:111-137` | LWW-per-column over SQLite; db_version bookkeeping — multi-master as an *extension* |

## Reading guides

1. [reading-shapiro-crdts.md](reading-shapiro-crdts.md) — CRDT foundations: convergence without coordination.
2. [reading-kleppmann-json-crdts.md](reading-kleppmann-json-crdts.md) — JSON CRDTs & the move op: identity beats paths.
3. [reading-sequence-crdts.md](reading-sequence-crdts.md) — Sequence CRDTs: what a decade of engineering does to RGA.
4. [reading-cr-sqlite.md](reading-cr-sqlite.md) — cr-sqlite: a real database goes multi-master.

## Experiments

```
cd experiments
cargo test              # 6 provided tests pass; 18 fix the contract for your stubs
cargo run --release --bin crdt_bench
```

Bench lanes: 1 = LWW's lie (provided). 2 = OR-Set convergence storm +
tombstone census. 3 = RGA editing trace (throughput, tombstone bloat).
4 = graph dangling storm (hidden edges, resurrection).

## Exercises

1. Implement the four stubs until all tests pass and lanes 2-4 print.
2. **automerge vs loro bench** (from PLAN; needs deps beyond this crate's
   convention, so it's an exercise): load both crates in a scratch project,
   replay an editing trace (diamond-types repo ships some under
   `benchmark_data/`), compare apply time + memory.
3. Delta-CRDTs: lane 1's `sync_every=1` cost is O(writes × map size)
   because state-based sync ships everything. Sketch the delta-mutator
   version of your OR-Set.
4. Garbage: your OR-Set keeps tombstones forever. What's the *causal
   stability* condition that lets you drop one? Who tracks it?
5. Alternative dangling-edge policies: cascade-delete (remove observed
   edge-dots when a node dies) vs. our hide-not-delete. Which breaks
   add-wins symmetry? Which would FalkorDB users expect?
6. Props keyed by node id survive remove/re-add (our choice). Automerge
   keys object state by *creation op*, so re-add = fresh object. When is
   each right?

## Cross-topic threads

- **Topic 15 (Raft)**: same problem, opposite trade. M31 asks you to run
  the *same workload* against both and compare write latency + conflict
  semantics.
- **Topic 29 (HLC)**: LWW needs timestamps that respect causality —
  that's the HLC from `29-distributed-txn/reading-spanner-hlc.md`.
  cr-sqlite's db_version is a Lamport clock in the same spirit.
- **Topic 5 (MVCC)**: tombstones-as-versions; RGA's deleted elements are
  MVCC's dead versions, and both need a GC horizon (causal stability ↔
  oldest active snapshot).
- **Topic 26 (probabilistic structures)**: vector clocks are exact
  causality; version vectors trimmed by dotted version vectors are the
  space-conscious cousin.

## Capstone M31 — active-active FalkorDB

Two masters, both accepting writes, no leader:

- nodes/edges = add-wins OR-Sets (`orset.rs` semantics, `graph.rs`
  composition) — concurrent `CREATE`/`DELETE` of the same node resolves
  add-wins; re-add resurrects hidden edges.
- properties = LWW maps with HLC timestamps (topic 29) — and now you can
  *quantify* the lost-write rate you're signing up for (lane 1).
- anti-entropy: periodic state merge (or dotted-version-vector deltas).
- Contrast with M15: same graph workload through Raft — measure the
  write-latency gap, then write down which conflicts Raft *prevented*
  that active-active *resolved* (and whether users would agree with the
  resolutions).
