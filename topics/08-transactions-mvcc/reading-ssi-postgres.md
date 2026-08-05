# SSI: serializable snapshot isolation without blocking anyone

How postgres turned snapshot isolation into a real `SERIALIZABLE` level using
passive markers instead of blocking locks — Ports & Grittner's VLDB '12 account
of productionizing the dangerous-structure theorem. Before the paper, this
chapter builds the theory one edge at a time: the hole in SI, the
rw-antidependency, the theorem that reduces every anomaly to one shape, and the
engineering that made detecting that shape cheap enough to ship. Prereq: the
Berenson critique ([reading-ansi-critique.md](reading-ansi-critique.md)) — you
need write skew cold — and the tuple headers from
[reading-postgres-heapam.md](reading-postgres-heapam.md), because half of SSI's
conflict detection is just re-reading `xmin`/`xmax`.

**Two attributions the literature routinely garbles, and this guide gets right
from the start.** The *theorem* — every serialization anomaly contains two
adjacent rw-antidependencies — is **Fekete et al.**, cited as [10] and stated as
Theorem 1 in §3.2. The *algorithm* that exploits it, SSI, is **Cahill, Röhm and
Fekete** (SIGMOD 2008), cited as [7] in §3.3. The *commit-ordering refinement* is
from **Cahill's thesis** [6], §3.3.1. Calling the whole bundle "Cahill's theorem"
(as the previous version of this guide did) collapses three separate results.

Line numbers below are `postgres/postgres@701f021`, checked with
`python3 tools/pinned-source.py`. Section numbers are from the arXiv version of
the paper (arXiv:1208.4179).

## The problem in one sentence

Snapshot isolation permits write skew, and the classical fix — strict two-phase
locking — makes readers block writers again, throwing away everything MVCC
bought; SSI gets full serializability by *watching* for one specific conflict
shape and aborting somebody, blocking no one, ever, and the paper's own
measurements put the bill at **5% throughput on a CPU-bound TPC-C variant**
(§8.2, 25 warehouses in tmpfs), **10–20% CPU on the SIBENCH microbenchmark**
(§8.1), and **nothing measurable at all when the workload is disk-bound** (§8.2,
150 warehouses).

> The previous version of this guide claimed "~7% overhead". That number is not
> in the paper. It has been replaced above by the three figures the paper
> actually reports, each with its section.

## The concepts, step by step

### Step 1 — the vocabulary, nailed down before anything moves

> **In:** the words this paper uses without defining, because its audience was
> the 2012 VLDB program committee. **Out:** a definition for each, so that
> Step 2 onward can be read literally.

- **Transaction** — a group of reads and writes the database promises to treat
  as one unit: all of it takes effect, or none of it.
- **Snapshot** — a frozen view of the database as of one instant. Under
  snapshot isolation every read a transaction makes is answered from its
  snapshot, so the same query twice returns the same answer even if the world
  moved on. §2.1: "as though the transaction operates on a private snapshot of
  the database taken before its first read."
- **MVCC** (multi-version concurrency control) — the mechanism that makes
  snapshots possible: writing a row leaves the old copy in place and adds a new
  one, so different transactions can be shown different versions. Postgres
  replaced its original lock-based storage manager with MVCC in 1999 (§3).
- **Tuple** — postgres's word for one row *version*. See
  [reading-postgres-heapam.md](reading-postgres-heapam.md).
- **`xmin` / `xmax`** — the transaction ids stamped into a tuple header saying
  which transaction created it and which deleted it. A visibility check is a
  function of these two plus your snapshot.
- **Serializable** — the execution produces the same result as *some* serial
  order of the same transactions, one after another with no overlap. This is
  the property applications actually want, because it is the one that lets you
  reason about a transaction on its own.
- **Write skew** — two concurrent transactions each read data the other is
  about to change, then write *disjoint* rows. No write-write conflict fires,
  both commit, and an invariant spanning both rows is broken. Berenson's
  A5B; see [reading-ansi-critique.md](reading-ansi-critique.md).
- **rw-antidependency** (the paper also says *rw-conflict*) — §3.1: "if T1
  writes a version of some object, and T2 reads the previous version of that
  object, then T1 appears to have executed *after* T2." Written `T2 --rw--> T1`,
  the arrow pointing the way the serial order would have to run.
- **Dangerous structure** — two rw-antidependencies end to end:
  `Tin --rw--> Tpivot --rw--> Tout`. The middle transaction is the **pivot**.
- **SSI** — serializable snapshot isolation: run under SI, watch for dangerous
  structures, abort somebody when one appears.
- **SIREAD lock** — the marker recording "this transaction read this object".
  §3.3: these locks "do not block conflicting writes (thus, 'lock' is somewhat
  of a misnomer)."
- **Garbage collection / vacuum** — in postgres, the background reclamation of
  dead tuples. SSI has its own parallel problem (Step 12): reclaiming the
  *tracking state*, which is not the same thing and has its own rules.

Why it matters: two of these — pivot and rw-antidependency — carry the entire
argument, and both are directional. Getting an arrow backwards silently inverts
which transaction the system decides to kill.

### Step 2 — the hole, worked on one concrete schedule

> **In:** a `doctors` table with two rows and the invariant "at least one doctor
> is on call". **Out:** an interleaving where both transactions commit and the
> invariant is false, with the exact reads and writes named.

This is the paper's Figure 1 (§2.1.1), which is itself based on Cahill et
al. [7]. Both transactions run the same procedure: count the doctors currently
on call; if that count is at least 2, take yourself off call.

Initial state, and the two transactions' snapshots:

```
doctors                  T1 (xid 100)                 T2 (xid 101)
  Alice  on_call = true    begin, snapshot S1           begin, snapshot S2
  Bob    on_call = true    S1 sees Alice=t, Bob=t       S2 sees Alice=t, Bob=t

  t0  T1: x <- SELECT count(*) FROM doctors WHERE on_call    -> x = 2
  t1  T2: x <- SELECT count(*) FROM doctors WHERE on_call    -> x = 2
  t2  T1: 2 >= 2, so UPDATE doctors SET on_call=f WHERE name='Alice'
  t3  T2: 2 >= 2, so UPDATE doctors SET on_call=f WHERE name='Bob'
  t4  T1: COMMIT                                             -> ok
  t5  T2: COMMIT                                             -> ok under plain SI

final state: Alice on_call = false, Bob on_call = false.  Nobody is on call.
```

Run either transaction alone and the invariant holds. Run them in either serial
order and it holds: the second one counts 1, fails the `>= 2` test and writes
nothing. Only the interleaving breaks it.

Why SI's own defences all miss it (§2.1.1): postgres's write locks are
*tuple*-level, and the two transactions update **different tuples** — Alice's
row and Bob's row — so nothing collides. First-committer-wins compares write
sets; these write sets are disjoint. The damage lives entirely in the
*read → write* crossings, which SI does not look at.

The paper's own contrast, worth memorising because it is the whole design
space in two sentences (§2.1.1): "in two-phase locking DBMS, each transaction
would take read locks that would conflict with the other's write. Similarly, in
an optimistic-concurrency system, the second transaction would fail to commit
because its read set is no longer up to date."

Why it matters: those two alternatives are exactly the two things this repo's
`experiments/src/mvcc.rs` makes you build (`Mode::Snapshot` reproduces the bug,
`Mode::Serializable` validates the read set). SSI is a *third* answer, and Step
5 is where it diverges from both.

### Step 3 — the serialization graph and its three kinds of edge

> **In:** an execution history. **Out:** a directed graph whose cycles are
> exactly the non-serializable executions.

From Adya et al. [2], via §3.1. One node per transaction; an edge `T1 -> T2`
means T1 must precede T2 in any serial order equivalent to what happened.
Three ways to earn one:

| edge | when | what it implies about time |
|---|---|---|
| **wr-dependency** | T1 writes a version, T2 *reads that version* | T1 committed **before** T2's snapshot (§3.2) |
| **ww-dependency** | T1 writes a version, T2 replaces it | T1 committed before T2 wrote — enforced by write locking (§3.2) |
| **rw-antidependency** | T1 writes a version, T2 read the version **before** it | T1 appears to run *after* T2 — and the two **overlapped** (§3.2) |

The last row is the load-bearing one and its asymmetry is the point. wr and ww
edges can only run from a *finished* transaction to a later one. An rw edge is
the only kind that can exist between two transactions that were **concurrent** —
"one must start while the other was active" (§3.2). Note also, from §3.1, that
"objects" is deliberately more abstract than "tuple": if T1 scans for all rows
with `x = 1` and T2 then inserts a matching row, that is a `T1 --rw--> T2` edge
even though no existing row was touched. Phantoms are rw edges too.

Applying this to Step 2 (the paper does it for you, §3.1, Figure 3a): T1's
update of Alice is invisible to T2's `SELECT`, so T2 appears to run first —
`T2 --rw--> T1`. T2's update of Bob is invisible to T1's `SELECT`, so
`T1 --rw--> T2`. That is a cycle of length two, and a cycle means the execution
matches no serial order at all:

```
                    rw
          T1  ------------->  T2
           ^                  |
           |        rw        |
           +------------------+

  T1 --rw--> T2 : T1 read Bob (old version), T2 wrote Bob
  T2 --rw--> T1 : T2 read Alice (old version), T1 wrote Alice
```

Why it matters: cycle detection is the textbook-correct answer and it is
expensive — you must materialise the graph and search it. Step 4 is the result
that lets you skip that.

### Step 4 — Theorem 1: every anomaly contains the same little shape

> **In:** the observation that anomalies are cycles. **Out:** a *local* test —
> two adjacent edges — that no anomaly can evade.

§3.2 quotes it verbatim:

> **Theorem 1 (Fekete et al. [10]).** Every cycle in the serialization history
> graph contains a sequence of edges `T1 --rw--> T2 --rw--> T3` where each edge
> is a rw-antidependency. Furthermore, T3 must be the first transaction in the
> cycle to commit.

Read the two halves separately, because the engineering treats them separately.

1. **Two adjacent rw edges.** Adya [1] had already shown every cycle contains at
   least two rw edges; Fekete et al. sharpened that to *adjacent*. Adjacency is
   what makes the test local: you never look further than one transaction's own
   two edge lists.
2. **T3 commits first — of the entire cycle.** §3.2 flags this as its own
   contribution to the reading: "this is actually a stronger statement than that
   given by Fekete et al., who state only that T3 must commit before T1 and T2.
   Though not explicitly stated, it is a consequence of their proof that T3 must
   be the first transaction in the entire cycle to commit." The strengthening is
   not pedantry — it is what licenses the commit-ordering optimisation in Step 5
   and the "don't abort anything until T3 commits" rule in Step 10.

And **Corollary 2**: T1 is concurrent with T2, and T2 is concurrent with T3,
because rw edges only occur between concurrent transactions. Plus the footnote
that matters for Step 2: "T1 and T3 may refer to the same transaction, for
cycles of length 2 such as the one in the write-skew example (Figure 3a)."

Check Step 2 against it. The cycle is `T1 --rw--> T2 --rw--> T1`. Take
Tin = T1, Tpivot = T2, Tout = T1 (the same transaction, per the corollary). T1
committed at t4 and T2 at t5, so Tout did commit first. The theorem holds, and
it says the anomaly is detectable by noticing that **T2 has an rw edge in and an
rw edge out**.

Why it matters: the pivot is a property of one node. That is the difference
between an O(graph) search and two list walks.

### Step 5 — SSI: check for the structure, not for the cycle

> **In:** Theorem 1. **Out:** an algorithm, its false-positive rate, and the
> variant postgres declined to build.

§3.3: SSI "checks for a 'dangerous structure' of two adjacent
rw-antidependency edges. If any transaction has both an incoming
rw-antidependency and an outgoing one, SSI aborts one of the transactions
involved."

Three consequences the paper draws explicitly, all in §3.3:

- **It is sound.** Theorem 1 guarantees no cycle can form without a dangerous
  structure appearing first, so aborting on the structure cannot miss an
  anomaly.
- **It is not complete.** "It may have false positives because not every
  dangerous structure is part of a cycle." Some aborted executions were
  perfectly serializable.
- **It only needs rw edges.** "Dangerous structures are composed entirely of
  rw-antidependencies, so SSI does not need to track wr- and ww-dependency
  edges." That halves the bookkeeping and, per §5.3, is also why the summarised
  representation in Step 12 can be so small.

The paper then makes a claim that is easy to skim past and is the best argument
in the whole section — SSI is *more* permissive than either classical school:

> "Essentially, both S2PL and classic OCC prevent concurrent transactions from
> having rw-conflicts. SSI allows some rw-conflicts as long as they do not form
> a dangerous structure, a less restrictive requirement." (§3.3)

with a worked instance: take the paper's three-transaction batch-processing
anomaly (§2.1.2, Figure 2) and delete the read-only `REPORT` transaction. What
remains is serializable in the order ⟨T2, T3⟩ despite containing a single rw
edge `T2 --rw--> T3`. "Neither S2PL nor OCC would permit this execution, whereas
SSI would allow it, because it contains only a single rw-antidependency."

Two refinements, §3.3.1:

- **Commit ordering** (Cahill's thesis [6]): since Theorem 1 requires T3 to
  commit first, a dangerous structure where T1 or T2 committed before T3 is a
  false positive and can be ignored. Postgres "use[s] an extension of this
  optimization." It does not remove all false positives — there may simply be no
  path `T3 ⇝ T1` closing the cycle.
- **PSSI** [18] removes all false positives by building the full graph and
  testing for cycles, cutting the abort rate "by up to 40%" on a microbenchmark
  built to stress false aborts. Postgres rejected it: it needs wr- and
  ww-dependencies too, which costs memory, and §6's optimisations "would not be
  compatible with PSSI." The clinching argument is measured, not aesthetic —
  "the workloads we evaluate in Section 8 have a serialization failure rate well
  under 1%, suggesting additional precision has a limited benefit."

Why it matters: this is the paper's method in miniature. Every place it takes
a *conservative* shortcut, it argues the cost is more aborts and never a wrong
answer, and then measures how many more aborts. Correctness is one-directional;
performance is negotiable.

### Step 6 — detecting an rw edge when the write happened first: no locks needed

> **In:** a serializable transaction reading a heap tuple. **Out:** the branch
> of postgres that derives an rw edge straight from `xmin`/`xmax`, with no read
> tracking at all.

§5.2 splits detection in two by chronology — "which one is needed depends on
whether the write happens chronologically before the read, or vice versa" — and
the write-first half is free, because the MVCC data already records it:

> "If the tuple is not visible because the transaction that created it had not
> committed when the reader took its snapshot, that indicates a rw-conflict: the
> reader must appear before the writer in the serial order." (§5.2)

That paragraph is this function:

```c
// postgres/src/backend/access/heap/heapam.c — HeapCheckForSerializableConflictOut, 9182-9228 (elided)
  9182  void
  9183  HeapCheckForSerializableConflictOut(bool visible, Relation relation,
  9184                                      HeapTuple tuple, Buffer buffer,
  9185                                      Snapshot snapshot)
  9186  {
  9187      TransactionId xid;
  9188      HTSV_Result htsvResult;
  9189
  9190      if (!CheckForSerializableConflictOutNeeded(relation, snapshot))
  9191          return;
  ...
  9204      htsvResult = HeapTupleSatisfiesVacuum(tuple, TransactionXmin, buffer);
  9205      switch (htsvResult)
  9206      {
  9207          case HEAPTUPLE_LIVE:
  9208              if (visible)
  9209                  return;
  9210              xid = HeapTupleHeaderGetXmin(tuple->t_data);
  9211              break;
  9212          case HEAPTUPLE_RECENTLY_DEAD:
  9213          case HEAPTUPLE_DELETE_IN_PROGRESS:
  9214              if (visible)
  9215                  xid = HeapTupleHeaderGetUpdateXid(tuple->t_data);
  9216              else
  9217                  xid = HeapTupleHeaderGetXmin(tuple->t_data);
  ...
  9226          case HEAPTUPLE_INSERT_IN_PROGRESS:
  9227              xid = HeapTupleHeaderGetXmin(tuple->t_data);
  9228              break;
```

Line **9208** is the whole idea. `visible` is the answer the ordinary visibility
check already produced; if the tuple is live *and* visible to you, there is no
edge and the function returns. Every other arm picks out the xid of the
transaction whose work you could not see — `xmin` when the tuple was created by
someone you cannot see (9210, 9217, 9227), the *updater's* xid when the tuple is
visible to you but somebody has since deleted it (9215) — and hands it to
`CheckForSerializableConflictOut` (`heapam.c:9263`), which lives in
`predicate.c:3952` and decides whether that xid belongs to a concurrent
serializable transaction:

```c
// postgres/src/backend/storage/lmgr/predicate.c — XidIsConcurrent, 3900-3917
  3900  static bool
  3901  XidIsConcurrent(TransactionId xid)
  3902  {
  3903      Snapshot    snap;
  3904
  3905      Assert(TransactionIdIsValid(xid));
  3906      Assert(!TransactionIdEquals(xid, GetTopTransactionIdIfAny()));
  3907
  3908      snap = GetTransactionSnapshot();
  3909
  3910      if (TransactionIdPrecedes(xid, snap->xmin))
  3911          return false;
  3912
  3913      if (TransactionIdFollowsOrEquals(xid, snap->xmax))
  3914          return true;
  3915
  3916      return pg_lfind32(xid, snap->xip, snap->xcnt);
  3917  }
```

That is literally the snapshot test from
[reading-postgres-heapam.md](reading-postgres-heapam.md) — below `xmin` means
finished before you started, at or above `xmax` means started after you, and in
between the answer is a scan of the in-progress array `xip[]`. Same three lines,
different question: not "can I see it?" but "did we overlap?"

Why it matters: half of SSI's conflict detection costs nothing, because postgres
was already computing the answer for visibility. The expensive half is Step 7.

### Step 7 — detecting an rw edge when the read happened first: the SIREAD lock manager

> **In:** the other chronology — you read a row, and only *later* does somebody
> write it. **Out:** postgres's predicate lock manager, its promotion rule, and
> the arithmetic of when a tuple marker becomes a table-wide one.

Nothing in the MVCC data records that you read something, so this direction
needs the marker. §5.2: postgres could not reuse any existing lock mechanism —
it "did not previously acquire read locks on data accessed in any isolation
level," and its write locks live in *tuple headers on disk*, not in a lock
table, so there is nothing to match against. Hence a new manager that "stores
only SIREAD locks. It does not support any other lock modes, and hence cannot
block."

Reads acquire markers on what they touched (`PredicateLockTID`,
`predicate.c:2550`, called from `heapam.c:1750`; `PredicateLockRelation`, `:2505`;
`PredicateLockPage`, `:2528`). Writers check for them. `heap_update` does it at
`heapam.c:3963`, `heap_delete` at `:2959`, and the insert paths at `:2054`,
`:2345`, `:2628`:

```c
// postgres/src/backend/storage/lmgr/predicate.c — CheckForSerializableConflictIn, 4265-4317 (elided)
  4265  CheckForSerializableConflictIn(Relation relation, const ItemPointerData *tid, BlockNumber blkno)
  4266  {
  4267      PREDICATELOCKTARGETTAG targettag;
  ...
  4286      /*
  4287       * It is important that we check for locks from the finest granularity to
  4288       * the coarsest granularity, so that granularity promotion doesn't cause
  4289       * us to miss a lock.  The new (coarser) lock will be acquired before the
  4290       * old (finer) locks are released.
  ...
  4295      if (tid != NULL)
  4296      {
  4297          SET_PREDICATELOCKTARGETTAG_TUPLE(targettag, ...);
  4302          CheckTargetForConflictsIn(&targettag);
  4303      }
  4304
  4305      if (blkno != InvalidBlockNumber)
  4306      {
  4307          SET_PREDICATELOCKTARGETTAG_PAGE(targettag, ...);
  4311          CheckTargetForConflictsIn(&targettag);
  4312      }
  4313
  4314      SET_PREDICATELOCKTARGETTAG_RELATION(targettag, ...);
  4317      CheckTargetForConflictsIn(&targettag);
  4318  }
```

**A divergence between the paper and the code, worth noticing.** §5.2.1 says
these checks "must be done in the proper order: coarsest to finest." The comment
at `predicate.c:4287-4290` says the opposite — "from the finest granularity to
the coarsest" — and the code at `:4295`, `:4305`, `:4314` runs tuple, then page,
then relation. The code at `701f021` is what ships; take its ordering as
authoritative and the paper's sentence as thirteen years stale (or a typo).
Either way the *reason* is the same in both: promotion must never open a window
in which a marker is invisible to a checker.

Three engineering moves make the memory bill payable, and only the first two are
usually quoted:

1. **Index-range locks for predicate reads** (§5.2.1). Real predicate locking
   [9] is not used; instead "index access methods acquire SIREAD locks on the
   'gaps' to detect phantoms. Currently, locks on B+-tree indexes are acquired
   at page granularity; we intend to refine this to next-key locking [16] in a
   future release." An insert into a range you scanned lands on a page you
   marked, and the edge appears.
2. **Granularity promotion.** Markers exist at tuple, page and relation level,
   and many fine ones collapse into one coarse one under pressure.
3. **No intention locks** (§5.2.1). "One simplification we were able to make is
   that intention locks were not necessary, despite the use of multigranularity
   locking (and contrary to a suggestion that intention-SIREAD locks would be
   required [7])." Since SIREAD locks cannot block, deadlock detection is
   unnecessary too, and the acquisition calls need not be placed away from held
   buffer latches.

**Promotion, worked on the real defaults.** The rule:

```c
// postgres/src/backend/storage/lmgr/predicate.c — MaxPredicateChildLocks and the promotion test, 2217-2229 + 2284-2295 (elided)
  2217  static int
  2218  MaxPredicateChildLocks(const PREDICATELOCKTARGETTAG *tag)
  2219  {
  2220      switch (GET_PREDICATELOCKTARGETTAG_TYPE(*tag))
  2221      {
  2222          case PREDLOCKTAG_RELATION:
  2223              return max_predicate_locks_per_relation < 0
  2224                  ? (max_predicate_locks_per_xact
  2225                     / (-max_predicate_locks_per_relation)) - 1
  2226                  : max_predicate_locks_per_relation;
  2227
  2228          case PREDLOCKTAG_PAGE:
  2229              return max_predicate_locks_per_page;
  ...
  2284          if (parentlock->childLocks >
  2285              MaxPredicateChildLocks(&targettag))
  2286          {
  ...
  2293              promotiontag = targettag;
  2294              promote = true;
  2295          }
```

The defaults are in `postgresql.conf.sample`: `max_pred_locks_per_transaction =
64` (`:877`), `max_pred_locks_per_relation = -2` (`:879`),
`max_pred_locks_per_page = 2` (`:882`). Substituting:

```
page threshold      = max_pred_locks_per_page                        = 2
relation threshold  = 64 / 2 - 1                                     = 31        (negative form, :2223-2226)

so, reading rows one at a time in a serializable transaction:

  tuple marker #1 in page 7   childLocks(page 7) = 1   1 > 2 ? no
  tuple marker #2 in page 7   childLocks(page 7) = 2   2 > 2 ? no
  tuple marker #3 in page 7   childLocks(page 7) = 3   3 > 2 ? YES -> one page marker replaces three tuple markers

  ... and the relation's child count counts BOTH tuples and pages (comment, :2203-2204):

  descendant marker #31 in table t   31 > 31 ? no
  descendant marker #32 in table t   32 > 31 ? YES -> one relation marker replaces all of them
```

So a serializable scan that touches 32 rows spread thinly across a table ends up
holding a single **relation**-level SIREAD lock. From that moment, *every* write
by *any* concurrent transaction to that table generates an rw edge against you,
whether or not it touched a row you read. That is the trade in one image: coarser
markers ⇒ false edges ⇒ false aborts ⇒ never a wrong answer.

Why 32 and not 32 000? Because the whole lock table is sized once at startup:
`NPREDICATELOCKTARGETENTS()` is `max_predicate_locks_per_xact ×
(MaxBackends + max_prepared_xacts)` (`predicate.c:263-264`), and §6 explains the
constraint behind it — postgres puts all shared memory in one System V segment,
whose default OS limit the paper gives as 32 MB on Linux. Promotion is not an
optimisation here; it is the thing that stops the table filling up.

Why it matters: this is the only part of SSI that is *pure* cost. Everything in
Step 6 was already being computed; the marker table is new memory, new
contention on lightweight locks, and the source of the 10–20% in §8.1.

### Step 8 — remembering the edges: lists, not bits

> **In:** an rw edge, just detected by Step 6 or Step 7. **Out:** where postgres
> puts it, and why it stores more than the original algorithm did.

§5.3 lays out the design space as a spectrum of how much you remember:

| implementation | per-transaction state | source |
|---|---|---|
| original SSI [7] | two single bits: has-in-edge, has-out-edge | §5.3 |
| Cahill's thesis [6] | two *pointers*, self-pointer meaning "more than one" | §5.3 |
| PSSI [18] | the entire graph, including wr and ww edges | §5.3 |
| **PostgreSQL** | **a list of all rw edges in, and all rw edges out** | §5.3 |

Postgres's reason is not generosity: "keeping pointers to the other transaction
involved in the rw-antidependency, rather than a simple flag, is necessary to
implement the commit ordering optimization described in Section 3.3 and the
read-only optimization of Section 4.1" (§5.3). You cannot ask "did T3 commit
first?" of a bit.

The struct:

```c
// postgres/src/include/storage/predicate_internals.h — SERIALIZABLEXACT, 78-119 (elided)
    78      SerCommitSeqNo commitSeqNo;
    79
    80      /* these values are not both interesting at the same time */
    81      union
    82      {
    83          SerCommitSeqNo earliestOutConflictCommit;   /* when committed with
    84                                                       * conflict out */
    85          SerCommitSeqNo lastCommitBeforeSnapshot;    /* when not committed or
    86                                                       * no conflict out */
    87      }           SeqNo;
    88      dlist_head  outConflicts;   /* list of write transactions whose data we
    89                                   * couldn't read. */
    90      dlist_head  inConflicts;    /* list of read transactions which couldn't
    91                                   * see our write. */
   ...
   108      dlist_head  possibleUnsafeConflicts;
   ...
   115      TransactionId xmin;         /* the transaction's snapshot xmin */
   116      uint32      flags;          /* OR'd combination of values defined below */
```

Read the comments on 88-91 as the definition of the arrow's direction, because
they are the least ambiguous statement of it anywhere: **out** = "write
transactions whose data we couldn't read"; **in** = "read transactions which
couldn't see our write". `earliestOutConflictCommit` at :83 is §6.1's trick —
the one extra number that lets a committed transaction be cleaned up without
losing the ability to answer "does this one have an out-conflict, and when did
it commit?"

Recording an edge is one function:

```c
// postgres/src/backend/storage/lmgr/predicate.c — SetRWConflict, 656-677 (elided)
   656  static void
   657  SetRWConflict(SERIALIZABLEXACT *reader, SERIALIZABLEXACT *writer)
   658  {
   659      RWConflict  conflict;
   660
   661      Assert(reader != writer);
   662      Assert(!RWConflictExists(reader, writer));
   663
   664      if (dlist_is_empty(&RWConflictPool->availableList))
   665          ereport(ERROR,
   666                  (errcode(ERRCODE_OUT_OF_MEMORY),
   ...
   673      conflict->sxactOut = reader;
   674      conflict->sxactIn = writer;
   675      dlist_push_tail(&reader->outConflicts, &conflict->outLink);
   676      dlist_push_tail(&writer->inConflicts, &conflict->inLink);
   677  }
```

Lines 673-676: the reader gets the out-edge, the writer gets the in-edge, and
the same `RWConflictData` node is threaded onto both lists. Note :664-668 — the
pool is fixed-size and exhausting it is an *error*, not a fallback. That is the
memory pressure Step 12 exists to relieve.

Why it matters: `inConflicts` and `outConflicts` are the two lists the pivot
test walks. Every later step is a traversal of these.

### Step 9 — the pivot test, in the three shapes it can arrive in

> **In:** an about-to-be-recorded edge `reader --rw--> writer`. **Out:** the
> decision "does adding this edge complete a dangerous structure?", and the
> three distinct cases the code checks.

Every edge goes through `FlagRWConflict` (`predicate.c:4430`), and its **first**
act is the check — before the edge is even recorded:

```c
// postgres/src/backend/storage/lmgr/predicate.c — FlagRWConflict, 4429-4444
  4429  static void
  4430  FlagRWConflict(SERIALIZABLEXACT *reader, SERIALIZABLEXACT *writer)
  4431  {
  4432      Assert(reader != writer);
  4433
  4434      /* First, see if this conflict causes failure. */
  4435      OnConflict_CheckForSerializationFailure(reader, writer);
  4436
  4437      /* Actually do the conflict flagging. */
  4438      if (reader == OldCommittedSxact)
  4439          writer->flags |= SXACT_FLAG_SUMMARY_CONFLICT_IN;
  4440      else if (writer == OldCommittedSxact)
  4441          reader->flags |= SXACT_FLAG_SUMMARY_CONFLICT_OUT;
  4442      else
  4443          SetRWConflict(reader, writer);
  4444  }
```

Lines 4438-4441 are Step 12's summarisation showing through: if one end has
already been collapsed into the shared `OldCommittedSxact` placeholder, the edge
degrades to a single "has a summarised conflict" bit.

The check itself opens with the theorem, drawn in the comment:

```c
// postgres/src/backend/storage/lmgr/predicate.c — OnConflict_CheckForSerializationFailure header, 4446-4466
  4446  /*----------------------------------------------------------------------------
  4447   * We are about to add a RW-edge to the dependency graph - check that we don't
  4448   * introduce a dangerous structure by doing so, and abort one of the
  4449   * transactions if so.
  ...
  4454   *      Tin ------> Tpivot ------> Tout
  4455   *            rw             rw
  4456   *
  4457   * Furthermore, Tout must commit first.
  4458   *
  4459   * One more optimization is that if Tin is declared READ ONLY (or commits
  4460   * without writing), we can only have a problem if Tout committed before Tin
  4461   * acquired its snapshot.
  4462   *----------------------------------------------------------------------------
  4463   */
  4464  static void
  4465  OnConflict_CheckForSerializationFailure(const SERIALIZABLEXACT *reader,
  4466                                          SERIALIZABLEXACT *writer)
```

:4457 is Theorem 1's second half and :4459-4461 is Theorem 3 from Step 11 —
both encoded as comments above the function that enforces them. The body then
asks three separate questions, because the new edge can complete the structure
in three different positions:

| case | shape | lines | the condition |
|---|---|---|---|
| 1 | `R --rw--> W --rw--> T2`, W already committed | `:4485-4487` | writer is committed **and** already has an out-conflict — so the structure is complete and (since the writer committed) we must be the reader |
| 2 | `R --rw--> W --rw--> T2`, the writer just became the pivot | `:4508-4532` | walk `writer->outConflicts`; fail if some T2 is prepared and neither the reader nor the writer committed before it, and (if the reader is read-only) T2 prepared before the reader's snapshot |
| 3 | `T0 --rw--> R --rw--> W`, the **reader** just became the pivot | `:4547-4578` | writer is prepared and reader is not read-only; walk `reader->inConflicts` for a T0 that is not doomed and did not commit before the writer prepared |

Case 3 is the one people forget: adding an edge makes *two* nodes gain an edge,
so both ends have to be re-examined. Note that "prepared" here is broader than
two-phase commit — `predicate.c:268-271`: "a sxact is marked 'prepared' once it
has passed `PreCommit_CheckForSerializationFailure`, even if it isn't using 2PC.
This is the point at which it can no longer be aborted."

Why it matters: every clause in cases 2 and 3 that mentions a commit sequence
number is Cahill's commit-ordering optimisation from Step 5, spending memory
(the lists of Step 8) to avoid a false abort.

### Step 10 — who dies: safe retry, worked on the doctors

> **In:** a detected dangerous structure. **Out:** the specific transaction
> postgres aborts, the rule that chooses it, and — traced through the real
> functions — the answer for Step 2's schedule.

§5.4 states the property that decides the choice:

> **Safe retry:** if a transaction is aborted, immediately retrying the same
> transaction will not cause it to fail again with the same serialization
> failure.

and derives three rules from it, for the structure `T1 --rw--> T2 --rw--> T3`:

1. **Do not abort anything until T3 commits.** Needed for the commit-ordering
   optimisation, and it also serves safe retry.
2. **Always abort T2 if possible** — the pivot. "T2 must have been concurrent
   with both T1 and T3. Because T3 is already committed, the retried T2 will not
   be concurrent with it and so will not be able to have a rw-conflict out to
   it." Aborting T1 instead would leave it still concurrent with T2, so the same
   structure could form again.
3. **If both T2 and T3 have committed, abort T1** — safe, because the retried T1
   is concurrent with neither.

Rule 1 has a consequence: a structure may be detected and left standing. So
there is a second check at commit time:

```c
// postgres/src/backend/storage/lmgr/predicate.c — PreCommit_CheckForSerializationFailure, 4659-4696 (elided)
  4659      dlist_foreach(near_iter, &MySerializableXact->inConflicts)
  4660      {
  4661          RWConflict  nearConflict =
  4662              dlist_container(RWConflictData, inLink, near_iter.cur);
  4663
  4664          if (!SxactIsCommitted(nearConflict->sxactOut)
  4665              && !SxactIsDoomed(nearConflict->sxactOut))
  4666          {
  4667              dlist_iter  far_iter;
  4668
  4669              dlist_foreach(far_iter, &nearConflict->sxactOut->inConflicts)
  4670              {
  4671                  RWConflict  farConflict =
  4672                      dlist_container(RWConflictData, inLink, far_iter.cur);
  4673
  4674                  if (farConflict->sxactOut == MySerializableXact
  4675                      || (!SxactIsCommitted(farConflict->sxactOut)
  4676                          && !SxactIsReadOnly(farConflict->sxactOut)
  4677                          && !SxactIsDoomed(farConflict->sxactOut)))
  4678                  {
  ...
  4694                      nearConflict->sxactOut->flags |= SXACT_FLAG_DOOMED;
  4695                      break;
```

This is a two-level walk: my in-edges (:4659) give me candidate pivots; each
pivot's own in-edges (:4669) give me the Tin. Line **4694** is rule 2 — the
pivot gets `SXACT_FLAG_DOOMED`. The comment at :4625-4629 gives the reason in
progress terms: "This transaction is committing writes, so letting it commit
ensures progress. If we canceled the far conflict, it might immediately fail
again on retry."

**The doctors, traced.** Take the schedule from Step 2 and follow it through
these functions. T1 = xid 100, T2 = xid 101.

```
t0  T1 SELECTs   -> SIREAD markers for T1 on the rows/pages it scanned
t1  T2 SELECTs   -> SIREAD markers for T2 on the same objects

t2  T1 UPDATEs Alice
      heapam.c:3963 -> CheckForSerializableConflictIn(rel, &oldtup.t_self, blkno)
      predicate.c:4302/4311/4317 find T2's SIREAD marker
      FlagRWConflict(reader = T2, writer = T1), predicate.c:4430
        OnConflict case 1  (:4485)  T1 committed?          no   -> no failure
        OnConflict case 2  (:4514)  walk T1->outConflicts  empty -> no failure
        OnConflict case 3  (:4547)  T1 prepared?           no   -> no failure
      SetRWConflict: record  T2 --rw--> T1

t3  T2 UPDATEs Bob
      same path; finds T1's marker
      FlagRWConflict(reader = T1, writer = T2)
        case 2 walks T2->outConflicts = { T1 }, but T1 is not prepared yet -> no failure
        case 3 needs T2 prepared; it is not                                -> no failure
      SetRWConflict: record  T1 --rw--> T2        <- the cycle now exists, undetected

t4  T1 COMMITs -> PreCommit_CheckForSerializationFailure (predicate.c:4632)
      :4648  am I already doomed?                       no
      :4659  walk T1->inConflicts        -> nearConflict->sxactOut = T2
      :4664  T2 committed or doomed?                    no
      :4669  walk T2->inConflicts        -> farConflict->sxactOut = T1
      :4674  farConflict->sxactOut == MySerializableXact ?   YES
      :4685  is T2 prepared?                            no
      :4694  T2->flags |= SXACT_FLAG_DOOMED
      T1 commits.

t5  T2 COMMITs -> PreCommit_CheckForSerializationFailure
      :4648  SxactIsDoomed(T2) -> ERROR 40001,
             "Canceled on identification as a pivot, during commit attempt."

RESULT: T1 commits, T2 aborts. Alice off call, Bob still on call.
        Equivalent to the serial order <T1, T2>: T2 re-run now counts 1,
        fails its 2 >= 2 test, and writes nothing.  Invariant holds.
```

Map that back onto the theorem. The structure is `T1 --rw--> T2 --rw--> T1`, so
Tin = T1, Tpivot = T2, Tout = T1 (Corollary 2's length-2 case). Tout committed
first, satisfying rule 1. Rule 2 says abort the pivot: T2. And safe retry is
real, not theoretical — on retry T2 is no longer concurrent with T1, so the
edge that killed it cannot re-form.

Why it matters: "SSI aborts somebody" is not a coin flip. Which somebody is
chosen is the difference between a retry loop that terminates and one that
livelocks.

### Step 11 — read-only transactions: the optimisation that made it shippable

> **In:** the observation that a read-only transaction cannot have an rw edge
> pointing *in*. **Out:** Theorem 3, safe snapshots, `DEFERRABLE`, and the
> measured wait.

A read-only transaction never writes, so nobody can fail to see its writes, so
it can never have an in-edge, so it can never be the pivot. But §2.1.2's
three-transaction anomaly shows it can still be **Tin** — the read-only `REPORT`
transaction is *essential* to that anomaly, "a surprising result discovered by
Fekete et al. [11]". So read-only transactions cannot simply be exempted.

§4.1 sharpens it into a theorem of the paper's own:

> **Theorem 3.** Every serialization anomaly contains a dangerous structure
> `T1 --rw--> T2 --rw--> T3`, where if T1 is read-only, T3 must have committed
> before T1 took its snapshot.

The proof is three lines and worth following (§4.1): if there is a cycle, some
T0 precedes T1 in it; that edge cannot be rw or ww because T1 wrote nothing, so
it is a wr-dependency; a wr-dependency means T0 committed before T1's snapshot;
and T3 commits before T0 by Theorem 1. Hence T3 committed before T1's snapshot.

Two things fall out:

- **Read-only snapshot ordering** — a dangerous structure whose T1 is read-only
  can be dismissed unless T3 committed before T1's snapshot. This is the
  `SxactIsReadOnly(reader)` clause at `predicate.c:4525-4526`, comparing
  `t2->prepareSeqNo` against `reader->SeqNo.lastCommitBeforeSnapshot` — the
  union member from Step 8's struct at `predicate_internals.h:85-86`.
- **Safe snapshots** (§4.2) — if no concurrent read/write transaction has, or
  could develop, an out-conflict to a transaction that committed before your
  snapshot, then you cannot be part of any anomaly. Such a transaction "can read
  any data (perform any query) without risk of serialization failure. It cannot
  be aborted, and does not need to take SIREAD locks." The catch: safety is not
  knowable when the snapshot is taken, only once every concurrent read/write
  transaction has finished. So postgres tracks the candidates
  (`possibleUnsafeConflicts`, `predicate_internals.h:108`) and, on success, sets
  `SXACT_FLAG_RO_SAFE` (`predicate.c:3542`), at which point the read-only
  transaction drops its markers and degrades to plain `REPEATABLE READ`.

`DEFERRABLE` (§4.3) turns that from luck into a guarantee by waiting for it:

```c
// postgres/src/backend/storage/lmgr/predicate.c — GetSafeSnapshot, 1493-1536 (elided)
  1493      while (true)
  1494      {
  1501          snapshot = GetSerializableTransactionSnapshotInt(origSnapshot,
  1502                                                           NULL, InvalidPid);
  1503
  1504          if (MySerializableXact == InvalidSerializableXact)
  1505              return snapshot;    /* no concurrent r/w xacts; it's safe */
  ...
  1513          MySerializableXact->flags |= SXACT_FLAG_DEFERRABLE_WAITING;
  1514          while (!(dlist_is_empty(&MySerializableXact->possibleUnsafeConflicts) ||
  1515                   SxactIsROUnsafe(MySerializableXact)))
  1516          {
  1517              LWLockRelease(SerializableXactHashLock);
  1518              ProcWaitForSignal(WAIT_EVENT_SAFE_SNAPSHOT);
  1519              LWLockAcquire(SerializableXactHashLock, LW_EXCLUSIVE);
  1520          }
  ...
  1523          if (!SxactIsROUnsafe(MySerializableXact))
  1526              break;              /* success */
  ...
  1535          ReleasePredicateLocks(false, false);
  1536      }
```

Line 1504-1505 is the special case §4.2 calls out: a snapshot taken when no
read/write transaction is active is *immediately* safe. Otherwise the loop waits
(:1514-1520) and, if the snapshot turns out unsafe, throws it away and retries
(:1535). §4.3 is candid that this can starve in theory. §8.4 measures it
instead: 1 200 deferrable transactions started against the heavy disk-bound
DBT-2++ workload had a **median wait of 1.98 s**, 90% under 6 s, and none over
20 s.

Why it matters: `pg_dump` is a long read-only transaction. Without this, a
nightly backup would take SIREAD locks over the whole database and prevent every
other transaction's markers from being released. §4.3 names that failure mode
exactly — long readers "inhibit cleanup of other transactions' SIREAD locks …
this can easily exhaust memory."

### Step 12 — bounded memory, and the one feature that breaks safe retry

> **In:** SIREAD state that must outlive its transaction. **Out:** the four
> techniques that bound it, and why `PREPARE TRANSACTION` can make safe retry
> impossible.

The retention rule comes from Corollary 2: only *concurrent* transactions can
share an rw edge, so a committed transaction's markers stay until every
transaction that overlapped it has finished. §6: "a single long-running
transaction can easily prevent thousands of transactions from being cleaned up."
Two requirements followed — bounded memory, and graceful degradation: "the
system should not fail to process new transactions because it runs out of
memory. Instead, it should be able to accept new transactions, albeit possibly
with a higher false positive abort rate."

The four techniques, §6:

1. **Safe snapshots and deferrable transactions** (§4.2) — Step 11.
2. **Granularity promotion** (§5.2) — Step 7.
3. **Aggressive cleanup** (§6.1) — drop a committed transaction's state the
   moment it stops being needed. The subtlety: if active T1 has an out-edge to
   committed T2, you still need to know whether T2 had an out-edge to some T3
   *and when T3 committed* — and T3 may be long gone. Hence the one extra field,
   `earliestOutConflictCommit` (`predicate_internals.h:83-84`). A second
   optimisation: when the only remaining active transactions are read-only, all
   committed transactions' SIREAD locks can be discarded outright, since no
   future write can conflict with them.
4. **Summarisation** (§6.2) — when the fixed slot count for committed
   transactions is exhausted, collapse old ones into one shared record. "It is
   usually sufficient to discover that a transaction has a conflict with some
   previously committed transaction, but not which one." That shared record is
   `OldCommittedSxact` (`predicate.c:364`, initialised at `:1275-1287`, fed by
   `SerialAdd` at `:839`), and it is exactly what `FlagRWConflict` degrades to at
   `:4438-4441`. Cost: a higher false-positive abort rate.

**Two-phase commit** (§7.1) is the one interaction that costs a *property*, not
just performance. A prepared transaction cannot be aborted, so the pre-commit
check must run before `PREPARE`. Worse, consider

```
  Tactive --rw--> Tprepared --rw--> Tcommitted
```

Rule 2 says abort the pivot — `Tprepared` — and you cannot. The only option is
`Tactive`, which on retry is still concurrent with `Tprepared` and likely to
fail identically. §7.1: this "sometimes makes it impossible to guarantee the
safe retry property." The code carries that concession in two places, killing
the reader instead of the writer at `predicate.c:4599-4610`
("Canceled on conflict out to pivot %u, during read") and committing suicide at
`:4685-4692` ("Canceled on commit attempt with conflict in from prepared
pivot"). And after a crash, §7.1 says postgres "conservatively assume[s] that
any prepared transaction has rw-antidependencies both in and out" — a prepared
transaction that survives a restart is treated as a pivot by default.

Why it matters: every earlier conservative shortcut cost extra aborts. This one
costs the guarantee that retrying helps — the only place in the paper where the
degradation is qualitative.

### Step 13 — the price, honestly

> **In:** §8's three benchmarks. **Out:** every number the paper reports, with
> the ratios worked out, and the part of the bill that is not the database's to
> pay.

§8 names two sources of overhead up front: tracking read dependencies (CPU, plus
contention on the lock manager's lightweight locks) and retries after
serialization failures, "some of which may be false positives." The comparison
baseline throughout is postgres's own `REPEATABLE READ` (= plain SI), with a
purpose-built S2PL implementation as a third point.

| benchmark | configuration | result | source |
|---|---|---|---|
| SIBENCH | in-memory (tmpfs), a single `<key,value>` table | tracking read dependencies costs **10–20% CPU**; SSI stays close to SI while S2PL falls away, because updates and scans cannot run concurrently under locking | §8.1 |
| DBT-2++ | 25 warehouses (3 GB), tmpfs, 4 threads — the CPU-bound worst case | SSI is a **5% slowdown** vs SI; SSI beats S2PL at every read-only fraction | §8.2, Fig 5a |
| DBT-2++ | 150 warehouses (19 GB), disk-bound, 36 threads | "the performance of SSI is indistinguishable from that of SI"; serialization failure rate **under 0.25%** in all cases | §8.2, Fig 5b |
| RUBiS | eBay-like auction site, 85% read-only, 6 GB dataset | see the arithmetic below | §8.3, Fig 6 |
| deferrable | started against the disk-bound DBT-2++ load, 1 200 trials | median **1.98 s** to a safe snapshot, 90% within 6 s, all within 20 s | §8.4 |

The RUBiS row is the one to work out, because it is an end-to-end application
number rather than a database microbenchmark. Figure 6:

```
             throughput (req/s)   serialization failures
  SI                 435                 0.004 %
  SSI                422                 0.03  %
  S2PL               208                 0.76  %

  SSI  vs SI    = 422 / 435 = 0.970   ->  3.0 % slower than snapshot isolation
  S2PL vs SI    = 208 / 435 = 0.478   -> 52.2 % slower than snapshot isolation
  SSI  vs S2PL  = 422 / 208 = 2.03    ->  SSI serves 2x the requests S2PL does

  failure rates: SSI aborts 0.03 / 0.004 = 7.5x as often as SI,
                 but S2PL aborts 0.76 / 0.03 = 25x as often as SSI.
```

That 2.03× is the paper's case in one number: on a read-heavy workload with
frequent rw-conflicts — §8.3 gives the concrete source, "queries that list the
current bids on all items in a particular category conflict with requests to bid
on those items" — locking loses half the machine, and SSI does not.

Note also the one honest concession in §8.2: TPC-C as written "is known not to
exhibit anomalies under snapshot isolation [10]", so the authors had to import
the "credit check" transaction from Cahill's TPC-C++ variant to make anomalies
possible at all. The benchmark was modified to make SSI's job *harder*, which is
the right direction, but it means the abort rates are not TPC-C's.

**And the part the database cannot pay.** A serialization failure arrives as
SQLSTATE 40001 with `errhint("The transaction might succeed if retried.")`
(`predicate.c:4597`, `:4609`, `:4656`, `:4692`). §3 assumes the retry exists:
"users must already be prepared to handle transactions aborted by serialization
failures, e.g. using a middleware layer that automatically retries
transactions." If the application does not retry, `SERIALIZABLE` does not
deliver serializable semantics — it delivers errors. All of §5.4's safe-retry
machinery is an optimisation of a retry loop that somebody else has to write.

Why it matters: the numbers above are the *tracking* cost. The abort cost is
workload-shaped and unbounded — a workload built to produce pivots will produce
them, and §8's "well under 1%" is a property of TPC-C and RUBiS, not a promise.

## Where each step lives in the code

All anchors are `postgres/postgres@701f021`. Verify with
`python3 tools/pinned-source.py show postgres predicate.c -r A:B`.

| Step | What | File | Lines |
|---|---|---|---|
| 6 | rw edge from MVCC data, write-first | `access/heap/heapam.c` | 9182-9263 |
| 6 | is the writer concurrent with me? | `storage/lmgr/predicate.c` | 3900-3917 |
| 6 | the read-side entry point | `storage/lmgr/predicate.c` | 3920-3936, 3952 |
| 7 | acquire a SIREAD marker | `storage/lmgr/predicate.c` | 2505, 2528, 2550 |
| 7 | writer checks for markers, finest → coarsest | `storage/lmgr/predicate.c` | 4265-4318 |
| 7 | per-target promotion threshold | `storage/lmgr/predicate.c` | 2217-2244 |
| 7 | the promotion test itself | `storage/lmgr/predicate.c` | 2255-2300 |
| 7 | lock table sizing | `storage/lmgr/predicate.c` | 263-264 |
| 7 | the defaults (64, −2, 2) | `utils/misc/postgresql.conf.sample` | 877, 879, 882 |
| 8 | per-transaction SSI state | `include/storage/predicate_internals.h` | 78-119 |
| 8 | the flag bits | `include/storage/predicate_internals.h` | 121-142 |
| 8 | record one rw edge | `storage/lmgr/predicate.c` | 656-677 |
| 9 | check, then record | `storage/lmgr/predicate.c` | 4429-4444 |
| 9 | the theorem, as a comment | `storage/lmgr/predicate.c` | 4446-4463 |
| 9 | case 1 / case 2 / case 3 | `storage/lmgr/predicate.c` | 4485-4487 / 4508-4532 / 4547-4578 |
| 10 | who dies, at edge time | `storage/lmgr/predicate.c` | 4580-4612 |
| 10 | who dies, at commit time | `storage/lmgr/predicate.c` | 4632-4705 |
| 11 | safe-snapshot wait for `DEFERRABLE` | `storage/lmgr/predicate.c` | 1487-1545 |
| 11 | the RO_SAFE hand-off | `storage/lmgr/predicate.c` | 3542 |
| 12 | the summarisation placeholder | `storage/lmgr/predicate.c` | 364, 1275-1287, 839 |
| 12 | "prepared" ≠ two-phase commit | `storage/lmgr/predicate.c` | 268-275 |

## How to read the paper (with the concepts in hand)

~1.5 h. The section numbering is not what a skim suggests: §4 is the read-only
theory (not implementation), §5 is the implementation, §6 memory, §7 feature
interactions, §8 the numbers.

1. **§1–§2** — skim. §2.1.1 is the doctors (Step 2), §2.1.2 the
   three-transaction batch anomaly you need for Step 11, §2.2 the four
   application-level workarounds and the Wisconsin Court System motivation:
   hundreds of relations, "over 20 full-time programmers", queries auto-generated
   by ORMs, so the n² analysis of which transaction pairs can skew "was simply
   not feasible."
2. **§3 — read carefully.** §3.1's three edge types, §3.2's Theorem 1 and
   Corollary 2, §3.3's algorithm and its two admissions (false positives; no
   need for wr/ww edges). Do not skip §3.3.1: PSSI is the road not taken and the
   reason is measured.
3. **§4 — read carefully.** Theorem 3 and its three-line proof, safe snapshots,
   `DEFERRABLE`. This is the part that is *not* in Cahill.
4. **§5** — the engineering. §5.2's two detection paths are the key structural
   idea; §5.2.1 the lock manager; §5.3 the state-size design space; §5.4 the
   safe-retry rules. Read §5.4 with `predicate.c:4632` open beside it.
5. **§6–§7** — memory bounding and feature interactions. §7.1 (2PC) is the only
   place the paper gives up a property rather than some performance.
6. **§8 — read the numbers**, and note which configuration each belongs to; the
   overheads range from 20% to unmeasurable depending on whether the bottleneck
   is CPU or disk.

For Cahill, Röhm & Fekete (SIGMOD 2008): the algorithm and the theorem
statement are enough — this paper productionizes both.

## Questions for notes.md

1. Why must SIREAD locks outlive commit? Construct the history where the
   dangerous structure completes *after* the reader committed. (§3.3 points at
   the `T1 --rw--> T2` edge in Example 2; Corollary 2 tells you how long the
   locks must then be kept.)
2. Granularity promotion trades memory for false aborts. Where is the same trade
   in your `mvcc.rs` `Serializable` mode? Your read set is whole keys — what
   would "escalating to a relation" mean there, and what would it do to
   `serializable_mode_prevents_write_skew`?
3. Read-only transactions can never be the pivot. Which edge can they not have,
   and why? Then: §2.1.2's anomaly *requires* a read-only transaction. Reconcile
   those two statements. (Theorem 3 is the reconciliation.)
4. Postgres stores a *list* of rw edges per transaction where the original SSI
   paper stored two bits. Name the two optimisations that the list buys (§5.3),
   and say what each would have to give up under the two-bit scheme.
5. M8: FalkorDB is single-writer. With exactly one writer at a time, can a
   dangerous structure form between two write transactions? Between a writer and
   concurrent readers? Is SSI machinery needed, or does single-writer + SI
   already equal serializable? Prove it from the pivot definition and Corollary
   2 — this is the M8 design shortcut.

## Takeaway

Serializability does not require blocking. It requires *noticing*. Fekete's
theorem shrinks "is this history serializable?" from a graph search to a
two-list walk at one node, and everything postgres added on top —
commit-ordering checks, read-only theory, granularity promotion, summarisation —
is a trade of precision for memory or memory for precision, each defended with a
number. The one thing never traded is soundness: every shortcut in the paper
produces *more* aborts, never a wrong answer. The bill, on the paper's own
benchmarks, is 3–5% throughput where the CPU is the bottleneck and nothing at
all where the disk is — plus a retry loop the application must write itself.

## Connections to this topic's experiment

`experiments/src/mvcc.rs` puts both halves of Step 2 in front of you, and it is
worth being precise about what it does and does not implement.

- `write_skew_happens_under_snapshot_isolation` (`mvcc.rs:189-207`) is Figure 1,
  key for key: `t1` reads `bob_on_call` and writes `alice_on_call`; `t2` reads
  `alice_on_call` and writes `bob_on_call`; the test asserts **both commits
  succeed** — "SI must ALLOW write skew". You have to be able to produce the bug
  before you can prevent it.
- `serializable_mode_prevents_write_skew` (`mvcc.rs:210-224`) prevents it, but
  **not the way postgres does**. `Mode::Serializable` is backward OCC: at commit,
  validate that nothing in your read set was committed after your snapshot. The
  module doc (`mvcc.rs:10-13`) says so — "stricter than postgres SSI, zero false
  negatives for write skew; count the false positives later."
- That "stricter" is measurable against this paper. §3.3's Example-2-minus-T1
  execution — a single rw edge, no dangerous structure — is serializable, and
  SSI permits it while "neither S2PL nor OCC would." Your `Mode::Serializable`
  is in the OCC column: it will abort executions SSI would have allowed. The
  end state is the same (`t2` gets `Err(CommitError::ReadConflict)` on the
  doctors); the false-positive rate is not.

**What this repo has measured, and what it has not.** The provided lane
(`FINDINGS.md` row 8, and the baseline table in
[notes.md](notes.md)) is a single global `Mutex<HashMap>`, 4 threads × 50 000
transactions × 4 operations, on an Apple M3 Pro:

| mix | global-lock txn/s | mvcc txn/s | aborts |
|---|---|---|---|
| read-heavy 95/5, 10K keys | 623 454 | stub | stub |
| write-heavy 50/50, 10K keys | 594 264 | stub | stub |
| write-heavy 50/50, 64 keys (HOT) | 676 691 | stub | stub |

**The headline is the flatness, and it is a negative result.** ~600k txn/s on
all three mixes: the mutex does not care whether the workload is 95% reads or
50% writes, or whether it collides on 10 000 keys or 64, because it already
serialized everything. The 64-key row is even the *fastest* — a cache-resident
working set, with no contention penalty to pay because there was only ever one
lock to contend on.

So: **this repo has not measured MVCC beating a mutex, and has not measured SSI
at all.** The `mvcc txn/s` and `aborts` columns are `stub` because you have not
written that code yet. Nothing in this guide should be read as a repo
measurement — every number above is Ports & Grittner's, on their 2011 hardware,
carrying its section. When you fill in those columns, the prediction worth
writing down first is in [notes.md](notes.md): MVCC should crush the baseline on
row 1, where readers never block, and may well *lose* on row 3, where
first-committer-wins converts key contention into aborted work the mutex never
had to redo.

## Done when

Answer each before unfolding it.

- [ ] Draw the dangerous structure, label all three transactions, and state the
      extra condition Theorem 1 imposes beyond "two adjacent rw edges".
  <details><summary>Answer</summary>

  `Tin --rw--> Tpivot --rw--> Tout`, with the extra condition that **Tout is the
  first transaction in the cycle to commit** (§3.2). The paper flags this as
  stronger than Fekete et al. explicitly stated — they said only that T3 commits
  before T1 and T2 — and it is what licenses both the commit-ordering
  optimisation (§3.3.1) and safe-retry rule 1, "do not abort anything until T3
  commits" (§5.4). It is drawn in the source at `predicate.c:4454-4457`.

  </details>

- [ ] Place both doctors transactions on that structure, and say which one
      postgres aborts and why the retry then succeeds.
  <details><summary>Answer</summary>

  The cycle has length 2: `T1 --rw--> T2 --rw--> T1`, so Tin = T1,
  Tpivot = T2, Tout = T1 (Corollary 2 allows T1 and T3 to be the same
  transaction). T1 commits first, satisfying rule 1. Safe-retry rule 2 says
  abort the pivot, so **T2 dies** — concretely, T1's
  `PreCommit_CheckForSerializationFailure` walks its in-edges to T2
  (`predicate.c:4659`), walks T2's in-edges back to itself (`:4669`, matching at
  `:4674`), and sets `SXACT_FLAG_DOOMED` on T2 at `:4694`. T2 then errors at
  `:4648` with SQLSTATE 40001. The retry succeeds because the retried T2 is no
  longer concurrent with T1: it sees Alice already off call, counts 1, fails
  its `>= 2` test, and writes nothing. Aborting T1 instead would have left it
  concurrent with T2, so the same structure could re-form — §5.4's reasoning
  for preferring the pivot.

  </details>

- [ ] Postgres detects rw-antidependencies two different ways. Name both, say
      which chronology each handles, and which one is free.
  <details><summary>Answer</summary>

  §5.2 splits on whether the write or the read came first. **Write first:** the
  MVCC data already answers it — if a tuple is invisible to you because its
  creator had not committed when you took your snapshot, that *is* the edge.
  `HeapCheckForSerializableConflictOut` (`heapam.c:9182`) reads the same
  `xmin`/`xmax` the visibility check just read, so this direction is
  essentially free. **Read first:** nothing in the tuple records that you read
  it, so it needs the SIREAD marker — a passive entry in a dedicated lock
  manager that "does not support any other lock modes, and hence cannot block"
  (§5.2), checked by every writer via `CheckForSerializableConflictIn`
  (`predicate.c:4265`). Only the second direction costs anything, and it is the
  source of §8.1's 10–20% CPU overhead.

  </details>

- [ ] A serializable transaction reads 40 rows scattered one per page across a
      table, with default settings. What SIREAD lock does it end up holding, and
      what does that do to its abort risk?
  <details><summary>Answer</summary>

  A single **relation**-level marker. The relation promotion threshold is
  `max_pred_locks_per_transaction / -max_pred_locks_per_relation - 1` =
  `64 / 2 - 1` = **31** (`predicate.c:2223-2226` with the defaults at
  `postgresql.conf.sample:877` and `:879`), and the count includes non-direct
  descendants (`:2203-2204`), so the 32nd marker trips
  `parentlock->childLocks > MaxPredicateChildLocks(...)` at `:2284-2285` and one
  relation marker replaces the lot. (Had those rows been packed 3-to-a-page,
  page-level promotion would have fired first, at threshold 2, `:2228-2229`.)
  Consequence: from then on, *any* write by *any* concurrent transaction to that
  table produces an rw edge against you, even to rows you never read. More false
  edges, more false aborts — and never a wrong answer. That one-directionality
  is the paper's recurring safety argument.

  </details>

- [ ] Why can a read-only transaction never be the pivot — and why is that not
      enough to exempt it from SSI tracking?
  <details><summary>Answer</summary>

  It can never have an rw edge pointing *in*, because an in-edge means "a
  transaction that couldn't see **our** write" (`predicate_internals.h:90-91`)
  and it has no writes. No in-edge, no pivot. But §2.1.2's batch-processing
  anomaly needs the read-only `REPORT` transaction as **Tin** — remove it and
  the execution is serializable in the order ⟨T2, T3⟩. That read-only
  transactions can participate at all was "a surprising result discovered by
  Fekete et al. [11]". The exemption therefore has to be conditional, and
  Theorem 3 (§4.1) supplies the condition: when Tin is read-only, Tout must have
  committed **before Tin's snapshot**. That gives the false-positive filter at
  `predicate.c:4525-4526` and the safe-snapshot rule of §4.2, under which a
  read-only transaction drops its SIREAD locks and degrades to plain
  `REPEATABLE READ`.

  </details>

- [ ] Name the number this guide reports for SSI's throughput cost, and the
      number the repo's own lane reports — and say why they cannot be compared.
  <details><summary>Answer</summary>

  The paper's cost is configuration-dependent: **10–20% CPU** on SIBENCH (§8.1),
  a **5% slowdown** on CPU-bound DBT-2++ (§8.2, 25 warehouses in tmpfs),
  **3.0%** on RUBiS (422 vs 435 req/s, §8.3 Fig 6), and **nothing measurable**
  on disk-bound DBT-2++ (§8.2, 150 warehouses). The repo's lane
  ([`FINDINGS.md`](../../FINDINGS.md) row 8, [notes.md](notes.md)) reports
  something else entirely: a global `Mutex<HashMap>` at **623 454 / 594 264 /
  676 691 txn/s** across read-heavy, write-heavy and hot-key mixes — flat,
  because a single mutex had already serialized everything. They cannot be
  compared because the repo has measured **no MVCC implementation and no SSI at
  all**: the `mvcc txn/s` and `aborts` columns are still `stub`. Any claim here
  that MVCC beats a mutex would be unmeasured.

  </details>

## References

**Papers**
- Ports & Grittner — "Serializable Snapshot Isolation in PostgreSQL"
  (VLDB 2012, [arXiv:1208.4179](https://arxiv.org/abs/1208.4179)) — ~1.5 h. §3
  the theory, §4 the read-only extension (this paper's own contribution), §5 the
  implementation, §6 memory, §7 feature interactions, §8 the numbers.
- Cahill, Röhm & Fekete — "Serializable Isolation for Snapshot Databases"
  (SIGMOD 2008) — the SSI *algorithm*, cited as [7] throughout. The
  dangerous-structure check and SIREAD locks originate here.
- Fekete, Liarokapis, O'Neil, O'Neil & Shasha — "Making Snapshot Isolation
  Serializable" (TODS 2005), cited as [10] — the *theorem* (Theorem 1 in §3.2)
  that every cycle contains two adjacent rw-antidependencies.
- Berenson et al. — "A Critique of ANSI SQL Isolation Levels" (SIGMOD 1995) —
  write skew as A5B; see [reading-ansi-critique.md](reading-ansi-critique.md).

**In postgres** (`postgres/postgres@701f021`)

| File | Lines | What |
|---|---|---|
| `src/backend/storage/lmgr/predicate.c` | 263-264 | lock-table sizing: `max_pred_locks_per_xact × (MaxBackends + max_prepared_xacts)` |
| `src/backend/storage/lmgr/predicate.c` | 268-275 | "prepared" is set by the pre-commit check even without 2PC |
| `src/backend/storage/lmgr/predicate.c` | 656-677 | `SetRWConflict` — one edge, threaded onto both lists |
| `src/backend/storage/lmgr/predicate.c` | 1487-1545 | `GetSafeSnapshot` — the `DEFERRABLE` wait |
| `src/backend/storage/lmgr/predicate.c` | 2217-2300 | promotion thresholds and the promotion test |
| `src/backend/storage/lmgr/predicate.c` | 2505-2550 | acquiring relation / page / tuple SIREAD markers |
| `src/backend/storage/lmgr/predicate.c` | 3900-3917 | `XidIsConcurrent` — the overlap test |
| `src/backend/storage/lmgr/predicate.c` | 3920-4010 | `CheckForSerializableConflictOut(Needed)` |
| `src/backend/storage/lmgr/predicate.c` | 4265-4318 | `CheckForSerializableConflictIn` — finest to coarsest |
| `src/backend/storage/lmgr/predicate.c` | 4429-4444 | `FlagRWConflict` — check first, record second |
| `src/backend/storage/lmgr/predicate.c` | 4465-4613 | the three-case pivot test, and who gets doomed |
| `src/backend/storage/lmgr/predicate.c` | 4632-4705 | `PreCommit_CheckForSerializationFailure` |
| `src/include/storage/predicate_internals.h` | 78-142 | `SERIALIZABLEXACT` and its flag bits |
| `src/backend/access/heap/heapam.c` | 9182-9263 | `HeapCheckForSerializableConflictOut` |
| `src/backend/access/heap/heapam.c` | 1750, 2054, 2345, 2628, 2959, 3963 | where the heap AM calls into SSI |
| `src/backend/utils/misc/postgresql.conf.sample` | 877, 879, 882 | the three predicate-lock defaults |

**In this repo**

| File | Lines | What |
|---|---|---|
| `experiments/src/mvcc.rs` | 10-13 | the contract for `Mode::Serializable`, and its own note that it is stricter than SSI |
| `experiments/src/mvcc.rs` | 189-207 | `write_skew_happens_under_snapshot_isolation` — Figure 1, key for key |
| `experiments/src/mvcc.rs` | 210-224 | `serializable_mode_prevents_write_skew` — read-set validation, not SSI |
| [`notes.md`](notes.md) | baseline table | the measured global-mutex lane; `mvcc` columns still `stub` |
| [`FINDINGS.md`](../../FINDINGS.md) | row 8 | the flat ~600k txn/s headline |
