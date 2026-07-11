# Reading guide — "Monkey: Optimal Navigable Key-Value Store" (SIGMOD '17)

Dayan, Athanassoulis, Idreos. ~2 h. The paper that turned bloom-filter sizing
from folklore ("10 bits/key everywhere") into an optimization problem.

## The setup

An LSM with L levels, size ratio T, total filter memory budget M. Every level
gets a bloom filter. Question: how should M be divided among levels?

## The one picture

```
 uniform (state of practice):        Monkey (optimal):

 L1 (small)  10 bits/key             L1   ~14 bits/key  (FPR tiny)
 L2          10 bits/key             L2   ~12 bits/key
 L3 (huge)   10 bits/key             L3    ~8 bits/key  (FPR larger, but
                                            fewer probes land here anyway)
 total FPR cost: sum of per-level    expected wasted IOs: MINIMIZED —
 FPRs, dominated by... all equally   exponentially decreasing FPR up the tree
```

Key observation: expected wasted IO for a zero-result lookup = **sum of the
per-level FPRs** (each level is one potential false probe). But bits-per-key
buys FPR *exponentially* (`fpr ≈ e^(−bits·ln²2)`), while level sizes grow by T.
Spending a bit at a small level buys the same FPR drop for T× fewer keys —
i.e., T× cheaper. Optimum: FPRs proportional to level size ⇒ bits/key
*decreasing* geometrically toward the bottom; the bottom level may get ~0
(its "filter" is the fact that every lookup ends there anyway).

## Reading order

1. §1–2 — the LSM cost model (worth it alone: R/W/M costs as formulas in T, L).
   Map each symbol to your mini-LSM's knobs.
2. §4 — the allocation argument (above). Follow the Lagrange-multiplier sketch
   once; then re-derive the "FPR ∝ level size" conclusion informally yourself.
3. §5 — merging co-tuning (T as a continuum from leveled to tiered). Skim —
   Dostoevsky does this better.
4. §6 evaluation — look for the ~2× lookup improvement at equal memory.

## Questions to answer in notes.md

1. In your mini-LSM (3 levels, T=10, 10M keys), compute uniform-vs-Monkey
   expected false probes per zero-result get at 10 bits/key average. Then
   *measure* zero-result gets both ways (the experiment supports per-level
   bits-per-key for exactly this).
2. Monkey assumes point lookups dominate. What breaks for range scans?
   (Filters don't help ranges at all — prefix blooms exist for a subset.)
3. FalkorDB angle: an attribute store doing existence checks before edge
   insertion is a zero-result-heavy workload — where would Monkey's argument
   apply outside an LSM?

## Done when

You can state the allocation rule ("equal *marginal* IO saved per bit ⇒ FPR
proportional to level size") and back it with the measured table from your
mini-LSM.
