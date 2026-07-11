# Reading guide — "Dostoevsky" (SIGMOD '18)

Dayan & Idreos. *Better Space-Time Trade-Offs for LSM-Tree Based Key-Value
Stores via Adaptive Removal of Superfluous Merging.* ~1.5 h. Monkey optimized
the filters; Dostoevsky optimizes the **merging itself**.

## The insight in one table

Merging at different levels buys different things:

| Level | What merging there improves | Who cares |
|---|---|---|
| upper (small) levels | almost nothing — they're small, probes are filtered | nobody |
| **largest level** | space amp (dead versions live here) + zero-result reads | everybody |

Leveled compaction merges *eagerly everywhere* — most of that work is
"superfluous" (the paper's word). Tiered merges *lazily everywhere* — cheap
writes but the largest level fragments into K runs, wrecking space amp and
zero-result lookups.

## Lazy Leveling (the contribution)

```
 tiered:            leveled:            lazy leveled (Dostoevsky):

 L1: ▧▧▧▧ K runs    L1: ▧ 1 run        L1: ▧▧▧▧ K runs   ← tiered on top
 L2: ▧▧▧▧           L2: ▧              L2: ▧▧▧▧             (writes cheap)
 L3: ▧▧▧▧           L3: ▧              L3: ▧ 1 run       ← leveled at bottom
                                                            (space + reads OK)
 WA: O(L)           WA: O(T·L)         WA: O(L + T)  ← T paid once, at bottom
```

Point lookups and space amp are dominated by the largest level; write amp is
dominated by the *upper* levels (data passes through them repeatedly). So:
tier the top, level the bottom. **Fluid LSM** generalizes with two knobs
(K = runs allowed at upper levels, Z = runs at the largest) and picks them per
workload — leveled (K=Z=1) and tiered (K=Z=T−1) become endpoints of a dial.

## Reading order

1. §2 — the cost table (Table 1). Reproduce it for yourself for T=10, L=3:
   write cost, point read (zero/non-zero result), range, space. This table IS
   the paper.
2. §3 — Lazy Leveling analysis. Check the claim: same point-read + space
   complexity as leveled, write cost close to tiered.
3. §4 — Fluid LSM + the tuning section (skim the solver, keep the knobs).
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
