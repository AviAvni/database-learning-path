# Monkey: bloom bits where they pay

"10 bits/key everywhere" was folklore; Monkey turned bloom-filter sizing into
an optimization problem and won ~2× fewer wasted IOs from the *same* DRAM.
Before the paper, this chapter builds the argument one step at a time — what
a zero-result lookup costs, how bits buy false-positive rate, why a bit
spent at a small level is 10× cheaper than the same bit at a big one — until
the allocation rule ("FPR proportional to level size") falls out. Then it
sets up the per-level-bits experiment in the mini-LSM.

## The problem in one sentence

A lookup for a key that *doesn't exist* must be told "no" by every level of
the LSM, and each level's bloom filter lies (says "maybe") about 1% of the
time at the standard 10 bits/key — so with a fixed DRAM budget for filters,
the question is: is spreading it uniformly across levels actually the
division that wastes the fewest disk reads? (Answer: no, by ~2×.)

## The concepts, step by step

### Step 1 — the setup: one filter per level, one shared memory budget

An LSM with L levels (say 3), size ratio T (say 10 — each level holds 10×
more keys than the one above), and a total filter memory budget M. Every
level gets a bloom filter (from the lsm-tree chapter: a bit array answering
"definitely not here" or "maybe here"; more bits per key ⇒ fewer wrong
"maybe"s, called **false positives**). Question: how should M be divided
among the levels? The state of practice — same bits/key everywhere — is an
answer nobody had ever justified.

### Step 2 — what a zero-result lookup costs: the sum of the FPRs

A **zero-result lookup** (probing a key that exists nowhere — the common
case for existence checks, inserts-if-absent, and joins) gets the answer
"no" only after every level's filter says no. Each level is one independent
chance of a false positive, and each false positive costs one wasted disk
IO (~100 µs) probing a segment that doesn't have the key. So:

```
 expected wasted IOs per zero-result lookup = fpr(L1) + fpr(L2) + … + fpr(Lmax)
```

At uniform 10 bits/key, that's ~1% + ~1% + ~1% ≈ 0.03 wasted IOs per lookup
for 3 levels. The objective is now precise: **minimize the sum of per-level
FPRs subject to a fixed total number of bits.** That's an optimization
problem, and the two facts in Steps 3 and 4 make it lopsided.

### Step 3 — fact one: bits buy FPR exponentially

A bloom filter's false-positive rate falls *exponentially* in bits per key:
`fpr ≈ e^(−bits·ln²2)`, i.e. every ~1.44 extra bits/key *halves* the FPR.
Concretely: 10 bits/key ⇒ ~0.8%, 12 bits/key ⇒ ~0.3%, 8 bits/key ⇒ ~2.2%.
Exponential returns mean the *marginal* value of a bit depends enormously on
where it's spent — the first bits at any level are hugely effective, the
20th bit is nearly worthless. Uniform allocation ignores this curvature.

### Step 4 — fact two: levels differ in size by T×, but not in penalty

The bottom level holds ~T× more keys than the level above it (with T=10 and
3 levels: 90% of all keys are in the last level) — but a false positive at
the bottom level costs exactly the same **one disk IO** as a false positive
at a tiny upper level. Now combine with Step 2's objective: lowering a
*small* level's FPR by some amount requires extra bits for few keys;
lowering the *huge* bottom level's FPR by the same amount requires extra
bits for T× more keys. **A unit of FPR reduction is T× cheaper (in bits) at
a smaller level.** Uniform bits/key is therefore spending most of the budget
where it buys the least.

### Step 5 — the optimum: FPR proportional to level size

Minimizing the FPR sum under the bit budget (the paper does it with
Lagrange multipliers; the informal version is "shift bits from where they're
expensive to where they're cheap until marginal value equalizes") gives a
clean closed form: **each level's FPR should be proportional to its size**,
which means bits/key *decrease* geometrically toward the bottom:

```
 uniform (state of practice):        Monkey (optimal):

 L1 (small)  10 bits/key             L1   ~14 bits/key  (FPR tiny)
 L2          10 bits/key             L2   ~12 bits/key
 L3 (huge)   10 bits/key             L3    ~8 bits/key  (FPR larger, but
                                            fewer probes land here anyway)
 total FPR cost: sum of per-level    expected wasted IOs: MINIMIZED —
 FPRs, dominated by... all equally   exponentially decreasing FPR up the tree
```

In the limit the bottom level may get ~0 bits — its "filter" is the fact
that every lookup for an existing key ends there anyway, so a filter that
mostly says "maybe" was buying nothing. The whole allocation, as the closed
form your mini-LSM can call:

```rust
// Pick a total zero-result FPR budget; hand each level a share
// PROPORTIONAL TO ITS SIZE, then convert fpr → bits/key.
fn monkey_alloc(level_keys: &[u64], total_fpr: f64) -> Vec<f64> {
    let n: u64 = level_keys.iter().sum();
    level_keys.iter().map(|&nk| {
        let fpr = total_fpr * nk as f64 / n as f64;   // p_i ∝ level size
        -fpr.ln() / (LN_2 * LN_2)                     // bits/key: fpr ≈ e^(−bits·ln²2)
    }).collect()                                      // small levels get MORE bits/key
}
```

### Step 6 — what it buys, and where the idea stops

Same total DRAM, ~2× fewer expected false probes on zero-result lookups —
that's the paper's headline evaluation number, and it's free: no new data
structure, just arithmetic at filter-build time. The rule to remember is the
marginal one: **equal IO saved per bit spent, everywhere ⇒ FPR proportional
to level size.** Two boundaries to keep in mind: the argument assumes point
lookups (filters don't help range scans at all — a scan must consult every
run regardless); and the paper's §5 goes on to co-tune the merge policy
itself with the same optimization mindset — skim that, because Dostoevsky
(next chapter) does the merging half properly.

## How to read the paper (with the concepts in hand)

1. §1–2 — the LSM cost model (worth it alone: R/W/M costs as formulas in T,
   L — Steps 1–2 with full generality). Map each symbol to your mini-LSM's
   knobs.
2. §4 — the allocation argument (Steps 3–5). Follow the Lagrange-multiplier
   sketch once; then re-derive the "FPR ∝ level size" conclusion informally
   yourself.
3. §5 — merging co-tuning (T as a continuum from leveled to tiered). Skim —
   Dostoevsky does this better.
4. §6 evaluation — look for the ~2× lookup improvement at equal memory
   (Step 6's number).

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

## References

**Papers**
- Dayan, Athanassoulis, Idreos — "Monkey: Optimal Navigable Key-Value
  Store" (SIGMOD 2017) — §1–2 for the LSM cost model, §4 for the
  allocation argument; skim §5 (Dostoevsky does the merging co-tuning
  better) and §6 for the ~2× lookup improvement at equal memory
