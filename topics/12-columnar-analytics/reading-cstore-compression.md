# C-Store: operate on compressed data

Every system in this topic descends from two papers out of the same
lab, read here as a pair: C-Store proposes the column-store
architecture, and the SIGMOD '06 follow-up proves the thesis this
topic is named for — the executor should OPERATE ON compressed data,
not just store it. Twenty years on, the value is seeing which of the
original bets survived, and in what disguise. Before you open either
paper, this chapter builds the ideas step by step, then hands you a
reading route through both.

## The problem in one sentence

2005's row-store OLTP engines read every column of every row to answer
analytics that touch 3 columns of 100 — a 30× bandwidth waste before
any work happens — and C-Store's answer (store columns separately,
sorted, compressed) raised a second question the follow-up paper
answers: once the data is compressed, must you decompress it to
compute?

## The concepts, step by step

### Step 1 — the column store: read only what the query touches

A **column store** keeps each column of a table in its own contiguous
file, so a query reads only the columns it mentions — a
`SELECT sum(price) WHERE date > X` on a 100-column, 400-byte-row table
reads ~16 of every 400 bytes instead of all of them, a 25× IO cut
before any cleverness. The second, less obvious win: a column is
SELF-SIMILAR — one type, similar values, often sorted or clustered —
which is exactly the shape every lightweight encoding (RLE,
dictionary, bit-packing) feeds on; rows interleave types and kill
every trick. Why it matters: everything in this topic — DuckDB,
ClickHouse, Parquet — is a descendant of this one storage decision.

### Step 2 — projections: store the table several times, each sorted differently

A **projection** in C-Store is a copy of some columns of the table
stored physically sorted on a chosen key — and the same table can have
several projections, each sorted differently, so each query picks the
copy whose sort order serves it. This is worth dwelling on because
**sort order is THE enabler**: a column sorted (or clustered) on the
filter key gets long RLE runs (compression) and tight min/max zones
(zone-map pruning) — clustering decides compressibility, stated in
2005; ClickHouse's mandatory `ORDER BY` is the same lesson made a
schema requirement. The cost that killed the idea in its full form:
every extra sorted copy multiplies storage and, worse, write
amplification — each insert lands in every projection. Why it matters:
the idea died as a default and survived as an option (ClickHouse's
secondary "projections" feature is literally named after it, paid for
by the merge machinery).

### Step 3 — WS/RS: writes and reads live in different structures

C-Store splits the store in two: a small **WS** (writeable store — an
uncompressed, insert-friendly structure that absorbs writes) and a
large **RS** (read store — the compressed, sorted projections), with a
background **tuple mover** batch-migrating WS contents into RS. Sound
familiar? It's the LSM shape (topic 4: mutable in-memory buffer +
immutable sorted bulk + background merge) invented independently for
analytics — surviving today as delta + main (SAP HANA) and inserts +
parts (ClickHouse). Why it matters: every read-optimized layout in
this book, columnar or graph (FalkorDB's Delta_Matrix, topic 13),
grows this same two-structure answer, because a layout can be
write-friendly or read-optimal but not both.

### Step 4 — positions: the currency of late materialization

A **position** is a row's ordinal within a projection (row 0, 1, 2 …),
and C-Store's operators exchange **position lists or bitmaps** — "rows
17, 204, 9,881 survived the filter" — instead of assembled tuples.
**Late materialization** is the resulting discipline: run filters and
joins on the cheap columns first, carry positions through the plan,
and fetch the wide payload columns only for final survivors. At 1%
selectivity on 1M rows, that's 10K payload fetches instead of 1M
decodes. Survives as DuckDB's selection vectors (topic 11) and
Parquet's late decode. Why it matters: positions are what let the next
step's compressed operators avoid ever building a row.

### Step 5 — the SIGMOD '06 thesis: execute per-run, not per-row

The follow-up paper's experiment: implement RLE, dictionary,
bit-packing, LZ, and null suppression in a column executor, then
compare **decompress-then-process** against **process-compressed** —
operators that understand the encoding and work on it directly:

```
 decompress-then-process:  [decode all] -> [scan rows]     bandwidth + work per ROW
 process-compressed:       [scan runs/codes directly]      work per RUN / per code
```

Operating on RLE is a different complexity class: `SUM` over a run =
value × length; a predicate evaluates ONCE per run, not per row —
sorted low-cardinality columns get speedups proportional to average
run length (the paper shows order-of-magnitude wins). The whole thesis
fits in one loop — a filtered SUM over RLE that never materializes a
row:

```rust
struct Run { value: u64, len: u32 }

// decompress-then-process is O(rows); this is O(runs).
// sorted low-cardinality columns: runs ≪ rows, often by 1000x
fn sum_where_gt(runs: &[Run], threshold: u64) -> u64 {
    let mut sum = 0;
    for r in runs {
        if r.value > threshold {               // predicate: ONCE per run
            sum += r.value * r.len as u64;     // aggregate: multiply, don't decode
        }
    }
    sum
}
```

Dictionary codes compose with Step 4's late materialization: compare
encoded ints, decode only survivors — string predicates become int
predicates (your scan_bench reproduces both effects). Why it matters:
compression stops being a storage tax paid at scan time and becomes
the executor's fast path.

### Step 6 — the lightweight/heavyweight split, proven

The same experiment condemns heavyweight codecs for the scan path:
LZ-class compression saved IO but cost CPU per block and offered **no
execution shortcuts** — there is no "sum a gzip block" trick, you must
inflate it. Lightweight encodings both shrink bytes AND admit
per-run/per-code execution; gzip-class codecs belong at rest. Why it
matters: this 2006 finding is Parquet's two compression layers
(semantic then block) and DuckDB's zstd-as-last-resort, decided twenty
years in advance.

### Step 7 — the abstraction that makes it maintainable

The naive implementation of process-compressed needs encodings ×
operators variants — 5 encodings × 20 operators = 100
implementations, unmaintainable. The paper's fix: operators consume
"compressed blocks" through an API exposing *properties* (isRLE?
isSorted? oneValue?) so each operator writes a few property-driven
cases, not one per encoding. DuckDB's vector-type flags
(FLAT/CONSTANT/DICTIONARY/FSST, topic 11) are this API, shipped in
production. Why it matters: this is the difference between a benchmark
paper and an architecture — the abstraction is what let the idea
survive into real engines.

## How to read the papers (with the concepts in hand)

**C-Store (VLDB '05)** — read for the architecture bets and score
them against twenty years of history:

| C-Store bet | survived as |
|---|---|
| columns, not rows, for reads | everything in this topic |
| projections: same table stored MULTIPLE times, each sorted differently | mostly died (storage cost); echoes in ClickHouse ORDER BY + materialized views, secondary "projections" feature literally named after it |
| WS/RS split: writeable store + read store, tuple mover between | LSM-shaped! delta + main (SAP HANA), parts + inserts (ClickHouse) |
| compression per column, chosen by data properties | DuckDB's analyze/score |
| late materialization: join on position lists, fetch payload last | DuckDB selection vectors, Parquet late decode |
| k-safety via projection redundancy instead of RAID | died; replication won |

Read the storage model (projections, WS/RS) carefully — Steps 2–3;
skim the k-safety and recovery sections (that bet died; replication
won). Watch for positions-as-join-currency (Step 4) — selection
vectors avant la lettre.

**SIGMOD '06** — read the experiment design, then internalize the
findings list: per-run execution (Step 5), the lightweight/heavyweight
split (Step 6), and the properties API (Step 7). The graphs showing
speedup vs average run length are the quantitative core — compare
them against your own scan_bench numbers.

## Questions for notes.md

1. SUM over RLE runs is O(runs). Which OTHER aggregates stay
   run-shortcuttable (min/max? count? avg?) and which break (distinct?
   median?)?
2. Projections died of write amplification. ClickHouse's projections
   feature revives them WITH the merge machinery paying the cost —
   what changed to make it affordable? (Background merges as the
   universal work-absorber.)
3. The WS/RS + tuple-mover design is an LSM with different names. Map
   the four components onto topic 4's vocabulary.
4. Position lists vs bitmaps for intermediate results: when does each
   win? (Selectivity — connect to your topic 11 select-vs-compact
   question.)
5. M12: `WHERE n.country = 'IL'` on a dictionary-encoded property
   column — write the process-compressed plan (code lookup, int
   compare, positions out) and count decodes for 1% selectivity.

## Done when

You can state the SIGMOD '06 thesis in one sentence ("expose encoding
properties to operators; execute per-run/per-code, decode losers
never"), and map C-Store's four big bets to their modern descendants.

## References

**Papers**
- Stonebraker et al. — "C-Store: A Column-oriented DBMS" (VLDB 2005)
  — read for the architecture bets and which survived twenty years
- Abadi, Madden, Ferreira — "Integrating Compression and Execution in
  Column-Oriented Database Systems" (SIGMOD 2006) — the
  compression-aware-execution experiment; internalize the findings list
  above
