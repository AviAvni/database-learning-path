# RocksDB's decade: write amp → space amp → CPU

Not a data-structures chapter — a **10-years-of-production** one. RocksDB's
development priorities shifted three times in a decade, and every shift was
driven by hardware economics rather than better algorithms. Read this for what
benchmarks don't show: the failure modes, API regrets, and configuration
sprawl that only appear at fleet scale.

## The arc (what to extract)

The paper's history of what RocksDB optimized for, in order:

```
2012 ───────► 2015 ───────► 2018 ───────► 2021
write amp     space amp     CPU           disaggregated / remote storage
(SSD wear,    (SSDs got     (storage got  (storage moves off-box;
 fillrandom    cheaper —     fast enough   topic 28 territory)
 benchmarks)   $/GB rules)   that CPU is
                             the bottleneck)
```

Each shift happened because the *hardware economics* moved, not because the
algorithms improved. Leveled compaction's high write amp was acceptable the
moment space amp mattered more — the RUM triangle steered by procurement.

## Read in this order

1. §1–2 — background + the resource-priority history (the arc above).
2. §3 — lessons on compaction: why leveled won at Facebook (space), universal
   kept for ingest-heavy; the tiered-vs-leveled discussion with production
   numbers instead of asymptotics.
3. **§4 — large-scale lessons.** The best section:
   - failure handling: silent corruption found by checksums *at every layer*
     (block, file, WAL record) — corruption rates at fleet scale make
     "unlikely" a certainty;
   - the timestamp/seqno API regrets;
   - configuration sprawl (hundreds of knobs) as an acknowledged failure.
4. §5 — future directions (2021 vintage): remote compaction, tiered storage —
   check which happened (topic 28 will).

## Questions to answer in notes.md

1. The paper says CPU became the bottleneck once NVMe arrived. Reconcile with
   your topic-0 finding (SipHash 21%, memory stalls dominant): which CPU costs
   does an LSM add on top of a hash table's? (Comparisons in merges, block
   decode/decompress, filter hashing per level.)
2. Why does RocksDB checksum at block AND file AND WAL-record level rather
   than trusting the filesystem? What's the FalkorDB/redis equivalent story?
   (RDB has a CRC; AOF... check.)
3. Pick the lesson from §4 most relevant to the capstone and write one
   paragraph on how it changes your M4 design.

## Done when

You can narrate the write-amp → space-amp → CPU priority arc with the hardware
reason for each transition.

## References

**Papers**
- Dong, Kryczka, Jin, Stumm — "RocksDB: Evolution of Development
  Priorities in a Key-value Store Serving Large-scale Applications"
  (ACM TODS 2021) — §4 (large-scale lessons) is the best section; §5's
  2021-vintage future directions are checkable predictions for topic 28
