# Dostoevsky: merge lazily, except at the last level

Monkey optimized the filters; Dostoevsky optimizes the **merging itself** — by
noticing that most of leveled compaction's work is "superfluous" (the paper's
word). This chapter builds the argument from the ground up: what leveled and
tiered actually promise, which level dominates each cost, why merging eagerly
at small levels buys nothing — until "tier the top, level the bottom" is the
obvious move — and then the Fluid-LSM dial that generalizes it.

## The problem in one sentence

Leveled compaction rewrites every key ~T times per level (T = size ratio,
typically 10) to keep *every* level a single sorted run — but the small upper
levels contribute almost nothing to read or space cost, so roughly
`(T−1)/T ≈ 90%` of that merging effort improves nothing anyone measures.

## The concepts, step by step

### Step 1 — the two classic policies, restated as runs per level

A **run** is a sorted, key-disjoint set of segments — one unit a point read
must probe once (lsm-tree chapter, Step 5). The two classic compaction
policies differ only in *how many runs each level tolerates* before merging:

- **Leveled**: every level holds exactly **1 run**. Each time data arrives
  from above, it is merged into the level's run immediately — and since the
  level is up to T× bigger than the arriving data, each incoming byte drags
  ~T resident bytes through the merge. Write amplification (bytes physically
  written per byte of user data) ≈ O(T·L) over L levels; reads probe 1 run
  per level.
- **Tiered**: each level accumulates up to **T runs** of similar size, then
  merges them all into one run that moves down a level. Each byte is
  rewritten only ~once per level — write amp ≈ O(L) — but reads must probe
  up to T runs per level, and the largest level may hold T stale copies of
  the same key.

Same data, same levels; the whole difference is eagerness of merging.

### Step 2 — where each cost actually lives

The three costs an LSM is judged on do not come from all levels equally:

- **Space amplification** (bytes on disk per byte of live data) is dominated
  by the **largest level** — it holds ~90% of the data at T=10, and dead
  versions of keys survive there until a merge drops them. Upper levels are
  ~10% of the data total; even fully duplicated they barely matter.
- **Zero-result point lookups** (the filter-tax workload from Monkey) are
  dominated by the largest level too: its filter has the most keys per bit
  and thus the highest false-positive contribution.
- **Write amplification** is dominated by the **upper levels**: every byte
  passes through L1, L2, … on its way down, getting rewritten at each stop.

| Level | What merging there improves | Who cares |
|---|---|---|
| upper (small) levels | almost nothing — they're small, probes are filtered | nobody |
| **largest level** | space amp (dead versions live here) + zero-result reads | everybody |

### Step 3 — the diagnosis: superfluous merging

Superfluous merging is merge work whose cost you pay but whose benefit no
metric reflects — and Step 2 says that's most of what leveled compaction
does. Keeping L1 (0.9% of the data) as one pristine run costs a full T×
rewrite of everything passing through, and buys: a filtered probe avoided
occasionally, on a level whose filter was nearly perfect anyway (Monkey gave
small levels the most bits/key), and space savings on 0.9% of the data.
Meanwhile tiered compaction is lazy *everywhere*, including the one level
where eagerness pays — its largest level fragments into T runs, wrecking
space amp (up to T stale copies of the hottest 90% of data) and zero-result
lookups (T bottom-level filters to get past instead of 1).

### Step 4 — the fix: lazy leveling

Lazy leveling applies each policy where it wins: **tiered at the upper
levels** (writes pass through cheaply — nobody needed those levels merged)
and **leveled at the largest level only** (the one place where 1 run buys
space amp and read cost for everybody):

```
 tiered:            leveled:            lazy leveled (Dostoevsky):

 L1: ▧▧▧▧ K runs    L1: ▧ 1 run        L1: ▧▧▧▧ K runs   ← tiered on top
 L2: ▧▧▧▧           L2: ▧              L2: ▧▧▧▧             (writes cheap)
 L3: ▧▧▧▧           L3: ▧              L3: ▧ 1 run       ← leveled at bottom
                                                            (space + reads OK)
 WA: O(L)           WA: O(T·L)         WA: O(L + T)  ← T paid once, at bottom
```

Read the write-amp column: the expensive T-fold merge is paid exactly
**once**, at the bottom, instead of at every level. Point-lookup and space
complexity match leveled (the largest level is 1 run — the only level where
it mattered); write cost is close to tiered. At T=10, L=3: leveled WA ≈ 30×,
lazy leveled ≈ 13×, for essentially the same read and space behavior.

### Step 5 — Fluid LSM: two knobs make it a dial, not a trick

Fluid LSM generalizes the whole family with two integers: **K** = runs
tolerated at each upper level, **Z** = runs tolerated at the largest level.
Leveled is (K=1, Z=1); tiered is (K=T−1, Z=T−1); lazy leveling is (K=T−1,
Z=1) — and everything between is legal, so the paper can *solve* for K and Z
per workload (write-heavy ⇒ raise Z toward tiered; space-constrained or
lookup-heavy ⇒ Z=1) rather than pick a named strategy. The whole family is
one compaction chooser with two thresholds:

```rust
// K = max runs at upper levels, Z = max runs at the largest level.
// K=Z=1 ⇒ leveled; K=Z=T−1 ⇒ tiered; K=T−1, Z=1 ⇒ lazy leveling.
fn choose(&self, v: &Version) -> Choice {
    for lvl in 0..v.last_level() {
        if v.runs(lvl) > self.k {                 // upper levels: tolerate K runs
            return Choice::MergeRunsInto(lvl + 1);
        }
    }
    if v.runs(v.last_level()) > self.z {          // largest level: tolerate Z
        return Choice::MergeLastLevel;            // T paid once, here
    }
    Choice::DoNothing
}
```

That chooser slots straight into the lsm-tree crate's compaction trait and
your mini-LSM's pluggable strategy — leveled and tiered stop being rivals
and become endpoints of one dial. The costs to keep honest: range scans
still pay for every run at every level (K > 1 hurts them regardless of Z),
and upper-level runs still need filter memory — which is why Monkey and
Dostoevsky compose.

## How to read the paper (with the concepts in hand)

1. §2 — the cost table (Table 1): Steps 1–2 as formulas. Reproduce it for
   yourself for T=10, L=3: write cost, point read (zero/non-zero result),
   range, space. This table IS the paper.
2. §3 — Lazy Leveling analysis (Step 4). Check the claim: same point-read +
   space complexity as leveled, write cost close to tiered.
3. §4 — Fluid LSM + the tuning section (Step 5; skim the solver, keep the
   knobs).
4. Evaluation — find the throughput-vs-skew plots.

## Questions to answer in notes.md

1. Your mini-LSM implements leveled and tiered. Using its measured write amp
   and read amp: on YOUR numbers, what would lazy leveling have scored?
   (Compute — upper levels tiered cost + bottom leveled cost.)
2. Why do range scans not benefit from lazy leveling the way point reads do?
   (Every run at every level must be merged into the scan regardless.)
3. RocksDB never shipped lazy leveling as such — universal compaction covers
   part of the space. From reading-rocksdb-compaction.md, which universal
   knobs approximate K and Z?

## Done when

You can reproduce Table 1 from memory for the three strategies (writes, point
reads, space) and say in one sentence why "merge lazily except the last level"
dominates.

## References

**Papers**
- Dayan & Idreos — "Dostoevsky: Better Space-Time Trade-Offs for LSM-Tree
  Based Key-Value Stores via Adaptive Removal of Superfluous Merging"
  (SIGMOD 2018) — §2's cost table (Table 1) IS the paper; §3 for the lazy
  leveling analysis, §4 for Fluid LSM (skim the solver, keep the knobs)
