# ARIES: recovery when you escape nothing

Postgres escapes undo via MVCC, SQLite-WAL escapes redo via page images, LMDB
escapes logging via COW — ARIES is the recovery method for engines that escape
*nothing*: update-in-place, steal, no-force. It is the most-cited recovery paper
and the vocabulary every other design in this topic is defined against; reading
it tells you exactly what each escape hatch is worth. Before the 70 pages, this
chapter builds the machine step by step: the two buffer policies that create the
problem, the LSN discipline that makes replay safe, the three recovery passes,
the CLR trick that lets recovery itself crash — and then runs all three passes
by hand over an eight-record log so the rules stop being abstract.

Every section number, figure number and quotation below was checked against the
paper as published: **Mohan, Haderle, Lindsay, Pirahesh & Schwarz, "ARIES: A
Transaction Recovery Method Supporting Fine-Granularity Locking and Partial
Rollbacks Using Write-Ahead Logging", ACM TODS 17(1), March 1992, pp. 94–162.**
Where this chapter uses a term the paper does not (`ATT`, `DPT`,
"physiological"), it says so.

## Vocabulary, defined once, before it is used

| Term | Meaning | Where |
|---|---|---|
| **WAL** (write-ahead logging) | a log record describing a change is made durable *before* the changed page may reach nonvolatile storage | §1 |
| **steal** | the buffer manager may write a dirty page to disk *before* its transaction commits ⇒ you owe **undo** | §2, from Haerder & Reuter [36] |
| **no-force** | commit does *not* require the transaction's data pages to reach disk, only its log ⇒ you owe **redo** | §2, [36] |
| **LSN** (log sequence number) | "the address of the first byte of the log record in the ever-growing log address space… monotonically increasing" | §4.1 |
| **page-LSN** | the LSN of the most recent log record applied to a page, stored *in* the page | §4.2 |
| **PrevLSN** | in every log record: the LSN of the same transaction's preceding record — a backward chain per transaction | §4.1 |
| **UndoNxtLSN** | present *only* in CLRs: "the value of PrevLSN of the log record that the current log record is compensating" | §4.1 |
| **CLR** (compensation log record) | the log record describing an undo action; redo-only, never itself undone | §1.1, §3 |
| **idempotent redo** | replay that is safe to repeat, because `page-LSN ≥ record.LSN` proves the change is already on the page | §6.2, Fig. 11 |
| **checkpoint** | a periodic pair of log records that bounds how far back restart must read | §5.4 |
| **fuzzy checkpoint** | a checkpoint that stops nothing and forces no dirty page to disk — it only writes out two small tables | §5.4 |
| **page-oriented redo** | redo touches only the page named in the record; no index retraversal, no other page | §1.1 |
| **logical undo** | undo may act on a *different* page than the original update, so another transaction can move an uncommitted record | §1.1 |
| **physiological logging** | Gray & Reuter's later name for exactly the above pairing — physical to a page, logical within it. **The ARIES paper never uses this word**; its terms are the two above | — |
| **transaction table** | the paper's name for what textbooks call the **ATT**: TransID, State ('P'/'U'), LastLSN, UndoNxtLSN | §4.3 |
| **dirty_pages table** | the paper's name for what textbooks call the **DPT**: PageID, RecLSN | §4.4 |
| **group commit** | letting one durability call cover many transactions' commit records | not in this paper — see `reading-postgres-xlog.md` |

**And the durability call itself.** ARIES's one hard I/O requirement at commit is
that the log be forced through the commit record. What "forced" costs is this
topic's ladder, measured by `experiments/src/bin/fsync_ladder.rs` on the machine
in `notes.md`: `write()` alone **1.17 µs** (page cache only — survives `kill -9`,
not power loss); `fdatasync()` / macOS `fsync()` **22.67 µs** (handed to the
drive, whose volatile cache may still hold it); macOS `fcntl(fd, F_FULLFSYNC)`
**2.97 ms** (the drive flushed its cache). 856 898 → 44 109 → 337 implied
durable commits/s: **19.4×** then a further **131×**. The middle rung was
measured on macOS as `fsync(2)` — there is no `fdatasync` on this machine — and
`fdatasync` is named only because it occupies the same rung on Linux. Every time
this chapter says "force the log", that is the price, and which rung you are on
decides whether ARIES's commit path costs microseconds or milliseconds.

## The problem in one sentence

An update-in-place engine that lets dirty pages reach disk *before* commit and
doesn't force them to disk *at* commit can crash into a state where the disk
holds half of transaction A's writes and none of transaction B's committed ones
— and recovery must reconstruct exactly which is which from nothing but an
append-only log, even if it crashes again halfway through doing so.

## The concepts, step by step

### Step 1 — steal and no-force: two freedoms, two debts

> **In:** a buffer manager (the component caching disk pages in RAM) and two
> policy questions about when its pages may or must be written.
> **Out:** a 2×2 matrix in which each convenient answer names a recovery pass
> you now owe.

**Steal** = the cache may evict a dirty page to disk *before* its transaction
commits. Freedom: evict whatever page is coldest. Debt: the disk now holds
uncommitted data, so after a crash you need **undo** — the ability to reverse
it. **No-force** = commit does *not* require writing the transaction's pages to
disk, only its log records. Freedom: commit costs one sequential log force, not
N random page writes. Debt: the disk may lack committed data, so you need
**redo**.

| | force (pages flushed at commit) | no-force |
|---|---|---|
| **no-steal** | no undo, no redo — but hopeless perf | redo only (your likely M5 design) |
| **steal** | undo only | **undo + redo — ARIES's territory** |

High-performance update-in-place engines (InnoDB, SQL Server, Db2) all choose
steal + no-force — both freedoms, both debts. ARIES is how you pay.

The paper's argument for steal is stronger than the usual "you might run out of
buffer space", and worth having: under fine-granularity (record-level) locking
with overlapping transactions, **"with a no-steal policy, a page may never get
written to nonvolatile storage if the page always contains uncommitted updates"**
(§2). No-steal is not merely inconvenient at high concurrency; it can be
*unsatisfiable*. This is the hinge that makes ARIES's whole apparatus necessary
rather than optional, and it is worth stating precisely because a redo-only
engine (M5, turso) buys its simplicity by refusing record-level locking on
shared pages.

*Why it matters:* every escape hatch in this topic is a cell in this matrix.
Naming your cell tells you which passes you must write.

### Step 2 — the LSN: one number that orders everything

> **In:** a log record about to be written, and the page it changes.
> **Out:** a monotonically increasing identifier that lets a single integer
> comparison answer "has this page already seen this update?"

The **LSN** is defined by §4.1 as "the address of the first byte of the log
record in the ever-growing log address space" and is therefore "monotonically
increasing" — a global timestamp for every change in the system, obtained for
free from the log's own geometry. The discipline that makes everything else
work: every page on disk carries the LSN of the last log record applied to it
(§4.2, the **page-LSN**). Now "has this page already seen this update?" is one
comparison — `page-LSN ≥ record.LSN ⇒ yes, skip` — and replaying the log becomes
**idempotent**: applying a record twice is impossible, because the first
application raised the page-LSN past it.

Two backward pointers complete the structure:

- **PrevLSN**, in every record, chains a transaction's own history backward
  through the log, so undo can walk one transaction without scanning. §4.1
  notes in a footnote that AS/400, Encompass and NonStop SQL *don't* link a
  transaction's records, "which makes undo inefficient since a sequential
  backward scan of the log must be performed."
- **UndoNxtLSN**, present *only* in CLRs (§4.1), holds "the value of PrevLSN of
  the log record that the current log record is compensating" — it is zero when
  nothing remains. This one field is the whole of Step 6.

The paper's own name is `UndoNxtLSN`; earlier versions of this chapter called it
`undoNext`, which appears nowhere in the paper.

Two more of §1.1's definitions matter, because they explain what the LSN buys.
**Page-oriented redo** means "the log record whose update is being redone
describes which page of the database was originally modified… and the same page
is modified during the redo processing… no other page of the database needs to
be examined" — so redo is one page fetch and one comparison, and pages recover
independently. **Logical undo** is the opposite freedom: undo may act on a
different page, which is what "permit[s] uncommitted updates of one transaction
to be moved to a different page by another transaction." §1.1 states the
trade-off in one line: "In the interest of efficiency, ARIES supports
page-oriented redo and it supports, in the interest of high concurrency, logical
undos."

*Why it matters:* this is the single idea the rest of the topic borrows. Postgres
stamps pages with LSNs for the same reason; turso's frames are idempotent for a
weaker version of the same reason (a whole page image needs no comparison at
all).

### Step 3 — fuzzy checkpoints: bounding how far back recovery reaches

> **In:** a running system with dirty pages and in-flight transactions, and a
> log that would otherwise have to be read from the beginning of time.
> **Out:** two small tables written into the log, and a master record pointing
> at them — with **no pause and no page flush**.

Stopping the system to flush everything would be a latency crater, so ARIES
checkpoints **fuzzily** (§5.4): it writes a `begin_chkpt` record, then an
`end_chkpt` record carrying the transaction table, the buffer pool's dirty_pages
table, and the file mapping; then the **master record** on disk is updated to
hold the `begin_chkpt` record's LSN. That master record is where restart begins.

The sentence to carry away is §5.4's: **"ARIES does not require that any dirty
pages be forced to nonvolatile storage during a checkpoint."** Cost: a few KB
written, zero pause, zero forced page I/O. What you buy is a bound: the
dirty_pages table's minimum RecLSN is where redo must start, and the transaction
table names undo's candidates.

The **RecLSN** of a page is the LSN of the *earliest* change that page might be
missing on disk — recorded when the page first became dirty. §4.4: "the minimum
RecLSN value in the table gives the starting point for the redo pass."

*Why it matters:* checkpoint cost and recovery bound are the two halves of one
dial, and ARIES's setting of it — pay nothing now, read a bounded amount later —
is why nobody had to invent a "stop the world" checkpoint again. Compare
postgres's `CreateCheckPoint` (`xlog.c:7400–7897`), which *does* flush its buffers
in `CheckPointGuts` and therefore pays much more up front.

### Step 4 — pass 1, analysis: rebuild the two tables

> **In:** the master record's `begin_chkpt` LSN, and every log record from there
> to the end of the log.
> **Out:** the transaction table and dirty_pages table as they stood at the
> instant of the crash, a `RedoLSN`, and a list of losers. No data page is
> touched and no log record is written.

Analysis (§6.1, Fig. 10) opens the log scan at the `begin_chkpt` record and
replays only the *bookkeeping*:

```
RESTART_ANALYSIS  (paper Fig. 10, condensed)
  Trans_Table, Dirty_Pages := empty
  open log scan at Master_Rec.ChkptLSN          -- the begin_chkpt record
  for each record until end of log:
      if trans-related and TransID not in Trans_Table:
          insert (TransID, 'U', LSN, PrevLSN)
      case update | compensation:
          Trans_Table[T].LastLSN := LSN
          if update  and undoable: Trans_Table[T].UndoNxtLSN := LSN
          if compensation:         Trans_Table[T].UndoNxtLSN := LogRec.UndoNxtLSN
          if redoable and PageID not in Dirty_Pages:
              insert (PageID, LSN)               -- RecLSN := this record's LSN
      case End_Chkpt:  merge in the checkpointed Trans_Table and Dirty_PagLst
      case prepare:    State := 'P'      case rollback: State := 'U'
      case end:        delete the Trans_Table entry
  for each entry with State='U' and UndoNxtLSN=0:
      write an 'end' record and remove it     -- rolled back, end record missing
  RedoLSN := minimum(Dirty_Pages.RecLSN)
```

Whoever remains in the transaction table with State `'U'` is a **loser** — still
running when the world ended. Note the last loop: a transaction that had already
been fully rolled back before the crash (UndoNxtLSN back to 0) but whose `end`
record never made it is quietly finished off here, without any undo work.

Two subtleties worth reading for. First, `end` is what removes a transaction —
and §5.3 says a transaction "is committed by writing an **end** record and
releasing its locks", so the `end` record *is* the durable commit point (a
separate `prepare` record is only needed for distributed transactions). Second,
the analysis pass is not strictly required: §6.1 observes the tables could be
rebuilt during redo instead, at the cost of starting redo at
`min(min(checkpoint RecLSNs), LSN(begin_chkpt))` — strictly earlier, so strictly
more work.

*Why it matters:* analysis is the pass that costs nothing and saves everything —
it is one sequential read that converts "the whole log" into "these pages from
this LSN" and "these three transactions."

### Step 5 — pass 2, redo: repeat history, even for losers

> **In:** `RedoLSN`, the dirty_pages table, and the log from `RedoLSN` forward.
> **Out:** the database's pages restored to their *exact* state at the instant of
> the crash — losers' updates included. No log record is written.

Redo (§6.2, Fig. 11) re-applies every update whose page hasn't seen it —
**including the updates of doomed loser transactions**. This "repeating history"
is the counterintuitive core of ARIES: the goal of redo is not "restore
committed work" but "restore the exact state at the instant of the crash". Only
from that state can undo run as perfectly ordinary transaction rollback — the
same code path as a user typing ROLLBACK — instead of a recovery-only mode
reasoning about half-restored pages. Redo pays with some wasted work
(re-applying updates it will immediately undo) and buys one rollback mechanism
instead of two.

**The redo test is three levels deep, not one.** Earlier versions of this chapter
gave only the third:

```
RESTART_REDO  (paper Fig. 11, condensed)
  for each record from RedoLSN to end of log:
    (1)  type is 'update' or 'compensation', and the record is redoable?
    (2)  PageID IN Dirty_Pages  AND  LSN >= Dirty_Pages[PageID].RecLSN ?
             -- both are table lookups; if either fails, the page is NOT fetched
    (3)  fetch and X-latch the page:
             IF Page.LSN < LogRec.LSN  THEN  redo it; Page.LSN := LogRec.LSN
             ELSE Dirty_Pages[PageID].RecLSN := Page.LSN + 1
```

Levels (1) and (2) are pure in-memory filters whose entire purpose is to *avoid
the page fetch* — "the RecLSN information serves to limit the number of pages
which have to be examined" (§6.2). Level (3) is the idempotence test proper. The
`ELSE` branch is the one everybody forgets: when the page turns out to be newer
than the record, the table's RecLSN was stale (the page was written to disk after
the checkpoint but before the failure), so redo *corrects the table* — and every
later record for that page can then be filtered at level (2) instead of level
(3). Step 7 shows this firing.

*Why it matters:* the difference between one test and three is the difference
between fetching every page named in the log and fetching only the pages that
might actually need work. On a large buffer pool that is most of restart time.

### Step 6 — pass 3, undo with CLRs: recovery that survives recovery

> **In:** the transaction table's losers and their `UndoNxtLSN` values.
> **Out:** every loser rolled back, a CLR written for each undone record, and a
> log from which a *second* crash can resume without undoing anything twice.

Undo reverses every loser's updates newest-first — and here is the trick that
makes ARIES bulletproof: **each undo action is itself logged**, as a **CLR**. A
CLR is redo-only (undoing an undo would re-apply the original mistake) and
carries an `UndoNxtLSN` pointing at the record *before* the one just
compensated. Crash during undo, and the next recovery's redo pass replays the
CLRs (restoring the partial rollback — repeating history again) while its
analysis pass reads each CLR's `UndoNxtLSN` straight into the transaction table
(Fig. 10, the `compensation` case in Step 4); undo then resumes exactly where it
left off. §3 states the resulting bound: "the number of CLRs written will be
exactly equal to the number of undoable log records written during forward
processing" — no matter how many times recovery itself crashes.

**Undo is one merged backward sweep, not a loop over transactions.** This is the
detail earlier versions of this chapter got wrong. §3 and §6.3 describe it as
"continually taking the **maximum** of the LSNs of the next log record to be
processed for each of the yet-to-be-completely-undone loser transactions":

```
RESTART_UNDO  (paper Fig. 12, condensed)
  WHILE some Trans_Table entry has State = 'U':
      UndoLSN := maximum(UndoNxtLSN) over entries with State = 'U'
      LogRec  := Log_Read(UndoLSN)
      case 'update' and undoable:
          X-latch the page; Undo_Update(Page, LogRec)
          write a 'compensation' record (a CLR) whose UndoNxtLSN := LogRec.PrevLSN
          Page.LSN := LSN(the CLR);  Trans_Table[T].LastLSN := LSN(the CLR)
          Trans_Table[T].UndoNxtLSN := LogRec.PrevLSN
          IF LogRec.PrevLSN = 0: write 'end'; delete the Trans_Table entry
      case 'compensation':
          Trans_Table[T].UndoNxtLSN := LogRec.UndoNxtLSN      -- skip the undone run
```

One sweep, strictly decreasing in LSN, hopping between losers. This matters for
a practical reason: it reads the log backward *once*, sequentially, instead of
once per loser.

Note also what the undo pass does **not** do: there is no page-LSN test. Redo
already put the page in its pre-crash state, so the record's effect is known to
be present. §10.1 shows that a scheme *without* repeating history is forced to
test `page_LSN >= record.LSN` before undoing, and that this test is exactly what
breaks — see Step 7's postscript.

**Nested top actions (§9 — not §10, and not a B-tree split).** Sometimes a
sub-sequence of a transaction's actions must survive the transaction's own
rollback. The paper's worked example (Fig. 14) is **file extension**: once a file
has been extended, other transactions may use the new area, so undoing the
extension "might very well lead to a loss of updates performed by the other
committed transactions." The mechanism is three steps (§9): remember the
transaction's current last-log-record position; log the nested action's records
as ordinary *undo-redo* records; and on completion write a **dummy CLR** whose
`UndoNxtLSN` points back to the remembered position. Rollback then hops straight
over the whole sequence. Crash *before* the dummy CLR, and the incomplete
sequence is undone normally — precisely because its records were undo-redo. "The
dummy CLR in a sense can be thought of as the commit record for the nested top
action", and unlike a real commit the transaction need not wait for it to be
forced. Index and hashing applications are in the companion papers, ARIES/IM and
ARIES/LHS [62, 59] — the B-tree split story lives there, not here.

*Why it matters:* CLRs are the reason ARIES recovery is restartable, and
restartability is the property that separates a recovery *method* from a
recovery *sketch*.

### Step 7 — the three passes, worked by hand

> **In:** an eight-record log, a checkpoint, and a disk whose pages are in a
> specific known state.
> **Out:** every decision all three passes make, with reasons — the only way to
> know you actually understand the rules.

**The log.** LSNs are shown 10 apart for readability; in reality they are byte
offsets (§4.1). The checkpoint's two tables are empty, so everything interesting
is reconstructed by analysis.

| LSN | Txn | Type | Page | PrevLSN | note |
|---:|---|---|---|---:|---|
| 10 | — | `begin_chkpt` | — | — | master record points here |
| 20 | — | `end_chkpt` | — | — | Trans_Table ∅, Dirty_PagLst ∅ |
| 30 | T1 | update | P5 | 0 | T1's first record |
| 40 | T2 | update | P7 | 0 | |
| 50 | T1 | update | P5 | 30 | |
| 60 | T2 | `end` | — | 40 | T2 commits (§5.3) |
| 70 | T3 | update | P9 | 0 | |
| 80 | T1 | update | P7 | 50 | ← crash right after this |

**Disk state at the crash.** The buffer manager wrote P7 out at some point after
LSN 40 was applied to it and before LSN 80 was, so on disk: `P5.LSN = 0`,
`P7.LSN = 40`, `P9.LSN = 0`.

**Pass 1 — analysis**, from LSN 10, applying Fig. 10:

| reading | Trans_Table | Dirty_Pages |
|---|---|---|
| 20 `end_chkpt` | ∅ | ∅ |
| 30 T1/P5 | T1: U, Last 30, UndoNxt 30 | P5 → RecLSN **30** |
| 40 T2/P7 | + T2: U, Last 40, UndoNxt 40 | + P7 → RecLSN **40** |
| 50 T1/P5 | T1: Last 50, UndoNxt 50 | P5 already present — RecLSN stays 30 |
| 60 T2 `end` | **T2 deleted** | — |
| 70 T3/P9 | + T3: U, Last 70, UndoNxt 70 | + P9 → RecLSN **70** |
| 80 T1/P7 | T1: Last 80, UndoNxt 80 | P7 already present — RecLSN stays 40 |

Analysis concludes: **losers = {T1, T3}** (T2 left the table at its `end`
record); **Dirty_Pages = {P5:30, P7:40, P9:70}**; **RedoLSN = min RecLSN = 30**.

**Pass 2 — redo**, from LSN 30, applying Fig. 11's three levels:

| LSN | level (2): in DPT and LSN ≥ RecLSN? | level (3): page fetch | outcome |
|---:|---|---|---|
| 30 P5 | yes, 30 ≥ 30 | `P5.LSN = 0 < 30` | **redo**; `P5.LSN := 30` |
| 40 P7 | yes, 40 ≥ 40 | `P7.LSN = 40`, not < 40 | **skip**; `Dirty_Pages[P7].RecLSN := 41` |
| 50 P5 | yes, 50 ≥ 30 | `P5.LSN = 30 < 50` | **redo**; `P5.LSN := 50` |
| 60 | not update/compensation | — | ignored |
| 70 P9 | yes, 70 ≥ 70 | `P9.LSN = 0 < 70` | **redo — and T3 is a loser** |
| 80 P7 | yes, 80 ≥ **41** | `P7.LSN = 40 < 80` | **redo**; `P7.LSN := 80` |

Two things to see. LSN 70 is redone *even though T3 will be rolled back three
lines from now* — that is repeating history. And LSN 40's `ELSE` branch fixed the
stale RecLSN for P7 from 40 to 41, which is what level (2) then tested LSN 80
against.

**Pass 3 — undo**, applying Fig. 12. Entering with T1 (UndoNxt 80) and T3
(UndoNxt 70):

| step | `max(UndoNxtLSN)` | undo | CLR written | CLR's `UndoNxtLSN` | table after |
|---|---:|---|---|---:|---|
| 1 | **80** (T1) | T1's update to P7 | LSN 90 | 50 | T1 UndoNxt 50, T3 UndoNxt 70 |
| 2 | **70** (T3) | T3's update to P9 | LSN 100 | 0 | T3 done → `end`, deleted |
| 3 | **50** (T1) | T1's update to P5 | LSN 110 | 30 | T1 UndoNxt 30 |
| 4 | **30** (T1) | T1's update to P5 | LSN 120 | 0 | T1 done → `end`, deleted |

The order is **80, 70, 50, 30** — one backward sweep that alternates between T1
and T3, not "finish T1, then start T3." Four undoable records in, four CLRs out,
exactly as §3 promises.

**Now crash again, immediately after CLR 100.** Restart from scratch:

- *Analysis* reads to the new end of log. At CLR 90 the `compensation` case sets
  `T1.UndoNxtLSN := 50` — the CLR's own field, not its PrevLSN. At CLR 100 it
  sets `T3.UndoNxtLSN := 0`, and Fig. 10's trailing loop ("State='U' and
  UndoNxtLSN=0") writes T3's `end` record and drops it. **T3 needs no undo work
  at all on this restart.**
- *Redo* replays CLRs 90 and 100 if their pages are behind — idempotently, by
  the same page-LSN test.
- *Undo* starts at `max(UndoNxtLSN) = 50` and does steps 3 and 4 above.

No update is undone twice, and the CLR count is still four. That is the whole
argument for CLRs, on concrete numbers.

**Postscript: why not just skip the losers in redo?** §10.1's Figures 15 and 16
answer it with three LSNs. A page's disk copy is at LSN 10. Loser T2 updated it
at LSN 20; non-loser T1 updated it at LSN 30. *Selective* redo (System R's
scheme: redo only committed and in-doubt transactions) skips 20, redoes 30, and
leaves the page at LSN 30. Undo then asks its usual question — is
`page_LSN ≥ 20`? — sees 30, and **undoes update 20 even though its effect was
never applied to the page.** The paper's own words: "By not repeating history,
the page_LSN is no longer a true indicator of the current state of the page."
Reversing the pass order doesn't save you either: undoing 20 first writes a CLR
whose LSN exceeds 30, so redo would then skip 30 although it isn't on the page.
Repeating history is not an aesthetic choice; it is what makes the page-LSN mean
something.

*Why it matters:* if you can run this table without looking, you can read §6 of
the paper at speed, and you can review your own recovery code.

## How to read the paper (with the concepts in hand)

1. This chapter's steps (or Franklin's "Crash Recovery" chapter / CMU 15-445
   recovery notes) until the three passes + CLRs feel obvious.
2. **§1.1** for the definitions — page-oriented redo, logical undo, CLR. Ten
   minutes here saves an hour later; the paper uses these terms as primitives
   from §2 onward.
3. **§2** for steal/no-force and the non-obvious argument that no-steal is
   unsatisfiable under fine-granularity locking.
4. **§6** for the passes in detail. Read Fig. 10, 11 and 12 as code, and check
   them against Step 7's table — the three-level redo test (Fig. 11) and the
   `maximum(UndoNxtLSN)` sweep (Fig. 12) are the two places a summary will have
   lied to you. Fig. 13 is the paper's own worked example (all records on one
   page; redo `3 4 4' 3' 5 6`, undo `6 5 2 1`).
5. **§10** — *Recovery Paradigms* — is the catalog of recovery bugs: the System R
   paradigms that break under WAL + fine-granularity locking (selective redo;
   undo before redo; no CLRs; not logging index and space-management changes; no
   LSNs on pages). §10.1's Figures 15 and 16 are the sharpest pages in the paper.
   (Earlier versions of this chapter pointed at §3 for this; §3 is the overview.)
6. Skim **§9** for nested top actions — file extension, dummy CLRs.

## Map to what you've read

- **postgres**: analysis + redo yes (`xlogrecovery.c`, `ApplyWalRecord` at
  `:1883` dispatching to `rm_redo` at `:1966`); undo replaced by MVCC + vacuum,
  so there is no undo pass and no CLR. Full-page images make redo idempotent
  even where LSN discipline alone would not suffice.
- **turso WAL**: no passes at all — recovery is commit-boundary detection over
  whole page images (`reading-turso-wal.md`). A page image is idempotent by
  construction, which is what buys the simplification.
- **redis AOF**: replay from the start of the log, or from a BASE snapshot; no
  LSNs, no undo, and a torn tail is simply truncated
  (`reading-redis-aof-rdb.md`).
- **Your M5 WAL**: if you chose logical records, you owe ARIES-style idempotent
  redo — stamp pages with LSNs and skip when the page is newer. If you chose
  no-steal, you owe no undo pass; say so explicitly, and say what it costs you in
  concurrency (Step 1).

## Questions to answer in notes.md

1. Why must CLRs be redo-only (never undone)? Walk a crash-during-undo using
   Step 7's second crash, and say what would go wrong if the CLR at LSN 90 were
   itself undoable.
2. Nested top action for a file extension (§9): why is letting the extension
   survive an aborted transaction both correct and necessary? Name the concrete
   loss that undoing it would cause.
3. Redo's level (2) test and its `ELSE` branch (Fig. 11) exist purely to avoid
   page fetches. On a 100 GB database with a 10 GB buffer pool and a checkpoint
   every 5 minutes, estimate how many pages levels (1) and (2) save you from
   fetching, and compare that to reading the log itself.
4. Which of steal/no-force does *your* topic-3 B+tree + WAL implement? Derive
   which passes your recovery needs. (Likely no-steal/no-force at first ⇒
   redo-only — say so explicitly, and say which cell of Step 1's matrix you are
   in.)
5. ARIES forces the log through the commit record. Using this topic's ladder,
   state the commit ceiling for a single-threaded committer on each of the three
   rungs, and say what group commit (`reading-postgres-xlog.md`) would change.

## Done when

Answer each before unfolding it.

- [ ] Fill the 2×2 steal/force matrix with (undo?, redo?) from memory, and give
      the paper's non-obvious argument for why steal is not optional.

  <details><summary>Answer</summary>

  no-steal/force: neither pass. no-steal/no-force: redo only. steal/force: undo
  only. steal/no-force: both — ARIES's cell. The non-obvious argument (§2): under
  fine-granularity locking with overlapping transactions, "with a no-steal
  policy, a page may never get written to nonvolatile storage if the page always
  contains uncommitted updates." No-steal can be *unsatisfiable*, not merely
  slow.
  </details>

- [ ] Explain repeating history in two sentences, and give the concrete failure
      that selective redo produces.

  <details><summary>Answer</summary>

  Redo re-applies *every* update from `RedoLSN` forward whose page hasn't seen
  it, losers included, so that the pages are in their exact pre-crash state; undo
  can then be ordinary transaction rollback rather than a special mode reasoning
  about half-restored pages. The failure (§10.1, Fig. 15–16): with a disk page at
  LSN 10, a loser's update at 20 and a non-loser's at 30, selective redo skips 20
  and redoes 30, leaving `page_LSN = 30`; undo's `page_LSN ≥ 20` test then says
  yes and undoes update 20 **although it was never applied to the page**.
  </details>

- [ ] State the redo test in full — all three levels — and say what the `ELSE`
      branch does.

  <details><summary>Answer</summary>

  (1) the record is an `update` or `compensation` and is redoable; (2)
  `PageID IN Dirty_Pages AND LSN >= Dirty_Pages[PageID].RecLSN` — both in-memory,
  and failing either means the page is never fetched; (3) fetch the page and test
  `Page.LSN < LogRec.LSN`; if so redo and set `Page.LSN := LogRec.LSN`. The
  `ELSE` branch sets `Dirty_Pages[PageID].RecLSN := Page.LSN + 1`: the page
  reached disk after the checkpoint, so the table's RecLSN was stale, and fixing
  it lets every later record for that page be filtered at level (2) instead of
  being fetched.
  </details>

- [ ] In what order does the undo pass process a log with two losers? Use Step 7.

  <details><summary>Answer</summary>

  One merged backward sweep, strictly decreasing in LSN, taking
  `maximum(UndoNxtLSN)` across all `State='U'` entries each iteration (§6.3,
  Fig. 12). In Step 7 that is **80 (T1), 70 (T3), 50 (T1), 30 (T1)** — it
  alternates between the transactions. It is *not* "roll back T1 completely, then
  roll back T3"; that would read the log backward once per loser.
  </details>

- [ ] How many CLRs does a recovery write, and does a second crash change the
      answer?

  <details><summary>Answer</summary>

  Exactly one per undoable log record written during forward processing (§3) —
  four in Step 7's log. A second crash does not change it: analysis reads each
  CLR's `UndoNxtLSN` into the transaction table (Fig. 10's `compensation` case),
  so undo resumes at `max(UndoNxtLSN)` and never revisits a compensated record.
  A loser whose `UndoNxtLSN` has reached 0 is finished off by Fig. 10's trailing
  loop with an `end` record and no undo work at all.
  </details>

- [ ] What does a fuzzy checkpoint write, and what does it deliberately *not* do?

  <details><summary>Answer</summary>

  It writes a `begin_chkpt` record, an `end_chkpt` record carrying the
  transaction table, the dirty_pages table and the file mapping, and updates the
  master record to the `begin_chkpt` LSN (§5.4). What it does not do: pause the
  system, or flush pages — "ARIES does not require that any dirty pages be forced
  to nonvolatile storage during a checkpoint." Recovery pays for that later, and
  the bill is bounded by `min(RecLSN)`.
  </details>

## References

**Paper** — Mohan, Haderle, Lindsay, Pirahesh & Schwarz, "ARIES: A Transaction
Recovery Method Supporting Fine-Granularity Locking and Partial Rollbacks Using
Write-Ahead Logging", *ACM Transactions on Database Systems* 17(1), March 1992,
pp. 94–162. DOI `10.1145/128765.128770`.

| section / figure | what this chapter took from it |
|---|---|
| §1.1 | page-oriented redo, logical undo, CLRs as redo-only (Step 2) |
| §2 | steal / no-force, and why no-steal can be unsatisfiable (Step 1) |
| §4.1–§4.4 | LSN, PrevLSN, UndoNxtLSN, page-LSN, the two tables, RecLSN (Steps 2–3) |
| §5.3 | the `end` record as the commit point (Steps 4, 7) |
| §5.4 | fuzzy checkpoints; "no dirty pages need be forced" (Step 3) |
| §6.1, Fig. 10 | the analysis pass, including its trailing cleanup loop (Steps 4, 7) |
| §6.2, Fig. 11 | the three-level redo test and the RecLSN correction (Steps 5, 7) |
| §6.3, Fig. 12 | `maximum(UndoNxtLSN)` — the merged backward sweep (Steps 6, 7) |
| Fig. 13 | the paper's own single-page worked example |
| §9, Fig. 14 | nested top actions; file extension; the dummy CLR (Step 6) |
| §10, §10.1, Figs. 15–16 | the catalog of broken recovery paradigms; why repeat history (Steps 5–7) |
| [36] Haerder & Reuter 1983 | where steal/no-steal/force/no-force come from |
| [59], [62] | ARIES/LHS and ARIES/IM — where the index and hashing applications live |

**Terminology note** — "ATT", "DPT" and "physiological logging" are textbook
shorthands, not the paper's words; it says *transaction table*, *dirty_pages
table*, and *page-oriented redo with logical undo*. Physiological logging is
Gray & Reuter, *Transaction Processing: Concepts and Techniques* (1993).

**Measurements** — `topics/05-durability-wal/notes.md`, "Baseline (provided lane,
Apple M3 Pro / APFS, measured 2026-07-28)", from
`experiments/src/bin/fsync_ladder.rs`; headline in `FINDINGS.md` row 5.

**Secondary reading** — Franklin, "Concurrency Control and Recovery" (in *The
Computer Science and Engineering Handbook*); CMU 15-445 lecture notes on
logging and recovery.
