# Learned indexes: the index is a model of the CDF

An index maps a key to a position in a sorted array. If the key
distribution is smooth, a handful of linear models approximates that map
with a **bounded error** (a hard cap ε on how far a prediction can miss)
you binary-search away — replacing a tree walk's dependent memory
accesses with two multiply-adds. Three designs mark the territory: RMI
(the provocation — fast, no guarantee), PGM (the guarantee — our stub),
and ALEX (the one that takes writes). This chapter builds the idea from
the reframe up — index as function, error bounds, segment construction,
updatability — then anchors each piece in the PGM and ALEX sources.

Every code anchor below is PGM-index at commit `c6fcf3d`
(`include/pgm/pgm_index.hpp` is 266 lines, `piecewise_linear_model.hpp`
is 365) and ALEX at commit `4370da6` (`src/core/alex_nodes.h` is 2330
lines), quoted with the line numbers each occupies in that revision.
Paper facts come from Kraska et al. 2018 ("The Case for Learned Index
Structures"), the PGM-index paper (Ferragina & Vinciguerra, VLDB 2020)
and the ALEX paper (Ding et al., SIGMOD 2020); every number names the
section or figure it came from.

## The problem in one sentence

On this topic's motivation bench a point-miss over 10M sorted u64 keys
costs **246 ns** by binary search ([FINDINGS.md](../../FINDINGS.md) row
26) — about **⌈log₂(10,000,000)⌉ = 24** dependent comparisons, each a
branch mispredict into a different cache line — and if the key
distribution is smooth, most of those 24 hops land where a
two-multiply-add linear model would have predicted for free.

## The concepts, step by step

### Step 1 — the reframe: an index is a function, and a B-tree is already a model

> **In:** nothing yet — this step fixes the reframe every later step
> leans on.
> **Out:** the identity `pos = n · CDF(key)`, and the "predict, then
> search a small window" template that Steps 2, 3 and 5 each implement
> with a different accuracy guarantee.

An index is a function from key to position in a sorted array — and that
function is precisely the **CDF** (cumulative distribution function: the
fraction of keys ≤ x, a number in [0, 1]) of the key distribution, scaled
by n. Kraska et al. open on exactly this — "a B-Tree-Index can be seen as
a model to map a key to the position of a record within a sorted array"
(2018, Abstract). Written out:

```
  pos(key) = n · CDF(key)          n = number of keys, CDF(key) ∈ [0, 1]
```

Name the symbols: **n** is the key count, **CDF(key)** is the fraction of
keys ≤ key, and **pos(key)** is that key's index in the sorted array.
Worked on 10M keys: a key at the 37th percentile (CDF = 0.37) sits at
pos = 10,000,000 × 0.37 = **3,700,000**; the median (CDF = 0.5) at
**5,000,000**. A B-tree computes this same `pos ≈ n · CDF(key)` as a
**piecewise-constant** approximation — one constant per leaf — with
worst-case-everything guarantees; if the CDF is *smooth*, a few **linear**
models predict the position in O(1) with a small residual to
binary-search away:

```
  B-tree:  ~log_B(n) node hops, each a dependent miss   (246 ns, ~24 comparisons)
  learned: 1-2 model evals + binary search of a 2ε      (the bet: most of the
           window                                        tree walk is predictable)
```

The bet, stated honestly: trade guaranteed log-time on any distribution
for near-constant time on distributions that are actually predictable —
auto-increment IDs, steady-ingest timestamps.

### Step 2 — RMI: the provocation, without a safety net

> **In:** the reframe from Step 1 — index as `n · CDF(key)`.
> **Out:** a fast but *unbounded* design; the missing error guarantee is
> exactly what Step 3 (PGM) supplies.

The **RMI** (recursive-model index, Kraska §3.2) is a hierarchy of models
— inspired by the mixture-of-experts idea — where an upper-stage model
does not predict the position but *picks* which lower-stage model does.
Formally, "at stage ℓ there are Mℓ models"; the stage-0 model f0(x) ≈ y
takes the key and selects a model in the next stage, "until the final
stage predicts the position" (Kraska §3.2, Figure 3). There is **no
search between stages** — each stage is a bare model evaluation. Their
experiments use two stages.

Why a hierarchy at all: one model over 100M keys cannot get the residual
small, but "reducing the error to 10k from 100M ... a precision gain of
100 ∗ 100 = 10000 to replace the first 2 layers of a B-Tree ... is much
easier" (Kraska §3.2). Worked: stage 0 narrows 100,000,000 → 10,000 (a
10⁴ cut), and the picked stage-1 model then narrows 10,000 → 100 — the
same 10⁴ overall, split across two easy models instead of one impossible
one.

The flaw that motivates everything after it: **no error bound**. A
stage-2 model that fits badly on some key region gives a prediction off
by thousands of slots, the correcting last-mile search becomes long and
unpredictable, and there is no principled ε to size it. (The naïve
single-network version made the point by counter-example: it took
"≈ 80,000 ns" per lookup in TensorFlow versus "≈ 300ns" for a B-tree
traversal over the same data, Kraska §2.3 — accuracy, not raw model
speed, is the game.) RMI proved the reframe was fast; it did not make it
safe.

### Step 3 — PGM: fix the error first, then minimize the model

> **In:** Step 2's missing guarantee.
> **Out:** the ε-bounded segment and the recursive lookup, whose one-pass
> construction Step 4 details.

The **PGM-index** (Piecewise Geometric Model) inverts the design: choose a
hard error bound **ε** (the maximum a prediction may differ from the true
position) *up front*, then compute the **minimum number of linear
segments** such that every key's predicted position is within ε of the
truth. That is the paper's Definition 2: "computing the PLA-model which
minimises the number of its segments ... provided that each segment is
ε-approximate for its covered range of keys." A lookup evaluates the
segment's line, then binary-searches a window of just **2ε + 2** slots.

That window is not folklore — it is two macros in the header:

```cpp
// pgm_index.hpp — the search-window macros, 32-33
    32  #define PGM_SUB_EPS(x, epsilon) ((x) <= (epsilon) ? 0 : ((x) - (epsilon)))
    33  #define PGM_ADD_EPS(x, epsilon, size) ((x) + (epsilon) + 2 >= (size) ? (size) : (x) + (epsilon) + 2)
```

`search` (:192-198) predicts `pos`, then returns `[PGM_SUB_EPS(pos, ε),
PGM_ADD_EPS(pos, ε, n))`. Name the symbols: **x** is the predicted
position, **epsilon** the bound, **size** = n the key count; the low edge
clamps at 0 and the high edge at n, and the trailing `+ 2` covers the
segment boundary. Worked at ε = 64, n = 10M, predicted pos = 5,000,000:
the window is [5,000,000 − 64, 5,000,000 + 64 + 2) = **[4,999,936,
5,000,066)**, width **130 = 2ε + 2**, so the final binary search is
⌈log₂130⌉ = **8** comparisons instead of the full 24. At ε = 16 the width
is 34 (⌈log₂34⌉ = 6); at ε = 256 it is 514 (⌈log₂514⌉ = 10).

To find the right segment among (say) 2,000 of them, PGM indexes the
segments' first keys with... another PGM, recursively, until one segment
remains (PGM paper §3.2: "proceed recursively by building another optimal
PLA-model ... until the PLA-model consists of one" segment). Each level is
itself ε-bounded, so `segment_for_key` (:134) descends level to level and
each hop is a **constant-size** search over a window of `EpsilonRecursive`
slots (:143-153, default EpsilonRecursive = 4), not a binary search over
all segments. Why it matters: the segments (a few KB) live in cache where
a B-tree's upper levels may not, and the ε guarantee holds on *any*
distribution — hostile keys cost more *segments* (space), never a longer
lookup. Our `epsilon_holds_on_hostile_distribution` test pins exactly
that.

### Step 4 — building segments in one pass: the shrinking cone

> **In:** the ε target from Step 3.
> **Out:** an O(n) streaming build, and the *static* limitation (one
> insert invalidates every later position) that Step 5 removes.

Computing an ε-bounded piecewise-linear fit sounds expensive but is a
streaming, O(n) pass: maintain the set of lines that could still fit every
point seen so far within ε, and emit a segment the moment that set goes
empty. PGM's `OptimalPiecewiseLinearModel` (`piecewise_linear_model.hpp:45`)
runs the *optimal* version — the streaming convex-hull method the PGM
paper proves yields the fewest segments for a given ε (Lemma 1: it
"computes the minimum number of segments"). It keeps upper and lower hulls
in a `rectangle[4]` (`add_point` :96; the point-outside test that closes a
segment at :130-136; hull maintenance :154-158; `get_segment` :190).

Our stub uses the simpler heuristic the PGM paper attributes to the
FITing-tree and calls the **shrinking cone** — "linear in time but does
not guarantee to find the optimal PLA-model" (PGM paper §3.1). Keep an
interval `[lo, hi]` of feasible slopes through the segment's *first*
point; each new point narrows it; emit when it empties. The narrowing is
the whole algorithm, and it is the stub's own doc comment:

```rust
// topics/26-probabilistic/experiments/src/pgm.rs — LearnedIndex::build stub, 22-32
    22      /// STUB — shrinking-cone greedy PLA over sorted, deduped keys:
    23      /// open a segment at (k0, pos0) with slope cone (lo, hi) = (0, inf);
    24      /// for each next point (k, pos), the segment can keep it iff some
    25      /// slope in the cone predicts pos within eps — narrow the cone to
    26      ///   lo = max(lo, (pos - eps - pos0) / (k - k0))
    27      ///   hi = min(hi, (pos + eps - pos0) / (k - k0))
    28      /// and close the segment (emit slope = (lo+hi)/2) when the cone
    29      /// empties, starting a fresh one at (k, pos).
    30      pub fn build(_keys: &[u64], _epsilon: usize) -> LearnedIndex {
    31          todo!("greedy shrinking-cone segmentation")
    32      }
```

Name the symbols: **(k0, pos0)** is the segment's anchor point, **(k,
pos)** the incoming key and its true rank, **eps** the bound, and
**[lo, hi]** the still-feasible slopes for a line *through the anchor*.
Worked on four points at ε = 1, anchor (k0, pos0) = (0, 0), cone starting
(0, ∞):

```
  pt (1, 0):  lo=max(0,  (0-1-0)/1)=0.000   hi=min(∞, (0+1-0)/1)=1.000   [0.000, 1.000] open
  pt (2, 2):  lo=max(0,  (2-1-0)/2)=0.500   hi=min(1, (2+1-0)/2)=1.000   [0.500, 1.000] open
  pt (3, 5):  lo=max(.5, (5-1-0)/3)=1.333   hi=min(1, (5+1-0)/3)=1.000   [1.333, 1.000] EMPTY
```

At the third point lo (1.333) exceeds hi (1.000): no single slope through
(0, 0) keeps all four within ε = 1, so the segment closes after three
points and a fresh one opens at (3, 5). The catch that the cone pays for
being cheap is visible here — a line *not* forced through the anchor,
`pos = 1.5·k − 0.5`, fits all four (residuals −0.5, +1.0, +0.5, −1.0, all
≤ 1), so the optimal convex-hull method would have kept the segment open.
That is why the stub emits ≥ as many segments as PGM, never fewer.

The cost profile that falls out: build is O(n) single-pass (versus a
B-tree's O(n log n) of page splits), and on uniform keys segments ≪ n —
PGM reserves only `n / (epsilon * epsilon)` of them (`pgm_index.hpp:97`),
which at n = 1M, ε = 64 is 1,000,000 / 4096 = **244**, comfortably under
the `uniform_data_compresses_hard` test's ceiling of n/500 = 2,000. But
the structure is *static*: one insert shifts every rank after it, so every
downstream segment is invalidated. That is Step 5's problem.

### Step 5 — ALEX: gapped arrays make the model updatable

> **In:** Step 4's static limitation — a PGM must rebuild on insert.
> **Out:** a design that absorbs writes in the *data layout*, at a
> write-amplification cost Step 6 puts on the scoreboard.

A static PGM rebuilds on change; **ALEX** makes the data layout absorb
updates instead. Its data nodes are **gapped arrays** — sorted arrays
with empty slots deliberately interspersed — and the model is used not
only to search but to *place*. ALEX does not pack the array full: it holds
density between a lower and an upper limit, "dl = 0.6 and du = 0.8 to
achieve average data storage utilization of 0.7" (ALEX §4.3.1), so on
average **~30% of slots are gaps**, not the half a first guess suggests.
Gaps are filled "with the closest key to the right of the gap, which helps
maintain exponential search performance" (§3.1).

The per-node model is a clamped linear predictor:

```cpp
// alex_nodes.h — AlexDataNode::predict_position, 1448-1452
  1448    inline int predict_position(const T& key) const {
  1449      int position = this->model_.predict(key);
  1450      position = std::max<int>(std::min<int>(position, data_capacity_ - 1), 0);
  1451      return position;
  1452    }
```

Line 1450 is the one to watch: the raw model output is clamped to
`[0, data_capacity_ - 1]`, so a wild prediction can never index out of the
array. Worked at `data_capacity_ = 1,000,000`: a model that outputs −3 is
clamped to **0**, one that outputs 1,050,000 to **999,999**, and a sane
500,000 passes through unchanged. Lookups then use **exponential search**
from that slot — `find_key` (:1456) calls `exponential_search_upper_bound`
(:1557), probing at distance 1, 2, 4, 8, … until the key is bracketed,
then binary-searching the bracket. Cost is O(log d) in the model's
*actual* error d, so it adapts without PGM's hard-ε accounting: if the
model is off by d = 100 slots the search takes about ⌈log₂100⌉ = **7**
doublings plus a short binary search; if it is spot-on, 0–1 probes. The
paper relies on exactly this — "exponential search without bounds is
faster than binary search with bounds ... because if the models are good,
their prediction is close enough to the correct position" (§3.1).

Insertion places a key near its predicted slot and shifts toward the
**closest gap** rather than the array end (`insert_element_at` calls
`closest_gap` :1935; the shift count is tallied in `num_shifts_`
:1915/:1927), so the data keeps matching the model as it arrives. When a
node's fill would exceed du it expands — allocating `n / dl` new slots and
re-inserting every element under a retrained model (§4.3.2). Those shifts
and expansions are ALEX's cost currency: **write amplification**. The
honest correction to the folklore that ALEX falls over on adversarial
inserts — the paper measures the opposite for the classic adversary:
initialized with the 50M smallest keys and fed the rest in ascending
sorted order, "ALEX has up to 3.6× higher throughput than B+Tree"
(§6.2.6). Where it *can* degrade is a model so mismatched to the arriving
keys that shifts run long and expansions come often; constructing that
case is the exercise, precisely because the sorted-insert one does not.

### Step 6 — the honest scoreboard: how each design degrades

> **In:** the three designs from Steps 2, 3 and 5.
> **Out:** the one axis that separates them — *which resource* gives out
> on hostile data.

The deep difference between the three is not speed on friendly data — it
is *which resource* gives out on hostile data:

```
              build      lookup (smooth keys)   lookup (hostile)   inserts
  B-tree      O(n log n) ~log_B(n) hops         same               native
  RMI         train      fast, NO bound         can be terrible    no
  PGM         O(n)       1-3 hops + 2ε window   MORE segments,     static (rebuild);
                                                bound still holds  PGM-dynamic: LSM-of-PGMs
  ALEX        O(n)       predict + exp search   more shifts /      native, gapped
                                                expansions         (robust to sorted, §6.2.6)
```

The ε guarantee is the dividing line: PGM degrades in *space* (more
segments) while its lookup stays bounded at 2ε + 2; RMI degrades in *time*
(no bound on the last-mile search); ALEX degrades in *write amplification*
(shifts and node expansions), though the paper shows that stays modest
even under sorted-order inserts (§6.2.6). The B-tree degrades in nothing
and wins on nothing — which is exactly why it is the incumbent.

## Where each step lives in the code

PGM — Steps 3–4
([`~/repos/PGM-index/include/pgm/`](https://github.com/gvinciguerra/PGM-index),
commit `c6fcf3d`):

| anchor | what it is |
|---|---|
| `pgm_index.hpp:32-33` | `PGM_SUB_EPS`/`PGM_ADD_EPS` — the window is [pos−ε, pos+ε+2), clamped to [0, n); the trailing `+2` covers the segment boundary (Step 3's quoted block) |
| `pgm_index.hpp:66-67` | `template<..., size_t Epsilon = 64, size_t EpsilonRecursive = 4, ...> class PGMIndex`; `build` :88 loops `make_segmentation` per level |
| `segment_for_key` :134 | the recursive descent (:143-153): each level is itself ε-bounded, so each hop is a *constant-size* search over `EpsilonRecursive` slots, not a binary search over all segments |
| `search` :192 | predict, widen by ε, return the `ApproxPos` window (approximate position + [lo, hi)) — our `search_window` |
| `piecewise_linear_model.hpp:45` | `OptimalPiecewiseLinearModel` — the optimal streaming convex-hull PLA (PGM paper Lemma 1: minimum segments in O(n)) |
| `add_point` :96; outside-test :130-136; hull update :154-158; `get_segment` :190 | maintains upper/lower hulls in `rectangle[4]`; the segment closes when a new point falls outside the feasible parallelogram (:133-136 returns `false`) |
| `make_segmentation` :276 | the greedy driver: `if (!opt.add_point(x,y)) { out(get_segment()); re-add }` (:280-286) |

ALEX — Step 5
([`~/repos/ALEX/src/core/alex_nodes.h`](https://github.com/microsoft/ALEX),
commit `4370da6`):

| anchor | what it is |
|---|---|
| `class AlexDataNode` :293 | gapped array + per-node linear model; `data_capacity_` :324 (slots) vs `num_keys_` :325 (filled) is the gap budget — default du = 0.8 fullness, ~0.7 utilization (§4.3.1) |
| `predict_position` :1448 | the model eval; :1450 clamps the output to `[0, data_capacity_-1]` (Step 5's quoted block) |
| `find_key` :1456 | predict, then `exponential_search_upper_bound` :1462 (defined :1557) from the predicted slot — cost O(log distance-of-model-error), no ε needed |
| `find_insert_position` :1497 | the same predict-then-search on the insert path |
| `check_exists` :474; `get_next_filled_position` :1513; `closest_gap` :1935 | the gap machinery: the bitmap marks gap vs key (:474), inserts shift toward the *closest gap* not the array end (:1935; shifts tallied in `num_shifts_` :1915/:1927) |

## Questions to answer in notes.md

1. Construct 4 points where the cone closes a segment but the hull method
   keeps going. (Hint: the cone forces every prediction line through the
   *first* point; optimal PLA doesn't.)
2. ε trades segment count against final-search width. Segments live in
   cache; the 2ε window is one or two line fetches into the data. Given
   the motivation numbers (246 ns ≈ 24 comparisons,
   [FINDINGS.md](../../FINDINGS.md) row 26), predict the ns/lookup curve
   for ε ∈ {16, 64, 256} on 10M uniform keys *before* running
   filter_bench.
3. `uniform_data_compresses_hard` demands < 2K segments for 1M random
   u64. Why is a *uniform* CDF the easy case, and what real key patterns
   are near-uniform? (auto-increment IDs, timestamps at steady ingest,
   ...) What breaks it? (hot/cold tenants, hash-distributed keys with
   gaps, ...)
4. Adversarial inserts: append keys so every new key lands at the same
   predicted slot (e.g. exponentially clustered values). What happens to
   ALEX's shifts-per-insert, and which classical structure degrades the
   same way under sorted-order inserts? (This is the "does ALEX survive
   adversarial inserts?" question in notes.md — predict, then read the
   paper's §6.2.6, which measures ALEX at up to 3.6× B+Tree throughput on
   sorted-order inserts, and §5.1's worst-case RMI-depth bound.)
5. **(cross-topic)** ALEX's gapped array + model placement vs a B-tree
   leaf with slotted-page free space (topic 2): both reserve slack to
   make inserts local. What does ALEX's *model* buy over the B-tree's
   binary search within the leaf, and when is it worth zero? (Uniform
   small leaves fit in one cache line either way.)

## Done when

Answer each before unfolding it.

- [ ] You can state the reframe: an index is a model of the CDF, and a B-tree is already one.

  <details><summary>Answer</summary>

  An index is a function from key to position in a sorted array, and that
  function is `pos(key) = n · CDF(key)` — n the key count, CDF(key) the
  fraction of keys ≤ key, so pos is the key's rank (Step 1; Kraska 2018,
  Abstract: "a B-Tree-Index can be seen as a model to map a key to the
  position of a record within a sorted array"). Worked: 10M keys, a key at
  CDF = 0.37 sits at 10,000,000 × 0.37 = 3,700,000.

  A B-tree is already a model of that CDF — a *piecewise-constant* one, one
  constant per leaf, with worst-case guarantees. The learned bet is to
  replace it with a *piecewise-linear* model where the CDF is smooth, so a
  prediction plus a tiny corrective search beats the ~24 dependent
  comparisons a 10M-key binary search costs (246 ns,
  [FINDINGS.md](../../FINDINGS.md) row 26).

  </details>

- [ ] You can explain what RMI provokes and what safety net it lacks.

  <details><summary>Answer</summary>

  The RMI (recursive-model index, Kraska §3.2, Figure 3) is a hierarchy of
  models where an upper stage *picks* the lower-stage model rather than
  predicting a position — "at stage ℓ there are Mℓ models ... until the
  final stage predicts the position", with no search between stages. It
  provokes by proving the reframe is fast: splitting a hard 100M→100 fit
  into two easy stages (100M→10k then 10k→100, "a precision gain of
  100 ∗ 100 = 10000", Kraska §3.2).

  The safety net it lacks is an **error bound**. A stage model that fits
  badly gives a prediction off by thousands of slots, and the last-mile
  search that corrects it has no principled ε to size it — so the
  worst-case lookup is unbounded. That missing guarantee is exactly what
  PGM supplies (Step 3).

  </details>

- [ ] You can explain PGM's inversion: fix the error bound first, then minimize the model.

  <details><summary>Answer</summary>

  PGM fixes a hard ε up front, then computes the *minimum* number of
  ε-approximate linear segments (PGM paper Definition 2: "the PLA-model
  which minimises the number of its segments ... provided that each
  segment is ε-approximate"). A lookup evaluates the segment's line and
  binary-searches a window of exactly 2ε + 2 slots, spelled out by
  `PGM_SUB_EPS`/`PGM_ADD_EPS` (`pgm_index.hpp:32-33`) and returned by
  `search` (:192).

  Worked at ε = 64, pos = 5,000,000, n = 10M: window [4,999,936,
  5,000,066), width 130 = 2ε + 2, final search ⌈log₂130⌉ = 8 comparisons
  versus the full 24. Segments are found by another PGM recursively
  (§3.2), each level ε-bounded so each hop is a constant-size search
  (`segment_for_key` :134, :143-153). The payoff: the guarantee holds on
  *any* distribution — hostile keys cost more segments (space), never a
  longer lookup.

  </details>

- [ ] You can construct four points where the shrinking cone closes a segment.

  <details><summary>Answer</summary>

  Take ε = 1, anchor (k0, pos0) = (0, 0), cone starting (0, ∞), and the
  points (1, 0), (2, 2), (3, 5). The cone narrows with
  `lo = max(lo, (pos−eps−pos0)/(k−k0))`, `hi = min(hi, (pos+eps−pos0)/(k−k0))`
  (the stub doc, `pgm.rs:22-32`):

  - (1, 0): lo = max(0, −1) = 0.000, hi = min(∞, 1) = 1.000 → [0.000, 1.000]
  - (2, 2): lo = max(0, 0.5) = 0.500, hi = min(1, 1.5) = 1.000 → [0.500, 1.000]
  - (3, 5): lo = max(0.5, 1.333) = 1.333, hi = min(1, 2.0) = 1.000 → **empty**

  At the third point lo > hi, so the segment closes after three points and
  a new one opens at (3, 5). This is also the answer to notes.md Q1: the
  line `pos = 1.5·k − 0.5` (not through the anchor) fits all four within
  ε = 1 (residuals −0.5, +1.0, +0.5, −1.0), so PGM's optimal convex-hull
  method keeps the segment open where the cone splits — the cone forces
  every candidate line through the *first* point, which optimal PLA does
  not.

  </details>

- [ ] You can explain how ε trades segment count against final search width.

  <details><summary>Answer</summary>

  ε is the only knob. A larger ε lets one line cover a wider key range, so
  there are *fewer* segments (PGM reserves `n/(epsilon*epsilon)`,
  `pgm_index.hpp:97` — at n = 1M, ε = 64 that is 244), but the final search
  window is *wider*: 2ε + 2 slots, `⌈log₂(2ε+2)⌉` comparisons in the data.

  Worked on 10M keys: ε = 16 → window 34, ⌈log₂34⌉ = 6; ε = 64 → window
  130, 8; ε = 256 → window 514, 10. So doubling-and-then-some of ε adds
  ~2 comparisons to the last-mile search while cutting segment count
  roughly with ε². Segments live in cache (cheap to touch); the 2ε window
  is one or two line fetches into the 76 MB data array (expensive), which
  is why the sweet spot is workload-dependent — the subject of notes.md
  Q2.

  </details>

- [ ] You can describe an adversarial insert sequence and how ALEX's gapped arrays respond.

  <details><summary>Answer</summary>

  A gapped array is a sorted array kept below a density limit — default
  du = 0.8, ~0.7 average utilization, so ~30% gaps (ALEX §4.3.1). Inserts
  place a key at its model-predicted slot and shift toward the *closest
  gap* (`closest_gap` :1935, shifts tallied in `num_shifts_`
  :1915/:1927); when a node would exceed du it expands to `n/dl` slots and
  re-inserts under a retrained model (§4.3.2). The adversary is a stream
  whose keys the node's linear model cannot fit — every insert then shifts
  far and expansions come often, driving up write amplification.

  The honest result, though: for the *classic* adversary — sorted-order
  inserts, every new key larger than all present — ALEX is measured at "up
  to 3.6× higher throughput than B+Tree" (§6.2.6), the same pattern that
  degrades a naïve B-tree's rightmost leaf. So the degrading case has to be
  a genuine model mismatch, not merely sorted input; that is the
  construction notes.md Q4 asks for, checked against §6.2.6 and the
  §5.1 RMI-depth bound.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five (Step references in parentheses): Q1 — four points where the
  cone splits but optimal PLA continues (Step 4; the (0,0),(1,0),(2,2),(3,5)
  construction above). Q2 — the predicted ns/lookup curve for
  ε ∈ {16, 64, 256}, reasoned from window widths 34/130/514 and the 246 ns
  ≈ 24-comparison baseline ([FINDINGS.md](../../FINDINGS.md) row 26).
  Q3 — why a uniform CDF compresses to < 2,000 segments for 1M keys and
  what breaks it (Step 4; near-uniform = auto-increment IDs, steady-ingest
  timestamps; broken by hot/cold tenants, hash-distributed keys). Q4 —
  the adversarial-insert construction and ALEX's shift/expansion response
  (Step 5, §6.2.6). Q5 — ALEX's model-placement vs a slotted B-tree leaf
  (topic 2).

  Write them as predictions *before* running `filter_bench`; the point of
  notes.md is to keep the wrong predictions next to the measured numbers.

  </details>

## References

**Papers**
- Kraska, Beutel, Chi, Dean, Polyzotis — "The Case for Learned Index
  Structures" (SIGMOD 2018,
  [arXiv:1712.01208](https://arxiv.org/abs/1712.01208)) — §2.3 (the naïve
  learned index), §3.2 (RMI, Figure 3), skim the rest
- Ferragina & Vinciguerra — "The PGM-index" (VLDB 2020,
  [pgm.di.unipi.it](https://pgm.di.unipi.it)) — §3.1 Definition 2
  (ε-approximate PLA), §3.1 the shrinking cone vs optimal streaming
  convex hull (Lemma 1), §3.2 the recursive construction
- Ding et al. — "ALEX: An Updatable Adaptive Learned Index" (SIGMOD
  2020, [arXiv:1905.08898](https://arxiv.org/abs/1905.08898)) — §3.1
  (gapped array + exponential search), §4.3 (density limits, node
  expansion), §6.2.6 (robustness to sorted-order inserts)

**Code**
- [PGM-index](https://github.com/gvinciguerra/PGM-index) @ `c6fcf3d`
  `include/pgm/` — `pgm_index.hpp` + `piecewise_linear_model.hpp`
- [ALEX](https://github.com/microsoft/ALEX) @ `4370da6` `src/core/` —
  `alex_nodes.h` is where the gapped-array machinery lives
