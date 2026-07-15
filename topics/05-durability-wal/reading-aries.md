# ARIES: recovery when you escape nothing

Postgres escapes undo via MVCC, SQLite-WAL escapes redo via page images, LMDB
escapes logging via COW — ARIES is the recovery method for engines that escape
*nothing*: update-in-place, steal, no-force. It is the most-cited recovery
paper and the vocabulary every other design in this topic is defined against;
reading it tells you exactly what each escape hatch is worth. Before the
70 pages, this chapter builds the machine step by step: the two buffer
policies that create the problem, the LSN discipline that makes replay safe,
the three recovery passes, and the CLR trick that lets recovery itself crash.

## The problem in one sentence

An update-in-place engine that lets dirty pages reach disk *before* commit
and doesn't force them to disk *at* commit can crash into a state where the
disk holds half of transaction A's writes and none of transaction B's
committed ones — and recovery must reconstruct exactly which is which from
nothing but an append-only log, even if it crashes again halfway through
doing so.

## The concepts, step by step

### Step 1 — steal and no-force: two freedoms, two debts

A buffer manager (the component caching disk pages in RAM) faces two policy
questions, and each "convenient" answer creates a recovery obligation.
**Steal** = the cache may evict a dirty page to disk *before* its
transaction commits (freedom: evict whatever page is coldest; debt: the disk
now holds uncommitted data, so after a crash you need **undo** — the ability
to reverse it). **No-force** = commit does *not* require writing the
transaction's pages to disk, only its log records (freedom: commit costs one
sequential log flush, not N random page writes; debt: the disk may lack
committed data, so you need **redo** — the ability to re-apply it). The
2×2 matrix:

| | force (pages flushed at commit) | no-force |
|---|---|---|
| **no-steal** | no undo, no redo — but hopeless perf | redo only (your likely M5 design) |
| **steal** | undo only | **undo + redo — ARIES's territory** |

High-performance update-in-place engines (InnoDB, SQL Server, Db2) all
choose steal + no-force — both freedoms, both debts. ARIES is how you pay.

### Step 2 — the LSN: one number that orders everything

The **LSN** (log sequence number) is a log record's byte offset in the log —
monotonically increasing, so it doubles as a global timestamp for every
change in the system. The discipline that makes everything else work: every
page on disk carries the LSN of the last log record applied to it
(**pageLSN**). Now "has this page already seen this update?" is one integer
comparison — `pageLSN ≥ record.LSN ⇒ yes, skip` — and replaying the log
becomes **idempotent** (safe to repeat: applying a record twice is
impossible because the first application raised the pageLSN). Each record
also carries its transaction's previous record's LSN (**prevLSN**), chaining
every transaction's history backward through the log for undo to walk.

### Step 3 — fuzzy checkpoints: bounding how far back recovery reaches

A checkpoint is a periodic log record that lets recovery start from
somewhere later than the beginning of time. Stopping the system to flush
everything would be a latency crater, so ARIES checkpoints **fuzzily** —
without stopping anything, it just snapshots two small tables into the log:
the **DPT** (dirty page table: which cached pages have unflushed changes,
and the LSN of the *earliest* change each might be missing on disk — its
recLSN) and the **ATT** (active transaction table: which transactions are
in flight, and their last LSN). Cost: a few KB written, zero pause. The DPT
tells redo where to start reading (the minimum recLSN); the ATT tells undo
who its candidates are.

## Vocabulary (the paper is unreadable without these)

| Term | Meaning |
|---|---|
| steal | dirty pages may hit disk BEFORE commit (⇒ need undo) |
| no-force | pages need NOT hit disk at commit (⇒ need redo) |
| LSN | log sequence number; every page stamps the LSN of its last change |
| CLR | compensation log record — undo work is itself logged, redo-only |
| DPT | dirty page table (checkpointed) — which pages might need redo |
| ATT | active transaction table (checkpointed) — who needs undo |

### Step 4 — pass 1, analysis: rebuild the two tables

After a crash, recovery's first pass reads the log forward from the last
checkpoint, replaying only the *bookkeeping*: it rebuilds the DPT and ATT as
they stood at the instant of the crash — pages that got dirtied after the
checkpoint enter the DPT, transactions that committed leave the ATT, and
whoever remains in the ATT at the end is a **loser** (a transaction that
was still running when the world ended). No data is touched; the pass just
answers two questions: *where must redo start* (min recLSN in the DPT) and
*who must undo roll back* (the losers in the ATT).

### Step 5 — pass 2, redo: repeat history, even for losers

Redo reads forward from the DPT's minimum recLSN and re-applies every
update whose page hasn't seen it (the Step 2 comparison: apply iff
`pageLSN < record.LSN`) — **including the updates of doomed loser
transactions**. This "repeating history" is the counterintuitive core of
ARIES: the goal of redo is not "restore committed work" but "restore the
*exact* state at the instant of the crash", losers and all. Why: only from
that exact state can undo run as perfectly ordinary transaction rollback —
the same code path as a user typing ROLLBACK — instead of a special
recovery-only mode reasoning about half-restored pages. Redo pays for this
with some wasted work (re-applying updates it will immediately undo); it
buys one rollback mechanism instead of two.

```
        log: …──[ckpt: DPT+ATT]────────────────────────► crash
 1. ANALYSIS   ────────────────►  rebuild DPT/ATT from ckpt forward
 2. REDO       ────────────────►  repeat HISTORY (even losers!) from
                                  min(recLSN in DPT) — page LSN ≥ record LSN ⇒ skip
 3. UNDO       ◄────────────────  roll back losers, writing CLRs;
                                  CLR.undoNext skips already-undone work
```

### Step 6 — pass 3, undo with CLRs: recovery that survives recovery

Undo walks each loser's prevLSN chain newest-first, reversing every update —
and here is the trick that makes ARIES bulletproof: **each undo action is
itself logged**, as a **CLR** (compensation log record). A CLR is redo-only
(it is never undone — undoing an undo would re-apply the original mistake)
and carries an **undoNext** pointer to the record *before* the one just
compensated. Now crash during undo: the next recovery's redo pass replays
the CLRs (restoring the partial rollback — repeating history again), and
undo resumes exactly at the last CLR's undoNext. No update is ever undone
twice, no matter how many times recovery itself crashes. The three passes,
as one function:

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

One refinement worth knowing exists: **nested top actions** let a structural
change (a B-tree split) survive even if the transaction that triggered it
aborts — other transactions may already be using the new page; physical
consistency and logical visibility are different things (paper §10).

## How to read the paper (with the concepts in hand)

1. This chapter's steps (or Franklin's "Crash Recovery" chapter / CMU 15-445
   recovery notes) until the three passes + CLRs feel obvious.
2. Paper §3 ("the problem"): why the naive undo-then-redo attempts fail —
   the best catalog of recovery bugs ever assembled.
3. §6: the passes in detail — read for `pageLSN ≥ recLSN ⇒ skip redo`
   (Step 2's idempotence via LSN comparison) and the CLR undoNext chain
   (Step 6).
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
