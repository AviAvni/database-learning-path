# Reading guide — "A Critique of ANSI SQL Isolation Levels" (Berenson et al., SIGMOD '95) (~1.5 h)

The paper that made isolation rigorous — and, accidentally, the paper that
NAMED snapshot isolation and its flaw, seven years before anyone shipped a
fix. Read it before the SSI paper or the SSI paper won't land.

## The setup

ANSI SQL-92 defined isolation levels by three prose "phenomena" (dirty
read, non-repeatable read, phantom). The authors show the prose is
ambiguous: a *strict* reading (only forbid the exact anomaly sequence)
permits histories everyone agrees are broken; a *loose* reading
over-forbids. Section 3's move: redefine everything as **history patterns**
over reads/writes/commits/aborts.

## The notation to internalize (worth the hour alone)

```
 P0 dirty write     w1[x] … w2[x]            (both uncommitted)
 P1 dirty read      w1[x] … r2[x]            before c1/a1
 P2 fuzzy read      r1[x] … w2[x]            before c1
 P3 phantom         r1[P] … w2[y in P]       predicate P, not item!
 P4 lost update     r1[x] … w2[x] … w1[x]
 A5A read skew      r1[x] … w2[x] w2[y] c2 … r1[y]
 A5B write skew     r1[x] r1[y] … w2[y] … w1[x]   (your doctors test!)
```

- The P3 correction is the famous one: ANSI's phantom was item-based;
  real phantoms are **predicate**-based. Locking a row you read doesn't
  lock the rows that WOULD have matched.
- The lost-update ladder: ANSI REPEATABLE READ (as literally written)
  permits P4. Locking-based RR doesn't. The prose and the implementations
  had diverged for a decade.

## Snapshot isolation, defined and dethroned

Section 4 defines SI: reads from a snapshot, first-committer-wins on
writes. Then the twist that structures your whole experiments crate:

- SI forbids P0–P2, P4, A5A — it sits ABOVE ANSI Repeatable Read.
- SI permits **A5B write skew** — so it's incomparable to serializable.
- Hence the paper's hierarchy is a partial order, not a ladder:

```
            Serializable
             /        \
   SI (no P4, allows A5B)   Repeatable Read (locking; no A5B via locks,
             \        /       allows phantoms P3)
            Read Committed
                 |
            Read Uncommitted
```

Oracle shipped SI *as* "serializable" for years. Postgres called it
"repeatable read" (honest) and later added SSI on top (next guide).

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
