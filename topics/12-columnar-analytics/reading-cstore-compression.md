# Reading guide — C-Store (VLDB '05) + "Integrating Compression and Execution" (SIGMOD '06) (~1.5 h)

The founding papers, read as a pair: C-Store proposes the architecture;
the SIGMOD '06 paper (Abadi, Madden, Ferreira — same lab) proves the
thesis this topic is named for: the executor should OPERATE ON
compressed data, not just store it.

## C-Store: the architecture bets (VLDB '05)

Read for which bets survived twenty years:

| C-Store bet | survived as |
|---|---|
| columns, not rows, for reads | everything in this topic |
| projections: same table stored MULTIPLE times, each sorted differently | mostly died (storage cost); echoes in ClickHouse ORDER BY + materialized views, secondary "projections" feature literally named after it |
| WS/RS split: writeable store + read store, tuple mover between | LSM-shaped! delta + main (SAP HANA), parts + inserts (ClickHouse) |
| compression per column, chosen by data properties | DuckDB's analyze/score |
| late materialization: join on position lists, fetch payload last | DuckDB selection vectors, Parquet late decode |
| k-safety via projection redundancy instead of RAID | died; replication won |

- The sorted-projection idea is worth dwelling on: sort order is THE
  enabler for RLE + zone maps (clustering decides compressibility —
  the ClickHouse ORDER BY lesson, stated in 2005).
- Positions (row ids within a projection) as the join currency between
  columns: operators exchange position BITMAPS/lists, not tuples —
  selection vectors avant la lettre.

## SIGMOD '06: compression-aware execution

The experiment: implement RLE, dictionary, bit-packing, LZ, null
suppression in a column executor, then compare two modes —
decompress-then-process vs process-compressed.

Findings to internalize:

- **Operating on RLE is a different complexity class**: `SUM` over a
  run = value × length; a predicate evaluates ONCE per run, not per
  row. Sorted low-cardinality columns get speedups proportional to
  average run length (they show order-of-magnitude wins).
- **Dictionary codes compose with late materialization**: compare
  encoded ints, decode only survivors. String predicates become int
  predicates (your scan_bench reproduces both of these).
- **Heavyweight (LZ) compression saved I/O but cost CPU per block with
  no execution shortcuts** — the case for LIGHTWEIGHT encodings in the
  scan path; gzip-class codecs belong at rest (Parquet's two layers).
- The abstraction that makes it maintainable: operators consume
  "compressed blocks" through an API exposing properties (isRLE?
  isSorted? oneValue?) so each operator needs a few cases, not
  encodings × operators implementations. DuckDB's vector-type flags
  (FLAT/CONSTANT/DICTIONARY/FSST, topic 11) are this API, shipped.

```
 decompress-then-process:  [decode all] -> [scan rows]     bandwidth + work per ROW
 process-compressed:       [scan runs/codes directly]      work per RUN / per code
```

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
