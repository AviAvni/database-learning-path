# Monkey: bloom bits where they pay

"10 bits/key everywhere" was folklore; Monkey turned bloom-filter sizing into
an optimization problem and won a large factor of wasted IOs back from the
*same* DRAM. Before the paper, this chapter builds the argument one step at a
time — what a zero-result lookup costs, how bits buy false-positive rate, why a
bit spent at a small level is T× cheaper than the same bit at a big one — until
the allocation rule ("FPR proportional to level size") falls out. Then it sets
up the per-level-bits experiment in the mini-LSM.

Every formula, symbol and number below is checked against the paper —
Dayan, Athanassoulis and Idreos, *Monkey: Optimal Navigable Key-Value Store*,
SIGMOD 2017 — and cited to the section, equation or figure it came from. Where
this guide previously disagreed with the paper, the paper won.

## The problem in one sentence

A lookup for a key that *doesn't exist* must be told "no" by every level of
the LSM, and each level's bloom filter lies (says "maybe") about 0.8% of the
time at the standard 10 bits/key — so with a fixed DRAM budget for filters,
the question is: is spreading it uniformly across levels actually the
division that wastes the fewest disk reads? (Answer: no — 2.8× worse than
optimal on the worked example below.)

## The concepts, step by step

### Step 1 — the setup: one filter per level, one shared memory budget

> **In:** the LSM shape from the lsm-tree chapter — L levels, each T× bigger
> than the one above, one bloom filter per run.
> **Out:** the six symbols the rest of the chapter argues in, and a memory
> budget to divide — which Step 2 turns into an objective function.

Fix the vocabulary first, because the paper's algebra is unreadable without it
and every symbol here is one the paper uses (Figure 6 is its glossary):

| symbol | meaning | worked value below |
|---|---|---|
| `N` | total number of entries in the tree | 10,000,000 |
| `T` | **size ratio**: each level holds T× the entries of the one above | 10 |
| `L` | number of levels on disk | 4 |
| `p_i` | false-positive rate of the filter at level *i* (1 = smallest) | to be chosen |
| `M_filters` | total main memory for all filters, in bits | 99,990,000 (≈ 11.9 MiB) |
| `R` | expected number of wasted IOs per zero-result lookup | the thing to minimize |

A **false positive** is a filter answering "maybe" for a key it does not hold;
the **false-positive rate** is how often that happens. Level sizes follow from
`T` alone. The paper states it in §4.1: the last level holds at most
`N·(T−1)/T` entries, and in general level *i* holds at most
`N/T^(L−i) · (T−1)/T`, "because smaller levels have exponentially smaller
capacities by a factor of T". At N = 10 M, T = 10, L = 4:

```
 level 1     9,000 entries    (0.09% of the data)
 level 2    90,000
 level 3   900,000
 level 4 9,000,000 entries    (90% of the data)
           ---------
 total   9,999,000
```

Question: how should `M_filters` be divided among these four filters? The state
of practice — same bits/key everywhere — is an answer nobody had ever justified.
The paper is blunt about it in §2: "To the best of our knowledge, all LSM-tree
based key-value stores use the same number of bits-per-entry across all Bloom
filters."

### Step 2 — what a zero-result lookup costs: the sum of the FPRs

> **In:** the per-level FPRs `p_1 … p_L` from Step 1, still unassigned.
> **Out:** the objective function `R = Σ p_i` — one number to minimize, which
> Steps 3 and 4 show is minimized *unevenly*.

A **zero-result lookup** (probing a key that exists nowhere — the common
case for existence checks, insert-if-absent, and joins) gets the answer
"no" only after every level's filter says no. Each level is one independent
chance of a false positive, and each false positive costs exactly one wasted
disk IO probing a run that does not have the key. The paper's Equation 3 says
precisely that:

```
 leveling:   R = Σ(i=1..L) p_i                 one run per level
 tiering:    R = (T−1) · Σ(i=1..L) p_i         up to T−1 runs per level,
                                               all the same size, so all the
                                               same FPR
                                               — Monkey §4.1, Equation 3
```

(The published Equation 3 prints the same `(T−1)·Σ` on both branches, which the
surrounding prose contradicts one paragraph earlier: "With leveling every level
has at most one run and so R is simply equal to the sum of FPRs across all
levels." The prose is the correct reading and is what this chapter uses.)

Why exactly one IO per false positive, regardless of which level? Because the
run's fence pointers — the index of the lsm-tree chapter's Step 3 — take the
lookup straight to the one qualifying page. The paper leans on this explicitly
in §4.1: "the I/O cost of probing any run is the same regardless of its size
(due to the fence pointers we only fetch the qualifying disk page)". That
sentence is the hinge of the entire argument.

At uniform 10 bits/key the four filters in Step 1 each have FPR 0.8193% (Step 3
derives that), so

```
 R_uniform = 4 × 0.008193 = 0.0328 wasted IOs per zero-result lookup
```

The objective is now precise: **minimize `Σ p_i` subject to a fixed
`M_filters`.** That is an optimization problem, and the two facts in Steps 3 and
4 make its answer lopsided.

### Step 3 — fact one: bits buy FPR exponentially

> **In:** one filter, one bits-per-entry budget.
> **Out:** the conversion `p ↔ bits`, in both directions — the substitution that
> turns Step 2's objective into something differentiable.

A bloom filter's false-positive rate falls *exponentially* in bits per entry.
The paper states it as Equation 2 in §2:

```
 FPR = e^( −(bits/entries) · ln(2)² )                    Monkey §2, Equation 2

 rearranged in §4.1 to size a filter:
 bits = −entries · ln(FPR) / ln(2)²

 ln(2)  = 0.693147
 ln(2)² = 0.480453
```

This is the same formula the lsm-tree crate implements as `calculate_m`
(`src/table/filter/standard_bloom/builder.rs:129-150`), which is a useful
reassurance that the paper is describing shipped filters, not an idealisation.

Two consequences to have at your fingertips:

```
 bits/key   FPR = e^(−bits·0.480453)
     8        2.143%
    10        0.8193%
    12        0.3132%
    14        0.1197%
    16        0.04578%

 halving the FPR costs  ln2 / ln²2 = 1/ln2 = 1.4427 bits per key — always,
 at any starting point.
```

Exponential returns mean the *marginal* value of a bit depends enormously on
where it is spent. The first bits at any level are hugely effective; the 20th
bit is nearly worthless. Uniform allocation ignores this curvature entirely.

### Step 4 — fact two: levels differ in size by T×, but not in penalty

> **In:** Step 2's uniform per-false-positive penalty and Step 3's per-entry
> cost curve.
> **Out:** the asymmetry — FPR reduction is T× cheaper at each shallower level —
> which Step 5 turns into the closed form.

The bottom level holds ~T× more entries than the level above it (with T = 10 and
L = 4: 90% of all entries are in level 4) — but a false positive at the bottom
level costs exactly the same **one disk IO** as a false positive at the tiny top
level. Combine that with Step 3: halving a level's FPR always costs 1.4427 bits
*per entry in that level*, so halving level 1's FPR costs

```
 level 1:  1.4427 × 9,000 entries     =    12,984 bits
 level 4:  1.4427 × 9,000,000 entries = 12,984,300 bits    — 1000× more
```

for exactly the same reduction in `R`. **A unit of FPR reduction is T× cheaper
(in bits) at each level you move up.** Uniform bits/key is therefore spending
most of the budget where it buys the least: in the worked example, 90% of
`M_filters` goes to level 4's filter, which contributes exactly one quarter of
`R`, the same quarter as level 1's filter that costs a thousandth as much.

### Step 5 — the optimum: FPR proportional to level size

> **In:** the objective from Step 2 and the two cost facts from Steps 3-4.
> **Out:** the allocation rule, and a concrete table of bits per key per level
> that Step 6 prices.

Minimizing `R = Σ p_i` subject to fixed `M_filters` is a multivariate
constrained optimization. The paper solves it with Lagrange multipliers — in
**Appendix B**, not in the body — and reports the result as Equations 5
(leveling) and 6 (tiering) in §4.1. The conclusion in one sentence, quoted from
§4.1: "the optimal FPR at Level i is T times higher than the optimal FPR at
Level i−1. In other words, the optimal FPR for level i is proportional to the
number of elements at level i."

Substituting Step 3's conversion, "FPR × T per level down" means bits per key
falls by a *constant* per level:

```
 gap between consecutive levels = ln(T) / ln(2)²
                                = 2.302585 / 0.480453
                                = 4.793 bits per key, at T = 10
```

Worked on the Step 1 tree — N = 10 M, T = 10, L = 4, and the *same*
99,990,000-bit budget (10 bits/key on average) in both columns:

| level | entries | uniform bits/key | uniform FPR | Monkey bits/key | Monkey FPR |
|---|---|---|---|---|---|
| 1 | 9,000 | 10.00 | 0.8193% | **23.85** | 0.001057% |
| 2 | 90,000 | 10.00 | 0.8193% | **19.05** | 0.01057% |
| 3 | 900,000 | 10.00 | 0.8193% | **14.26** | 0.1057% |
| 4 | 9,000,000 | 10.00 | 0.8193% | **9.47** | 1.057% |
| | | `R` = **0.0328** | | `R` = **0.0117** | |

Read the two FPR columns as the whole idea. Uniform spends 9.47-plus bits on
every one of the nine million bottom-level entries to buy an 0.8193% FPR there,
and the same 10 bits on each of the nine thousand top-level entries. Monkey
takes half a bit per key away from level 4 — barely moving its FPR, from 0.82%
to 1.06% — and spends the 4.5 million bits that frees on levels 1-3, whose FPRs
fall by factors of 8, 78 and 775. The sum drops from 0.0328 to 0.0117:
**2.79× fewer wasted IOs per zero-result lookup, at identical memory.**

Run it the other way and the same allocation reaches the uniform tree's
`R = 0.0328` using 78.6 Mbit instead of 99.99 Mbit — **21% less filter DRAM for
identical lookup cost**, on this tree.

Push `R` higher still and the deepest filters vanish entirely. The paper handles
this explicitly (§4.1): for larger `R`, "more of the Bloom filters at the
deepest levels cease to exist as their optimal FPRs converge to 1", and
Equations 5 and 6 carry a term `L_filtered = L − max(0, ⌊R−1⌋)` for exactly that
— they solve the smaller problem on the shallowest `L_filtered` levels and give
the rest no filter at all. A filter that says "maybe" almost always was buying
nothing.

The allocation, as the closed form your mini-LSM can call:

```rust
// ILLUSTRATION — not quoted from a repo. The bits↔FPR conversion is Monkey §2
// Equation 2, the same one lsm-tree implements at
// src/table/filter/standard_bloom/builder.rs:129-150.
fn monkey_alloc(level_entries: &[u64], total_fpr: f64) -> Vec<f64> {
    let n: u64 = level_entries.iter().sum();
    level_entries
        .iter()
        .map(|&nk| {
            let fpr = total_fpr * nk as f64 / n as f64; // p_i ∝ level size (§4.1)
            -fpr.ln() / (LN_2 * LN_2)                   // bits/key, Equation 2 inverted
        })
        .collect() // small levels come out with MORE bits per key
}
```

### Step 6 — what it buys, and where the idea stops

> **In:** the allocation from Step 5.
> **Out:** the measured payoff, the asymptotic claim behind it, and the two
> boundaries — one of which is Dostoevsky's opening.

The asymptotic result is sharper than the worked example and is the reason the
paper exists (§4.3, Table 1): the state of the art has lookup cost
`O(e^(−M/N) · log_T(N·E / M_buffer))`, i.e. proportional to `L`, while Monkey
has `O(e^(−M/N))`. "Monkey shaves a factor of O(L) from the complexity of lookup
cost for both tiering and leveling… lookup cost R in Monkey is asymptotically
independent of the number of levels L." The intuition is Step 5's table
continued downward: because the FPRs decay geometrically going up, their sum
converges instead of growing with `L`.

The measured payoff, from §5:

- **Setup** (§5, "Default Set-up"): Monkey implemented on top of LevelDB,
  differing *only* in filter allocation; 1 GB of 1 KB entries; 16 K uniformly
  random zero-result point lookups; size ratio 2; 1 MB buffer;
  `M_filters/N` = 5 bits per element; block cache disabled; a 500 GB 7200 RPM
  disk, 32 GB RAM, 4 × 2.7 GHz cores.
- **Lookup latency**: "Monkey reduces lookup latency by an increasing margin as
  the data volume grows (**50%–80%** for the data sizes we experimented with)"
  — Abstract, and §5 "Monkey dominates LevelDB by up to 80%".
- **IOs per lookup**: Figure 11(A) is annotated **≈1 I/O per lookup for LevelDB
  against ≈0.2 for Monkey** at the largest data size — a 5× reduction, and the
  clearest single number in the paper.
- **Memory**: Figure 11(C) — Monkey matches LevelDB's lookup performance with up
  to **≈60% smaller** filter memory.

Note what does *not* appear in that list: this repo has no measured lane for
topic 4 (its benches measure only your code), so every number above is the
paper's, on the paper's 2017 spinning disk. The rule to carry away is the
marginal one — **equal IO saved per bit spent, everywhere ⇒ FPR proportional to
level size** — and it is free: no new data structure, just arithmetic at
filter-build time.

Two boundaries. First, the argument assumes **point** lookups: filters do not
help range scans at all, since a scan must consult every run regardless, so a
scan-heavy workload gets nothing from any of this. Second, Monkey holds the
merge policy fixed while it tunes filters; §4.2-4.3 and Appendix D go on to
co-tune `T` and the merge policy against the same cost model, but the merging
half is done properly by Dostoevsky, the next chapter — which is also where the
sum-of-FPRs objective gets its second term.

## How to read the paper (with the concepts in hand)

Budget about 2 h. The section numbers below are the paper's own.

1. **§2 Background** — Equation 2, the FPR↔bits relation (Step 3), and the
   statement that everyone uses uniform bits per entry. Ten minutes.
2. **§3 LSM-tree design space** — the R/W/M cost model in terms of `T` and `L`
   (Step 1 with full generality). Map each symbol to your mini-LSM's knobs.
3. **§4.1 Minimizing Lookup Cost** — the heart: Equation 3 (Step 2), Equation 4
   (the memory model), Equations 5-6 (Step 5's optimum). Read Figure 6 first as
   a glossary. The Lagrange derivation itself is **Appendix B** — follow it once
   if you want it, but re-deriving "FPR ∝ level size" informally from Steps 3-4
   is the version worth being able to reproduce.
4. **§4.3 Scalability and Tunability** — Table 1, where the `O(L)` factor is
   shaved (Step 6's asymptotic claim).
5. **§5 Experimental Analysis** — Figure 11(A) for the ≈1 → ≈0.2 IOs per lookup,
   Figure 11(C) for the ≈60% memory saving. Check the setup paragraph before
   quoting anything: 5 bits per element, size ratio 2, block cache *off*.
6. Skim **§4.2 and Appendix D** (co-tuning `T` and the merge policy) — Dostoevsky
   does that half better. §6 is Related Work and §7 the conclusion; neither
   carries numbers.

## Questions to answer in notes.md

1. In your mini-LSM (3 levels, T=10, 10M keys), compute uniform-vs-Monkey
   expected false probes per zero-result get at 10 bits/key average — the
   4-level version is worked in Step 5, so redo it at L = 3 and check that the
   advantage *shrinks*. Then *measure* zero-result gets both ways (the
   experiment supports per-level bits-per-key for exactly this).
2. Monkey assumes point lookups dominate. What breaks for range scans?
   (Filters don't help ranges at all — prefix blooms exist for a subset.)
3. FalkorDB angle: an attribute store doing existence checks before edge
   insertion is a zero-result-heavy workload — where would Monkey's argument
   apply outside an LSM?

## Done when

Answer each before unfolding it.

- [ ] You can write down `R` for leveling and for tiering, naming every symbol.

  <details><summary>Answer</summary>

  `R` is the expected number of wasted IOs for a *zero-result* point lookup —
  one where the key exists nowhere, so every filter consulted that says "maybe"
  costs one useless disk read.

  Leveling: `R = Σ(i=1..L) p_i`, where `p_i` is the false-positive rate of level
  *i*'s filter and `L` is the number of levels on disk. Tiering:
  `R = (T−1) · Σ(i=1..L) p_i`, because tiering keeps up to `T−1` runs per level
  (the T-th arrival triggers a merge), all the same size and therefore all with
  the same FPR. Monkey §4.1, Equation 3 — whose leveling branch is misprinted as
  the tiering one; the prose above it gives the correct reading.

  The step that makes this a *sum* rather than something level-weighted is that
  a false positive costs one IO no matter which level it happened at: fence
  pointers take the probe straight to the single qualifying page (§4.1). If
  probing a big run cost more than probing a small one, the whole optimum would
  move.

  </details>

- [ ] You can convert between bits per key and FPR in both directions, and say what 1.4427 bits buys.

  <details><summary>Answer</summary>

  `FPR = e^(−(bits/entries)·ln(2)²)` (Monkey §2, Equation 2), and inverted for
  sizing, `bits = −entries · ln(FPR) / ln(2)²` (§4.1). With
  `ln(2)² = 0.480453`: 8 bits/key → 2.143%, 10 → 0.8193%, 12 → 0.3132%,
  14 → 0.1197%, 16 → 0.04578%.

  Because the relation is exponential, the cost of *halving* the FPR is constant:
  `ln2 / ln²2 = 1/ln2 = 1.4427` bits per key, from any starting point. That
  constancy is what makes the optimization clean — the marginal price of an FPR
  halving at level *i* is 1.4427 × (entries at level *i*) bits, so it differs
  between levels only through the entry count, by exactly a factor of `T` per
  level.

  The same formula is what fjall's lsm-tree implements in `calculate_m`
  (`src/table/filter/standard_bloom/builder.rs:129-150`), so this is a
  description of shipped filters, not an idealisation.

  </details>

- [ ] You can state the allocation rule and produce the bits-per-key ladder for a concrete tree.

  <details><summary>Answer</summary>

  Rule: **equal marginal IO saved per bit spent, everywhere ⇒ each level's FPR
  proportional to its number of entries** — "the optimal FPR at Level i is T
  times higher than the optimal FPR at Level i−1" (§4.1, from Equations 5-6,
  derived by Lagrange multipliers in Appendix B).

  Since FPR × T per level down, bits per key fall by a constant
  `ln(T)/ln(2)² = 4.793` per level at T = 10. On the worked tree — N = 10 M,
  T = 10, L = 4, entries 9 K / 90 K / 900 K / 9 M, budget 99,990,000 bits (10
  bits/key average):

  | level | uniform | Monkey |
  |---|---|---|
  | 1 | 10 bits, 0.8193% | 23.85 bits, 0.001057% |
  | 2 | 10 bits, 0.8193% | 19.05 bits, 0.01057% |
  | 3 | 10 bits, 0.8193% | 14.26 bits, 0.1057% |
  | 4 | 10 bits, 0.8193% | 9.47 bits, 1.057% |

  `R` falls from 0.0328 to 0.0117 — 2.79× fewer wasted IOs at identical memory —
  or, holding `R` fixed at 0.0328 instead, the Monkey allocation needs 78.6 Mbit
  against 99.99 Mbit, 21% less DRAM. Bottom line: giving up half a bit per key at
  the level holding 90% of the data funds an 8×-to-775× FPR improvement
  everywhere else.

  </details>

- [ ] You can say what the paper actually measured, and on what hardware, without rounding it into folklore.

  <details><summary>Answer</summary>

  §5: Monkey built on LevelDB and differing *only* in filter allocation; 1 GB of
  1 KB entries; 16 K uniformly random zero-result point lookups; size ratio 2;
  1 MB buffer; 5 bits per element of filter memory; LevelDB's block cache
  **disabled**; a 500 GB 7200 RPM disk with 32 GB RAM and 4 × 2.7 GHz cores.

  Results: lookup latency 50%–80% lower, with the margin *growing* as data volume
  grows (Abstract; §5 "up to 80%"); Figure 11(A) annotated ≈1 IO per lookup for
  LevelDB against ≈0.2 for Monkey; Figure 11(C), equal performance at up to ≈60%
  less filter memory.

  The caveats that matter when quoting it: a 2017 spinning disk makes a saved IO
  worth ~10 ms, so the latency ratio is not transferable to NVMe; the block cache
  was off; and the default size ratio was 2, not the 10 this chapter's worked
  example uses. This repo adds no measurement of its own here — topic 4 has no
  `verify.sh` lane, because its benches measure only your code.

  </details>

- [ ] You can name the two things Monkey does *not* fix.

  <details><summary>Answer</summary>

  First, **range scans**. Every result here is about point lookups, and about
  zero-result ones in particular; a range scan must open every run whose key
  range overlaps the range regardless of what any filter says, so a scan-heavy
  workload sees none of this benefit. Prefix bloom filters help a narrow subset
  (scans over a fixed key prefix) and nothing else.

  Second, **the merge policy**. Monkey holds the merge policy fixed while it
  optimizes filters; the `R = Σ p_i` objective says nothing about update cost,
  and the same tree tuned for lookups may be paying 20× write amplification to
  get there (topic 4's `notes.md` works that out for leveled at T = 10, L = 4).
  §4.2-4.3 and Appendix D extend the cost model to co-tune `T` and the policy,
  but the properly worked answer — different policies at different levels — is
  Dostoevsky's, in the next chapter.

  </details>

## References

**Papers**
- Dayan, Athanassoulis, Idreos — *Monkey: Optimal Navigable Key-Value Store*,
  SIGMOD 2017. Read §2 (Equation 2), §3 (the design space), §4.1 (Equations 3-6,
  the whole argument), §4.3 Table 1 (the `O(L)` saving), §5 Figure 11 (the
  numbers). Appendix B is the Lagrange derivation; Appendix C is the iterative
  allocator for variable entry sizes; Appendix D co-tunes the size ratio.

| Claim in this chapter | Source |
|---|---|
| `R` = sum of per-level FPRs (leveling); ×(T−1) for tiering | §4.1, Equation 3 + preceding prose |
| One IO per false positive regardless of level, via fence pointers | §4.1 |
| `FPR = e^(−(bits/entries)·ln²2)` | §2, Equation 2 |
| Level *i* holds `N/T^(L−i) · (T−1)/T` entries | §4.1 |
| Filter memory model `M_filters` in terms of the `p_i` | §4.1, Equation 4 |
| Optimal FPR is T× higher per level down ⇒ ∝ level size | §4.1, Equations 5-6; derived in Appendix B |
| Deepest filters disappear as their optimal FPR → 1 | §4.1, the `L_filtered` term |
| Lookup cost loses its `O(L)` factor | §4.3, Table 1 |
| 50%–80% lower lookup latency; ≈1 → ≈0.2 IOs; ≈60% less memory | Abstract; §5 and Figure 11(A), (C) |

**Code**
- `lsm-tree src/table/filter/standard_bloom/builder.rs:129-150` at `8526dd3` —
  Equation 2, shipped, for comparison with the paper's algebra.
