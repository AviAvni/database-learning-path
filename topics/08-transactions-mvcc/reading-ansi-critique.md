# Isolation levels, made rigorous: history patterns and write skew

Berenson et al.'s SIGMOD '95 critique is the paper that made isolation
rigorous — and, in the same ten pages, the paper that first defined snapshot
isolation and named the anomaly that dethrones it. Before you open it, this
chapter builds the vocabulary from zero: what a history is, why prose
definitions of isolation fail *in two directions at once*, the pattern catalog
that replaced them, and where snapshot isolation lands in the resulting
hierarchy. Read it before the SSI chapter or that one won't land.

Carry the paper's own warning while you read: its subject is that the ANSI
phenomena are **ambiguous**. A summary that lists "dirty read, non-repeatable
read, phantom" as three settled bugs has already committed the error the paper
attacks — each of those three names has a strict reading and a broad reading,
the two disagree about real histories, and the paper argues at length (§3,
Remark 4) that only the broad one was intended. Worse, the strict reading of
all three *still* admits the classical inconsistent-analysis bug (§3, history
H1), and none of the six readings mentions the write anomaly the paper has to
add from scratch (P0, §3, Remark 3).

Every claim below cites the section, table or remark it came from in the
SIGMOD '95 version (pp. 1–10,
[arXiv:cs/0701157](https://arxiv.org/abs/cs/0701157)), read in full for this
chapter. `§4.2` is a section, `Table 4` a table, `Remark 8` one of the paper's
ten numbered results, `H5` one of its named example histories.

## The problem in one sentence

ANSI SQL-92 defined its four isolation levels with three sentences of English
prose (§2.2's P1, P2, P3) that each support two incompatible formal readings —
and the paper shows that under the strict reading the levels fail to exclude
executions everyone agrees are wrong (§3, H1/H2/H3), while under either reading
they omit dirty writes entirely (§3, Remark 3) and cannot tell apart isolation
levels that commercial systems were already shipping (§4) — so for a decade
"REPEATABLE READ" was a word two vendors could both honour while behaving
differently on the same workload.

## The concepts, step by step

### Step 1 — a history: concurrency reduced to one interleaved string

> **In:** nothing yet — this step fixes the notation every later step is
> written in.
> **Out:** a four-symbol shorthand for executions, plus the predicate form
> `r1[P]`, which Steps 2–10 use to state every anomaly as a pattern.

A **transaction** groups a set of actions that transform the database from one
consistent state to another (§2.1). A **history** models the interleaved
execution of a set of transactions as a *linear ordering* of their actions —
reads and writes of specific data items (§2.1). A **data item** is deliberately
broad: "a table row, a page, an entire table, or a message on a queue" (§2.1,
following [EGLT]).

Two actions **conflict** if they are performed by distinct transactions on the
same data item and at least one is a write (§2.1). A history's **dependency
graph** has committed transactions as nodes and one edge per conflicting pair,
oriented in the order they occurred; two histories are **equivalent** when they
have the same committed transactions and the same dependency graph, and a
history is **serializable** when it is equivalent to some *serial* history —
one that runs the transactions one at a time, in sequence (§2.1). That is the
definition the whole paper is measured against.

The notation, introduced in §2.2 immediately after the three ANSI phenomena:

```
 r1[x]        transaction 1 reads data item x
 w1[x]        transaction 1 writes data item x  (insert, update or delete)
 c1           transaction 1 commits
 a1           transaction 1 aborts  (ROLLBACK)
 r1[P]        transaction 1 reads the set of records satisfying predicate P
 w1[P]        transaction 1 writes a record satisfying predicate P
 r1[x=50]     the same read, with the value it returned — used in §3's examples
 ...          "and later, in this order"
```

So `w1[x] ... r2[x] ... a1` says "T2 read T1's uncommitted write, and then T1
aborted" — T2 read a value that never committed. One line replaces a paragraph,
and two people can now check *mechanically* whether a given execution matches a
forbidden shape. That precision is the paper's contribution; everything else
follows from it.

Why it matters: from here on, "does my database permit X?" is a pattern-match
against a string, not an argument about English.

### Step 2 — what ANSI actually wrote, and the table that made it famous

> **In:** the notation from Step 1.
> **Out:** the three ANSI phenomena in the paper's own words and ANSI's
> four-level table — the object Steps 3–6 dismantle.

An **isolation level** is a contract about which **phenomena** — action
subsequences that may lead to anomalous, perhaps non-serializable, behaviour
(§1) — a transaction is forbidden to experience. The paper is careful about one
distinction most summaries drop: a **phenomenon** is a shape that *might* lead
to trouble, while an **anomaly** is an actual non-serializable outcome (§1;
"there is a technical distinction between anomalies and phenomena"). Step 3 is
where that distinction becomes the whole argument.

ANSI SQL-92 named three, quoted here in compressed form from §2.2:

- **P1 (Dirty Read)** — T1 modifies a data item; T2 then reads it before T1
  commits or rolls back. If T1 rolls back, T2 has read a data item that was
  never committed and so never really existed.
- **P2 (Non-repeatable or Fuzzy Read)** — T1 reads a data item; T2 then
  modifies or deletes it and commits; T1 rereads and gets a modified value, or
  finds it gone.
- **P3 (Phantom)** — T1 reads a set of data items satisfying some search
  condition; T2 then creates data items satisfying that condition and commits;
  T1 repeats the read and gets a different set.

And Table 1 (§2.2) crossed them with four levels:

```
 Table 1 — ANSI SQL isolation levels, defined by the three original phenomena
                          P1 (or A1)     P2 (or A2)     P3 (or A3)
                          Dirty Read     Fuzzy Read     Phantom
 ANSI READ UNCOMMITTED    Possible       Possible       Possible
 ANSI READ COMMITTED      Not Possible   Possible       Possible
 ANSI REPEATABLE READ     Not Possible   Not Possible   Possible
 ANOMALY SERIALIZABLE     Not Possible   Not Possible   Not Possible
```

Two details of that table are already the paper talking back. First, the top
row is not called SERIALIZABLE: §2.2 notes that [ANSI] Subclause 4.28 separately
requires the SERIALIZABLE level to provide "commonly known as fully serializable
execution", and that "the prominence of the table compared to this extra proviso
leads to a common misconception that disallowing the three phenomena implies
serializability" — so the paper renames the phenomena-only level **ANOMALY
SERIALIZABLE** and keeps it distinct. Second, §2.2 warns in place that "Table 1
is not a final result; Table 3 will supersede it" (Step 6).

Why it matters: every "the three isolation anomalies are…" listicle you have
read is reproducing Table 1, without the two disclaimers printed on it.

### Step 3 — the fork: each phenomenon splits into a strict and a broad reading

> **In:** the three English phenomena from Step 2.
> **Out:** *two* catalogs from one — strict A1/A2/A3 and broad P1/P2/P3. Step 4
> tests them against real histories; Step 6 keeps only the broad one; Step 10's
> hierarchy needs both, because snapshot isolation sits between them.

§2.2 takes P1 apart. The English does not actually insist that T1 abort — it
says that *if* it does, something unfortunate might follow. So there are two
formalisations:

```
 A1 (strict): w1[x] ... r2[x] ... (a1 and c2 in either order)
 P1 (broad):  w1[x] ... r2[x] ... ((c1 or a1) and (c2 or a2) in any order)
```

The strict form forbids the *actual anomaly*: two of the four possible
commit/abort pairings. The broad form forbids the *phenomenon*: all four, so it
outlaws the interleaving whether or not anything bad ends up happening. §2.2:
"Interpreting (2.2) as the meaning of P1 prohibits an execution sequence if
something anomalous might [happen] in the future."

The same split applies to the other two, giving the six patterns the paper
carries forward (§2.2):

```
 P1: w1[x] ... r2[x] ...        ((c1 or a1) and (c2 or a2) in any order)
 A1: w1[x] ... r2[x] ...        (a1 and c2 in any order)

 P2: r1[x] ... w2[x] ...        ((c1 or a1) and (c2 or a2) in any order)
 A2: r1[x] ... w2[x] ... c2 ... r1[x] ... c1

 P3: r1[P] ... w2[y in P] ...   ((c1 or a1) and (c2 or a2) in any order)
 A3: r1[P] ... w2[y in P] ... c2 ... r1[P] ... c1
```

Read A2 and A3 closely: the strict forms contain the *re-read*. That is what
"non-repeatable" and "phantom" mean literally — you have to read twice and get
two answers. The broad forms P2 and P3 do not require a second read at all;
they fire the moment a concurrent write lands on something you read.

One more correction is smuggled into P3 (§2.2): "the English statement of ANSI
SQL P3 just prohibits inserts to a predicate, but P3 above intentionally
prohibits any write (insert, update, delete) affecting a tuple satisfying the
predicate once the predicate has been read."

Why it matters: this is the fork the topic README's five-row anomaly table
silently picks a side of. When someone says "repeatable read prevents
non-repeatable reads", ask which of A2 and P2 they mean; the two levels differ
on real histories (Step 4's H2), and they differ again on lost update (Step 7).

### Step 4 — three histories decide it: the strict reading is untenable

> **In:** the two catalogs from Step 3.
> **Out:** the paper's verdict (Remark 4 — the broad readings are the correct
> ones), argued on three concrete histories with real values. Steps 5–6 build
> the repaired catalog on top of that verdict.

The method: exhibit a history that everybody agrees is broken, and show the
strict catalog permits it.

**H1, against A1** (§3) — a $40 transfer between two bank balances, x and y,
which should keep x + y = 100:

```
 H1: r1[x=50] w1[x=10] r2[x=10] r2[y=50] c2 r1[y=50] w1[y=90] c1

 T1's intent:   x: 50 − 40 = 10        y: 50 + 40 = 90        10 + 90 = 100  ✓
 T2 reads:      x = 10  and  y = 50  ⇒ 10 + 50 =  60          ✗  (should be 100)
 the shortfall: 100 − 60 = 40 — exactly the amount in flight
```

§3 calls this "the classical inconsistent analysis problem". Now check it
against the strict catalog: A1 needs one of the two transactions to abort —
neither does. A2 needs a data item read twice by the same transaction — nothing
is. A3 needs a predicate. **H1 violates none of A1, A2, A3, and is not
serializable.** It does violate P1 (`w1[x] ... r2[x] ...` with both committing),
so the broad reading catches it.

**H2, against A2** (§3) — no dirty data at all this time:

```
 H2: r1[x=50] r2[x=50] w2[x=10] r2[y=50] w2[y=90] c2 r1[y=90] c1

 T1 reads:  x = 50 (pre-transfer)  and  y = 90 (post-transfer)  ⇒ 140
 the truth: 100 either side of T2                                ✗ 40 too much
```

T1 never reads anything uncommitted, so P1 is satisfied; it never reads the
same item twice, so A2 does not apply. §3: "The problem with H2 is that by the
time T1 reads y, the value for x is out of date." P2 — `r1[x] ... w2[x] ...`,
no re-read required — disqualifies it at `w2[x=10]`.

**H3, against A3** (§3) — T1 lists the active employees, T2 inserts one and
updates the stored count z, T1 then reads z:

```
 H3: r1[P] w2[insert y to P] r2[z] w2[z] c2 r1[z] c1
```

No predicate is evaluated twice, so A3 permits it, and yet T1's list and T1's
count disagree. P3 forbids it.

Three histories, three strict patterns defeated — hence:

> **Remark 4** (§3). Strict interpretations A1, A2, and A3 have unintended
> weaknesses. The correct interpretations are the Broad ones.

Why it matters: "my database forbids dirty reads, non-repeatable reads and
phantoms" is not one claim, it is two, and only the broad one implies anything
about H1 and H2.

### Step 5 — P0, the phenomenon ANSI forgot

> **In:** the broad catalog P1–P3, endorsed by Step 4.
> **Out:** one new pattern, P0, that no ANSI level below SERIALIZABLE excludes
> and every real locking system prevents. Step 6 folds it into the repaired
> table.

§3 opens on a compliment — Remark 2: the locking levels of Table 2 are at least
as strong as the same-named ANSI levels. Then it asks whether they are *more*
isolated, and answers yes, at the very bottom of the ladder:

```
 P0 (Dirty Write): w1[x] ... w2[x] ... ((c1 or a1) and (c2 or a2) in any order)
```

A **dirty write** is one uncommitted transaction overwriting another
uncommitted transaction's write. ANSI excludes it only at SERIALIZABLE (§3);
Locking READ UNCOMMITTED excludes it everywhere, because long-duration write
locks are the one thing every locking system holds.

Two reasons it must be forbidden, both from §3. First, consistency. Assume a
constraint x = y, and let T1 write 1 to both while T2 writes 2 to both:

```
 w1[x] w2[x] w2[y] c2 w1[y] c1

 x: written 1 by T1, then 2 by T2  → 2   (T2's write survives)
 y: written 2 by T2, then 1 by T1  → 1   (T1's write survives)
 result x = 2, y = 1 — the constraint x = y is broken, and each transaction
 alone would have preserved it.
```

Second, recovery. Consider `w1[x] w2[x] a1`: you cannot undo `w1[x]` by
restoring x's before-image, because that would wipe out T2's update — and if
you *don't* restore it and T2 later aborts, T2's before-image is now wrong too.
§3: "Even the weakest locking systems hold long duration write locks.
Otherwise, their recovery systems would fail."

> **Remark 3** (§3). ANSI SQL isolation should be modified to require P0 for
> all isolation levels.

Why it matters: this is the paper finding a bug not in a database but in the
*standard* — a phenomenon so basic that every implementation prevented it and
nobody noticed the spec didn't ask them to.

### Step 6 — the repaired catalog, and what the patterns really are

> **In:** the broad P1–P3 (Step 4) plus P0 (Step 5).
> **Out:** Table 3 — the four-phenomenon table that supersedes Table 1 — and
> Remark 6's punchline about what the patterns encode. Steps 7–9 add levels
> that live *between* these rows.

§3 restates the four patterns in their final form, dropping the `(c2 or a2)`
clauses that do not restrict anything:

```
 P0: w1[x] ... w2[x] ...      (c1 or a1)     Dirty Write
 P1: w1[x] ... r2[x] ...      (c1 or a1)     Dirty Read
 P2: r1[x] ... w2[x] ...      (c1 or a1)     Fuzzy / Non-Repeatable Read
 P3: r1[P] ... w2[y in P] ... (c1 or a1)     Phantom
```

```
 Table 3 — the levels redefined by the four phenomena (supersedes Table 1)
                       P0            P1            P2            P3
                       Dirty Write   Dirty Read    Fuzzy Read    Phantom
 READ UNCOMMITTED      Not Possible  Possible      Possible      Possible
 READ COMMITTED        Not Possible  Not Possible  Possible      Possible
 REPEATABLE READ       Not Possible  Not Possible  Not Possible  Possible
 SERIALIZABLE          Not Possible  Not Possible  Not Possible  Not Possible
```

Then the observation that explains why this table is stable where Table 1 was
not (§3): "For single version histories, it turns out that the P0, P1, P2, P3
phenomena are disguised versions of locking." Forbidding P0 ≡ long-duration
write locks on items and predicates; forbidding P1 ≡ well-formed reads;
forbidding P2 ≡ long-duration item read locks; forbidding P3 ≡ long-duration
*predicate* read locks.

> **Remark 6** (§3). The locking isolation levels of Table 2 and the
> phenomenological definitions of Table 3 are equivalent. Put another way, P0,
> P1, P2, and P3 are disguised redefinitions of locking behavior.

That is the sting in the tail. ANSI's designers "sought a definition that would
admit many different implementations, not just locking" (§2.2) — and the only
repair that makes their phenomena precise turns them back into a description of
a lock manager. Which is exactly why Steps 7–9 have to leave this catalog
behind to describe a multi-version system.

Note also §2.3 and Remark 1's ladder for locking levels — the **duration** of a
lock is the whole vocabulary there: **long duration** means held until after
commit or abort, **short duration** means released as soon as the action
completes. Locking REPEATABLE READ is precisely "long-duration read locks on
*items*, short-duration read locks on *predicates*" (Table 2) — which is why it
stops fuzzy reads and not phantoms.

### Step 7 — P4 lost update: the level that lives between two rows

> **In:** Table 3 from Step 6.
> **Out:** two more patterns (P4, P4C) and the first level that Table 3 cannot
> place — the shape of the argument Step 8 repeats for snapshot isolation.

§4.1 introduces the anomaly Cursor Stability exists to prevent:

```
 P4  (Lost Update):        r1[x] ... w2[x] ... w1[x] ... c1
 P4C (Cursor Lost Update): rc1[x] ... w2[x] ... w1[x] ... c1   (rc = read cursor)
```

Worked on the paper's own history H4 (§4.1), where both transactions are
incrementing a balance:

```
 H4: r1[x=100] r2[x=100] w2[x=120] c2 w1[x=130] c1

 T2's increment: 120 − 100 = +20   committed at c2
 T1's increment: 130 − 100 = +30   computed from the stale read r1[x=100]
 final x = 130 ⇒ total applied = 130 − 100 = +30
 expected if serial                = +20 + 30 = +50
 lost                              = 50 − 30 = 20 — precisely T2's update
```

Where does P4 sit? §4.1: it is possible at READ COMMITTED, because forbidding
P0 or P1 does not exclude H4 — there is no read-after-write of uncommitted
data, and T2 commits before T1's write. But forbidding **P2** does exclude it,
"since w2[x] comes after r1[x] and before T1 commits or aborts". So P4 is
strictly between READ COMMITTED and REPEATABLE READ, and that gap is where a
real, widely shipped level lives:

> **Remark 7** (§4.1). READ COMMITTED « Cursor Stability « REPEATABLE READ.

**Cursor Stability** holds a read lock on the row the cursor is currently
positioned on, released when the cursor moves or closes; that alone converts
P4 into P4C for cursor-mediated updates (§4.1). §4.1 also notes the practical
consequence: "READ COMMITTED, in some systems, is actually the stronger Cursor
Stability. The ANSI standard allows this."

Notation: `L1 « L2` means L1 is **weaker** than L2 — every non-serializable
history allowed by L2 is also allowed by L1, and at least one is allowed by L1
and not L2 (§2.3). `L1 »« L2` means **incomparable**: each allows a
non-serializable history the other forbids. Step 10 needs both symbols.

Why it matters: the pattern of this step — an anomaly that a real product's
real level prevents, sitting between two rows of the official table — is
repeated one level up, and that repetition is what produces snapshot isolation.

### Step 8 — snapshot isolation, defined here, by the people about to break it

> **In:** the vocabulary of Steps 1–7, and the observation from Step 7 that
> real levels fall between the official rows.
> **Out:** SI's three mechanisms (start timestamp, commit timestamp,
> first-committer-wins), and the reason single-valued histories stop being an
> adequate description. Step 9 attacks it; Step 10 places it.

§4.2 introduces it in one paragraph, and this is where snapshot isolation gets
its name and its first formal definition:

- **Start-Timestamp.** Each transaction reads data from a **snapshot** of the
  *committed* data as of the time it started — any time before its first read.
  "A transaction running in Snapshot Isolation is never blocked attempting a
  read as long as the snapshot data from its Start-Timestamp can be
  maintained." Updates by other transactions active after that timestamp are
  invisible to it.
- **Its own writes are in the snapshot.** A transaction re-reading what it
  wrote sees its own value.
- **Commit-Timestamp and first-committer-wins.** At commit T1 takes a
  **Commit-Timestamp** larger than any existing start or commit timestamp. It
  commits "only if no other transaction T2 with a Commit-Timestamp in T1's
  execution interval [Start-Timestamp, Commit-Timestamp] wrote data that T1
  also wrote. Otherwise, T1 will abort." §4.2 names this **first-committer-wins**
  and states what it buys: it "prevents lost updates (phenomenon P4)".

Note what is compared: **write sets against write sets**. Nothing about reads
enters the test. Hold that; it is Step 9's entire content.

SI needs a richer notation, because a data item now has several versions at
once. §4.2 rewrites Step 4's H1 as a **multi-valued (MV) history**, subscripting
each item with the transaction that produced that version:

```
 H1.SI:    r1[x0=50] w1[x1=10] r2[x0=50] r2[y0=50] c2 r1[y0=50] w1[y1=90] c1
                       ↑              ↑
                       T1 creates version x1        T2 still reads version x0
 T2 sees x0 + y0 = 50 + 50 = 100  ✓ — consistent, unlike H1's 60 in Step 4
```

Same physical interleaving as H1, opposite verdict — because T2 reads the *old
version*, not the half-transferred one. §4.2 shows the MV history maps to a
serializable single-valued one:

```
 H1.SI.SV: r1[x=50] r1[y=50] r2[x=50] r2[y=50] c2 w1[x=10] w1[y=90] c1
```

"Mapping of MV histories to SV histories is the only rigorous touchstone needed
to place Snapshot Isolation in the Isolation Hierarchy" (§4.2).

Why it matters: your `experiments/src/mvcc.rs` implements exactly this §4.2
definition — snapshot at `begin`, buffered writes, write-set comparison at
commit returning `CommitError::WriteConflict`.

### Step 9 — read skew and write skew: the anomalies ANSI has no name for

> **In:** SI's mechanism from Step 8, in particular that only write sets are
> compared.
> **Out:** A5A and A5B, worked on the paper's H5 and on the doctors schedule
> your test suite reproduces. Step 10 uses A5B to place SI in the hierarchy.

§4.2 first generalises what is going wrong. A **constraint violation** is the
generic anomaly: databases satisfy a constraint predicate C(DB) over multiple
items, every transaction preserves it in isolation, and a transaction that
*reads* a state violating it produces garbage. Then two named shapes (§4.2,
"A5 (Data Item Constraint Violation)"):

```
 A5A (Read Skew):  r1[x] ... w2[x] ... w2[y] ... c2 ... r1[y] ... (c1 or a1)
 A5B (Write Skew): r1[x] ... r2[y] ... w1[y] ... w2[x] ... (c1 and c2 occur)
```

Read A5B's shape carefully, because it is the one the rest of this topic turns
on: **T1 reads x and writes y; T2 reads y and writes x.** The write sets are
{y} and {x} — disjoint. First-committer-wins compares write sets and finds
nothing. But each transaction's write was justified by a read that the other
transaction invalidated.

§4.2's own instance, H5, with a bank constraint "x + y must stay positive"
(balances may go negative individually as long as the pair does not):

```
 H5: r1[x=50] r1[y=50] r2[x=50] r2[y=50] w1[y=−40] w2[x=−40] c1 c2

 T1's check: x + y = 50 + 50 = 100 > 0, so writing y = −40 leaves
             50 + (−40) = 10 > 0  ✓ (against T1's snapshot)
 T2's check: x + y = 50 + 50 = 100 > 0, so writing x = −40 leaves
             (−40) + 50 = 10 > 0  ✓ (against T2's snapshot)
 committed:  x + y = (−40) + (−40) = −80  ✗ — the constraint is dead,
             and 10 − (−80) = 90 units of "safety" evaporated
```

The doctors-on-call form your tests use is the same pattern with booleans
(`experiments/src/mvcc.rs`, `write_skew_happens_under_snapshot_isolation`, which
*passes when the anomaly occurs* — you must be able to produce the bug before
you prevent it). Mapping it onto A5B with x = `bob_on_call`, y = `alice_on_call`:

```
 invariant C(DB): at least one doctor on call. Initially alice=1, bob=1.

   T1                                T2                       A5B symbol
   ──────────────────────────────    ──────────────────────   ──────────
   begin  (snapshot: alice=1,bob=1)
   read bob        → 1                                        r1[x]
                                     begin (same snapshot)
                                     read alice     → 1       r2[y]
   "bob is on call, so I may
    take alice off"
   write alice = 0                                            w1[y]
                                     "alice is on call, so I
                                      may take bob off"
                                     write bob = 0            w2[x]
   commit ✓                                                   c1
                                     commit ✓                 c2

 committed state: alice = 0, bob = 0 — nobody on call.
 T1's write set {alice}, T2's write set {bob}: intersection is EMPTY, so
 first-committer-wins (Step 8) has nothing to compare and admits both.
```

Both transactions commit. Not one, not "the first" — **both**, and the invariant
is broken by a pair of individually correct transactions. That is the whole
indictment.

Two sharp observations from §4.2. First: "Fuzzy Reads (P2) is a degenerate form
of Read Skew where x = y" — A5A is P2 generalised to two related items. Second,
and this is why A5A/A5B are labelled with an A: "Clearly neither A5A nor A5B
could arise in histories where P2 is precluded, since both A5A and A5B have T2
write a data item that has been previously read by an uncommitted T1. Thus
phenomena A5A and A5B are only useful for distinguishing isolation levels that
are below REPEATABLE READ in strength." They are not new *locking* phenomena;
they are the resolution needed to describe multi-version levels that the
single-version catalog blurs together.

Why it matters: write skew is the anomaly ANSI has no name for, and the reason
it has no name is structural — ANSI's three phenomena were written for a
single-version world, and A5B needs two transactions' *read* sets to be visible
to see anything wrong at all.

### Step 10 — the hierarchy is a partial order, not a ladder

> **In:** every pattern from Steps 3–9, and SI's mechanism from Step 8.
> **Out:** Table 4 reproduced faithfully, plus the three remarks that place SI
> — the answer to "is snapshot isolation strong or weak?", which is "neither".

Table 4 (§5) is the paper's final artifact, eight phenomena wide. Reproduced in
full, including the "Sometimes Possible" cells, which are the interesting ones:

```
 Table 4 — Isolation types characterized by possible anomalies allowed (§5)

                    P0      P1      P4C     P4      P2      P3      A5A     A5B
                    Dirty   Dirty   Cursor  Lost    Fuzzy   Phan-   Read    Write
                    Write   Read    Lost    Update  Read    tom     Skew    Skew
                                    Update
 READ UNCOMMITTED   Not     Poss.   Poss.   Poss.   Poss.   Poss.   Poss.   Poss.
   == Degree 1      Poss.
 READ COMMITTED     Not     Not     Poss.   Poss.   Poss.   Poss.   Poss.   Poss.
   == Degree 2      Poss.   Poss.
 Cursor Stability   Not     Not     Not     Some-   Some-   Poss.   Poss.   Some-
                    Poss.   Poss.   Poss.   times   times                   times
 REPEATABLE READ    Not     Not     Not     Not     Not     Poss.   Not     Not
                    Poss.   Poss.   Poss.   Poss.   Poss.           Poss.   Poss.
 Snapshot           Not     Not     Not     Not     Not     Some-   Not     POSS-
                    Poss.   Poss.   Poss.   Poss.   Poss.   times   Poss.   IBLE
 SERIALIZABLE       Not     Not     Not     Not     Not     Not     Not     Not
   == Degree 3      Poss.   Poss.   Poss.   Poss.   Poss.   Poss.   Poss.   Poss.
```

The Snapshot row is the paper's punchline in one line of a table: **seven
"Not Possible" cells and one "Possible", and the one is write skew.** Note the
Phantom cell too — "Sometimes Possible", not "Possible": §4.2 gives the case
that makes it sometimes ("a set of job tasks determined by a predicate cannot
have a sum of hours greater than 8"; two transactions each read the predicate,
each insert a task, neither write set intersects, both commit) and separately
observes that "Snapshot Isolation has no phantoms (in the strict sense of the
ANSI definitions A3)" — the strict A3 needs a *re-read*, and an SI transaction
re-reading a predicate always sees its own frozen snapshot. Strict-vs-broad,
Step 3, deciding a table cell.

Three remarks place SI, and no two of them say the same thing:

> **Remark 8** (§4.2). READ COMMITTED « Snapshot Isolation.
> Proof: first-committer-wins precludes P0, the timestamp mechanism prevents
> P1, and A5A is possible under READ COMMITTED but not under SI.
>
> **Remark 9** (§4.2). REPEATABLE READ »« Snapshot Isolation — *incomparable*.
> "Snapshot Isolation histories prohibit histories with anomaly A3, but allow
> A5B, while REPEATABLE READ does the opposite."
>
> **Remark 10** (§4.2). ANOMALY SERIALIZABLE « SNAPSHOT ISOLATION.
> SI precludes A1, A2 *and* A3 — so it is strictly stronger than Table 1's
> phenomena-only "SERIALIZABLE" from Step 2.

Remark 10 is the sentence to keep. Table 1's checklist — the one every tutorial
reproduces — is *passed* by an isolation level that lets two doctors take
themselves off call simultaneously. Drawn as the partial order those remarks
define (Figure 2 in §5 draws the same lattice, and additionally places Cursor
Stability and Oracle Consistent Read on it):

```
                        SERIALIZABLE == Degree 3
                     (== Date/IBM "Repeatable Read")
                        /                      \
              gap: P3 (phantom)          gap: A5B (write skew)
                      /                          \
        REPEATABLE READ   »«  incomparable  »«   Snapshot Isolation
        (Table 3 / locking)   (Remark 9)         (§4.2, first defined here)
                      \                          /
              Remark 7 \                        / Remark 8
                        \                      /
                         READ COMMITTED == Degree 2
                                  |
                         READ UNCOMMITTED == Degree 1
                                  |
                              Degree 0   (no isolation but write atomicity)

 Cursor Stability sits on the left edge, strictly between READ COMMITTED and
 REPEATABLE READ (Remark 7). ANOMALY SERIALIZABLE — Table 1's phenomena-only
 top row — sits BELOW Snapshot Isolation (Remark 10), not at the top.
```

Why it matters: "stronger isolation" is not a dial. Two levels can each forbid
something the other permits, and the ladder picture in most documentation is
the reason people are surprised when a SERIALIZABLE-labelled Oracle transaction
produces write skew — a mislabelling the SSI paper confirms was still true in
2012 ("users requesting SERIALIZABLE mode actually received snapshot isolation
(as they still do in the Oracle DBMS)", Ports & Grittner, VLDB 2012, §2).

## How to read the paper (with the concepts in hand)

~1.5 h, ten pages. The core is §3 and §4.2; the rest supports them.

1. **§1–2.1** — skim; motivation and the serializability vocabulary of Step 1.
   Do not skip the sentence distinguishing *phenomenon* from *anomaly* (§1) —
   Step 3 is built on it.
2. **§2.2 — read carefully.** The three ANSI phenomena, the notation, the
   strict/broad split, Table 1 and the ANOMALY SERIALIZABLE naming (Steps 2–3).
3. **§2.3** — Table 2, the locking levels defined by lock *scope*, *mode* and
   *duration*. Skim, but keep the long/short duration distinction; Remark 6
   needs it.
4. **§3 — read carefully.** Work H1, H2 and H3 yourself before reading the
   verdict, matching each against A1/A2/A3 and then against P1/P2/P3 (Step 4).
   Then P0 and Remark 3, then Table 3 (Steps 5–6).
5. **§4.1** — Cursor Stability, P4, P4C, H4, Remark 7 (Step 7). The shape of
   the argument matters more than the level.
6. **§4.2 — read carefully, twice.** The SI definition, H1.SI, H5, A5A/A5B, and
   Remarks 8–10 (Steps 8–10). This section is the spec your `mvcc.rs`
   implements and the hole the next chapter fills.
7. **§4.3, §5** — Oracle Read Consistency (a *statement*-level snapshot, not a
   transaction-level one), then Table 4 and Figure 2. Reproduce Table 4 from
   memory afterwards; it compresses the whole paper.

## Questions for notes.md

1. Write the doctors-on-call write skew in the paper's history notation, and
   show which forbidden phenomenon it does NOT match — check it against all
   eight columns of Table 4, not just A5B.
2. Why can't first-committer-wins catch write skew? (One sentence: the conflict
   is r→w across transactions, not w→w — Step 8's comparison never looks at a
   read set.)
3. Table 4 gives Snapshot "Not Possible" for P2 (Fuzzy Read) and "Possible" for
   A5B (Write Skew) — but §4.2 says that in the single-valued interpretation,
   "forbidding P2 also precludes A5B". Reconcile the two: which interpretation
   is each cell written in, and what does that tell you about describing a
   multi-version system with a single-version catalog?
4. Predicate phantoms in a graph: `MATCH (n:Person) WHERE n.age > 40` runs twice
   in a transaction while another transaction CREATEs a matching node. Which
   structure would M8 need to lock or validate — a label matrix? an index
   range? Is that even expressible as key locks (recall the RocksDB guide's
   Q3)?
5. Your `mvcc.rs` implements exactly the §4.2 definition. Which test maps to
   which phenomenon? Label each with its P/A number in a comment — and say
   which Table 4 columns your implementation has *no test for*.

## Takeaway

The catalog is the artifact: P0/P1/P2/P3 as history patterns (Table 3), P4 and
P4C for the levels between the rows, A5A and A5B for the multi-version levels
the single-version catalog cannot see. Snapshot isolation is defined in §4.2 of
its own critics' paper, and it is *strong* — it beats Table 1's entire
checklist (Remark 10) — but incomparable with locking REPEATABLE READ (Remark
9) and short of serializable by exactly one shape: A5B.

## Connections to this topic's experiment

Your `experiments/src/mvcc.rs` is the §4.2 spec in Rust:
`first_committer_wins_on_write_write_conflict` is the Step 8 commit rule,
`write_skew_happens_under_snapshot_isolation` is the A5B history of Step 9 and
passes *when the anomaly occurs*, and `serializable_mode_prevents_write_skew`
closes it by validating the read set — which is strictly stronger (and more
abort-happy) than the SSI machinery of the next chapter.

What this repo has measured so far is only the *baseline* that MVCC has to
beat: one global `Mutex<HashMap>`, 4 threads × 50 000 transactions × 4 ops,
**623 454 / 594 264 / 676 691 txn/s** on read-heavy 95/5, write-heavy 50/50 and
64-hot-key mixes respectively ([notes.md](notes.md), Apple M3 Pro, 2026-07-28;
[FINDINGS.md](../../FINDINGS.md) row 8). The finding is the *flatness*: a mutex
cannot exploit a read-heavy mix and cannot be hurt by a hot one, because it had
already serialized everything. The `mvcc txn/s` and `aborts` columns are still
`stub` — **this repo has not measured an MVCC implementation beating a mutex,
and no claim here should be read as if it had.** Yours will produce those
numbers, and Step 9's A5B is why the Serializable column will have a non-zero
abort count that the Snapshot column does not.

## Done when

Answer each before unfolding it.

- [ ] You can define snapshot isolation in one sentence of the paper's own vocabulary, and name the exact anomaly that separates it from serializable — without looking.

  <details><summary>Answer</summary>

  §4.2: a transaction reads from a snapshot of the committed data as of its
  Start-Timestamp (any time before its first read), sees its own writes in that
  snapshot, and at commit takes a Commit-Timestamp and succeeds only if no
  other transaction with a Commit-Timestamp inside its execution interval
  [Start-Timestamp, Commit-Timestamp] wrote data it also wrote —
  first-committer-wins.

  The anomaly is **A5B, Write Skew**:
  `r1[x] ... r2[y] ... w1[y] ... w2[x] ... (c1 and c2 occur)`. It is the single
  "Possible" cell in Table 4's Snapshot row. It survives because the commit
  test compares write sets — {y} against {x}, disjoint — and never looks at
  what either transaction read.

  </details>

- [ ] You can state the difference between A2 and P2 and give a history that separates them.

  <details><summary>Answer</summary>

  A2 is the strict reading, `r1[x] ... w2[x] ... c2 ... r1[x] ... c1` — it
  requires T1 to actually *re-read* x and get a different answer. P2 is the
  broad reading, `r1[x] ... w2[x] ... (c1 or a1)` — it fires as soon as a
  concurrent transaction writes something T1 read, re-read or not (§2.2).

  The separating history is H2 from §3:
  `r1[x=50] r2[x=50] w2[x=10] r2[y=50] w2[y=90] c2 r1[y=90] c1`. T1 reads
  x = 50 before T2's transfer and y = 90 after it, computing a total of 140
  where the truth is 100 on either side. Nothing is read twice, so A2 permits
  it; P2 forbids it at `w2[x=10]`. Remark 4 concludes from H1, H2 and H3 that
  the broad readings are the ones ANSI must have meant.

  </details>

- [ ] You can say why the paper had to invent P0, and what breaks without it.

  <details><summary>Answer</summary>

  P0 (Dirty Write), `w1[x] ... w2[x] ... (c1 or a1)`, is a second uncommitted
  transaction overwriting the first one's uncommitted write. ANSI excludes it
  only at SERIALIZABLE (§3), yet every real locking system prevents it at every
  level, because long-duration write locks are the one thing they all hold —
  so the standard failed to describe even the systems it was written for
  (Remark 3).

  Two things break. Consistency: with constraint x = y, T1 writing 1 to both
  and T2 writing 2 to both can interleave as `w1[x] w2[x] w2[y] c2 w1[y] c1`,
  leaving x = 2 and y = 1 — each transaction correct alone, the constraint dead
  (§3). Recovery: in `w1[x] w2[x] a1` you cannot restore x's before-image to
  undo T1 without erasing T2's update, and if you skip the restore, T2's own
  before-image is now wrong should T2 abort later. §3: "Even the weakest locking
  systems hold long duration write locks. Otherwise, their recovery systems
  would fail."

  </details>

- [ ] You can place snapshot isolation in the hierarchy using all three of Remarks 8, 9 and 10, and explain why the picture is not a ladder.

  <details><summary>Answer</summary>

  Remark 8: READ COMMITTED « Snapshot Isolation — SI is strictly stronger,
  because first-committer-wins precludes P0, the timestamp mechanism precludes
  P1, and read skew A5A is possible under READ COMMITTED but not under SI.
  Remark 9: REPEATABLE READ »« Snapshot Isolation — *incomparable*: SI forbids
  A3 (a re-read of a predicate always returns the frozen snapshot) which
  locking REPEATABLE READ permits, while REPEATABLE READ forbids A5B (its
  long-duration item read locks conflict with the other transaction's write)
  which SI permits. Remark 10: ANOMALY SERIALIZABLE « SNAPSHOT ISOLATION — SI
  precludes A1, A2 and A3, so it beats Table 1's entire checklist.

  It is not a ladder because Remark 9's »« is a real incomparability, not a
  missing measurement: neither level's permitted-history set contains the
  other's. Any single "isolation strength" number would have to order two
  levels that genuinely do not compare, which is exactly the mistake that lets
  a system advertise Table 1 compliance (Remark 10) while permitting the
  doctors bug.

  </details>

- [ ] You can explain why write skew has no ANSI name, structurally — not just historically.

  <details><summary>Answer</summary>

  Because ANSI's three phenomena are written entirely in terms of one
  transaction's *reads* colliding with another's *writes on the same item*, and
  in A5B — `r1[x] ... r2[y] ... w1[y] ... w2[x]` — no item is both read and
  written by the same transaction, and the two write sets are disjoint. There
  is nothing for an item-scoped, single-version phenomenon to point at. Seeing
  the bug requires holding *both* transactions' read sets and both write sets
  at once and noticing that the reads justified the writes.

  §4.2 makes the same point from the other side: A5A and A5B "could [not] arise
  in histories where P2 is precluded, since both A5A and A5B have T2 write a
  data item that has been previously read by an uncommitted T1. Thus phenomena
  A5A and A5B are only useful for distinguishing isolation levels that are
  below REPEATABLE READ in strength." In a locking world they are redundant;
  they only become *necessary* once a system stops taking read locks at all —
  which is the entire design of the multi-version levels §4.2 had to add.

  </details>

## References

**Papers**
- Berenson, Bernstein, Gray, Melton, O'Neil, O'Neil — "A Critique of ANSI SQL
  Isolation Levels" (SIGMOD 1995, pp. 1–10,
  [arXiv:cs/0701157](https://arxiv.org/abs/cs/0701157)) — ~1.5 h. Anchors used
  in this chapter:

| Where | What |
|---|---|
| §1 | phenomenon vs anomaly; the four ANSI levels |
| §2.1 | transaction, history, conflict, dependency graph, serializable |
| §2.2 | P1/P2/P3 in English; the history notation; A1–A3 vs P1–P3; Table 1; ANOMALY SERIALIZABLE |
| §2.3 | Table 2 — locking levels by scope, mode and duration; Remark 1 |
| §3 | Remark 2; H1, H2, H3; Remark 4 (broad wins); P0 and Remark 3; Table 3; Remark 6 |
| §4.1 | Cursor Stability; P4, P4C; H4; Remark 7 |
| §4.2 | Snapshot Isolation defined; H1.SI, H1.SI.SV; A5A, A5B; H5; Remarks 8, 9, 10 |
| §4.3 | Oracle Read Consistency — a per-statement snapshot, no first-committer-wins |
| §5 | Table 4 and Figure 2 — the full lattice |

- Ports & Grittner — "Serializable Snapshot Isolation in PostgreSQL"
  (VLDB 2012, [arXiv:1208.4179](https://arxiv.org/abs/1208.4179)) — §2 for the
  confirmation that SERIALIZABLE-means-SI was still shipping in 2012, and §2.1.1
  for the doctors-on-call figure. The next chapter,
  [reading-ssi-postgres.md](reading-ssi-postgres.md), reads it in full.
