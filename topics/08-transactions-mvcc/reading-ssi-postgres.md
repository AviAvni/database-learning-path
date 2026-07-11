# SSI: serializable snapshot isolation without blocking anyone

How postgres turned SI into SERIALIZABLE with passive markers instead of
blocking locks — Ports & Grittner's VLDB '12 account of productionizing
Cahill's dangerous-structure theorem. Prereq: the Berenson critique
([reading-ansi-critique.md](reading-ansi-critique.md)) — you need write
skew cold.

## The theory in one diagram

Every SI anomaly contains a **dangerous structure**: two consecutive
rw-antidependencies with the pivot in the middle:

```
        rw            rw
  T_in ────► T_pivot ────► T_out        rw edge: T reads x, then U writes x
                                        (U "un-reads" T's snapshot)
  … and T_out commits FIRST of the three.
```

Cahill's theorem: every non-serializable SI execution has this shape. So:
track rw-antidependency edges; when a txn accumulates BOTH an inbound and
an outbound rw edge (it became a pivot), abort somebody. This is
conservative — some aborted histories were actually fine (false positives)
— but it never misses a real cycle.

The whole detector, conceptually — two flags per transaction and one rule:

```rust
fn on_rw_antidependency(reader: TxnId, writer: TxnId, g: &mut ConflictGraph) {
    g[reader].out_rw = true;             // reader ──rw──► writer
    g[writer].in_rw = true;
    for t in [reader, writer] {
        if g[t].in_rw && g[t].out_rw {   // t became a pivot:
            abort_someone(t, g);         //   T_in ─rw─► t ─rw─► T_out
        }                                // conservative: false positives yes,
    }                                    // missed cycles never
}
```

Doctors write skew as the structure: T1 reads bob's row (later written by
T2) ⇒ T1 ──rw──► T2; T2 reads alice's row (later written by T1) ⇒
T2 ──rw──► T1. A cycle of length 2 — each txn is a pivot.

## What the paper adds over Cahill (§4–§7)

1. **SIREAD locks** — not locks at all: passive markers "I read this",
   at tuple/page/relation granularity with escalation under memory
   pressure (coarser = more false aborts, never wrong results).
   Predicate reads are handled by locking the read RANGE via index pages —
   this is the answer to phantoms that key-based OCC (RocksDB Q3) can't give.
2. **Commit ordering refinement** — only abort when T_out committed first
   (fewer false positives than raw Cahill).
3. **Safe snapshots & DEFERRABLE** — a read-only txn can prove it can
   never be part of a dangerous structure and drop ALL tracking; RO
   backups run at serializable for free (after possibly waiting).
4. **Memory bounding** — SIREAD state must survive commit (rw edges can
   form after you commit!) and is only cleaned when overlapping txns end;
   §7's summarization is the price of bounded RAM.
5. **2PC** interaction, and why prepared transactions make everything
   worse.

## The costs (§8, the honest part)

- ~7% overhead on their benchmarks at low contention; abort rate is the
  real cost and it's workload-shaped (hot rw pairs → pivot storms).
- Retry is the application's job: serialization_failure (40001) means
  "run it again", so SSI only works if the app loops.

## Questions for notes.md

1. Why must SIREAD locks outlive commit? Construct the history where the
   dangerous structure completes after the reader committed.
2. Lock escalation trades memory for false aborts. Where's the same trade
   in your mvcc.rs Serializable mode (hint: your read-set granularity is
   whole keys — what's the graph equivalent of escalating to a relation)?
3. Read-only txns: why can they NEVER be T_pivot? (Which edge can't they
   have?) How does that justify the safe-snapshot optimization?
4. M8: FalkorDB is single-writer. With exactly one writer at a time, can
   a dangerous structure form at all between two write txns? Between a
   writer and concurrent readers? So is SSI machinery needed, or does
   single-writer + SI already equal serializable? (Prove it with the
   pivot definition — this is the M8 design shortcut.)

## Done when

You can draw the dangerous structure from memory, place both write-skew
txns on it, and answer Q4 — it decides how much of this paper M8 needs.

## References

**Papers**
- Ports & Grittner — "Serializable Snapshot Isolation in PostgreSQL"
  (VLDB 2012, [arXiv:1208.4179](https://arxiv.org/abs/1208.4179)) —
  ~1.5 h; §4–§7 are the production engineering, §8 the honest costs
- Cahill, Röhm, Fekete — "Serializable Isolation for Snapshot Databases"
  (SIGMOD 2008) — the dangerous-structure theorem this paper
  productionizes; the theorem statement is enough
