# RocksDB's decade: write amp → space amp → CPU

Not a data-structures chapter — a **production retrospective**. RocksDB's
development priorities shifted three times in eight years, and every shift was
driven by hardware economics rather than better algorithms. Before the paper,
this chapter walks the arc one era at a time — what hardware fact made each
metric the bottleneck, and what RocksDB changed in response — then points you
at the sections where the fleet-scale lessons live. Read it for what
benchmarks don't show: the failure modes, API regrets, and configuration
sprawl that only appear at fleet scale.

**A note on which paper this is.** The title is *Evolution of Development
Priorities in Key-value Stores Serving Large-scale Applications: The RocksDB
Experience*, by Dong, Kryczka, Jin and Stumm. It appeared at **USENIX FAST
'21** and, extended, as ACM **Transactions on Storage** 17(4), Article 26 (TOS,
not TODS). Every section number, quotation and figure below is checked against
the openly available FAST '21 version — so if you are reading the journal
version, expect the section numbering to differ slightly.

## The problem in one sentence

The "right" LSM configuration is not a property of the algorithm but of the
hardware bill: over eight years the binding constraint at Facebook moved from
SSD *endurance* (write amp) to SSD *capacity* ($/GB — space amp) to *CPU and
DRAM price* — three different objective functions for the same engine.

## The concepts, step by step

### Step 1 — the arc: one engine, three objective functions

> **In:** RocksDB as you know it from the compaction chapter — scores, stalls,
> filters, MANIFEST.
> **Out:** the observation that none of those defaults were chosen on algorithmic
> grounds, and the three-era timeline Steps 2-5 walk.

A production storage engine is tuned to whichever resource currently runs out
first — and at fleet scale that resource is decided by procurement, not computer
science. The paper's own summary, from the abstract: "We describe how and why
RocksDB's resource optimization target migrated from write amplification, to
space amplification, to CPU utilization."

```
 2012 ─────────► ~2015 ─────────► ~2018 ─────────► 2021
 write amp       space amp        CPU & DRAM       disaggregated storage
 (flash erase    (flash cycles    (space-amp wins  (CPU and SSD can be
  cycles are      and IOPS both    already banked;  provisioned separately;
  the budget)     turned out to    CPU/memory       "a current priority")
                  be slack; $/GB   prices rose
                  rules)           relative to SSD)
```

Each shift happened because the *hardware economics* moved, not because the
algorithms improved. This is the RUM triangle (topic 1) steered by procurement —
the same trade-off space, with the weights set by the price list.

Do not read the arrow as "the previous metric stopped mattering". §3 is explicit
that write amplification "continues to be an issue" for write-heavy workloads,
and the CPU era is motivated by the space-amp work being *done*, not by space
ceasing to matter.

### Step 2 — the write-amp era: flash wears out

> **In:** an LSM and a 2012 SSD with a finite erase budget.
> **Out:** why write amplification was the founding metric, with the measured
> range RocksDB actually achieves — and the reason it was not enough.

Write amplification (bytes physically written to flash per byte of user data)
was the founding obsession because flash cells have a finite program/erase
budget, and at fleet scale that is a hardware replacement line-item. §3: "When we
started developing RocksDB, we initially focused on saving flash erase cycles
and thus write amplification, following the general view of the community at the
time."

The measured numbers, all §3 "Write amplification":

```
 SSD-internal write amp (observed)            1.1 – 3
 storage/database software write amp          up to 100
     (a full 4/8/16 KB page written for a <100 B change)
 RocksDB Leveled Compaction                   10 – 30
 RocksDB Tiered Compaction                     4 – 10
     — "although with lower read performance"

 RocksDB vs InnoDB, LinkBench on MySQL:
     RocksDB issues 5% as many writes per transaction
```

Two things to take from that block. First, **10-30× is what leveled compaction
actually costs in production**, which brackets the ~20× that
`topics/04-lsm-deep-dive/notes.md` derives from `T/2 × L` at T = 10, L = 4 — a
rare case where the textbook model and the fleet agree. Second, the paper is
candid that 10-30 "is too high for write-heavy applications. For this reason we
added Tiered Compaction" — the design-space chapter's `Tier`, adopted for a
purely economic reason.

The B-tree comparison is the one to keep: **5% of InnoDB's writes per
transaction**. That is the LSM's whole pitch in one number, and it lines up with
this repo's own measured lane — `FINDINGS.md` row 1 (`./verify.sh 01`, Apple M3
Pro, 2026-07-28): the same 108 MB of records occupies 48 MB under fjall's LSM
and 6.8 GB under redb's copy-on-write B-tree.

This is the era your mini-LSM's `write_amp` experiment recreates.

### Step 3 — the space-amp era: $/GB beats endurance

> **In:** the write-amp era's assumption that flash endurance is the scarce
> resource.
> **Out:** the measurement that falsified it, the compaction change it caused,
> and the numbers to quote instead of "space amp ≈ 1.1×".

By the mid-2010s the assumption had simply stopped being true. §3: "we observed
that for most applications, space utilization was far more important than write
amplification, given that neither flash write cycles nor write overhead were
constraining. In fact the number of IOPS utilized in practice was low compared
to what the SSD could provide."

Note the shape of that argument: they did not find a better algorithm, they
*measured their own fleet* and discovered they had been optimizing slack. The
supporting evidence is Figure 3 — a survey of **42 different production
deployments** of ZippyDB and MyRocks, each serving a different application,
measured over a month across four axes (flash endurance, read bandwidth, space,
CPU). "Most of the workloads are space constrained."

The engineering response was **Dynamic Leveled Compaction**: size each level
from the *actual* size of the last level rather than from static targets. (This
is `level_compaction_dynamic_level_bytes`, the branch you saw guarding the score
formula at `rocksdb db/version_set.cc:4135`.) The measured effect, §3 and
Table 4 — RocksDB 5.9, all defaults, constant 2 MB/s write rate, keys chosen
randomly from a prepopulated space:

| keys | fully compacted | steady state | overhead |
|---|---|---|---|
| 200 M | 12.0 GB | 13.5 GB (dynamic) | **12.4%** |
| 200 M | 12.0 GB | 15.1 GB (LevelDB-style) | 25.6% |
| 1,000 M | 60.1 GB | 67.5 GB (dynamic) | **12.4%** |
| 1,000 M | 60.3 GB | 73.8 GB (LevelDB-style) | 22.4% |

"Dynamic Leveled Compaction limits space overhead to 13%, while Leveled
Compaction can add more than 25%. Moreover, space overhead in the worst case
under Leveled Compaction can be as high as **90%**, while it is stable for
dynamic leveling."

So the figure to quote for leveled space amplification is **1.13× with dynamic
leveling, 1.25× with static leveling and up to 1.9× worst case** — not a flat
"~1.1×". And the number that made the business case: "for UDB, one of Facebook's
main databases, the space footprint was reduced to **50%** when InnoDB was
replaced by RocksDB."

When storage is billed by the byte-month, write amp is a tax you pay once; space
amp is rent you pay forever. This is the Dostoevsky chapter's Step 2 in
production dollars.

### Step 4 — the CPU era: not the bottleneck, but the price

> **In:** the popular claim that NVMe outran the software.
> **Out:** the paper's flat rejection of that claim, and the *actual* reason CPU
> became an optimization target — which is a different argument with different
> consequences.

Here the paper says something more interesting than the story usually told, and
it is worth reading twice because it contradicts the received version:

> An issue of concern sometimes raised is that SSDs have become so fast that
> software is no longer able to take advantage of their full potential. That is,
> with SSDs, the bottleneck has shifted from the storage device to the CPU, so
> fundamental improvements to the software are necessary. **We do not share this
> concern based on our experience**, and we do not expect it to become an issue
> with future NAND flash based SSDs for two reasons. First, only a few
> applications are limited by the IOPS provided by the SSDs… most applications
> are limited by space. Second, we find that any server with a high-end CPU has
> more than enough compute power to saturate one high-end SSD. **RocksDB has
> never had an issue making full use of SSD performance in our environment.**
> (§3, "CPU utilization")

So: **CPU did not become the bottleneck.** What happened is a price movement.
The paper's actual argument for the CPU era, same section:

- "reducing CPU overheads has become an important optimization target, given
  that the low hanging fruit of reducing space amplification **has been
  harvested**";
- "until several years ago, the price of CPUs and memory was reasonably low
  relative to SSDs, but **CPU and memory prices have increased substantially**,
  so decreasing CPU overhead and memory usage has increased in importance";
- and it "improves the performance of the few applications where the CPU is
  indeed constraining" — a minority, per Figure 3's 42 deployments.

The named early work is filter-side, which ties straight back to the compaction
chapter: "prefix bloom filters, applying the bloom filter **before** index
lookups, and other bloom filter improvements." (The ribbon filter — ~30% less
space for 3-4× the build CPU — is this era's later artifact, and the trade
direction tells you the era: spend CPU, save DRAM, because DRAM got expensive.)

The paper also names the two cases where CPU *does* bind: a badly balanced host
(one CPU, many SSDs), and intensive write-dominated workloads — for which the
suggested fix is a lighter compression option, or the observation that "the
workload may simply not be suitable for SSDs since it would exceed the typical
flash endurance budget that allows the SSD to last **2-5 years**."

Reconcile all of this with your topic 0 finding — SipHash at 21% of a HashMap
lookup, memory stalls dominant — and note that the LSM stacks its own CPU costs
(merge comparisons, block decode and decompression, filter hashing at every
level, checksum verification) on top of a hash table's.

### Step 5 — what comes next: the disk leaves the box

> **In:** three eras of local, directly-attached flash.
> **Out:** the 2021-vintage forward look, stated as the paper states it — which
> is narrower than the usual paraphrase.

§3's "Adapting to newer technologies" surveys the candidates and mostly *rejects*
them, with one exception. Open-channel SSDs, multi-stream SSDs and ZNS "would
benefit only a minority of the applications using RocksDB, given that most
applications are space constrained, not erase cycle or latency constrained" —
Step 3's finding used as a filter on the roadmap. In-storage computing: unclear
benefit, would need API changes through the whole stack.

The exception:

> **Disaggregated (remote) storage** appears to be a much more interesting
> optimization target and is a current priority… With remote storage, it is
> easier to make full use of both CPU and SSD resources at the same time,
> because they can be separately provisioned on demand (something much more
> difficult to achieve with locally attached SSDs). (§3)

Note the reason: not throughput, but **independent provisioning** — the same
economics argument as every other era. Storage-class memory gets three
possibilities and no commitment; the paper notes drily that using SCM as main
storage is awkward because "RocksDB tends to be bottlenecked by space or CPU,
rather than I/O".

And under "Main Data Structure Revisited", the answer to the question this whole
topic circles: "We continuously revisit the question of whether LSM-trees remain
appropriate, but continue to come to the conclusion that they do." The one
concession is **key-value separation** for large objects (WiscKey-style), shipped
as **BlobDB**.

§8's roadmap includes one line that should make you sit up after the previous two
chapters: "we plan to **unify leveled and tiered compaction** and improve
adaptivity." That is Dostoevsky's Fluid LSM-tree, on RocksDB's own to-do list.
The open questions are worth reading as a research menu — hybrid SSD/HDD, the
cost of long runs of consecutive deletion markers, better write throttling,
efficient replica comparison, SCM, and a generic integrity handoff API.

### Step 6 — the fleet-scale lessons: what only production teaches

> **In:** the three eras, all of which are performance stories.
> **Out:** the three sections that are not about performance at all — and the
> measured failure rates that make them the most valuable part of the paper.

§4 (serving large-scale systems), §5 (failure handling) and §6 (the key-value
interface) are what running the engine on hundreds of thousands of machines
proves.

**Silent corruption has a measured rate.** §5 quantifies it rather than
gesturing at it, by comparing primary and secondary indexes in MyRocks tables
that have both — any inconsistency must have been introduced below the
application:

> Based on our measurements, corruptions are introduced at the RocksDB level
> roughly **once every three months for each 100 PB of data**. Worse, in **40%**
> of those cases, the corruption had already propagated to other replicas.

And separately, from one storage-system bug in network-failure handling:
"roughly **17 checksum mismatches for every petabyte of physical data
transferred**." That 40% is the sentence that justifies the whole design: if
corruption reaches replicas before detection, replication is not a safety net.

**Hence checksums at every layer, each catching a different threat** (§5,
"Multi-layer protection"): *block* checksums (inherited from LevelDB, verified on
**every read**, keeping filesystem-level corruption away from clients); *file*
checksums (added in **2020**, recorded in the MANIFEST's SSTable entry and
validated wherever the file is transferred, so corruption cannot ride a backup
into a replica); *handoff* checksums (passed down to the filesystem so WAL
appends are validated incrementally at write time — "unfortunately, local file
systems rarely support this"); plus a *planned* application-layer checksum. Each
layer distrusts the one below it, by design.

**API regrets are forever** (§6). RocksDB uses internal **56-bit sequence
numbers**, incremented on every client write and not settable by the
application. Snapshots pin a version — but only going forward: "RocksDB does not
support taking a snapshot of the past, since there is no API to specify a
time-point." And because each instance assigns its own sequence numbers, "it is
essentially impossible to create versions of data that offer cross-shard
consistent reads." Applications work around it by encoding timestamps in the key
(which hurts point lookups) or the value (which hurts scans); the fix under way
is user-defined timestamps as a first-class concept.

**Configuration sprawl is an acknowledged failure** (§4, "Managing
configurations"). "A common complaint now is that there are far too many options
and that it is too difficult to understand their effects; i.e., it has become
very difficult to specify an 'optimal' configuration." Worse, the optimum depends
on the application above, not just the system embedding RocksDB: across the
**39 ZippyDB deployments** sampled in Table 5 there are **over 25 distinct
configurations** (14 of them differing in the compaction area alone) — despite
"significant efforts… to use uniform configurations wherever possible". Contrast
Monkey and Dostoevsky's "solve for the knob" ethos: the paper's own list of
suggestions that *did not* work out opens with "**Customizability is always good
to users**".

These are the parts benchmarks can't show, and the reason to read a production
retrospective instead of another asymptotic analysis.

## How to read the paper (with the concepts in hand)

Budget about 2 h. Section numbers below are the **FAST '21** version's.

1. **§1-2** — background and RocksDB architecture. Skim §2.2 if the compaction
   chapter is fresh; do read §2.1 on flash economics.
2. **§3 Evolution of resource optimization targets** — Steps 1-5, the whole arc,
   in the authors' words. Read the "CPU utilization" subsection twice: it argues
   *against* the popular framing. Figure 3 (42 deployments) and Table 4 (space
   overhead) are the two quantitative anchors.
3. **§4 Lessons on serving large-scale systems** — resource management across
   many instances on a host, WAL treatment, rate-limited file deletions, data
   format compatibility, configuration management (Table 5), replication and
   backup.
4. **§5 Lessons on failure handling** (Step 6) — the best section. The corruption
   rates, the multi-layer checksum design (Fig. 4), and differentiated error
   handling.
5. **§6 Lessons on the key-value interface** — the 56-bit sequence numbers, why
   snapshots don't compose across shards, user-defined timestamps.
6. **§8 Future Work** and the appendix — the six open questions, the numbered
   "lessons learned", and the short list of "suggestions that did not work out",
   which is the most quotable page in the paper.

## Questions to answer in notes.md

1. The popular story says CPU became the bottleneck once NVMe arrived; §3 says
   RocksDB "has never had an issue making full use of SSD performance". Which
   argument does the paper actually make for optimizing CPU, and what evidence
   backs it? Then reconcile with your topic-0 finding (SipHash 21%, memory
   stalls dominant): which CPU costs does an LSM add on top of a hash table's?
   (Comparisons in merges, block decode/decompress, filter hashing per level,
   checksum verification.)
2. Why does RocksDB checksum at block AND file AND WAL-record level rather
   than trusting the filesystem? (§5's answer is the 40% figure — corruption
   reaches replicas before detection.) What's the FalkorDB/redis equivalent
   story? (RDB has a CRC; AOF… check.)
3. Pick the lesson from §4-§6 most relevant to the capstone and write one
   paragraph on how it changes your M4 design.

## Done when

Answer each before unfolding it.

- [ ] You can narrate the three-era arc with the hardware reason for each transition.

  <details><summary>Answer</summary>

  **Write amp (2012-)**: flash cells have a finite program/erase budget, so an
  engine writing 30× the user data wears a fleet out 30× faster. RocksDB
  inherited this priority from the community consensus of the time.

  **Space amp (~2015-)**: they measured their own fleet and found the assumption
  false — "neither flash write cycles nor write overhead were constraining. In
  fact the number of IOPS utilized in practice was low compared to what the SSD
  could provide." Figure 3's 42-deployment survey shows most workloads space
  constrained. Flash was now cheap and durable enough that $/GB dominated.

  **CPU (~2018-)**: *not* because the SSD outran the software (§3 explicitly
  rejects that), but because the space-amp low-hanging fruit had been harvested
  and "CPU and memory prices have increased substantially" relative to SSDs. It
  is a cost-per-server argument, not a saturation argument.

  The through-line: every transition was forced by a price list, not by a better
  algorithm. Same RUM triangle, different weights.

  </details>

- [ ] You can give real numbers for RocksDB's write and space amplification.

  <details><summary>Answer</summary>

  Write amp (§3): SSD-internal 1.1-3 as observed; software-level "sometimes as
  high as 100" when a full 4/8/16 KB page is written for a sub-100-byte change;
  **RocksDB leveled compaction 10-30**; **tiered compaction 4-10**, "although
  with lower read performance". Against a B-tree: on LinkBench over MySQL,
  "RocksDB issues only 5% as many writes per transaction as InnoDB".

  Space amp (§3, Table 4 — RocksDB 5.9, defaults, 2 MB/s constant write rate):
  **Dynamic Leveled Compaction holds space overhead to ~13%** (12.4% at both
  200 M and 1000 M keys); static LevelDB-style leveling "can add more than 25%"
  (22.4-25.6% measured) and "in the worst case can be as high as **90%**". The
  business number: UDB's footprint fell to **50%** when InnoDB was replaced by
  RocksDB.

  The 10-30× leveled figure brackets this repo's own `T/2 × L ≈ 20×` model at
  T = 10, L = 4 (`notes.md`) — model and fleet agreeing is worth noting, since
  they usually don't.

  </details>

- [ ] You can state what the paper says about CPU being the bottleneck, and why that matters.

  <details><summary>Answer</summary>

  It denies it. §3, "CPU utilization": "We do not share this concern based on our
  experience… any server with a high-end CPU has more than enough compute power
  to saturate one high-end SSD. RocksDB has never had an issue making full use of
  SSD performance in our environment." The exceptions named are unbalanced hosts
  (one CPU, several SSDs) and write-dominated workloads — for which the paper
  suggests lighter compression, or notes the workload may not suit SSDs at all
  given a 2-5 year endurance budget.

  CPU became a target for two other reasons: the space-amp work was done, and
  CPU/DRAM prices rose relative to flash, so shaving CPU and memory buys
  cheaper hardware configurations. Early work was filter-side — prefix bloom
  filters, applying the filter *before* index lookups.

  Why it matters: the two framings prescribe different work. "The CPU is the
  bottleneck" says rewrite the hot path. "CPU is expensive relative to flash"
  says trade CPU *for* DRAM and space where the price list favours it — which is
  exactly what the ribbon filter does (~30% less space for 3-4× build CPU), and
  it would look like a bad trade under the first framing.

  </details>

- [ ] You can quote the corruption rate and explain the multi-layer checksum design from it.

  <details><summary>Answer</summary>

  §5: corruption is "introduced at the RocksDB level roughly **once every three
  months for each 100 PB of data**", measured by comparing primary and secondary
  indexes in MyRocks tables — and "in **40%** of those cases, the corruption had
  already propagated to other replicas". Separately, one storage-system bug in
  network-failure handling produced "roughly **17 checksum mismatches for every
  petabyte of physical data transferred**".

  The 40% is the design driver: if corruption reaches replicas before anyone
  notices, replication is not a safety net, so detection has to happen *early*
  and at *every* layer. Hence: **block** checksums (from LevelDB, verified on
  every read, keeping filesystem-level corruption from clients); **file**
  checksums (added 2020, stored in the MANIFEST's SSTable entry and validated on
  every transfer, so corruption cannot ride a backup into a replica); **handoff**
  checksums (passed down with WAL writes for incremental validation — but "local
  file systems rarely support this"); plus a planned application-layer checksum.
  Each layer distrusts the one below.

  </details>

- [ ] You can name two things the paper admits it got wrong, with specifics.

  <details><summary>Answer</summary>

  **Configuration sprawl** (§4, "Managing configurations"): "a common complaint
  now is that there are far too many options and that it is too difficult to
  understand their effects". Worse, the optimum depends on the application above
  the embedding system — across the 39 ZippyDB deployments in Table 5 there are
  over 25 distinct configurations (14 differing in compaction alone), despite
  deliberate effort to unify them. The appendix's "suggestions that did not work
  out" opens with "Customizability is always good to users."

  **Versioning in the API** (§6): 56-bit sequence numbers are internal,
  incremented per client write and not settable; snapshots only pin the present,
  because "RocksDB does not support taking a snapshot of the past, since there is
  no API to specify a time-point"; and since each instance numbers independently,
  "it is essentially impossible to create versions of data that offer cross-shard
  consistent reads". Applications encode timestamps in the key (hurting point
  lookups) or the value (hurting scans). User-defined timestamps are the fix in
  progress.

  Two more from the same appendix list, both worth a moment: "RocksDB can be
  blind to CPU bit flips" and "It's OK to panic when seeing any I/O error."

  </details>

## References

**Papers**
- Dong, Kryczka, Jin, Stumm — *Evolution of Development Priorities in Key-value
  Stores Serving Large-scale Applications: The RocksDB Experience*, USENIX FAST
  '21 (open access), extended as ACM Transactions on Storage 17(4), Art. 26.
  §3 is the three-era arc, §5 (failure handling) is the best section, §8 and the
  appendix hold the open questions and the "suggestions that did not work out".
  All citations below are to the FAST '21 version.

| Claim in this chapter | Source |
|---|---|
| Optimization target migrated write amp → space amp → CPU | Abstract; §3 |
| SSD-internal WA 1.1-3; software WA up to 100 | §3, "Write amplification" |
| Leveled WA 10-30; tiered WA 4-10 | §3, "Write amplification" |
| RocksDB issues 5% of InnoDB's writes per transaction (LinkBench) | §3 |
| IOPS were slack; most workloads space constrained | §3, "Space amplification"; Figure 3 (42 deployments) |
| Dynamic Leveled Compaction: ~13% overhead vs >25%, worst case 90% | §3 and Table 4 |
| UDB footprint reduced to 50% replacing InnoDB | §3 |
| "We do not share this concern" — CPU is not the bottleneck | §3, "CPU utilization" |
| CPU targeted because space-amp fruit harvested and CPU/DRAM prices rose | §3, "CPU utilization" |
| Prefix bloom filters, filter before index lookup | §3, "CPU utilization" |
| SSD endurance budget sized for 2-5 years | §3, "CPU utilization" |
| Open-channel / multi-stream / ZNS benefit only a minority | §3, "Adapting to newer technologies" |
| Disaggregated storage is "a current priority", for independent provisioning | §3 |
| LSM-trees remain appropriate; BlobDB for key-value separation | §3, "Main Data Structure Revisited" |
| Plan to unify leveled and tiered compaction; six open questions | §8 |
| Corruption once per 3 months per 100 PB; 40% already replicated | §5, "Frequency of silent corruptions" |
| 17 checksum mismatches per PB transferred | §5 |
| Block / file (2020) / handoff checksum layers | §5, "Multi-layer protection"; Fig. 4 |
| 56-bit sequence numbers; no snapshot of the past; no cross-shard versions | §6, "Versions and timestamps" |
| 39 ZippyDB deployments, over 25 distinct configurations | §4, "Managing configurations"; Table 5 |
| "Customizability is always good to users" among failed suggestions | Appendix, "Suggestions that did not work out" |

**Code**
- `rocksdb db/version_set.cc:4135` at `7c80a5a` — the
  `level_compaction_dynamic_level_bytes` branch, i.e. Dynamic Leveled
  Compaction from §3, in the scoring code.

**Repo cross-references**
- `FINDINGS.md` row 1 — the measured fjall-vs-redb space comparison used in
  Step 2, since topic 4 has no lane of its own.
- `topics/04-lsm-deep-dive/notes.md` — the `T/2 × L ≈ 20×` write-amp model that
  §3's 10-30 range brackets.
- `topics/04-lsm-deep-dive/reading-rocksdb-compaction.md` — the scores, stalls
  and filters this chapter gives the economic backstory for.
- `topics/04-lsm-deep-dive/reading-dostoevsky.md` — Fluid LSM-tree, which §8
  lists as RocksDB roadmap.
