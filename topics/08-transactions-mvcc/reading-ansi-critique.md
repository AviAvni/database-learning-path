# Isolation levels, made rigorous: history patterns and write skew

Berenson et al.'s SIGMOD '95 critique is the paper that made isolation
rigorous — and, accidentally, the paper that NAMED snapshot isolation and
its flaw, seven years before anyone shipped a fix. Before you open it, this
chapter builds the vocabulary from zero: what a history is, why prose
definitions of isolation fail, the pattern catalog that replaced them, and
where snapshot isolation lands in the resulting hierarchy. Read it before
the SSI chapter or that one won't land.

## The problem in one sentence

ANSI SQL-92 defined its four isolation levels with three sentences of
English prose so ambiguous that the industry spent a decade shipping
incompatible things under the same names — Oracle sold snapshot isolation
labeled **SERIALIZABLE** for years, silently permitting an anomaly (write
skew) that the standard's authors never wrote down.

## The concepts, step by step

### Step 1 — a history: concurrency reduced to one interleaved string

A **transaction** is a group of reads and writes that must behave as one
atomic unit, and a **history** is the actual interleaved order in which the
database executed the operations of several concurrent transactions. The
paper's entire method is to stop arguing about prose and write histories in
a four-symbol notation:

```
 r1[x]   transaction 1 reads item x
 w1[x]   transaction 1 writes item x
 c1      transaction 1 commits
 a1      transaction 1 aborts
```

So `w1[x] r2[x] a1` is a complete, unambiguous description of "T2 read T1's
uncommitted write, and then T1 aborted" — T2 read data that never existed.
One line replaces a paragraph, and two people can now check mechanically
whether a given execution is allowed. That precision is the paper's whole
contribution; everything else follows from it.

### Step 2 — isolation levels are defined by their bugs

An **isolation level** is not a feature list — it is a contract about which
**anomalies** (specific broken history shapes, like the dirty read above)
the database promises to prevent. Lower levels permit more anomalies in
exchange for more concurrency; **serializable**, the top level, promises
the result is equivalent to *some* one-at-a-time execution of the
transactions — no anomaly of any shape.

This bottom-up view matters because it is checkable: given a history, you
can pattern-match it against the forbidden shapes. "Which anomalies does my
level permit?" is the only question that survives contact with a real bug
report.

### Step 3 — why prose fails: the strict/loose ambiguity

ANSI defined each anomaly in English, and the paper's Section 3 shows the
prose supports two incompatible readings. A *strict* reading — forbid only
the exact completed anomaly sequence — permits histories everyone agrees
are broken (e.g. two uncommitted transactions overwriting each other's
writes, which ANSI never mentions at all). A *loose* reading — forbid any
history that could ever extend into the anomaly — over-forbids and outlaws
harmless executions. Two vendors could both "conform" and behave
differently on the same workload.

The fix is Section 3's move: redefine every phenomenon as a **history
pattern** in the Step 1 notation — a shape of interleaved operations, with
no interpretation left to the reader.

### Step 4 — the pattern catalog: P0 through A5B

Here is the catalog to internalize (worth the hour alone) — each line is a
forbidden interleaving shape, in Step 1's notation:

```
 P0 dirty write     w1[x] … w2[x]            (both uncommitted)
 P1 dirty read      w1[x] … r2[x]            before c1/a1
 P2 fuzzy read      r1[x] … w2[x]            before c1
 P3 phantom         r1[P] … w2[y in P]       predicate P, not item!
 P4 lost update     r1[x] … w2[x] … w1[x]
 A5A read skew      r1[x] … w2[x] w2[y] c2 … r1[y]
 A5B write skew     r1[x] r1[y] … w2[y] … w1[x]   (your doctors test!)
```

Two corrections in this table restructured the field:

- **P3, the famous one**: ANSI's phantom was item-based; real phantoms are
  **predicate**-based. `r1[P]` means "T1 ran a query whose WHERE clause is
  predicate P" — and `w2[y in P]` inserts a *new* row matching P. Locking
  every row you read doesn't help, because the dangerous row didn't exist
  when you read. You must somehow lock rows that *would have* matched.
- **The lost-update ladder (P4)**: ANSI REPEATABLE READ, as literally
  written, permits P4 — a read-modify-write silently overwriting another
  transaction's committed write. Every locking implementation of RR
  prevents it. The prose and the implementations had diverged for a decade
  without anyone noticing, because nobody had written the patterns down.

### Step 5 — snapshot isolation: defined here, in its critics' paper

**Snapshot isolation (SI)** is the scheme where every transaction reads
from a frozen **snapshot** — the database exactly as of its start time,
ignoring all later commits — and writes are checked only against writes:
if two overlapping transactions write the same item, the **first committer
wins** and the second aborts. No reader ever blocks a writer or vice versa.

Section 4 of this paper is where SI was first formally defined — by the
people about to expose its flaw. Measured against Step 4's catalog, SI
forbids P0–P2, P4, and A5A: no dirty anything, no fuzzy reads, no lost
updates, no read skew. That places it strictly ABOVE ANSI Repeatable Read.
It looks, at first inspection, indistinguishable from serializable.

### Step 6 — write skew: the anomaly that dethrones SI

Look at A5B again: `r1[x] r1[y] … w2[y] … w1[x]`. T1 reads both items and
writes x; T2 reads both and writes y. The write sets `{x}` and `{y}` are
**disjoint** — so first-committer-wins, which only compares write sets,
sees no conflict and lets both commit. But each transaction's write was
justified by a read the other invalidated. The topic README's
doctors-on-call test is exactly this: invariant "at least one doctor on
call", both transactions verify it against their snapshot, each removes a
different doctor, both commit, invariant dead.

So SI permits A5B while locking-based Repeatable Read (which holds read
locks) forbids it — yet SI forbids phantoms that RR permits. Neither
dominates: the hierarchy is a **partial order**, not a ladder:

```
            Serializable
             /        \
   SI (no P4, allows A5B)   Repeatable Read (locking; no A5B via locks,
             \        /       allows phantoms P3)
            Read Committed
                 |
            Read Uncommitted
```

Why it matters: Oracle shipped SI *as* "serializable" for years. Postgres
called it "repeatable read" (honest) and later added SSI on top (next
guide). And your `mvcc.rs` experiment implements Section-4 SI verbatim —
including the test that *demonstrates* write skew before you prevent it.

## How to read the paper (with the concepts in hand)

~1.5 h. The core is §3 and §4; the rest supports them.

1. **§1–2** — skim; motivation and the ANSI prose being critiqued. You have
   the punchline already (Step 3).
2. **§3 — read carefully.** The strict/loose ambiguity argument and the
   history-pattern redefinitions (Steps 3–4). Work each P-pattern by
   writing out a concrete history that matches it. Don't skip the P3
   discussion — predicate vs item is the correction people still get wrong.
3. **§4 — read carefully.** The formal definition of SI and its placement
   in the hierarchy (Steps 5–6). This section is the spec your `mvcc.rs`
   implements.
4. **Tables and the level-hierarchy figure** — reproduce them from memory
   afterwards; they compress the whole paper.

## Questions for notes.md

1. Write the doctors-on-call write skew in the paper's history notation,
   and show which forbidden phenomenon it does NOT match (that's why SI
   lets it through).
2. Why can't first-committer-wins catch write skew? (One sentence: the
   conflict is r→w across txns, not w→w.)
3. Predicate phantoms in a graph: "MATCH (n:Person) WHERE n.age > 40"
   ran twice in a txn while another txn CREATEs a matching node. Which
   structure would M8 need to lock/validate — a label matrix? an index
   range? Is that even expressible as key locks (recall RocksDB guide Q3)?
4. Your mvcc.rs implements exactly Section-4 SI. Which tests map to which
   phenomena? (Label each test with its P/A number in a comment.)

## Done when

You can define SI in one sentence of history notation and name the exact
anomaly that separates it from serializable — without looking.

## References

**Papers**
- Berenson, Bernstein, Gray, Melton, O'Neil, O'Neil — "A Critique of ANSI
  SQL Isolation Levels" (SIGMOD 1995,
  [arXiv:cs/0701157](https://arxiv.org/abs/cs/0701157)) — ~1.5 h; §3's
  history notation and §4's SI definition are the core
