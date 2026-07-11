# Reading guide — Learned indexes: Kraska'18 → PGM → ALEX

**Sources:**
- Kraska, Beutel, Chi, Dean, Polyzotis — "The Case for Learned Index
  Structures" (SIGMOD 2018) — read §1-3 (RMI), skim the rest
- Ferragina & Vinciguerra — "The PGM-index" (VLDB 2020) + code at
  [`~/repos/PGM-index/include/pgm/`](https://github.com/gvinciguerra/PGM-index)
- Ding et al. — "ALEX: An Updatable Adaptive Learned Index" (SIGMOD 2020) +
  code at [`~/repos/ALEX/src/core/`](https://github.com/microsoft/ALEX)

## 1. Reframe: a B-tree is already a model

Kraska's opening move: an index maps key → position, i.e. it approximates
the CDF of the key distribution scaled by n. A B-tree is a piecewise-constant
approximation with worst-case-everything guarantees; if the CDF is *smooth*,
a few linear models predict position in O(1) with a small error to
binary-search away:

```
  pos ≈ n · CDF(key)

  B-tree:  log_B(n) node hops, each a cache miss    (167 ns measured, ~23 misses)
  learned: 1-2 model evals + binary search of 2ε    (the bet: most of the
           window                                    tree walk is predictable)
```

RMI (Kraska §3) = a fixed 2-stage hierarchy of models where stage 1 *picks*
the stage-2 model. Its flaw: **no error bound** — a bad model means a long
exponential search, and there's no principled way to size the stages.

## 2. PGM — the version with a guarantee (this is our stub)

PGM inverts the design: fix the error ε *first*, then compute the **minimum
number of linear segments** such that every key's predicted position is
within ε of the truth. Then index the segments' first keys with... another
PGM, recursively, until one segment remains.

| anchor ([`~/repos/PGM-index/include/pgm/`](https://github.com/gvinciguerra/PGM-index)) | what it is |
|---|---|
| `pgm_index.hpp:32-33` | `PGM_SUB_EPS`/`PGM_ADD_EPS` — the window is [pos−ε, pos+ε+2), clamped; the +2 matters (segment boundaries) |
| `pgm_index.hpp:67` | `class PGMIndex`; `build` :88 loops `make_segmentation` per level |
| `segment_for_key` :134 | the recursive descent: each level is itself ε-bounded, so each hop is a *constant-size* search (:143-152), not a binary search over all segments |
| `search` :192 | predict, widen by ε, return the window — our `search_window` |
| `piecewise_linear_model.hpp:45` | `OptimalPiecewiseLinearModel` — O'Rourke '81 streaming convex-hull method |
| `add_point` :96, hull updates :154-190 | maintains upper/lower convex hulls of the feasible-slope region; segment closes when hulls cross |
| `make_segmentation` :276 | the greedy driver: `if (!opt.add_point(x,y)) { out(segment); start fresh }` |

The hull method is *optimal* (fewest segments for a given ε). Our stub uses
the simpler **shrinking cone**: keep an interval [lo, hi] of feasible slopes
through the segment's first point; each new point narrows it; emit when
empty. Same ε guarantee, ≥ as many segments, and O(1) state instead of two
hulls.

**Q1.** Construct 4 points where the cone closes a segment but the hull
method keeps going. (Hint: the cone forces every prediction line through
the *first* point; optimal PLA doesn't.)

**Q2.** ε trades segment count against final-search width. Segments live in
cache; the 2ε window is one or two line fetches into the data. Given the
motivation numbers (167 ns ≈ 23 misses), predict the ns/lookup curve for
ε ∈ {16, 64, 256} on 10M uniform keys *before* running filter_bench.

**Q3.** `uniform_data_compresses_hard` demands < 2K segments for 1M random
u64. Why is a *uniform* CDF the easy case, and what real key patterns are
near-uniform? (auto-increment IDs, timestamps at steady ingest, ...) What
breaks it? (hot/cold tenants, hash-distributed keys with gaps, ...)

## 3. ALEX — answering "but what about inserts?"

A static PGM re-builds on change. ALEX makes the *data layout* absorb
updates: nodes are **gapped arrays** (~50% slack), and the model is used
not only to search but to *place* — model-based insertion puts a key at its
predicted slot, so the model stays accurate as data arrives.

| anchor ([`~/repos/ALEX/src/core/alex_nodes.h`](https://github.com/microsoft/ALEX)) | what it is |
|---|---|
| `class AlexDataNode` :293 | gapped array + per-node linear model; `num_keys_` :325 vs slots = the gap budget |
| `predict_position` :1448 | the model eval |
| `find_key` :1456 | predict, then `exponential_search_upper_bound` :1462 from the predicted slot — cost is O(log distance-of-model-error), no ε needed |
| `find_insert_position` :1497 | same predict-then-search on the insert path |
| :28, :474, :1513 | the gap machinery: bitmap marks gap vs key; inserts shift toward the *closest gap*, not the array end |

Exponential search is the right primitive when the error is usually 0-2
slots but unbounded: cost adapts to actual error, and it's why ALEX can
skip PGM's hard-ε accounting. When a node overflows its density bound it
splits and retrains — the B-tree skeleton reappears, but with models as
node search and gaps as write absorbers.

**Q4.** Adversarial inserts: append keys so every new key lands at the same
predicted slot (e.g. exponentially clustered values). What happens to
ALEX's shifts-per-insert, and which classical structure degrades the same
way under sorted-order inserts? (This is the "does ALEX survive
adversarial inserts?" question in notes.md — predict, then read the paper's
§5.5.)

**Q5 (cross-topic).** ALEX's gapped array + model placement vs a B-tree
leaf with slotted-page free space (topic 2): both reserve slack to make
inserts local. What does ALEX's *model* buy over the B-tree's binary search
within the leaf, and when is it worth zero? (Uniform small leaves fit in
one cache line either way.)

## 4. The honest scoreboard

```
              build      lookup (smooth keys)   lookup (hostile)   inserts
  B-tree      O(n log n) ~log_B(n) misses       same               native
  RMI         train      fast, NO bound         can be terrible    no
  PGM         O(n)       1-3 hops + 2ε window   MORE segments,     PGM-dynamic:
                                                bound still holds  LSM-of-PGMs
  ALEX        O(n)       predict + exp search   retrain storms     native, gapped
```

The ε guarantee is the deep difference: PGM degrades in *space* (more
segments) on hostile data while lookup stays bounded; RMI degrades in
*time*; ALEX degrades in *write amplification*. Our
`epsilon_holds_on_hostile_distribution` test pins the PGM behavior.
