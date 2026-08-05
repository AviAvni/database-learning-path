# Postgres MVCC: every tuple carries its own visibility

Postgres stores versions IN the table: each heap tuple's header names its
creator and deleter, and visibility is a pure function of (tuple header,
snapshot) — no lock manager consulted on the read path. Before you open
heapam, this chapter builds that machine step by step — the versioned
tuple, the header fields, the snapshot, the visibility function that is
the spec of snapshot isolation, a worked example on real numbers, the write
paths, and the debt collectors — then hands you the file:line anchors to
watch each piece work.

**Every line number in this guide was re-verified against
`postgres/postgres@701f021`** (check with `python3 tools/pinned-source.py
ref postgres`). Postgres's heap files are large — `heapam.c` is 9264 lines,
`heapam_visibility.c` is 1753 — so use
`python3 tools/pinned-source.py grep postgres <pattern> --path <file>` to
jump, not `show`.

## The problem in one sentence

Let hundreds of readers scan a table while writers update it, with no
reader ever taking a lock — postgres's answer is to never overwrite a row
and to make "can I see this version?" a pure function of two integers in
the row's own header against three fields in the reader's snapshot, paying
for it with dead versions that a vacuum process must collect later.

## The concepts, step by step

### Step 1 — versions live in the table itself

> **In:** a table, a stream of UPDATEs, and readers who must not block.
> **Out:** a heap that holds several versions of the same logical row at
> once, and the obligation to decide per version whether a given reader
> should see it.

The vocabulary this guide uses, defined once:

- A **transaction** is a group of reads and writes that must appear to
  happen all-at-once or not at all.
- **MVCC** (multi-version concurrency control) means writers never
  overwrite: each update creates a new **version** of the row.
- A **tuple** is postgres's name for one such version — a physical row
  image plus a header.
- The **heap** is the file of 8 KB pages holding those tuples.
- A **version chain** is the successive versions of one logical row, linked
  through the header.
- **Visibility** is the predicate "should this reader see this version?".
- **Garbage collection** is reclaiming versions no live reader can still
  see; postgres's implementation is called **vacuum**.

In postgres an UPDATE never modifies the row in place: it inserts a
complete new copy of the row elsewhere in the heap and marks the old copy
as superseded. A DELETE just marks. Nothing is physically removed at
delete/update time; old versions sit in the heap next to live ones until a
cleanup pass reclaims them.

Consequence: at any instant the heap contains *several versions of the same
logical row*, and every reader must decide, per version, "is this the one I
should see?" — using only information stored in the version itself plus the
reader's own context. That decision procedure is the rest of this chapter.

Why it matters: everything postgres does differently from an in-memory
engine (Hekaton, previous guide) follows from "the version is a row image
on a page". A page-resident header has no room for a commit timestamp, so
you get xids plus a commit log; a page cannot be half-freed, so you get
vacuum.

### Step 2 — the tuple header: creator, deleter, and a chain pointer

> **In:** a tuple sitting on a page, with nothing else to consult.
> **Out:** three header fields — `t_xmin`, `t_xmax`, `t_ctid` — that encode
> the tuple's whole MVCC life, and the caveat that makes chain-walking
> defensive.

Each tuple carries a header naming who made it and who killed it. The
identifiers are **xids** (transaction ids — a global 32-bit counter handing
out one id per writing transaction, in start order):

```c
// src/include/access/htup_details.h — HeapTupleFields, 124-125
   124  	TransactionId t_xmin;		/* inserting xact ID */
   125  	TransactionId t_xmax;		/* deleting or locking xact ID */

// src/include/access/htup_details.h — HeapTupleHeaderData, 161
   161  	ItemPointerData t_ctid;		/* current TID of this or newer tuple (or a
```

- **`t_xmin`** — the xid of the transaction that *inserted* this version.
- **`t_xmax`** — the xid of the transaction that *deleted, superseded or
  locked* it (note the header comment says "deleting **or locking**" —
  `t_xmax` is not purely a tombstone, which is why Step 5 has to test
  `HEAP_XMAX_IS_LOCKED_ONLY`).
- **`t_ctid`** — a pointer to the *newer* version of the same row, forming
  the version chain through the heap.

So the tuple's whole MVCC life is three fields: born at `t_xmin`, died at
`t_xmax`, successor at `t_ctid`. An UPDATE = insert new version + set old
tuple's `t_xmax` + link `t_ctid`.

The source shouts a caveat about that last field, in the comment at
`htup_details.h:86-111`:

```c
// src/include/access/htup_details.h — the t_ctid comment, 86-111 (elided)
    86  	 * A word about t_ctid: whenever a new tuple is stored on disk, its t_ctid
    88  	 * its t_ctid is changed to point to the replacement version of the tuple.  Or
    93  	 * t_ctid points to itself (in which case, if XMAX is valid, the tuple is
    94  	 * either locked or deleted).  One can follow the chain of t_ctid links
    98  	 * tuple.  Hence, when following a t_ctid link, it is necessary to check
   105  	 * t_ctid is sometimes used to store a speculative insertion token, instead
   111  	 * see a speculative insertion token while following a chain of t_ctid links,
```

Two hazards in one field: the chain can be broken by cleanup, so following
`t_ctid` requires re-checking that the next tuple's `xmin` equals this
tuple's `xmax`; and `t_ctid` is overloaded to carry speculative-insertion
tokens and a "moved to another partition" marker. Chains are walked
defensively.

Why it matters: "the version is self-describing" is only two-thirds true
here. `t_xmin` and `t_xmax` are xids, not timestamps, and an xid does not
say whether its transaction committed. That gap is Step 3.

### Step 3 — hint bits: caching "did that transaction commit?"

> **In:** an xid in a tuple header, and a global commit log that knows its
> fate.
> **Out:** the caching scheme that keeps the commit-log probe off the hot
> path, and the two costs it creates.

An xid alone does not say whether its transaction committed or aborted —
that lives in the **clog** (commit log: a global array, two bits per
transaction). Probing the clog per tuple per read would add a lookup to
every visibility test. So the first reader that pays the probe writes the
answer *back into the tuple header* as **hint bits**:

```c
// src/include/access/htup_details.h — infomask hint bits, 204-208
   204  #define HEAP_XMIN_COMMITTED		0x0100	/* t_xmin committed */
   205  #define HEAP_XMIN_INVALID		0x0200	/* t_xmin invalid/aborted */
   206  #define HEAP_XMIN_FROZEN		(HEAP_XMIN_COMMITTED|HEAP_XMIN_INVALID)
   207  #define HEAP_XMAX_COMMITTED		0x0400	/* t_xmax committed */
   208  #define HEAP_XMAX_INVALID		0x0800	/* t_xmax invalid/aborted */
```

Note line 206: the two "impossible together" bits set at once is the
encoding for **frozen** — a tuple whose xmin is so old it needs no
comparison at all. That matters in Step 9.

Every later reader tests one bit and skips the clog. The costs, both real:

1. **Reads now write.** Setting a hint bit dirties the page, so a pure
   SELECT can generate write I/O — and the comment at
   `heapam_visibility.c:106-108` names the failure it has to guard against:
   "the page must not be undergoing IO at this time (otherwise we e.g. could
   corrupt PG's page checksum or even the filesystem's, as is known to
   happen with btrfs)".
2. **Permission has to be acquired**, and that is not free, so it is
   amortized across a page:

```c
// src/backend/access/heap/heapam_visibility.c — SetHintBitsState, 83-99 (elided)
    83   * To be allowed to set hint bits, SetHintBits() needs to call
    84   * BufferBeginSetHintBits(). However, that's not free, and some callsites call
    85   * SetHintBits() on many tuples in a row. For those it makes sense to amortize
    86   * the cost of BufferBeginSetHintBits(). Additionally it's desirable to defer
    87   * the cost of BufferBeginSetHintBits() until a hint bit needs to actually be
    91  typedef enum SetHintBitsState
    93  	/* not yet checked if hint bits may be set */
    94  	SHB_INITIAL,
    95  	/* failed to get permission to set hint bits, don't check again */
    96  	SHB_DISABLED,
    97  	/* allowed to set hint bits */
    98  	SHB_ENABLED,
    99  } SetHintBitsState;
```

Three states, because "we have not asked yet" and "we asked and were told
no" are different and only the first is worth retrying. Amortize-and-batch,
the same pattern as topic 6's buffer usage counters — and reader-writes-
metadata, the same trick.

Why it matters: hint bits are what make Step 5's visibility function cheap
*in the common case*. Every cost below is stated for the hinted path unless
said otherwise.

### Step 4 — the snapshot: three fields that freeze time

> **In:** a reader that needs a stable definition of "committed already".
> **Out:** `xmin`, `xmax`, `xip[]` — and the cost model of testing an xid
> against them.

A **snapshot** is the reader's definition of "now": a compact description
of exactly which transactions had finished when the snapshot was taken.

```c
// src/include/utils/snapshot.h — SnapshotData, 148-165 (elided)
   148  	 * An MVCC snapshot can never see the effects of XIDs >= xmax. It can see
   149  	 * the effects of all older XIDs except those listed in the snapshot. xmin
   150  	 * is stored as an optimization to avoid needing to search the XID arrays
   153  	TransactionId xmin;			/* all XID < xmin are visible to me */
   154  	TransactionId xmax;			/* all XID >= xmax are invisible to me */
   162  	 * note: all ids in xip[] satisfy xmin <= xip[i] < xmax
   164  	TransactionId *xip;
   165  	uint32		xcnt;			/* # of xact ids in xip[] */
```

- every xid `< xmin` — finished before I started: decided (visible if
  committed);
- every xid `>= xmax` — started after me: **invisible**, unconditionally,
  *even if it has already committed in real time*;
- `xip[]` — the xids in progress at snapshot time: **invisible**, even if
  they commit a microsecond later.

`xcnt` at line 165 is the length of `xip[]`, and it is the cost driver.
Building a snapshot means scanning the shared array of running backends
(`GetSnapshotData`, `procarray.c:2114`) — the scan that was postgres's
multicore scalability wall until the 2020 rework added
`GetSnapshotDataReuse` (`procarray.c:2034`), which reuses the previous
snapshot wholesale when nothing has committed since.

The membership test is `XidInMVCCSnapshot` (`snapmgr.c:1869`), and it is
worth reading exactly, because it is *not* what you would guess:

```c
// src/backend/utils/time/snapmgr.c — XidInMVCCSnapshot, 1879-1924 (elided)
  1879  	/* Any xid < xmin is not in-progress */
  1880  	if (TransactionIdPrecedes(xid, snapshot->xmin))
  1881  		return false;
  1882  	/* Any xid >= xmax is in-progress */
  1883  	if (TransactionIdFollowsOrEquals(xid, snapshot->xmax))
  1884  		return true;
  1899  		if (!snapshot->suboverflowed)
  1901  			/* we have full data, so search subxip */
  1902  			if (pg_lfind32(xid, snapshot->subxip, snapshot->subxcnt))
  1903  				return true;
  1924  		if (pg_lfind32(xid, snapshot->xip, snapshot->xcnt))
  1925  			return true;
```

`pg_lfind32` is a **linear** search, SIMD-accelerated — not a binary
search, even though `xip[]` is sorted:

```c
// src/include/port/pg_lfind.h — pg_lfind32, 158-167 (elided)
   158  	/*
   159  	 * For better instruction-level parallelism, each loop iteration operates
   160  	 * on a block of four registers.
   161  	 */
   163  	const uint32 nelem_per_vector = sizeof(Vector32) / sizeof(uint32);
   164  	const uint32 nelem_per_iteration = 4 * nelem_per_vector;
```

`Vector32` is `__m128i` on x86 and `uint32x4_t` on ARM (`simd.h:30`,
`simd.h:35`) — 128 bits, so `nelem_per_vector` = 4 and
`nelem_per_iteration` = **16 xids per loop iteration**.

Work the cost. With 10 000 concurrent write transactions, `xcnt` = 10 000,
so a worst-case miss costs 10 000 ÷ 16 = **625 iterations**, per tuple, per
visibility test. Both cheap branches fire first (lines 1880 and 1883), so
the scan only runs for xids in the `[xmin, xmax)` window — but a snapshot
taken during a write storm has a wide window. Compare Hekaton, where the
same question is two integer comparisons against fields already in the
cache line ([`reading-inmemory-mvcc.md`](reading-inmemory-mvcc.md) Step 2).
That contrast is question 3.

Why it matters: this is the one place in postgres's read path whose cost
scales with *concurrency* rather than with data size. It is the reason the
2020 `GetSnapshotDataReuse` work existed and the reason long-running
transactions hurt everyone, not just themselves.

### Step 5 — the visibility function: the spec of snapshot isolation

> **In:** Step 2's header fields, Step 3's hint bits, Step 4's snapshot.
> **Out:** `HeapTupleSatisfiesMVCC` — a pure function of (tuple, snapshot)
> and the operational definition of snapshot isolation in postgres.

A version is visible iff its creator is visible to my snapshot AND its
deleter (if any) is not. `HeapTupleSatisfiesMVCC`
(`heapam_visibility.c:939`) is that sentence plus a decade of engineering.
Its skeleton, with the lines that carry the argument:

```c
// src/backend/access/heap/heapam_visibility.c — HeapTupleSatisfiesMVCC, 956-1092 (heavily elided)
   956  	if (!HeapTupleHeaderXminCommitted(tuple))
   958  		if (HeapTupleHeaderXminInvalid(tuple))
   959  			return false;                       // creator aborted
   963  		else if (TransactionIdIsCurrentTransactionId(HeapTupleHeaderGetRawXmin(tuple)))
   965  			if (HeapTupleHeaderGetCmin(tuple) >= snapshot->curcid)
   966  				return false;	/* inserted after scan started */
  1005  		else if (XidInMVCCSnapshot(HeapTupleHeaderGetRawXmin(tuple), snapshot))
  1006  			return false;                       // creator in flight at snapshot time
  1007  		else if (TransactionIdDidCommit(HeapTupleHeaderGetRawXmin(tuple)))
  1008  			SetHintBitsExt(tuple, buffer, HEAP_XMIN_COMMITTED, ...
  1021  		if (!HeapTupleHeaderXminFrozen(tuple) &&
  1022  			XidInMVCCSnapshot(HeapTupleHeaderGetRawXmin(tuple), snapshot))
  1023  			return false;		/* treat as still in progress */
  1026  	/* by here, the inserting transaction has committed */
  1028  	if (tuple->t_infomask & HEAP_XMAX_INVALID)	/* xid invalid or aborted */
  1029  		return true;                            // never deleted
  1031  	if (HEAP_XMAX_IS_LOCKED_ONLY(tuple->t_infomask))
  1032  		return true;                            // xmax is a lock, not a delete
  1071  		if (XidInMVCCSnapshot(HeapTupleHeaderGetRawXmax(tuple), snapshot))
  1072  			return true;                        // deleter in flight ⇒ still alive to me
  1089  		if (XidInMVCCSnapshot(HeapTupleHeaderGetRawXmax(tuple), snapshot))
  1090  			return true;		/* treat as still in progress */
```

Line **1005** is the one that makes this "snapshot" isolation rather than
"read committed": a creator that committed *after* my snapshot was taken is
invisible to me forever, no matter how long I run. Line 1071 is its mirror
for deletes.

Two details the skeleton hides, both important:

- **Line 963 comes before line 1005 for a reason.** Your own xid is never
  stored in your own snapshot — the comment at `snapmgr.c:1862-1866` says
  "GetSnapshotData never stores either top xid or subxids of our own backend
  into a snapshot", so `XidInMVCCSnapshot` would wrongly report your own
  in-flight writes as *not* in progress. The current-transaction check must
  run first. Read-your-own-writes lives at line 965, at **command**
  granularity (`CommandId`), not transaction granularity.
- **Lines 922-936 explain a deliberate non-optimization.** When the
  inserting transaction is still running according to your snapshot,
  postgres does *not* update the hint bits, even if the transaction has in
  fact committed: "Checking the true transaction state would require access
  to high-traffic shared data structures, creating contention we'd rather do
  without, and it would not change the result of our visibility check
  anyway."

The same function, minus the hint-bit engineering:

```rust
// ILLUSTRATION — not quoted from postgres. This is the logical skeleton of
// HeapTupleSatisfiesMVCC at heapam_visibility.c:939 and XidInMVCCSnapshot at
// snapmgr.c:1869, with hint bits, MultiXacts, subtransactions and frozen
// xids removed. Read the real thing; this is only a crib for Step 6.
fn satisfies_mvcc(t: &Tuple, s: &Snapshot) -> bool {
    // "visible xid" = committed AND not still in flight at snapshot time
    let vis = |xid: Xid| committed(xid) && !in_snapshot(xid, s);
    if t.xmin == s.my_xid {
        if t.cmin >= s.cur_cid { return false; } // heapam_visibility.c:965
    } else if !vis(t.xmin) {
        return false;                            // creator invisible to me
    }
    match t.xmax {
        None => true,                            // never deleted (:1028)
        Some(x) if x == s.my_xid => t.cmax >= s.cur_cid,
        Some(x) => !vis(x),                      // deleter invisible ⇒ row lives
    }
}

fn in_snapshot(xid: Xid, s: &Snapshot) -> bool { // committed AFTER my snapshot?
    if xid < s.xmin { return false; }            // snapmgr.c:1880
    if xid >= s.xmax { return true; }            // snapmgr.c:1883
    s.xip.contains(&xid)                         // pg_lfind32: LINEAR SIMD scan
}
```

Note what is absent: no locks, no waiting, no consulting other backends —
a pure function of two arguments. That purity is the entire read-side
scalability story. There is a second visibility function,
`HeapTupleSatisfiesUpdate` (`heapam_visibility.c:511`), used by writers to
find the latest version and report "being updated by someone else"; that is
where waiting and the EvalPlanQual re-check originate. And
`HeapTupleSatisfiesMVCCBatch` (`heapam_visibility.c:1690`) runs the same
predicate over a whole page at once — topic 11 foreshadowing.

Why it matters: this function *is* the isolation level. There is no
separate rulebook; snapshot isolation in postgres is defined by which
branches of these 150 lines return true.

### Step 6 — the visibility function, executed on numbers

> **In:** one concrete snapshot and six concrete tuple headers.
> **Out:** six visibility decisions, each traced to the line of
> `heapam_visibility.c` that produced it — the exercise you should be able
> to do from memory.

Take one reader, backend B, whose transaction was assigned xid **105** and
which is executing its **third** command (`curcid = 2`, counting from 0).
Its snapshot S:

```
 S.xmin   = 100      → every xid <  100 has finished; trust the clog
 S.xmax   = 110      → every xid >= 110 is invisible, unconditionally
 S.xip[]  = [103, 107]  (xcnt = 2)   → in flight when S was taken
 S.curcid = 2
 my xid   = 105      → NOT in xip[]; own xids are never stored (snapmgr.c:1862)
```

Six tuples, and the decision each one gets:

| # | Tuple header | Decision | Line that decides it |
|---|---|---|---|
| 1 | `xmin=95, xmax=103` | **visible** | `:1071` |
| 2 | `xmin=103, xmax=0` | invisible | `:1005` |
| 3 | `xmin=112, xmax=0` | invisible | `:1005` (via `snapmgr.c:1883`) |
| 4 | `xmin=88, xmax=99` | invisible | `:1089` falls through |
| 5 | `xmin=105, cmin=0, xmax=0` | **visible** | `:968` |
| 6 | `xmin=105, cmin=3, xmax=0` | invisible | `:965` |

Traced one at a time:

**Tuple 1 — `xmin=95, xmax=103`, hint bit `HEAP_XMIN_COMMITTED` set.**
Line 956's test fails (xmin *is* hinted committed), so we take the `else`
at :1018. Line 1022: `XidInMVCCSnapshot(95, S)` → `snapmgr.c:1880`,
`95 < 100` → **false**, so we do not return early. Line 1026: the inserter
committed. Line 1028: `xmax = 103` is valid, so not `HEAP_XMAX_INVALID`.
Line 1031: not lock-only. Line 1061: xmax has no `HEAP_XMAX_COMMITTED` hint
(103 is still running). Line 1063: `103 ≠ 105`, not mine. Line 1071:
`XidInMVCCSnapshot(103, S)` → not `< 100`, not `>= 110`, and
`pg_lfind32(103, [103,107], 2)` finds it → **true** → `return true`.
**Visible.** The row was deleted by a transaction that had not committed
when S was taken, so as far as B is concerned it is still alive — and it
will stay alive to B even after 103 commits.

**Tuple 2 — `xmin=103, xmax=0`** (the new version 103 wrote). No hint bit,
so line 956 is entered; line 958 no; line 963 `103 ≠ 105`; line 1005
`XidInMVCCSnapshot(103, S)` → true → **`return false`. Invisible.** Tuples
1 and 2 together are one version chain, and B sees exactly one of them —
the old one. That is snapshot isolation in a single row.

**Tuple 3 — `xmin=112, xmax=0`.** Line 1005 → `snapmgr.c:1883`,
`TransactionIdFollowsOrEquals(112, 110)` → true → in-snapshot → **`return
false`. Invisible.** Note what was *not* consulted: whether 112 committed.
It may have committed long before B ran this query; xid 112 started after
S was taken, so it is invisible regardless.

**Tuple 4 — `xmin=88, xmax=99`, both hinted committed.** Line 1022:
`88 < 100` → not in snapshot → inserter visible. Line 1028: xmax valid.
Line 1061: `HEAP_XMAX_COMMITTED` *is* set, so we take the `else` at :1086.
Line 1089: `XidInMVCCSnapshot(99, S)` → `99 < 100` → false. Falls through
to `return false`. **Invisible** — deleted by a transaction that finished
before B started.

**Tuple 5 — `xmin=105` (mine), `cmin=0`, `xmax` invalid.** Line 956 entered
(own xid, no hint bit). Line 963: `TransactionIdIsCurrentTransactionId(105)`
→ **true**. Line 965: `cmin = 0 >= curcid = 2`? No. Line 968:
`HEAP_XMAX_INVALID` → **`return true`. Visible.** This is
read-your-own-writes: B inserted this in command 0 and reads it in command
2.

**Tuple 6 — `xmin=105` (mine), `cmin=3`.** Same path to line 965:
`cmin = 3 >= curcid = 2` → **`return false`** — "inserted after scan
started". Same transaction, same xid, opposite answer, decided entirely by
`CommandId`. If B's statement were `UPDATE t SET n = n + 1`, this is the
line that stops it looping forever over the rows it is itself producing.

Why it matters: notice how few of the six decisions consulted the clog
(only the unhinted ones), and how many were settled by the two integer
comparisons at `snapmgr.c:1880` and `:1883`. The `xip[]` scan ran exactly
twice out of six. That distribution is why the design works.

### Step 7 — the write paths, and the HOT shortcut

> **In:** the read-side machinery of Steps 2–6.
> **Out:** three write paths that are almost anticlimactic, plus the one
> optimization that changes an update's cost by a factor of *number of
> indexes*.

- **insert** (`heapam.c:2004`) — new tuple: xmin = my xid, xmax = 0.
- **delete** (`heapam.c:2717`) — nothing moves; set xmax, adjust flags. A
  "delete" is a metadata write to an existing tuple.
- **update** (`heapam.c:3201`) — insert + mark + link, per Step 1… with one
  big exception.

The exception is **HOT** (heap-only tuple) updates. The decision is two
conditions:

```c
// src/backend/access/heap/heapam.c — the HOT decision inside heap_update, 3972-3981
  3972  	if (newbuf == buffer)
  3974  		/*
  3975  		 * Since the new tuple is going into the same page, we might be able
  3976  		 * to do a HOT update.  Check if any of the index columns have been
  3977  		 * changed.
  3978  		 */
  3979  		if (!bms_overlap(modified_attrs, hot_attrs))
  3981  			use_hot_update = true;
```

`modified_attrs` comes from `HeapDetermineColumnsInfo` (called at
`heapam.c:3382`, defined at `heapam.c:4360`). If the new version fits on
the *same page* and no HOT-blocking indexed column changed, the index keeps
pointing at the chain head and readers walk `t_ctid` within the page:

```
 HOT chain (one page):        index entry ──► lp 1 (root, HOT_UPDATED)
                                                │ t_ctid
                                              lp 3 (HEAP_ONLY_TUPLE)
                                                │ t_ctid
                                              lp 5 (HEAP_ONLY_TUPLE) ◄ live
 readers walk the chain under the page latch; prune collapses it later.
```

The flags are set at `heapam.c:4029-4036`, and the index work is signalled
to the caller at `heapam.c:4159-4167`:

```c
// src/backend/access/heap/heapam.c — what indexes a HOT update still touches, 4159-4167
  4159  	if (use_hot_update)
  4161  		if (summarized_update)
  4162  			*update_indexes = TU_Summarizing;
  4163  		else
  4164  			*update_indexes = TU_None;
  4166  	else
  4167  		*update_indexes = TU_All;
```

**A correction to a claim this guide used to make**: HOT does *not* always
skip all index updates. `TU_Summarizing` at line 4162 means a HOT update
still has to maintain summarizing indexes (BRIN), because per the comment
at `heapam.c:4154-4157` a summary such as a "minmax bounds of the block may
change with this update". Only `TU_None` skips everything.

Work the arithmetic anyway, because the order of magnitude is the point. A
table with 5 non-summarizing B-tree indexes:

- **non-HOT update** → 1 heap insert + 5 index inserts = **6 writes**.
- **HOT update** → 1 heap insert + 0 index inserts = **1 write**, six times
  less work, and no index bloat to vacuum later.

Why it matters: "UPDATE = INSERT + DELETE" is only half true in postgres,
and which half you get depends on two things a schema designer controls —
whether the updated column is indexed, and whether `fillfactor` leaves room
on the page for the new version.

### Step 8 — the debt collectors: prune and vacuum

> **In:** a heap accumulating one dead tuple per update and per delete.
> **Out:** two collectors with different scopes, and the ordering
> constraint that forces vacuum to be two-phase.

- **Page pruning** — `heap_page_prune_opt` (`pruneheap.c:271`). Any
  *reader* that touches a page with prunable garbage cleans that one page in
  passing: dead versions removed, HOT chains collapsed to a redirect line
  pointer. The fast exit is at `pruneheap.c:293-295` (`PageGetPruneXid`
  returns invalid → return immediately), so the check costs almost nothing
  on clean pages. No vacuum needed for the common case.
- **Vacuum** — `heap_vacuum_rel` (`vacuumlazy.c:624`), scanning via
  `lazy_scan_heap` (`vacuumlazy.c:1279`). The full pass, and it is
  deliberately two-phase:

```c
// src/backend/access/heap/vacuumlazy.c — lazy_vacuum, 2454-2461
  2454  	else if (lazy_vacuum_all_indexes(vacrel))
  2456  		/*
  2457  		 * We successfully completed a round of index vacuuming.  Do related
  2458  		 * heap vacuuming now.
  2459  		 */
  2460  		lazy_vacuum_heap_rel(vacrel);
```

Indexes first (`lazy_vacuum_all_indexes`, `vacuumlazy.c:2494`), *then* the
heap (`lazy_vacuum_heap_rel`, `vacuumlazy.c:2640`). The order is not
stylistic: a heap line pointer that has been marked reusable can be handed
to a brand-new, unrelated row at any moment, so any index entry still
pointing at it would silently return the wrong row. Question 1 makes you
construct that corruption.

There is even a bypass: at `vacuumlazy.c:2436-2437`, if fewer than
`BYPASS_THRESHOLD_PAGES` worth of pages hold dead items *and* the dead-item
store is under 32 MB, vacuum skips index vacuuming entirely — the comment
at `:2387-2392` explains why ("avoids sharp discontinuities in the duration
and overhead of successive VACUUM operations").

Why it matters: pruning is opportunistic and page-local; vacuum is
scheduled and table-global; and the thing that makes vacuum expensive is
not the heap, it is having to walk every index.

### Step 9 — the bill: 32-bit xids and freezing

> **In:** a 32-bit counter that hands out one value per writing
> transaction, forever.
> **Out:** why vacuum is not optional, and the one flag combination that
> takes a tuple out of the comparison game.

Xids are 32-bit, so the counter wraps after about 4.2 billion writing
transactions. Postgres compares xids *modulo* that space
(`TransactionIdPrecedes`, used at `snapmgr.c:1880`), which works only while
every live xid is within half the space of every other. So vacuum has a
second job besides reclaiming space: **freezing** old tuples — setting
`HEAP_XMIN_FROZEN` (`htup_details.h:206`, the two mutually-exclusive bits
set together) to mark an xmin as "older than everything, do not compare".
Step 5's line 1021 is where the frozen check short-circuits the snapshot
test.

Fall too far behind and the database refuses new writes rather than risk
returning wrong answers. The failsafe path is visible in the vacuum code
around `vacuumlazy.c:2468` ("This happens when relfrozenxid or relminmxid
is too far in the past").

Why it matters: this is the operational price of storing versions in the
table with 32-bit ids. Hekaton's 64-bit timestamps do not wrap in any
practical timeframe — one of the clearest cases in this topic where a
representation choice made for the disk became an operational burden.

## Where each step lives in the code

Read `HeapTupleSatisfiesMVCC` in full first — it is the spec of SI — and
the `:86-111` comment in `htup_details.h` before chasing `t_ctid`. ~2.5 h
total. All anchors verified at `postgres/postgres@701f021`.

| Step | File | Lines | What |
|---|---|---|---|
| 2 | `src/include/access/htup_details.h` | 124-125, 161 | `t_xmin`, `t_xmax`, `t_ctid` |
| 2 | `src/include/access/htup_details.h` | 86-111 | the t_ctid chain-walking caveats |
| 3 | `src/include/access/htup_details.h` | 204-208 | hint bit definitions; 206 is FROZEN |
| 3 | `src/backend/access/heap/heapam_visibility.c` | 83-99, 101-130 | `SetHintBitsState`, and why permission is batched |
| 4 | `src/include/utils/snapshot.h` | 138, 153, 154, 164-165 | `SnapshotData`; `xmin`, `xmax`, `xip[]`, `xcnt` |
| 4 | `src/backend/storage/ipc/procarray.c` | 2114, 2034 | `GetSnapshotData`; `GetSnapshotDataReuse` (the 2020 fix) |
| 4 | `src/backend/utils/time/snapmgr.c` | 1869, 1880, 1883, 1924 | `XidInMVCCSnapshot`; the two range tests; the `pg_lfind32` scan |
| 4 | `src/include/port/pg_lfind.h` | 153, 163-164 | `pg_lfind32`; 16 xids per iteration |
| 4 | `src/include/port/simd.h` | 30, 35 | `Vector32` = 128 bits |
| 5, 6 | `src/backend/access/heap/heapam_visibility.c` | 939, 922-936, 956-1092 | `HeapTupleSatisfiesMVCC`; the deliberate hint-bit non-optimization |
| 5 | `src/backend/access/heap/heapam_visibility.c` | 511, 1690 | `HeapTupleSatisfiesUpdate`; `HeapTupleSatisfiesMVCCBatch` |
| 7 | `src/backend/access/heap/heapam.c` | 2004, 2717, 3201 | `heap_insert`, `heap_delete`, `heap_update` |
| 7 | `src/backend/access/heap/heapam.c` | 3382, 4360 | `HeapDetermineColumnsInfo` — call site, then definition |
| 7 | `src/backend/access/heap/heapam.c` | 3233, 3972-3981, 4029-4036, 4159-4167 | `use_hot_update`: declared, decided, flagged, and what indexes it still touches |
| 8 | `src/backend/access/heap/pruneheap.c` | 271, 293-295 | `heap_page_prune_opt` and its fast exit |
| 8 | `src/backend/access/heap/vacuumlazy.c` | 624, 1279, 2369, 2436-2437, 2454-2461, 2494, 2640 | `heap_vacuum_rel`, `lazy_scan_heap`, `lazy_vacuum`, the bypass, indexes-then-heap |
| 9 | `src/backend/access/heap/vacuumlazy.c` | 2468 | the relfrozenxid failsafe |

## Questions for notes.md

1. Why must the index-entry deletion happen BEFORE line pointers are
   recycled (`vacuumlazy.c:2454-2461`)? Construct the corruption if the
   order flipped — name the two rows involved and the query that returns
   the wrong one.
2. Hint bits make reads write. Which topic-6 lesson does that complicate,
   and what does `heapam_visibility.c:106-108` say goes wrong if the rule is
   broken during page I/O?
3. Step 4's arithmetic: 10 000 concurrent writers means `xcnt = 10 000` and
   a worst-case 625 SIMD iterations per `XidInMVCCSnapshot` miss. What does
   Hekaton's timestamp design pay for the same question, and what does it
   give up to get there?
4. Trace tuple 1 of Step 6 again after xid 103 commits and B takes a *new*
   snapshot. Which line's answer flips, and which tuple becomes visible?
5. FalkorDB angle: postgres stores versions IN the table, so old versions
   inflate the heap. For a graph whose "table" is a sparse matrix, where
   would old versions live — and is that closer to append-only (postgres)
   or delta (Oracle/InnoDB) in the Wu/Pavlo taxonomy?

## Takeaway

Postgres's read path is a pure function: three fields in the tuple header
against three fields in the snapshot, with a commit-log probe cached into
the header the first time anyone pays it. Nothing on that path takes a
lock. The bill arrives everywhere else — dead versions that vacuum must
collect index-first, a snapshot whose cost scales with concurrent writers,
and a 32-bit id space that must be frozen before it wraps.

## Connections to this topic's experiment

The exercise in `experiments/src/mvcc.rs` is postgres's read path in
miniature: `Txn::get` (`experiments/src/mvcc.rs:89`) has to answer exactly
the question Step 5 answers, and `snapshot_reads_are_stable` and
`uncommitted_writes_are_invisible` are Step 6's tuples 1 and 2 as tests.

The topic's *measured* lane is a different thing entirely, and worth being
precise about. The provided benchmark measures a single global
`Mutex<HashMap>` — 4 threads × 50 000 transactions × 4 operations — and on
an Apple M3 Pro (measured 2026-07-28, recorded in [`notes.md`](notes.md))
it returns:

| Workload | Keys | mutex txn/s |
|---|---|---|
| read-heavy 95/5 | 10 000 | 623 454 |
| write-heavy 50/50 | 10 000 | 594 264 |
| write-heavy 50/50 | 64 (hot) | 676 691 |

Those three numbers are **flat** — the read/write mix and the key skew move
them by about 12%, which for workloads this different is no signal at all.
The reason is the negative result recorded in
[`FINDINGS.md`](../../FINDINGS.md) row 8: the mutex had already serialized
everything, so the workload's shape could not reach the measurement.

This repo has **not** measured MVCC beating that mutex. The `mvcc txn/s`
and `aborts` columns in `notes.md` are `stub` — filling them is the
exercise. Nothing in this guide is evidence that MVCC is faster here; the
argument for postgres's design is a *scalability* argument (readers never
block writers), and a 4-thread benchmark on a laptop is not where that
argument is settled.

## Done when

Answer each before unfolding it.

- [ ] Given the snapshot `xmin=100, xmax=110, xip=[103,107]`, decide the
      visibility of a tuple with `xmin=95, xmax=103`, and name the line of
      `heapam_visibility.c` that returns the answer.

<details><summary>Answer</summary>

**Visible**, at `heapam_visibility.c:1071`. The inserter 95 is below
`S.xmin`, so `XidInMVCCSnapshot(95, S)` returns false at `snapmgr.c:1880`
and the creator is visible. The deleter 103 is in `xip[]`, so
`XidInMVCCSnapshot(103, S)` returns true at `snapmgr.c:1924`, and line 1071
reads that as "the deleting transaction was still in flight when I took my
snapshot" → `return true`. The row stays visible to this reader even after
103 commits, because the snapshot does not change.

</details>

- [ ] A transaction with xid 105 reads a tuple it inserted itself. Why is
      `TransactionIdIsCurrentTransactionId` checked *before*
      `XidInMVCCSnapshot`, and what decides the answer instead?

<details><summary>Answer</summary>

Because your own xid is never in your own snapshot: "GetSnapshotData never
stores either top xid or subxids of our own backend into a snapshot"
(`snapmgr.c:1862-1866`). If `XidInMVCCSnapshot(105, S)` ran first it would
return false — "not in progress" — and the tuple would be treated as
committed by a stranger. The current-transaction test at
`heapam_visibility.c:963` runs first, and the answer is then decided by
**CommandId** at line 965: `cmin >= snapshot->curcid` → invisible
("inserted after scan started"). So read-your-own-writes has
statement-level granularity, which is what stops `UPDATE t SET n = n + 1`
from looping over its own output.

</details>

- [ ] `XidInMVCCSnapshot` searches a sorted array. Why does it not binary
      search, and what does it cost with 10 000 concurrent writers?

<details><summary>Answer</summary>

It calls `pg_lfind32` (`snapmgr.c:1902` and `:1924`), a **SIMD-accelerated
linear** scan: `pg_lfind.h:163-164` sets `nelem_per_iteration = 4 *
nelem_per_vector`, and `Vector32` is 128 bits (`simd.h:30`, `simd.h:35`), so
each iteration compares **16 xids**. A worst-case miss over `xcnt = 10 000`
therefore costs 10 000 ÷ 16 = **625 iterations** — but with no branch
misprediction and perfectly sequential access, which is why a branchy binary
search over ~13 levels is not obviously better at these sizes. The two range
tests at `:1880` and `:1883` fire first and eliminate most xids before the
scan is reached at all.

</details>

- [ ] Does a HOT update always skip index maintenance? Quote the code.

<details><summary>Answer</summary>

No. `heapam.c:4159-4167` sets `*update_indexes` to `TU_None` only when
`summarized_update` is false; otherwise a HOT update yields
`TU_Summarizing`, because per `heapam.c:4154-4157` "the update may still
need to update summarized indexes, lest we fail to update those summaries
and get incorrect results (for example, minmax bounds of the block may
change with this update)" — BRIN. The saving is still large: with 5
ordinary B-tree indexes, a non-HOT update is 6 writes and a HOT update is 1.

</details>

- [ ] Why does `lazy_vacuum` vacuum indexes before the heap, and what
      breaks if you swap them?

<details><summary>Answer</summary>

`vacuumlazy.c:2454-2461` calls `lazy_vacuum_all_indexes` and only then
`lazy_vacuum_heap_rel`. Heap vacuuming is what marks line pointers reusable,
and a reusable line pointer can be handed to a completely unrelated new row
immediately. If the heap were cleaned first, an index entry still pointing
at that slot would resolve to whatever row now occupies it, and an index
scan would silently return the wrong row — corruption with no error. Index
first, heap second, always.

</details>

- [ ] State this topic's measured result and what it does not show.

<details><summary>Answer</summary>

The provided lane measures a global `Mutex<HashMap>` at 623 454 / 594 264 /
676 691 txn/s for read-heavy 10K-key, write-heavy 10K-key, and write-heavy
64-hot-key workloads (Apple M3 Pro, 2026-07-28; `notes.md`). The result is
**flat across all three**, which is the negative finding in
[`FINDINGS.md`](../../FINDINGS.md) row 8: the mutex had already serialized
everything, so workload shape could not influence throughput. It does
**not** show MVCC outperforming a mutex — those columns are `stub`, and the
case for postgres's design is about readers not blocking writers under
concurrency, which this 4-thread lane does not test.

</details>

## References

**Code** — all anchors verified at
[`postgres/postgres@701f021`](https://github.com/postgres/postgres)

| File | Lines | What |
|---|---|---|
| `src/include/access/htup_details.h` | 86-111, 124-125, 161, 204-208 | tuple header, `t_ctid` caveats, hint bits |
| `src/include/utils/snapshot.h` | 138-165 | `SnapshotData` |
| `src/backend/storage/ipc/procarray.c` | 2034, 2114 | snapshot construction and reuse |
| `src/backend/utils/time/snapmgr.c` | 1862-1866, 1869-1925 | `XidInMVCCSnapshot` |
| `src/include/port/pg_lfind.h` | 89-99, 147-207 | linear and SIMD search helpers |
| `src/include/port/simd.h` | 30, 35 | `Vector32` |
| `src/backend/access/heap/heapam_visibility.c` | 83-130, 511, 917-1092, 1690 | hint-bit state, writer-side check, `HeapTupleSatisfiesMVCC`, batch variant |
| `src/backend/access/heap/heapam.c` | 2004, 2717, 3201-4167, 4360 | insert, delete, update, HOT |
| `src/backend/access/heap/pruneheap.c` | 271-300 | opportunistic pruning |
| `src/backend/access/heap/vacuumlazy.c` | 624, 1279, 2369-2470, 2494, 2640 | vacuum, two-phase ordering, bypass, failsafe |

Read `HeapTupleSatisfiesMVCC` in full first — it is the spec of SI — and
the `:86-111` comment in `htup_details.h` before chasing `t_ctid`. ~2.5 h.

**In this repo**

| Where | What |
|---|---|
| [`notes.md`](notes.md) | the measured mutex baseline and the `stub` MVCC columns |
| [`FINDINGS.md`](../../FINDINGS.md) row 8 | flat ~600k txn/s, because the mutex already serialized everything |
| `experiments/src/mvcc.rs:89` | `Txn::get` — where Step 5's predicate goes |
| [`reading-inmemory-mvcc.md`](reading-inmemory-mvcc.md) | the same problem with the disk deleted |
| [`reading-ssi-postgres.md`](reading-ssi-postgres.md) | what postgres adds on top of this to reach SERIALIZABLE |
