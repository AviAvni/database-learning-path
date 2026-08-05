# Dostoevsky: merge lazily, except at the last level

Monkey optimized the filters; Dostoevsky optimizes the **merging itself** — by
noticing that most of leveled compaction's work is "superfluous" (the paper's
word). This chapter builds the argument from the ground up: what leveled and
tiered actually promise, which level dominates each cost, why merging eagerly
at small levels buys nothing — until "tier the top, level the bottom" is the
obvious move — and then the Fluid-LSM dial that generalizes it.

Every formula and number below is checked against the paper — Dayan & Idreos,
*Dostoevsky: Better Space-Time Trade-Offs for LSM-Tree Based Key-Value Stores
via Adaptive Removal of Superfluous Merging*, SIGMOD 2018 — and cited to the
section, equation or figure it came from.

## The problem in one sentence

Leveled compaction rewrites every key ~T times per level (T = size ratio,
typically 10) to keep *every* level a single sorted run — but the small upper
levels contribute almost nothing to read or space cost, so roughly
`(T−1)/T ≈ 90%` of that merging effort improves nothing anyone measures.

## The concepts, step by step

### Step 1 — the two classic policies, restated as runs per level

> **In:** the LSM shape from the lsm-tree chapter — a memtable, flushes, levels
> that grow by a factor of T.
> **Out:** the two named policies expressed as a single integer per level (how
> many runs it tolerates), which is the form Step 6 turns into a dial.

Vocabulary first, since the paper's whole argument is in these symbols
(its Table 1 is the glossary):

| symbol | meaning |
|---|---|
| `N` | number of entries in the tree |
| `T` | **size ratio** — each level holds T× the entries of the one above |
| `L` | number of levels on disk |
| `B` | entries per disk block (an IO moves one block) |
| `M` | total main memory given to bloom filters, in bits |
| `p_i` | false-positive rate of the filters at level *i* |
| `s` | size of a range-scan's target range, as a fraction of the key space |

A **run** is a sorted, key-disjoint set of tables — one unit a point read must
probe once (lsm-tree chapter, Step 5). The two classic compaction policies
differ only in *how many runs each level tolerates* before merging:

- **Leveled**: every level holds exactly **1 run**. Each time data arrives from
  above it is merged into the level's run immediately — and since the level is
  up to T× bigger than the arriving data, each incoming byte drags resident
  bytes through the merge with it. Update cost `O(L·T / B)`; reads probe 1 run
  per level.
- **Tiered**: each level accumulates up to **T−1 runs** of similar size, and the
  T-th arrival triggers a merge of all of them into one run that moves down.
  Each byte is rewritten ~once per level — update cost `O(L / B)` — but reads
  probe up to T−1 runs per level, and the largest level may hold T−1 stale
  copies of the same key, so space amplification is `O(T)`.

(Both complexities are Figure 6, rows (A) and (G).) Same data, same levels; the
whole difference is eagerness of merging.

Worth knowing before you go looking for tiered in this repo's reference crate:
`lsm-tree` at `8526dd3` ships **leveled only** — its tiered strategy is
commented out of the module tree:

```rust
// fjall-rs/lsm-tree@8526dd3 — src/compaction/mod.rs
     7  pub(crate) mod fifo;
     8  pub(crate) mod leveled;
    // ... 9-17: other modules ...
    18  // pub(crate) mod tiered;
    19  pub(crate) mod worker;
    20
    21  pub use fifo::Strategy as Fifo;
    22  pub use filter::{CompactionFilter, Factory, ItemAccessor, Verdict};
    23  pub use leveled::Strategy as Leveled;
    24  // pub use tiered::Strategy as SizeTiered;
```

So "leveled vs tiered" is a live design argument, not a menu you pick from —
which is exactly why the parameterization in Step 6 is the useful takeaway.

### Step 2 — where each cost actually lives

> **In:** the two policies from Step 1 and the four costs an LSM is judged on.
> **Out:** the attribution — which *level* dominates each cost — which is the
> whole evidence base for the diagnosis in Step 3.

The costs do not come from all levels equally, and this asymmetry is the entire
paper:

- **Space amplification** (bytes on disk per byte of live data) is dominated by
  the **largest level**. The paper's argument (§4.1, "Space-Amplification"): in
  the worst case every entry at levels 1…L−1 is an update to an existing entry
  at level L, and that fraction is `1/T` of the data, so space amplification is
  at most `O(1/T)`. At T = 10 the upper levels are 10% of the data *in total* —
  even fully duplicated they barely matter.
- **Zero-result point lookups** (the filter-tax workload from Monkey) are
  dominated by the largest level too. Under the optimal allocation the bottom
  level's FPR is `p_L = R·(T−1)/T` (Equation 5) — the bottom level *is* 90% of
  the lookup cost at T = 10, by construction, because that is where the entries
  are and bits are scarcest per entry.
- **Long range lookups** are dominated by the largest level: it "contains
  exponentially more entries than all other levels", so the cost is `O(s/B)`
  regardless of what the upper levels look like (§4.1, "Range Lookups").
- **Update cost** is dominated by the **upper levels**: every byte passes
  through L1, L2, … on its way down, getting rewritten at each stop. With
  leveling that is L rewrites each dragging a level's worth of resident data.
- **Short range lookups** are the one exception — they touch every run at every
  level and so they *do* care about upper-level fragmentation. Keep this one in
  your pocket; it is the bill Step 4 pays.

| level | what merging there improves | who cares |
|---|---|---|
| upper (small) levels | short range lookups, and almost nothing else | short scans only |
| **largest level** | space amp, zero-result lookups, long range lookups | everybody |

### Step 3 — the diagnosis: superfluous merging

> **In:** Step 2's attribution table.
> **Out:** a named defect in *both* classic policies — each is eager or lazy in
> the wrong place — which Step 4 fixes by splitting the difference.

**Superfluous merging** is merge work whose cost you pay but whose benefit no
metric reflects — and Step 2 says that is most of what leveled compaction does.
The abstract puts it exactly: "merge operations from all levels of LSM-tree but
the largest (i.e., most merge operations) reduce point lookup cost, long range
lookup cost, and storage space by a negligible amount while significantly adding
to the amortized cost of updates."

Concretely at T = 10, L = 4: keeping L1 (0.09% of the data) as one pristine run
costs a full rewrite of everything passing through it, and buys a filtered probe
avoided occasionally on a level whose filter Monkey already made nearly perfect
(23.9 bits/key, FPR 0.001%), plus space savings on 0.09% of the data.

Meanwhile tiered compaction is lazy *everywhere*, including the one level where
eagerness pays. Its largest level fragments into T−1 runs, which wrecks space
amplification (`O(T)` instead of `O(1/T)` — a factor of `T²`) and multiplies
zero-result lookup cost by T (Figure 6(D): `O(T·e^(−M/N))` versus
`O(e^(−M/N))`).

Both classic policies are therefore wrong in the same way: they apply one
eagerness setting to levels whose cost structures differ by a factor of `T^L`.

### Step 4 — the fix: lazy leveling

> **In:** the diagnosis from Step 3.
> **Out:** a policy that is tiered above and leveled at the bottom, and the four
> complexities it lands on — one of which (short range lookups) is worse, and
> Step 5 has to buy the point-lookup one back with filter bits.

**Lazy Leveling** applies each policy where it wins: **tiered at levels
1…L−1** (writes pass through cheaply — nobody needed those levels merged) and
**leveled at level L only** (the one place where 1 run buys space amp and read
cost for everybody). §4.1: "Lazy leveling at its core is a hybrid of leveling
and tiering: it applies leveling at the largest level and tiering at all other
levels."

```
 tiered:              leveled:            lazy leveled (Dostoevsky):

 L1: ▧▧▧▧ T−1 runs    L1: ▧ 1 run        L1: ▧▧▧▧ T−1 runs  ← tiered on top
 L2: ▧▧▧▧             L2: ▧              L2: ▧▧▧▧              (writes cheap)
 L3: ▧▧▧▧             L3: ▧              L3: ▧ 1 run        ← leveled at bottom
                                                               (space + reads OK)
 update: O(L/B)       O(L·T/B)           O((L+T)/B)  ← T paid once, at bottom
```

The update-cost derivation is one sentence in §4.1: "An updated entry with Lazy
Leveling participates in `O(1)` merge operations per level across Levels 1 to
L−1 and in `O(T)` merge operations at Level L. The overall number of merge
operations per entry is therefore `O(L + T)`."

The full comparison, transcribed from Figure 6 (all six rows):

| cost | tiering | leveling | lazy leveling |
|---|---|---|---|
| update | `O(L/B)` | `O(L·T/B)` | `O((L+T)/B)` |
| zero-result point lookup | `O(T·e^(−M/N))` | `O(e^(−M/N))` | `O(e^(−M/N))` |
| point lookup, existing key | `O(1 + T·e^(−M/N))` | `O(1)` | `O(1)` |
| short range lookup | `O(L·T)` | `O(L)` | `O(1 + (L−1)·T)` |
| long range lookup | `O(s·T/B)` | `O(s/B)` | `O(s/B)` |
| space amplification | `O(T)` | `O(1/T)` | `O(1/T)` |

Read the column: lazy leveling matches **leveling** on four of the six rows,
beats it decisively on updates, and loses only on short range lookups — the one
cost Step 2 flagged as caring about upper-level fragmentation. That is the
paper's claim in full, and it is a genuinely surprising one: three of the four
things people buy leveling *for* did not need leveling above the bottom level.

Put the update row on this repo's own arithmetic. `topics/04-lsm-deep-dive/notes.md`
uses `T/2 × L` for leveled write amplification (a level is on average half full
when data merges into it, so the resident data dragged through averages T/2×,
not T×) and `L` for tiered:

```
 T = 10, L = 4

 leveled       T/2 × L        = 5 × 4          = 20×   (notes.md)
 tiered        L              = 4              =  4×   (notes.md)
 lazy leveling (L−1) + T/2    = 3 + 5          =  8×   (same convention:
                                                        tiered above, leveled
                                                        once at the bottom)
```

**8× instead of 20× — a 2.5× cut in write amplification — while the space,
long-scan and point-read columns above stay exactly where leveling had them.**
That number is arithmetic on this repo's stated model, not a measurement: topic
4 has no `verify.sh` lane, because its benches measure only your code. For a
*measured* sense of what this family of costs looks like in practice, the
nearest lane in this repo is `FINDINGS.md` row 1 (`./verify.sh 01`, Apple M3
Pro, 2026-07-28): the same 108 MB of records lands as **48 MB** on disk under
fjall's LSM against **6.8 GB** under redb's copy-on-write B-tree — space
amplification 0.45× versus 63.28×, a 140× spread.

### Step 5 — the filter allocation that keeps the read column honest

> **In:** lazy leveling's shape from Step 4, which now has T−1 runs to probe at
> every upper level instead of 1.
> **Out:** the FPR assignment that keeps zero-result lookup cost at
> `O(e^(−M/N))` anyway, worked on concrete numbers — and the memory floor below
> which the trick stops working.

Step 4's table claims lazy leveling matches leveling on point lookups. That
cannot be free: the tree now has `(T−1)·(L−1) + 1` runs to probe instead of `L`.
The cost is paid in **filter allocation**, and §4.1's "Bloom Filters Allocation"
is where the paper earns the claim.

Start with the objective, Monkey's rule adapted to more runs per level (§4.1,
Equation 3):

```
 R = p_L + (T−1) · Σ(i=1..L−1) p_i                     Dostoevsky §4.1, Eq 3

 R    expected wasted IOs per zero-result point lookup
 p_i  the FPR shared by every run at level i
```

The memory model is Monkey's, unchanged — and note the sentence that makes it
work with multiple runs per level: "Since the filters at any given level all
have the same FPR, we can directly apply this equation regardless of the numbers
of runs at a level." A level's filters cost `M_i = −N_i·ln(p_i)/ln(2)²` bits
whether that's one run or nine, because it is the same total number of entries
either way.

```
 M = −(N / ln²2) · ((T−1)/T) · Σ(i=1..L) ln(p_i) / T^(L−i)     §4.1, Eq 4
```

Minimizing Equation 3 subject to Equation 4 (Lagrange multipliers; derivation in
**Appendix A**) gives:

```
 p_i =  R · (T−1)/T          for i = L                        §4.1, Eq 5
        R / T^(L−i+1)        for 1 ≤ i < L
```

and substituting back gives the closed form for R itself:

```
 R = e^(−(M/N)·ln²2) · T^(T/(T−1)) / (T−1)^((T−1)/T)          §4.1, Eq 6
```

Worked at T = 10, L = 4, N = 10 M, M/N = 10 bits per entry — the same tree and
the same budget as the Monkey chapter's Step 5, so the two are directly
comparable:

| level | runs | leveling: bits/key, FPR | lazy leveling: bits/key, FPR |
|---|---|---|---|
| 1 | 1 vs 9 | 23.85, 0.0011% | 27.96, 0.00011% |
| 2 | 1 vs 9 | 19.06, 0.0106% | 23.17, 0.0015% |
| 3 | 1 vs 9 | 14.26, 0.1057% | 18.38, 0.0146% |
| 4 | 1 vs 1 | 9.47, 1.0566% | 9.01, 1.3160% |
| | | `R` = **0.01174** | `R` = **0.01465** |

Equation 6 evaluates to 0.014646 at these parameters, agreeing with the direct
solve to 0.2%. So lazy leveling's zero-result lookups cost **25% more wasted
IOs than leveling at the same memory** — a constant factor, exactly as the paper
says: "the multiplicative term at the right-hand side of Equation 6 is a small
constant for any value of T. Therefore, the cost complexity is `O(e^(−M/N))`,
the same as with leveling despite having eliminated most merge operations."

Where does the extra memory come from? Read the bits column: lazy leveling
shifts ~0.5 bits/key *off* the bottom level (9.01 vs 9.47) and onto the upper
levels (+4.1 bits/key each), because each upper level now needs a lower FPR to
survive being multiplied by T−1 in Equation 3. Trading 25% more false probes for
2.5× less write amplification is the deal on the table.

**The trick has a memory floor.** As `M/N` shrinks the optimal FPRs rise toward
1 and filters start disappearing (bottom level first, since its FPR is highest).
Equation 7 gives the threshold:

```
 M/N threshold = (1/ln²2) · ( ln(T)/(T−1) + ln(T−1)/T )        §4.1, Eq 7

 T = 10  →  0.99 bits per entry
 T = 3   →  1.62 bits per entry   ← the global maximum over all T
```

The paper's own comment: mainstream stores default to 10 or 16 bits per entry,
"an order of magnitude larger", so the analysis holds everywhere it matters; for
sensors and mobile devices below 1.62 bits/entry, Appendix C adapts it by
merging more at larger levels. It is a rare case of an optimization whose
precondition is *satisfied by an order of magnitude* rather than marginally, and
worth noticing as a modelling habit.

### Step 6 — Fluid LSM: two knobs make it a dial, not a trick

> **In:** three named policies (tiered, leveled, lazy leveled) and their costs.
> **Out:** two integers that generate all three and everything between, so a
> tuner can *solve* for a policy instead of picking one from a list.

**Fluid LSM-tree** generalizes the whole family with two integers (§4.2):
**K** = runs tolerated at each of levels 1…L−1, **Z** = runs tolerated at the
largest level. The paper's parameterization, quoted:

- `K = 1` and `Z = 1` give leveling.
- `K = T−1` and `Z = T−1` give tiering.
- `K = T−1` and `Z = 1` give Lazy Leveling.

The mechanism is one detail worth keeping: each level has an *active* run that
incoming runs merge into, with a size threshold of `T/K` percent of the level's
capacity for levels 1…L−1 and `T/Z` percent at level L; when the active run hits
its threshold a new active run starts, and when the level is at capacity all its
runs merge and flush down (§4.2, "Basic Structure"). K and Z are not just
counters — they set how big each run is allowed to get.

Every cost from Step 4 becomes a formula in K and Z (Figure 8):

| cost | Fluid LSM |
|---|---|
| update | `O((T/B) · (L/K + 1/Z))` |
| zero-result point lookup | `O(Z · e^(−M/N))` |
| point lookup, existing key | `O(1 + Z · e^(−M/N))` |
| short range lookup | `O(Z + (L−1)·K)` |
| long range lookup | `O(s·Z / B)` |
| space amplification | `O((Z−1) + 1/T)` |

Check the row against Step 4's table by substituting: `K=Z=1` turns the update
row into `O(T·(L+1)/B) = O(T·L/B)` (leveling); `K=Z=T−1` turns it into
`O((T/(T−1))·(L+1)/B) ≈ O(L/B)` (tiering); `K=T−1, Z=1` gives
`O((T/B)·(L/(T−1) + 1)) ≈ O((L+T)/B)` (lazy leveling). Notice that **`Z` alone
drives every read and space cost** — `K` appears only in the short-range row.
That is Step 2's attribution table restated as algebra, and it is the cleanest
one-line summary of the paper.

The dial slots into a real interface. `lsm-tree`'s compaction strategy is a
single method returning a `Choice`:

```rust
// fjall-rs/lsm-tree@8526dd3 — src/compaction/mod.rs
    63  /// Describes what to do (compact or not)
    64  #[derive(Debug, Eq, PartialEq)]
    65  pub enum Choice {
    66      /// Just do nothing.
    67      DoNothing,
    68
    69      /// Moves tables into another level without rewriting.
    70      Move(Input),
    71
    72      /// Compacts some tables into a new level.
    73      Merge(Input),
    // ... 74-79: Drop(HashSet<TableId>) for FIFO-style expiry ...
    80  }
    // ... 81-95: trait CompactionStrategy, get_name, get_config ...
    96      /// Decides on what to do based on the current state of the LSM-tree's levels
    97      fn choose(&self, version: &Version, config: &Config, state: &CompactionState) -> Choice;
```

A Fluid strategy is that method with two thresholds instead of one:

```rust
// ILLUSTRATION — not quoted from a repo. This is Fluid LSM-tree (Dostoevsky
// §4.2) written against the real trait above, src/compaction/mod.rs:97.
// K = max runs at levels 1..L-1, Z = max runs at the largest level.
// K=Z=1 ⇒ leveling; K=Z=T−1 ⇒ tiering; K=T−1, Z=1 ⇒ lazy leveling.
fn choose(&self, version: &Version, _c: &Config, _s: &CompactionState) -> Choice {
    let last = version.last_level();
    for lvl in 0..last {
        if version.run_count(lvl) > self.k {      // upper levels: tolerate K runs
            return Choice::Merge(self.input_for(lvl, lvl + 1));
        }
    }
    if version.run_count(last) > self.z {         // largest level: tolerate Z
        return Choice::Merge(self.input_for(last, last));  // the T-fold cost,
    }                                                      // paid once, here
    Choice::DoNothing
}
```

Two honest costs to carry away. Short range scans pay `O(Z + (L−1)·K)` — `K > 1`
hurts them regardless of `Z`, and no filter helps a scan. And upper-level runs
still need filter memory, at a *higher* bits/key than leveling would need
(Step 5's table) — which is why Monkey and Dostoevsky compose rather than
compete: you need Monkey's allocation to make Dostoevsky's shape affordable.

### Step 7 — Dostoevsky itself, and what the evaluation actually reports

> **In:** the Fluid design space parameterized by `T`, `K`, `Z`.
> **Out:** how the system picks a point in it, and an honest reading of what the
> paper measured — which is less quantitative than you might expect.

**Dostoevsky** is the system that searches that space at runtime (§4.3). It
weights the four costs by their observed frequency in the workload —
`w` updates, `r` zero-result lookups, `v` non-zero lookups, `q` range lookups —
and maximizes worst-case throughput:

```
 τ = Ω⁻¹ · ( w·W + r·R + v·V + q·Q )⁻¹                        §4.3, Eq 14

 Ω   time to read one block from storage
 W,R,V,Q   the update / zero-result / existing-key / range cost formulas
```

Two pruning insights make the search cheap: there are only `⌈log₂(N/(P·B))⌉`
meaningful values of `T`, and the objective is **convex** in `K` and `Z`
(lookup costs increase monotonically in both, update cost decreases), so a
divide-and-conquer on each converges logarithmically. Total
`O(log₂(N/(B·P))³)` iterations, which "takes a fraction of a second". Re-tuning
runs every 16 buffer flushes in their implementation.

Now the evaluation, and read this part carefully because it is *not* what the
other three papers in this topic do (§5):

- **Implementation**: Dostoevsky built on **RocksDB**, with Equation 9's filter
  allocation embedded in the code and Fluid LSM-tree implemented via RocksDB's
  event-listener API to schedule custom merges.
- **Setup**: a RAID of 500 GB 7200 RPM disks, 32 GB DDR4, 4 × 2.7 GHz cores with
  8 MB L3, Ubuntu 16.04, ext4 with journaling off; direct IO; 2 MB buffer;
  **10 bits per entry** of filter memory; fence pointers one per 32 KB block;
  block cache 10% of the dataset.
- **Results**: Figure 10 plots **normalized** throughput — the y-axis is scaled
  to Dostoevsky's own result — as the proportion of zero-result lookups sweeps
  from 0.5% to 95%. The claim the paper makes is *dominance*, not a factor:
  "Dostoevsky dominates all fixed policies by encompassing all of them and
  fluidly transitioning among them." The abstract likewise says "strictly
  dominates" with no percentage attached.
- The most concrete artifact in Figure 10(A) is the row of chosen tunings
  printed above the plot: `T,Z,K` runs from tiering-like settings at the
  update-heavy end to `Z=1, K=1` (pure leveling) at the lookup-heavy end, and
  "these tunings are all unique to the Lazy Leveling and Fluid LSM-tree design
  spaces, except at the edges."

So: do not quote a speedup number for Dostoevsky, because the paper does not
publish one. Its quantitative content is the complexity table (Figure 6), the
closed-form models (Equations 3-14), and the tunings in Figure 10(A). This repo
adds no measurement of its own here either — topic 4 has no `verify.sh` lane.

## How to read the paper (with the concepts in hand)

Budget about 2.5 h. Section numbers are the paper's own — note that its
**Table 1 is a glossary of terms, not a cost table**; the cost table is
**Figure 6**.

1. **§2 Background** — the LSM mechanics and Table 1's symbols. Skim if the
   lsm-tree chapter is fresh; do read Table 1 as a glossary.
2. **§3 Design Space and Problem Analysis** — Figures 3-5, which quantify Step
   2's attribution: how much of each cost comes from which level. This is the
   evidence for "superfluous".
3. **§4.1 Lazy Leveling** — the core. **Figure 6 IS the paper** (Steps 4-5);
   reproduce its six rows for T = 10, L = 4 before moving on. Equations 3-8 are
   Step 5; the Lagrange derivation is Appendix A and the closed form for R is
   Appendix B.
4. **§4.2 Fluid LSM-tree** — K and Z, and Figure 8's cost table (Step 6).
   Substitute the three named policies into every row until the table stops
   needing to be looked up.
5. **§4.3 Dostoevsky** — the auto-tuner (Step 7). Skim the solver; keep the
   convexity argument, since that is what makes it usable online.
6. **§5 Evaluation** — Figure 10(A) for the tunings, 10(B) for "no single merge
   policy rules", 10(C) for scalability. Read the setup paragraph first: 2017
   spinning disks, direct IO, 10 bits/entry.

## Questions to answer in notes.md

1. Your mini-LSM implements leveled and tiered. Using its measured write amp
   and read amp: on YOUR numbers, what would lazy leveling have scored?
   (Compute — upper levels tiered cost + bottom leveled cost; Step 4 does this
   for `notes.md`'s model, so the method is there.)
2. Why do range scans not benefit from lazy leveling the way point reads do?
   (Every run at every level must be merged into the scan regardless — and
   Figure 8's short-range row `O(Z + (L−1)·K)` is the only one containing `K`.)
3. RocksDB never shipped lazy leveling as such — universal compaction covers
   part of the space. From reading-rocksdb-compaction.md, which universal knobs
   approximate K and Z?

## Done when

Answer each before unfolding it.

- [ ] You can state which level dominates each of the four costs, and why.

  <details><summary>Answer</summary>

  **Space amplification — largest level.** Worst case, every entry at levels
  1…L−1 is an update to something at level L; that fraction is `1/T` of the
  data, so space amp is `O(1/T)` under leveling (§4.1, "Space-Amplification").

  **Zero-result point lookups — largest level.** Under the optimal allocation
  the bottom level's FPR is `p_L = R·(T−1)/T` (Equation 5): 90% of the wasted-IO
  budget at T = 10, because that is where 90% of the entries are and so where
  bits per entry are scarcest.

  **Long range lookups — largest level.** It "contains exponentially more entries
  than all other levels", so the cost is `O(s/B)` whatever the upper levels do.

  **Update cost — upper levels.** Every byte is rewritten once per level on its
  way down, and under leveling each rewrite drags a level's worth of resident
  data with it: `O(L·T/B)`.

  **The exception: short range lookups** care about every run at every level, so
  they are the one cost that upper-level fragmentation genuinely hurts. That is
  the bill lazy leveling pays.

  </details>

- [ ] You can reproduce Figure 6 for the three policies — update, point lookup, space — and say which row lazy leveling loses.

  <details><summary>Answer</summary>

  | cost | tiering | leveling | lazy leveling |
  |---|---|---|---|
  | update | `O(L/B)` | `O(L·T/B)` | `O((L+T)/B)` |
  | zero-result point lookup | `O(T·e^(−M/N))` | `O(e^(−M/N))` | `O(e^(−M/N))` |
  | point lookup, existing key | `O(1 + T·e^(−M/N))` | `O(1)` | `O(1)` |
  | short range lookup | `O(L·T)` | `O(L)` | `O(1 + (L−1)·T)` |
  | long range lookup | `O(s·T/B)` | `O(s/B)` | `O(s/B)` |
  | space amplification | `O(T)` | `O(1/T)` | `O(1/T)` |

  Lazy leveling matches leveling on four rows, strictly beats it on updates, and
  loses only on **short range lookups** (`O(1 + (L−1)·T)` against `O(L)`) —
  precisely the cost that Step 2 identified as the one caring about upper-level
  fragmentation. Everything else people buy leveling for turns out not to need
  leveling above the bottom level.

  The one-sentence version of why it dominates: the expensive T-fold merge is
  the only thing that improves space amp, long scans and point lookups, and it
  only does so at the largest level — so pay it there once and nowhere else.

  </details>

- [ ] You can put the update row on this repo's arithmetic and say what lazy leveling would score.

  <details><summary>Answer</summary>

  `notes.md` models leveled write amplification as `T/2 × L` (a level averages
  half full when data merges into it) and tiered as `L`. At T = 10, L = 4:
  leveled 20×, tiered 4×. Lazy leveling is tiered above and leveled once at the
  bottom, so `(L−1) + T/2 = 3 + 5 = 8×` — a **2.5× cut** against leveling, while
  the space, long-scan and point-read rows stay where leveling had them.

  Two honesty notes. That is arithmetic on a stated model, not a measurement:
  topic 4 has no `verify.sh` lane, because its benches measure only your code.
  And the constant differs from the paper's `O(L+T)`, which does not carry the
  `/2`; the asymptotics agree, the constant is this repo's convention.

  </details>

- [ ] You can explain how lazy leveling keeps leveling's point-lookup complexity despite having T−1 runs per upper level, and what it costs.

  <details><summary>Answer</summary>

  Through filter allocation, not through structure. The objective becomes
  `R = p_L + (T−1)·Σ(i=1..L−1) p_i` (Equation 3) — upper levels are multiplied
  by their run count — but the *memory* model is unchanged, because "the filters
  at any given level all have the same FPR… regardless of the numbers of runs at
  a level". Optimizing Equation 3 against Equation 4 gives Equation 5:
  `p_L = R(T−1)/T`, and `p_i = R/T^(L−i+1)` below. The closed form (Equation 6)
  is `R = e^(−(M/N)ln²2) · T^(T/(T−1)) / (T−1)^((T−1)/T)`, whose trailing factor
  is "a small constant for any value of T" — hence `O(e^(−M/N))`, same as
  leveling.

  The price is that constant. At T = 10, L = 4, 10 bits per entry: leveling gets
  `R = 0.01174`, lazy leveling `R = 0.01465` — **25% more wasted IOs** — funded
  by moving ~0.5 bits/key off the bottom level onto the upper levels (which need
  ~+4.1 bits/key each to survive the ×(T−1)).

  There is also a floor: below `M/N = (1/ln²2)(ln T/(T−1) + ln(T−1)/T)` bits per
  entry the FPRs converge to 1 and the argument stops. That threshold peaks at
  **1.62 bits/entry** (at T = 3) and is 0.99 at T = 10 — an order of magnitude
  below the 10-to-16 bits/entry everyone actually uses (Equation 7 and the
  paragraph after it).

  </details>

- [ ] You can state the Fluid LSM parameterization and say which knob drives which costs.

  <details><summary>Answer</summary>

  `K` = runs tolerated at each of levels 1…L−1; `Z` = runs tolerated at the
  largest level. `K=1, Z=1` is leveling; `K=T−1, Z=T−1` is tiering;
  `K=T−1, Z=1` is lazy leveling (§4.2). Each level keeps an active run that
  incoming runs merge into, capped at `T/K` percent of the level's capacity
  (`T/Z` at level L).

  From Figure 8: update `O((T/B)(L/K + 1/Z))`, zero-result lookup
  `O(Z·e^(−M/N))`, existing-key lookup `O(1 + Z·e^(−M/N))`, short range
  `O(Z + (L−1)·K)`, long range `O(s·Z/B)`, space amp `O((Z−1) + 1/T)`.

  **`Z` appears in every row; `K` appears only in the update row and the
  short-range row.** So the bottom-level knob controls all the read and space
  behaviour, and the upper-level knob is a pure trade between write cost and
  short scans — which is Step 2's attribution table restated as algebra.

  </details>

- [ ] You can say what the paper's evaluation actually reports, without inventing a speedup.

  <details><summary>Answer</summary>

  §5: Dostoevsky implemented **on RocksDB** (Equation 9's allocation embedded in
  the code, Fluid LSM-tree built on RocksDB's event-listener API); a RAID of
  500 GB 7200 RPM disks, 32 GB DDR4, 4 × 2.7 GHz cores, 8 MB L3, Ubuntu 16.04,
  ext4 journaling off; direct IO, 2 MB buffer, 10 bits per entry of filters,
  fence pointers per 32 KB block, block cache at 10% of the dataset.

  Figure 10 plots **normalized** throughput against the proportion of
  zero-result lookups (0.5% → 95%); the y-axis is scaled to Dostoevsky's own
  result, so the figure shows *dominance*, not a factor. The paper's words are
  "strictly dominates" (abstract) and "dominates all fixed policies by
  encompassing all of them" (§5). No headline speedup percentage exists to
  quote — the quantitative content is Figure 6's complexities, Equations 3-14,
  and the `T,Z,K` tunings printed above Figure 10(A), which sweep from
  tiering-like settings at the update-heavy end to `Z=1, K=1` at the
  lookup-heavy end.

  </details>

## References

**Papers**
- Dayan & Idreos — *Dostoevsky: Better Space-Time Trade-Offs for LSM-Tree Based
  Key-Value Stores via Adaptive Removal of Superfluous Merging*, SIGMOD 2018.
  §3 for the problem analysis, §4.1 for Lazy Leveling (Figure 6 and Equations
  3-8), §4.2 for Fluid LSM-tree (Figure 8), §4.3 for the tuner, §5 for the
  evaluation. Appendix A is the Lagrange derivation, Appendix B the closed form
  for R, Appendix C the sub-1.62-bits/entry adaptation.

| Claim in this chapter | Source |
|---|---|
| Lazy Leveling = tiering above, leveling at level L | §4.1, "Basic Structure" |
| Most merges reduce lookup cost and space "by a negligible amount" | Abstract |
| Update cost `O((L+T)/B)`; leveling `O(L·T/B)`; tiering `O(L/B)` | §4.1 "Updates"; Figure 6(A) |
| Space amp `O(1/T)` for lazy leveling and leveling, `O(T)` for tiering | §4.1 "Space-Amplification"; Figure 6(G) |
| Short range `O(1 + (L−1)·T)`; long range `O(s/B)` | §4.1 "Range Lookups"; Figure 6(B), (C) |
| `R = p_L + (T−1)·Σ p_i` | §4.1, Equation 3 |
| Filter memory model, same regardless of runs per level | §4.1, Equation 4 and preceding sentence |
| `p_L = R(T−1)/T`, `p_i = R/T^(L−i+1)` | §4.1, Equation 5 (Appendix A) |
| Closed form for R; complexity `O(e^(−M/N))` | §4.1, Equation 6 (Appendix B) |
| Memory floor: 0.99 bits/entry at T=10, max 1.62 at T=3 | §4.1, Equation 7 |
| `V = 1 + R − p_L`, `O(1)` | §4.1, Equation 8 |
| K/Z parameterization and the three named policies | §4.2, "Parameterization" |
| Fluid cost table in K and Z | Figure 8 |
| Throughput objective, convexity, `O(log₂(N/BP)³)` search | §4.3, Equation 14 |
| Built on RocksDB; disk/memory setup; 10 bits/entry | §5, "Experimental Infrastructure" and "Implementation" |
| Normalized throughput, "dominates", no speedup factor | §5 and Figure 10; abstract |

**Code**
- `lsm-tree src/compaction/mod.rs:65-97` at `8526dd3` — the `Choice` enum and
  the `CompactionStrategy::choose` signature a Fluid strategy would implement;
  `:18` and `:24` show the tiered strategy commented out of the module tree.

**Repo cross-references**
- `topics/04-lsm-deep-dive/notes.md` — the `T/2 × L` and `L` write-amp model
  used in Step 4.
- `FINDINGS.md` row 1 — the measured fjall-vs-redb space-amplification lane borrowed
  in Step 4, since topic 4 has no lane of its own.
- `topics/04-lsm-deep-dive/reading-monkey.md` — the filter allocation Step 5
  extends.
