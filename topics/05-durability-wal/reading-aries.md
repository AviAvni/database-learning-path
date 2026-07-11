# ARIES: recovery when you escape nothing

Postgres escapes undo via MVCC, SQLite-WAL escapes redo via page images, LMDB
escapes logging via COW — ARIES is the recovery method for engines that escape
*nothing*: update-in-place, steal, no-force. It is the most-cited recovery
paper and the vocabulary every other design in this topic is defined against;
reading it tells you exactly what each escape hatch is worth.

## Why read it when postgres/turso don't do full ARIES

ARIES is the recovery design for **update-in-place + steal/no-force** engines
(InnoDB, SQL Server, Db2). Postgres escapes undo via MVCC; SQLite-WAL escapes
redo via page images; LMDB escapes logging via COW. ARIES is what you need
when you escape *nothing* — reading it tells you exactly what those escapes
are worth.

## Vocabulary (the paper is unreadable without these)

| Term | Meaning |
|---|---|
| steal | dirty pages may hit disk BEFORE commit (⇒ need undo) |
| no-force | pages need NOT hit disk at commit (⇒ need redo) |
| LSN | log sequence number; every page stamps the LSN of its last change |
| CLR | compensation log record — undo work is itself logged, redo-only |
| DPT | dirty page table (checkpointed) — which pages might need redo |
| ATT | active transaction table (checkpointed) — who needs undo |

## The three passes

```
        log: …──[ckpt: DPT+ATT]────────────────────────► crash
 1. ANALYSIS   ────────────────►  rebuild DPT/ATT from ckpt forward
 2. REDO       ────────────────►  repeat HISTORY (even losers!) from
                                  min(recLSN in DPT) — page LSN ≥ record LSN ⇒ skip
 3. UNDO       ◄────────────────  roll back losers, writing CLRs;
                                  CLR.undoNext skips already-undone work
```

**Repeating history** is the counterintuitive core: redo replays *everything*,
including doomed transactions, to restore the exact crash-moment state — THEN
undo runs as ordinary, loggable transaction rollback. This is what makes
recovery-during-recovery safe (crash during undo ⇒ CLRs ensure no double-undo).

The three passes, as one function:

```rust
fn recover(log: &Log, ckpt: &Checkpoint) {
    let (dpt, att) = analysis(log, ckpt);          // 1. who was dirty, who was active
    for rec in log.from(dpt.min_rec_lsn()) {       // 2. REDO: repeat history —
        if page_lsn(rec.page_id) < rec.lsn {       //    even losers' updates.
            apply_redo(rec);                       //    pageLSN ≥ recLSN ⇒ skip:
        }                                          //    idempotence by LSN compare
    }
    for txn in att.losers() {                      // 3. UNDO: ordinary rollback,
        for rec in txn.updates_newest_first() {    //    but each undo is LOGGED
            let clr = log.append_clr(rec);         //    as a redo-only CLR
            clr.undo_next = rec.prev_lsn;          //    crash mid-undo? restart
            apply_undo(rec);                       //    resumes at undo_next —
        }                                          //    no double-undo, ever
    }
}
```

## Read in this order

1. A summary (above) until the three passes + CLRs feel obvious.
2. Paper §3 ("the problem"): why the naive undo-then-redo attempts fail —
   the best catalog of recovery bugs ever assembled.
3. §6: the passes in detail — read for `pageLSN ≥ recLSN ⇒ skip redo`
   (idempotence via LSN comparison) and the CLR undoNext chain.
4. Skim §10 (nested top actions — how B-tree splits survive rollback: the
   split stays even if the insert that caused it aborts).

## Map to what you've read

- postgres: ANALYSIS+REDO yes (xlogrecovery.c), UNDO replaced by MVCC vacuum;
  FPIs make redo idempotent even without perfect LSN discipline.
- turso WAL: no passes at all — commit boundary detection only.
- Your M5 WAL: if you chose logical records (reading-turso-wal.md Q3), you owe
  ARIES-style idempotent redo: stamp pages with LSNs, skip if page is newer.

## Questions to answer in notes.md

1. Why must CLRs be redo-only (never undone)? Walk a crash-during-undo.
2. Nested top action for a B-tree split: why is letting the split survive an
   aborted insert both correct and necessary? (Other txns may already use the
   new page; physical consistency ≠ logical visibility.)
3. Which of steal/no-force does YOUR topic-3 B+tree + WAL implement? Derive
   which passes your recovery needs. (Likely no-steal/no-force at first ⇒
   redo-only — say so explicitly.)

## Done when

You can fill the 2×2 steal/force matrix with (undo?, redo?) from memory and
explain repeating history in two sentences.

## References

**Papers**
- Mohan, Haderle, Lindsay, Pirahesh, Schwarz — "ARIES: A Transaction
  Recovery Method Supporting Fine-Granularity Locking and Partial
  Rollbacks Using Write-Ahead Logging" (ACM TODS 1992) — 70 pages; read a
  summary first (Franklin's "Crash Recovery" chapter or CMU 15-445
  recovery notes), then dip into §3 and §6, skim §10
