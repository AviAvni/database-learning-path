# RocksDB's decade: write amp → space amp → CPU

Not a data-structures chapter — a **10-years-of-production** one. RocksDB's
development priorities shifted three times in a decade, and every shift was
driven by hardware economics rather than better algorithms. Before the paper,
this chapter walks the arc one era at a time — what hardware fact made each
metric the bottleneck, and what RocksDB changed in response — then points you
at the sections where the fleet-scale lessons live. Read it for what
benchmarks don't show: the failure modes, API regrets, and configuration
sprawl that only appear at fleet scale.

## The problem in one sentence

The "right" LSM configuration is not a property of the algorithm but of the
hardware bill: over one decade the binding constraint at Facebook moved from
SSD *endurance* (write amp) to SSD *capacity* ($/GB — space amp) to *CPU*
(NVMe made storage faster than the code driving it) — three different
objective functions for the same engine.

## The concepts, step by step

### Step 1 — the arc: one engine, three objective functions

A production storage engine is tuned to whichever resource currently runs
out first — and at fleet scale that resource is decided by procurement, not
computer science. The paper's history of what RocksDB optimized for, in
order:

```
2012 ───────► 2015 ───────► 2018 ───────► 2021
write amp     space amp     CPU           disaggregated / remote storage
(SSD wear,    (SSDs got     (storage got  (storage moves off-box;
 fillrandom    cheaper —     fast enough   topic 28 territory)
 benchmarks)   $/GB rules)   that CPU is
                             the bottleneck)
```

Each shift happened because the *hardware economics* moved, not because the
algorithms improved. This is the RUM triangle (topic 1) steered by
procurement — the same trade-off space, with the weights set by the price
list. Steps 2–5 take the eras one at a time.

### Step 2 — the write-amp era: flash wears out

Write amplification (bytes physically written to flash per byte of user
data) was the founding obsession because flash cells have a finite erase
budget — a 2012-era SSD tolerated only a few thousand program/erase cycles
per cell, so an engine with WA 30 wears the drive out 30× faster than the
raw data rate suggests, and at fleet scale that is a hardware replacement
line-item. RocksDB's founding pitch over its LevelDB ancestor was exactly
this: batch more, merge smarter, benchmark `fillrandom` WA. This is the era
your mini-LSM's write-amp experiment recreates.

### Step 3 — the space-amp era: $/GB beats endurance

By ~2015 SSDs had gotten cheap and durable enough that the dominant cost
was simply *how many bytes of flash you must buy* — space amplification
(bytes on disk per byte of live data). This flipped the compaction
preference: leveled compaction's *high* write amp became acceptable because
its space amp is excellent (~1.1×, since the bottom level is one run holding
~90% of data with few stale versions — the Dostoevsky chapter's Step 2 in
production dollars), while tiered's up-to-K× space overhead priced it out of
most Facebook services. Universal (tiered) compaction survived only for
ingest-heavy workloads. When storage is billed by the byte-month, WA is a
tax you pay once; space amp is rent you pay forever.

### Step 4 — the CPU era: NVMe outran the software

Around 2018, NVMe drives delivering hundreds of thousands of IOPS at tens of
microseconds stopped being the bottleneck — the CPU cycles spent *per
operation* (merge comparisons, block decode and decompression, filter
hashing at every level, checksum verification) became the limiting resource.
The optimization target moved inside the CPU: cheaper comparators, less
decompression on the read path, filter designs trading build CPU for DRAM
(the ribbon filter from the compaction chapter is this era's artifact).
Reconcile with your topic 0 finding — SipHash at 21%, memory stalls dominant
— and the LSM adds its own CPU stack on top of a hash table's.

### Step 5 — the remote-storage era: the disk leaves the box

By 2021 the direction is **disaggregated storage** — SSTs living on shared
remote storage (network-attached), with compute and capacity scaled
independently and compaction potentially offloaded to other machines
("remote compaction"). The economics again: pooled storage beats stranded
per-box capacity at fleet scale. The predictions in §5 are 2021-vintage and
checkable — topic 28 will grade them.

### Step 6 — the fleet-scale lessons: what only production teaches

The paper's most valuable section (§4) is not about performance at all;
it's what running the engine on hundreds of thousands of machines proves:

- **Silent corruption is a certainty, not a risk.** At fleet scale,
  "unlikely" bit-flips (controller bugs, RAM, kernel) happen daily —
  which is why RocksDB checksums at *every* layer independently: per block,
  per file, per WAL record, trusting no layer below.
- **API regrets are forever**: sequence numbers and (missing) user
  timestamps leaked into the public API early and constrain everything
  since.
- **Configuration sprawl is an acknowledged failure**: hundreds of knobs,
  most users unable to set them — the price of a decade of "add an option"
  compromises (contrast Monkey/Dostoevsky's "solve for the knob" ethos).

These are the parts benchmarks can't show and the reason to read a TODS
retrospective instead of another asymptotic analysis.

## How to read the paper (with the concepts in hand)

1. §1–2 — background + the resource-priority history (Steps 1–5's arc, in
   the authors' words).
2. §3 — lessons on compaction: why leveled won at Facebook (space, Step 3),
   universal kept for ingest-heavy; the tiered-vs-leveled discussion with
   production numbers instead of asymptotics.
3. **§4 — large-scale lessons** (Step 6). The best section: failure
   handling and layered checksums, the timestamp/seqno API regrets,
   configuration sprawl.
4. §5 — future directions (2021 vintage, Step 5): remote compaction, tiered
   storage — check which happened (topic 28 will).

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
