# Compaction is four axes, not two strategies

"Leveled vs tiered" is a false binary: a compaction policy is an independent
choice on four design axes — trigger, layout, granularity, data movement — and
every system you've read in this topic sits somewhere in that grid. Before
the paper, this chapter builds the four axes one at a time, with the systems
you already know as coordinates. This is the taxonomy chapter; read it LAST
of the four papers, because it organizes the other three.

Every axis name, option list and number below is checked against the paper —
Sarkar, Staratzis, Zhu, Athanassoulis, *Constructing and Analyzing the LSM
Compaction Design Space*, PVLDB 14(11): 2216-2229, 2021 — and cited to the
section, observation or takeaway it came from.

## The problem in one sentence

After three papers and two codebases you have seen at least five distinct
compaction behaviors described with only two words ("leveled", "tiered") —
and decisions that dominate p99.9 write latency, like whether a compaction
moves one 64 MB file or one 25 GB level, don't even have a name in that
vocabulary.

## The concepts, step by step

### Step 1 — a compaction policy is a bundle of independent decisions

> **In:** the compaction behaviours you have already met — lsm-tree's leveled
> strategy, RocksDB's scores and universal compaction, Dostoevsky's K and Z.
> **Out:** four named questions every one of them answers, and the observation
> that the answers are independently choosable — which Steps 2-5 take one at a
> time.

Every compaction, in every engine, answers the same four questions. The paper's
own phrasing (§3.1), verbatim:

```
 1) Compaction trigger:     When to re-organize the data layout?
 2) Data layout:            How to lay out the data physically on storage?
 3) Compaction granularity: How much data to move at-a-time during
                            layout re-organization?
 4) Data movement policy:   Which block of data to be moved during
                            re-organization?
                                       — Constructing and Analyzing…, §3.1
```

"Leveled" and "tiered" are answers to question 2 *only*. They get used as if
they answered all four, which is what hides the other three from view.

Two structural facts about the grid, from §3.2. First, **data layout is
single-valued; trigger, granularity and data movement policy are
multi-valued** — an engine has exactly one layout but may have several triggers
and several file-picking rules active at once. Second, the space is large:
"Plugging in some typical values for the cardinality of the primitives, we
estimate the cardinality of the compaction universe as **>10⁴**, a vast yet
largely unexplored design space."

Unbundling matters because the axes control *different* observable costs.

### Step 2 — axis 1, the trigger: what event starts a compaction

> **In:** the four questions from Step 1.
> **Out:** the five triggers in production use, and the recognition that the
> familiar one (level saturation) is a choice, not a law — Step 3 then shows
> that trigger and layout are genuinely separable.

The trigger is the predicate that fires a compaction job. §3.1.1 lists the
common ones:

```
 i)   Level saturation:  level size goes beyond a nominal threshold
 ii)  #Sorted runs:      sorted run count for a level reaches a threshold
 iii) File staleness:    a file lives in a level for too long
 iv)  Space amplification (SA): overall SA surpasses a threshold
 v)   Tombstone-TTL:     files have expired tombstone-TTL
                                       — §3.1.1
```

The familiar one is **level saturation** — RocksDB's score ≥ 1.0 from the
compaction chapter, where the score is bytes-in-level ÷ target-bytes-for-level
(`db/version_set.cc:4136-4137`). The paper notes a wrinkle worth knowing: some
engines measure saturation by *file count* rather than bytes, which "works only
when all immutable files are of equal size, or for systems that have a tunable
file size" — RocksDB's L0 trigger (`level0_file_num_compaction_trigger`, default
4) is exactly this variant, and it is why L0 is special-cased in its scoring
code.

The other four are not exotic. **#Sorted runs** is tiering's trigger and, with
space amplification, is what RocksDB's universal compaction uses (§3.2:
"compactions are triggered when either (a) the number of sorted runs in a level
or (b) the estimated space amplification in the tree reaches certain
thresholds. This interpretation of tiering is also referred to as universal
compaction in systems like RocksDB"). **Tombstone-TTL** exists because a delete
is not persistent until its tombstone reaches the last level — a compaction
trigger driven by privacy regulation rather than performance, which is a
genuinely different reason for a database to do work.

### Step 3 — axis 2, the layout: what shape the levels are kept in

> **In:** the trigger from Step 2, which decides *when*.
> **Out:** the invariant that decides *what shape* — five options, of which the
> vocabulary in your head names only two — plus the one measured result that
> attributes point-lookup latency to this axis.

The layout is the invariant about runs per level — the axis Dostoevsky already
turned into a dial. §3.1.2's list:

```
 i)   Leveling:    one sorted run per level
 ii)  Tiering:     multiple sorted runs per level
 iii) 1-leveling:  tiering for Level 1; leveling otherwise
 iv)  L-leveling:  leveling for last level; tiering otherwise
 v)   Hybrid:      a level can be tiering or leveling independently
                                       — §3.1.2
```

Read iii) and iv) carefully, because they are easy to get backwards.
**1-leveling is leveling with a *tiered first level*** — laziness at the top, to
absorb ingest bursts without stalling. **L-leveling is tiering with a *leveled
last level*** — which is exactly Dostoevsky's Lazy Leveling, and Table 1 files
Dostoevsky under L-leveling. They are near-opposites, and the paper reaches for
1-leveling far more often than you would guess, because:

> **1-Lvl** … is the default data layout for RocksDB. (§3.2)

RocksDB's L0 tolerates multiple overlapping runs and "is allowed to grow
perpetually in order to avoid write-stalls in ingestion-heavy workloads"
(§3.1.2). So the engine everyone calls "leveled" is, in this taxonomy, a hybrid
— and Table 1 lists its layout as "Leveling / 1-Leveling" for exactly that
reason.

This is the axis that moves point-lookup latency, and the paper measures it
(**O4**, §5.1.2): point lookups are best with `Full` leveling and worst with
tiering — mean latency **1.1-1.9× higher** for tiering on existing keys and
**~2.2× higher** on non-existing keys. Note that this is *far short* of the
textbook prediction: "For non-empty lookups in a tree with size ratio T,
theoretically, the lookup cost for tiering should be T× higher than its leveling
equivalent." The measured gap is 2.2×, not 10×, and the paper explains why —
RocksDB's tiering keeps fewer sorted runs than textbook tiering, and the block
cache plus lookup temporality absorb much of the rest. A 5× discrepancy between
the asymptotic model and the measurement, explained rather than hidden, is worth
more than either number alone.

### Step 4 — axis 3, granularity: how much data one job moves

> **In:** a layout (Step 3) and a trigger that just fired (Step 2).
> **Out:** how big the resulting job is — the axis that turns out to control
> tail latency, with the measured spread that proves it.

Granularity is the size of a single compaction job's input. §3.1.3's list:

```
 i)   Level:               all data in two consecutive levels
 ii)  Sorted runs:         all sorted runs in a level
 iii) Sorted file:         one sorted file at a time
 iv)  Several sorted files: several sorted files at a time
                                       — §3.1.3
```

**Full compaction** (level granularity — your mini-LSM, and the 1996 paper's
rolling merge in spirit) merges an entire level at once: with a 2.5 GB L2 that
is one job occupying the disk for tens of seconds, and every one of those
seconds is back-pressure. **Partial compaction** (file granularity — RocksDB:
pick one ~64 MB file plus its next-level overlaps) does comparable total work as
many small jobs spread over time.

The measurements, all from §5.1.1 on the setup in Step 6:

- **O1** — compaction data movement dwarfs the data itself: `Full` moves **63×**
  the ingested bytes (32× read + 31× written); `Tier` **23×**.
- **O2** — partial compaction moves **34%-56% less data than `Full`**, for two
  reasons the paper separates: (1) a file with no overlap in its parent level is
  "only logically merged" — a **pseudo-compaction**, pure metadata, zero IO; and
  (2) a smaller granularity lets you *choose* a cheap file (that is Step 5's
  axis). Partial strategies run **4× more compaction jobs**, which is the number
  of tree levels.
- **TA I** — "Full-level compactions perform about 1/L times fewer compactions
  than partial compaction routines, however, full-level compaction moves nearly
  **2L times more data per compaction**."
- **O3** — `Full`'s mean compaction latency is 1.2-1.9× higher than partial
  leveling and 2.1× higher than tiering. Also: CPU is **~50%** of compaction
  time regardless of strategy, dominated by the in-memory sort-merge — so
  compaction is not the pure-IO activity it is usually drawn as.
- **TA II**, the number to remember — "Tail write stall for `Tier` is **~25 ms**,
  while for partial leveling (`Old`) it is as low as **1.3 ms**."

That last pair is a **19× spread in tail write latency** between two
configurations of the same engine on the same workload. Granularity is a
**tail-latency knob** — topic 2's rehash-spike lesson (one big pause versus many
amortized ones) at LSM scale.

One correction to the folklore, from the paper's own numbers: partial compaction
is *not* merely the same work rearranged. §2 describes it that way ("does not
radically change the total amount of data movement… but amortizes this data
movement uniformly over time"), but O2 measures 34-56% *less* total movement,
because finer granularity is what makes pseudo-compactions and cheap-file
picking possible at all. When the background section and the measurement
disagree, take the measurement.

Pseudo-compaction is the same optimization the reference crates call a **trivial
move** — `lsm-tree`'s `Choice::Move` (`src/compaction/mod.rs:70`, taken at
`src/compaction/leveled/mod.rs:524-527` and `:574-577`) and RocksDB's trivial
move. Note where it sits: the taxonomy does not give it an axis of its own; it
falls out of choosing file granularity. For sequential or bulk-load ingest it
dominates everything — a sorted snapshot can cascade to the bottom level
entirely by relinking, write amp 1.0 — which is the M4 graph-snapshot question
answered by a mechanism the two-word vocabulary could not express.

### Step 5 — axis 4, data movement: which file gets picked

> **In:** partial compaction from Step 4, which has just decided to move *one*
> file and now has to say *which*.
> **Out:** seven picking policies, each optimizing a different metric — and the
> paper's negative result about what this axis does *not* affect.

Data movement policy answers "which block of data to be moved" — in the
literature's more common name, the **file picking policy**. It only exists when
granularity is partial: §3.1.4 opens "When partial compaction is employed, the
data movement policy selects which file(s) to choose for compaction", and §3.2
notes that a full-level design "by definition, does not need a data movement
policy". The axes are not independent in that one respect.

```
 i)   Round-robin:                  chooses files in a round-robin manner
 ii)  Least overlapping parent:     file with least overlap with "parent"
 iii) Least overlapping grandparent: as above with "grandparent"
 iv)  Coldest:                      the least recently accessed file
 v)   Oldest:                       the oldest file in a level
 vi)  Tombstone density:            file with #tombstones above a threshold
 vii) Tombstone-TTL:                file with expired tombstone-TTLs
                                       — §3.1.4
```

Each entry is a different metric being optimized, and §3.1.4 names them:
round-robin and random "do not focus on optimizing for any particular
performance metric, but help in reducing space amplification"; **coldest**
optimizes read throughput; **least overlap** minimizes write amplification;
**tombstone density** reduces space amplification; **tombstone-TTL** bounds
delete latency. One axis, five different goals — and the measured payoff for the
write-amp choice is real but modest: `LO+1` and `LO+2` "move **10%-23% less
data** than other partial compaction strategies" (O2).

Now the negative result, which is more interesting than the positive one:

> **TA III:** The point lookup latency is largely unaffected by the data
> movement policy. In presence of Bloom filters (with high enough memory) and
> small enough block cache, the point query latency remains largely unaffected
> by the data movement policy as long as the number of sorted runs in the tree
> remains the same. (§5.1.2)

File picking changes *which* bytes move, not *how many runs exist* — and reads
pay per run, so reads cannot tell. The axis that moves point lookups is layout
(Step 3); the axis that moves tail writes is granularity (Step 4); this axis
moves write amplification, space amplification and delete latency. Four axes,
four different cost columns — that is the whole reason to have the taxonomy.

The paper's own filter configuration is worth noting since it lands exactly on
the Monkey chapter's arithmetic: 10 bits per key giving "FPR = 0.8%" (§5.1.2) —
the same 0.819% that `e^(−10·ln²2)` predicts and that fjall's filters deliver at
0.844%.

### Step 6 — using the grid: place every system, then trust only same-engine data

> **In:** all four axes.
> **Out:** a coordinate for every system in this topic, the methodology that
> makes cross-strategy numbers trustworthy, and the headline finding.

With four axes, every policy you've met becomes a coordinate:

| system | layout | trigger | granularity | movement |
|---|---|---|---|---|
| your mini-LSM | leveling or tiering | level size | level | n/a (whole level) |
| lsm-tree crate | leveling | level size, run count | several files | — |
| RocksDB default | **1-leveling** | level saturation, #runs, staleness, SA, TS-TTL | file (single/multiple) | round-robin, least-overlap ±1/±2, coldest, oldest, TS-density, TS-TTL |
| RocksDB universal | tiering | #sorted runs + space amp | sorted run | — |
| Dostoevsky | **L-leveling** | per-level (K, Z) | file and level | least-overlap |

(The RocksDB and Dostoevsky rows are Table 1's, transcribed.) Note the two
entries the two-word vocabulary gets wrong on its own terms: RocksDB "leveled"
is 1-leveling, and RocksDB "universal" *is* the paper's `Tier`.

The paper's second contribution is methodological, and it is the Fair
Benchmarking lesson (topic 0) applied at scale: they implement **ten** strategies
— `Full`, `LO+1`, `LO+2`, `RR`, `Cold`, `Old`, `TSD`, `TSA`, `Tier`, `1-Lvl` —
*inside one codebase* (modified RocksDB, "more than a hundred design knobs"),
then run **more than 2000 experiments** varying one axis at a time. Cross-engine
comparisons cannot do this: they confound all four axes with the storage format,
the filter implementation, the thread pool and everything else. §5's setup is a
single AWS `t2.2xlarge` (8 vCPU at 3.0 GHz, 32 GB RAM, 45 MB L3, Ubuntu 20.04)
with a 40 GB io2 SSD at 4000 provisioned IOPS; RocksDB at size ratio 10, 8 MB
write buffer, 10 bits/key filters, 8 MB block cache, direct IO, one compaction
thread, 128 B entries, 10 M inserts.

And the headline, Key Takeaway A (§1):

> **There is no perfect compaction strategy.** When it comes to selecting a
> compaction strategy for an LSM-engine, there is no single best. Thus, a
> compaction strategy needs to be custom-tailored to specific combinations of
> workload, LSM tuning, and performance goals.

The RUM conjecture, empirically, again — this time with 12 observations
attached. §6's practical distillation: avoid `Tier` where worst-case latency
matters (its tail is the 25 ms in Step 4, and O10 shows it *worsening* with data
size beyond 8 GB), avoid `LO+2` where predictability matters, and prefer partial
leveling or `1-Lvl` for stable performance.

Two things the taxonomy does *not* cover, worth noticing because a good taxonomy
makes its own gaps visible. **Filter memory allocation** is not an axis — Monkey
moves on none of the four, so a fifth primitive would be needed to express it.
And **trivial move / pseudo-compaction** has no axis either; it is an emergent
consequence of choosing file granularity (Step 4). Both are real design
decisions with measured effects, sitting outside a design space the paper
estimates at >10⁴ points.

## How to read the paper (with the concepts in hand)

Budget about 2 h. Section numbers are the paper's own.

1. **§3.1** — the four primitives and their option lists (Steps 1-5). Read
   Figure 3 first; it is the whole taxonomy on one page.
2. **§3.2 and Table 1** — where twenty-plus real systems land. Fill in the grid
   in Step 6 for: your mini-LSM, the lsm-tree crate, RocksDB leveled, RocksDB
   universal, FIFO. Table 2 defines the ten codified strategies you will meet
   throughout §5.
3. **§4 Benchmarking Compactions** — the one-engine methodology (Step 6). Short,
   and the reason to believe §5.
4. **§5 findings** — the keepers: **O1-O3 and TA I-II** for granularity and tail
   latency (Step 4); **O4 and TA III** for what moves point lookups and what
   does not (Steps 3 and 5); **O10** for how `Tier` degrades with data size.
   Read the setup paragraph before quoting anything.
5. **§6 Discussion** — "Avoiding the Worst Choices" is the practitioner's page.
6. Skim the workload-sensitivity plots (§5.2) — note which finding you'll test.

## Questions to answer in notes.md

1. Your write_amp experiment compacts whole levels. Predict, then measure if
   time allows: what does per-insert p99.9 look like vs a per-file granularity
   variant? (This is topic 2's rehash-spike lesson at LSM scale; the paper's
   own answer is TA II's 25 ms vs 1.3 ms.)
2. Which axis does Dostoevsky's lazy leveling move on? (Layout only — Table 1
   files it as L-leveling; trigger, granularity and movement stay orthogonal.)
   Which does Monkey move on? (None — filter memory is not one of the four;
   where would you add it, and what would its option list be?)
3. For M4's graph-snapshot SSTs: bulk-loading a snapshot is one giant sorted
   run. Which axis choices make ingest cheap? (File granularity, so that
   pseudo-compaction / trivial move applies — no merge at all.)

## Done when

Answer each before unfolding it.

- [ ] You can name the four primitives, the question each answers, and which one is single-valued.

  <details><summary>Answer</summary>

  From §3.1, verbatim: (1) **compaction trigger** — when to re-organize the data
  layout? (2) **data layout** — how to lay out the data physically on storage?
  (3) **compaction granularity** — how much data to move at-a-time during layout
  re-organization? (4) **data movement policy** — which block of data to be moved
  during re-organization?

  **Data layout is single-valued**; trigger, granularity and data movement
  policy are multi-valued, so an engine has one layout but can carry several
  triggers and several picking rules at once (§3.2). "Leveled" and "tiered" are
  answers to question 2 only, which is why the vocabulary hides three quarters
  of the design space. The paper estimates the full space at **>10⁴** distinct
  strategies.

  </details>

- [ ] You can give the five layouts, and say what 1-leveling and L-leveling actually mean.

  <details><summary>Answer</summary>

  Leveling (one run per level); tiering (multiple runs per level); **1-leveling**
  — *tiering for Level 1, leveling otherwise*; **L-leveling** — *leveling for the
  last level, tiering otherwise*; hybrid — each level chooses independently
  (§3.1.2).

  These two are near-opposites and easy to swap. 1-leveling is laziness at the
  *top*, to absorb ingest bursts without stalling — and it is **RocksDB's
  default** (§3.2: "1-Lvl … is the default data layout for RocksDB"; Table 1
  lists RocksDB as "Leveling / 1-Leveling"). L-leveling is laziness
  *everywhere but the bottom* — which is Dostoevsky's Lazy Leveling, and Table 1
  files Dostoevsky under L-leveling.

  </details>

- [ ] You can say which axis moves which cost, with a number for each.

  <details><summary>Answer</summary>

  **Layout → point lookups.** O4: tiering's mean point-lookup latency is
  1.1-1.9× leveling's on existing keys and ~2.2× on non-existing keys. (Theory
  predicts T× = 10×; the measured 2.2× is explained by RocksDB's tiering keeping
  fewer runs than textbook tiering, plus the block cache and lookup temporality.)

  **Granularity → tail write latency, and total data moved.** TA II: tail write
  stall is ~25 ms for `Tier` against 1.3 ms for partial leveling (`Old`) — a 19×
  spread. O1: `Full` moves 63× the ingested bytes, `Tier` 23×. O2: partial
  compaction moves 34-56% less than `Full` while running 4× more jobs. TA I:
  full-level compaction does ~1/L as many compactions, each moving ~2L times
  more data.

  **Data movement policy → write amp, space amp, delete latency — but not
  reads.** O2: `LO+1`/`LO+2` move 10-23% less data than other partial
  strategies. TA III: "The point lookup latency is largely unaffected by the
  data movement policy… as long as the number of sorted runs in the tree remains
  the same." Reads pay per run; picking a different file does not change the run
  count.

  **Trigger →** when everything above happens, and (via tombstone-TTL) how
  quickly a delete becomes persistent.

  </details>

- [ ] You can explain why the paper implements ten strategies in one engine, and what that buys.

  <details><summary>Answer</summary>

  Because a cross-engine comparison confounds all four axes with the storage
  format, filter implementation, thread pool and everything else — the Fair
  Benchmarking lesson from topic 0. So they integrate ten codified strategies
  (`Full`, `LO+1`, `LO+2`, `RR`, `Cold`, `Old`, `TSD`, `TSA`, `Tier`, `1-Lvl`)
  into one modified RocksDB codebase exposing "more than a hundred design knobs",
  and run **more than 2000 experiments** varying one primitive at a time (§1,
  "Experimental Contribution 1"; §4).

  What it buys is attribution. Without it, "tiering has worse tail latency" is a
  claim about two products; with it, it is a claim about one primitive with
  everything else held fixed — and it is why O4 can go on to say *why* the
  measured 2.2× falls short of the theoretical 10×, instead of just reporting a
  ratio.

  The setup, for quoting: AWS `t2.2xlarge`, 8 vCPU at 3.0 GHz, 32 GB RAM, 45 MB
  L3, Ubuntu 20.04, 40 GB io2 SSD at 4000 provisioned IOPS; RocksDB at size
  ratio 10, 8 MB write buffer, 10 bits/key filters (FPR 0.8%), 8 MB block cache,
  direct IO, one compaction thread, 128 B entries, 10 M inserts.

  </details>

- [ ] You can state the headline finding and name two design decisions the taxonomy does not cover.

  <details><summary>Answer</summary>

  Key Takeaway A (§1): "**There is no perfect compaction strategy.** … there is
  no single best. Thus, a compaction strategy needs to be custom-tailored to
  specific combinations of workload, LSM tuning, and performance goals." The RUM
  conjecture, arrived at empirically, with 12 observations behind it. §6's
  practical version: avoid `Tier` when worst-case latency matters (25 ms tails,
  and O10 shows it degrading further past 8 GB), avoid `LO+2` when
  predictability matters, prefer partial leveling or `1-Lvl` for stability.

  Not covered: **filter memory allocation** — Monkey moves on none of the four
  primitives, so expressing it needs a fifth. And **trivial move /
  pseudo-compaction** — real, measured (part of O2's 34-56%), named in the text,
  but not an axis; it is an emergent consequence of choosing file granularity.
  A taxonomy that makes its own gaps visible is doing its job.

  </details>

## References

**Papers**
- Sarkar, Staratzis, Zhu, Athanassoulis — *Constructing and Analyzing the LSM
  Compaction Design Space*, PVLDB 14(11): 2216-2229, 2021.
  Artifacts at `https://disc.bu.edu/lsm-compaction`. §3 is the taxonomy, §4 the
  one-engine methodology, §5 the twelve observations and seven takeaways, §6 the
  practitioner's summary.

| Claim in this chapter | Source |
|---|---|
| The four primitives and their questions | §3.1 |
| Layout single-valued, other three multi-valued; space >10⁴ | §3.2 |
| Five triggers | §3.1.1 |
| Five layouts, incl. 1-leveling and L-leveling definitions | §3.1.2 |
| `1-Lvl` is RocksDB's default layout; `Tier` is universal compaction | §3.2; Table 1 |
| Four granularity options; partial compaction defined | §3.1.3, §2 |
| Seven data movement policies, and the metric each targets | §3.1.4 |
| `Full` moves 63× ingested bytes, `Tier` 23× | O1, §5.1.1 |
| Partial moves 34-56% less, runs 4× more jobs; `LO±` 10-23% less | O2, §5.1.1 |
| Full does 1/L as many compactions, each ~2L× larger | TA I |
| `Full` mean latency 1.2-1.9× partial, 2.1× tiering; CPU ~50% | O3 |
| Tail write stall 25 ms (`Tier`) vs 1.3 ms (`Old`) | TA II |
| Tiering point lookups 1.1-1.9× / ~2.2× leveling, vs T× predicted | O4, §5.1.2 |
| Point lookup latency unaffected by movement policy | TA III |
| `Tier` degrades past 8 GB | O10, §5.2 |
| Ten strategies, one codebase, >100 knobs, >2000 experiments | §1, §4 |
| EC2 setup, RocksDB config, 10 bits/key → FPR 0.8% | §5, "Experimental Setup"; §5.1.2 |
| "There is no perfect compaction strategy" | Key Takeaway A, §1 |
| Avoid `Tier` / `LO+2`; prefer partial leveling or `1-Lvl` | §6, "Avoiding the Worst Choices" |

**Code**
- `lsm-tree src/compaction/mod.rs:70` at `8526dd3` — `Choice::Move`, the
  pseudo-compaction of O2; taken at `src/compaction/leveled/mod.rs:524-527`
  and `:574-577`.
- `rocksdb db/version_set.cc:4136-4137` at `7c80a5a` — the level-saturation
  trigger of §3.1.1, as a score.

**Repo cross-references**
- `topics/04-lsm-deep-dive/reading-rocksdb-compaction.md` — the scoring and
  stall machinery this chapter classifies.
- `topics/04-lsm-deep-dive/reading-dostoevsky.md` — L-leveling, from the inside.
- `topics/00-performance-toolbox/reading-fair-benchmarking.md` — why §4's
  one-engine methodology is the only way these numbers mean anything.
