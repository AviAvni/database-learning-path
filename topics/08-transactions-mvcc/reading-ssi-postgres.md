# SSI: serializable snapshot isolation without blocking anyone

How postgres turned SI into SERIALIZABLE with passive markers instead of
blocking locks — Ports & Grittner's VLDB '12 account of productionizing
Cahill's dangerous-structure theorem. Before the paper, this chapter
builds the theory one edge at a time: the hole in SI, the
rw-antidependency, the theorem that reduces every anomaly to one shape,
and the engineering that made detecting that shape cheap enough to ship.
Prereq: the Berenson critique
([reading-ansi-critique.md](reading-ansi-critique.md)) — you need write
skew cold.

## The problem in one sentence

Snapshot isolation permits write skew, and the classical fix — two-phase
locking — makes readers block writers again, throwing away SI's whole
value; SSI gets full serializability for ~7% overhead by *watching* for
one specific conflict shape and aborting somebody, blocking no one, ever.

## The concepts, step by step

### Step 1 — the hole, restated as a target

Under SI (snapshot isolation — every transaction reads a frozen snapshot,
and only write-write conflicts abort), two transactions can each read data
the other is about to change, write disjoint items, and both commit —
write skew, the doctors-on-call bug. The conflict is invisible to
first-committer-wins because it lives in the *read→write* crossings, not
in the write sets.

So the engineering target is precise: detect harmful read→write crossings
between concurrent transactions, cheaply, without making reads take
blocking locks. Everything below is that one sentence, made rigorous and
then made fast.

### Step 2 — the rw-antidependency: the edge that matters

An **rw-antidependency** is the relationship "T read something, then a
concurrent U wrote it": T's snapshot didn't include U's write, so U has,
in effect, *un-read* T's view — if these two were serialized, T would have
to come BEFORE U for T's read to make sense. Draw it as an arrow
`T ──rw──► U`.

Concrete: T1 reads bob's row (`r1[bob]`), concurrent T2 later writes it
(`w2[bob]`) ⇒ `T1 ──rw──► T2`. Note the asymmetry with ordinary conflicts:
nobody waited, nobody failed — the edge is just a *fact about the
interleaving*, recordable at the moment the write happens if someone
remembered the read. These edges are the raw material; one edge alone is
harmless.

### Step 3 — the dangerous structure: Cahill's theorem

Cahill's theorem (SIGMOD '08): every non-serializable SI execution
contains **two consecutive rw-antidependencies** — a transaction with one
inbound AND one outbound rw edge, called the **pivot** — with the further
condition that the downstream transaction commits first:

```
        rw            rw
  T_in ────► T_pivot ────► T_out        rw edge: T reads x, then U writes x
                                        (U "un-reads" T's snapshot)
  … and T_out commits FIRST of the three.
```

So you don't need to build the full serialization graph and search it for
cycles (the textbook-correct but expensive answer). Track rw edges; when
any transaction accumulates both directions — it became a pivot — abort
somebody. The whole detector is two flags per transaction and one rule:

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

This is **conservative**: some aborted histories were actually fine (a
pivot doesn't guarantee a full cycle) — but it never misses a real one.
Check it against write skew: T1 reads bob's row (later written by T2) ⇒
`T1 ──rw──► T2`; T2 reads alice's row (later written by T1) ⇒
`T2 ──rw──► T1`. A cycle of length 2 — *each* transaction is a pivot, and
the detector fires. Why it matters: false positives cost a retry;
false negatives would cost correctness. SSI buys certainty with spare
aborts.

### Step 4 — SIREAD locks: remembering reads without blocking anyone

To record rw edges you must remember who read what — that's the **SIREAD
lock**, which despite the name blocks nothing: it is a passive marker
"transaction T read this", checked by writers to generate edges. Three
engineering moves make the memory bill payable:

- **Granularity with escalation**: markers exist at tuple, page, and
  relation level; under memory pressure, many fine markers collapse into
  one coarse one. Coarser = more false rw edges = more false aborts —
  never wrong results. (Correctness is one-directional here; only
  performance degrades.)
- **Predicate reads**: "reads" include rows that *would have* matched — the
  phantom problem from the Berenson chapter. SSI handles it by marking the
  index RANGE the query scanned (via index pages), so an insert into that
  range generates the edge. This is the answer to phantoms that key-based
  OCC (RocksDB Q3) cannot give.
- **Outliving commit**: an rw edge can form AFTER the reader commits (a
  later writer hits its marker), so SIREAD state must survive commit and
  is only cleaned when all overlapping transactions end — question 1 makes
  you construct the history that requires this.

### Step 5 — the paper's refinements on raw Cahill

Ports & Grittner's production additions (§4–§7):

1. **Commit ordering refinement** — only abort when T_out actually
   committed first of the three (the theorem's full condition); raw
   pivot-detection over-fires, this trims false positives.
2. **Safe snapshots & DEFERRABLE** — a read-only transaction has no writes,
   hence can never have an *inbound* rw edge, hence can never be a pivot;
   if it can additionally prove no concurrent writer endangers it, it
   drops ALL tracking. `BEGIN ... DEFERRABLE` waits for such a safe
   snapshot: long read-only backups then run at serializable for *zero*
   overhead.
3. **Memory bounding** — §7's summarization: when tracking state hits its
   RAM budget, collapse old transactions' state into a summary
   (conservative again — more false aborts, bounded memory).
4. **Two-phase commit interaction** — prepared transactions can linger
   indefinitely between prepare and commit, wedging cleanup; §7 explains
   why they make everything worse.

### Step 6 — the price tag, honestly (§8)

~7% throughput overhead at low contention on their benchmarks — the
markers and edge checks are cheap. The real cost is **aborts**, and it is
workload-shaped: hot read-write pairs create pivot storms. And the
contract shifts to the application: a serialization failure (SQLSTATE
40001) means "run the whole transaction again" — SSI only delivers
serializable semantics if the app retries in a loop. No retry loop, no
serializability; just errors.

## How to read the paper (with the concepts in hand)

~1.5 h. §4–§7 are the production engineering, §8 the honest costs.

1. **§1–2** — skim; SI background and the write-skew motivation you
   already have (Step 1).
2. **§3** — the theory: rw-antidependencies and the dangerous structure
   (Steps 2–3). Verify the doctors example against Fig-level detail;
   confirm the "T_out commits first" condition.
3. **§4–§5 — read carefully.** SIREAD locks, granularity escalation,
   index-range predicate handling, commit-ordering refinement, safe
   snapshots (Steps 4–5). This is the part no other system had built.
4. **§6–§7** — memory bounding and 2PC; read §7 for *why* state must
   outlive commit — it's the subtlest correctness point in the paper.
5. **§8 — read the numbers.** Overhead vs abort rate; note which
   benchmarks stress pivot storms (Step 6).

For Cahill et al. (SIGMOD '08): the theorem statement is enough — this
paper productionizes it.

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
