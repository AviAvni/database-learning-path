# Reading guide — Hekaton (SIGMOD '13) + "An Empirical Evaluation of In-Memory MVCC" (Wu et al., VLDB '17) (~2.5 h)

Read Hekaton first (a design), then Wu/Pavlo (the design SPACE with
benchmark-backed prices). Together they answer: what does MVCC look like
when the disk-era assumptions are deleted?

## Hekaton — MVCC with no locks, no latches, no pages

The version record is self-describing, like a postgres tuple but
timestamp-based:

```
 ┌──────────┬─────────┬──────────────┬─────────┐
 │ begin_ts │ end_ts  │ index links  │ payload │
 └──────────┴─────────┴──────────────┴─────────┘
 live version: end_ts = ∞
 during update: end_ts = writer's txn-id (acts as the write lock!)
 visibility: begin_ts ≤ my_read_ts < end_ts
```

Key moves to internalize:

1. **Txn-ids double as locks.** Storing a txn-id in end_ts is the
   write-write conflict check: a second writer sees a txn-id there and
   aborts/waits. One CAS = lock + version link. (Bit-smuggling again —
   the id/timestamp distinction is one bit.)
2. **Commit processing, not commit point.** At commit, get commit_ts, then
   *validate* (serializable = re-check read set unchanged + rescan scan
   predicates), then write log, then **fix up** all your begin/end_ts
   fields from txn-id → commit_ts. Readers who hit a txn-id must chase the
   txn's state — visibility can depend on an in-flight commit
   (commit dependencies, taken instead of blocking).
3. **Indexes point at version chains**; lock-free hash + Bw-tree (topic 9's
   protagonists) — MVCC and lock-free structures co-designed.
4. **Cooperative GC**: any thread that walks past a version older than the
   oldest active read_ts unlinks it. No vacuum process; the workload
   cleans itself in proportion to how much it reads.

Contrast postgres on every axis: ts vs xid+clog+hint-bits; validation vs
SIREAD; cooperative GC vs vacuum; new-to-old chains vs t_ctid old-to-new.

## Wu/Pavlo — the menu with prices (VLDB '17)

They implement every combination in one system and measure. The axes:

| Axis | Options | Verdict (their workloads) |
|---|---|---|
| concurrency control | MVTO / MVOCC / MV2PL / SI+SSN | no universal winner; MVTO strong; the *version machinery* dominates CC choice |
| version storage | append-only / delta / time-travel | **delta wins for writes** (N2O append-only for reads); append-only pays full-tuple copies |
| ordering | newest-to-oldest / oldest-to-newest | N2O wins — readers want the newest; O2N walks garbage first |
| GC | tuple-level background / cooperative / txn-level / epoch | cooperative + epoch wins; background vacuum-style lags under write bursts |
| index mgmt | logical pointers / physical | logical (indirection) — physical means every version churns every index |

The meta-lesson (their words, roughly): everyone argues about CC
algorithms, but **version storage and GC decide throughput**. Storage
layer > protocol. (The RUM triangle strikes again.)

## Questions for notes.md

1. Hekaton's end_ts-as-lock: write the CAS-based first-writer-wins in
   pseudocode. Your mvcc.rs does the same check where? (Point at the line
   once implemented.)
2. Delta storage wins for writes; append-only N2O for reads. Which is a
   GraphBLAS **delta matrix** (topic 20)? So M8's "copy-on-write + deltas"
   sits where in the Wu/Pavlo taxonomy — and what does their data predict
   about its read path?
3. Logical vs physical index pointers: FalkorDB's node ids ARE logical
   indirection into matrices. What does that make "index management" cost
   for a graph MVCC — which updates still have to touch indexes?
4. Cooperative GC in proportion to reads: what happens to a write-only
   hot key that nobody reads? (Wu/Pavlo call this out — find the fix.)
5. Predict, then check §6 of Wu/Pavlo: at 40 cores, high contention, what
   ruins MVOCC — validation aborts or timestamp allocation?

## Done when

You can fill the 5-axis table from memory and place postgres, Hekaton,
and your M8 design in it — one row each.
