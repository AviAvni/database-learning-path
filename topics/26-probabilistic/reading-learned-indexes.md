# Learned indexes: the index is a model of the CDF

An index maps key → position. If the key distribution is smooth, a
handful of linear models approximates that map with a bounded error you
binary-search away — replacing a tree walk's cache misses with two
multiply-adds. Three designs mark the territory: RMI (the provocation),
PGM (the guarantee — our stub), and ALEX (the one that takes writes).
This chapter builds the idea from the reframe up — index as function,
error bounds, segment construction, updatability — then anchors each
piece in the PGM and ALEX sources.

## The problem in one sentence

On the motivation bench, a point-miss binary search over 10M sorted u64
keys costs **167 ns ≈ 23 dependent cache misses** — and if the key
distribution is smooth, most of those 23 hops land exactly where a
two-multiply-add linear model would have predicted for free.

## The concepts, step by step

### Step 1 — the reframe: an index is a function, and a B-tree is already a model

An index is a function from key to position in a sorted array — and that
function is precisely the **CDF** (cumulative distribution function: the
fraction of keys ≤ x) of the key distribution, scaled by n. This is
Kraska's opening move: a B-tree computes `pos ≈ n · CDF(key)` as a
piecewise-constant approximation with worst-case-everything guarantees;
if the CDF is *smooth*, a few linear models predict the position in O(1)
with a small error to binary-search away:

```
  pos ≈ n · CDF(key)

  B-tree:  log_B(n) node hops, each a cache miss    (167 ns measured, ~23 misses)
  learned: 1-2 model evals + binary search of 2ε    (the bet: most of the
           window                                    tree walk is predictable)
```

The bet, stated honestly: trade guaranteed log-time on any distribution
for near-constant time on distributions that are actually predictable —
auto-increment IDs, steady-ingest timestamps.

### Step 2 — RMI: the provocation, without a safety net

The RMI (recursive model index, Kraska §3) is a fixed 2-stage hierarchy
of models where stage 1's model doesn't predict the position — it *picks*
which stage-2 model does. The stage-2 model then predicts a position, and
the search corrects the residual error. The flaw that motivates
everything after it: **no error bound**. A model that fits badly on some
key region gives predictions off by thousands of slots, the correcting
search becomes long and unpredictable, and there's no principled way to
size the stages. RMI proved the reframe was fast; it didn't make it safe.

### Step 3 — PGM: fix the error first, then minimize the model

PGM inverts the design: choose a hard error bound ε *up front*, then
compute the **minimum number of linear segments** such that every key's
predicted position is within ε of the truth — lookup = evaluate the
segment's line, then binary-search a window of just 2ε+2 slots. To find
the right segment among (say) 2,000 of them, index the segments' first
keys with... another PGM, recursively, until one segment remains — each
level is itself ε-bounded, so each hop is a *constant-size* search, not a
binary search over all segments. Why it matters: the segments (a few KB)
live in cache where a B-tree's top levels don't even, and the ε guarantee
holds on *any* distribution — hostile keys cost more *segments* (space),
never a longer lookup. Our `epsilon_holds_on_hostile_distribution` test
pins exactly that.

### Step 4 — building segments in one pass: the shrinking cone

Computing the minimal ε-bounded piecewise-linear fit sounds expensive but
is a streaming, O(n) pass: maintain the set of lines that could still fit
every point seen so far within ε, and emit a segment the moment that set
goes empty. PGM's `OptimalPiecewiseLinearModel` uses O'Rourke '81's
streaming convex-hull method (provably *fewest* segments for a given ε);
our stub uses the simpler **shrinking cone**: keep an interval [lo, hi]
of feasible slopes through the segment's first point; each new point
narrows it; emit when empty. Same ε guarantee, ≥ as many segments, and
O(1) state instead of two hulls:

```rust
struct Cone { x0: u64, y0: f64, lo: f64, hi: f64 }   // slopes through (x0,y0)

fn add_point(c: &mut Cone, x: u64, y: usize, eps: f64) -> bool {
    let (dx, dy) = ((x - c.x0) as f64, y as f64 - c.y0);
    c.lo = c.lo.max((dy - eps) / dx);   // each point NARROWS the feasible
    c.hi = c.hi.min((dy + eps) / dx);   // slope interval...
    c.lo <= c.hi                        // ...empty ⇒ emit segment, start fresh
}
```

The cost profile that falls out: build is O(n) single-pass (vs a B-tree's
O(n log n) of page splits), on 1M uniform keys under 2K segments suffice
(the `uniform_data_compresses_hard` test), and the structure is *static* —
one insert invalidates every position after it. Which is Step 5's
problem.

### Step 5 — ALEX: gapped arrays make the model updatable

A static PGM re-builds on change; ALEX makes the *data layout* absorb
updates instead. Its nodes are **gapped arrays** — sorted arrays with
~50% empty slots left deliberately interspersed — and the model is used
not only to search but to *place*: model-based insertion puts a new key
at its predicted slot (shifting only to the closest gap), so the data
keeps matching the model as it arrives. Lookups use **exponential
search** from the predicted slot (probe at distance 1, 2, 4, 8... then
binary-search the bracketed range): cost is O(log of the model's actual
error), so it adapts — usually 0–2 slots — without needing PGM's hard-ε
accounting. When a node overflows its density bound it splits and
retrains: the B-tree skeleton reappears, but with models as node search
and gaps as write absorbers. The cost: hostile insert patterns pile keys
onto one predicted slot and trigger shift/retrain storms — write
amplification is where ALEX degrades.

### Step 6 — the honest scoreboard: how each design degrades

The deep difference between the three is not speed on friendly data —
it's *which resource* gives out on hostile data:

```
              build      lookup (smooth keys)   lookup (hostile)   inserts
  B-tree      O(n log n) ~log_B(n) misses       same               native
  RMI         train      fast, NO bound         can be terrible    no
  PGM         O(n)       1-3 hops + 2ε window   MORE segments,     PGM-dynamic:
                                                bound still holds  LSM-of-PGMs
  ALEX        O(n)       predict + exp search   retrain storms     native, gapped
```

The ε guarantee is the dividing line: PGM degrades in *space* (more
segments) while lookup stays bounded; RMI degrades in *time*; ALEX
degrades in *write amplification*. The B-tree degrades in nothing and
wins on nothing — which is exactly why it's the incumbent.

## Where each step lives in the code

PGM — Steps 3–4
([`~/repos/PGM-index/include/pgm/`](https://github.com/gvinciguerra/PGM-index)):

| anchor | what it is |
|---|---|
| `pgm_index.hpp:32-33` | `PGM_SUB_EPS`/`PGM_ADD_EPS` — the window is [pos−ε, pos+ε+2), clamped; the +2 matters (segment boundaries) |
| `pgm_index.hpp:67` | `class PGMIndex`; `build` :88 loops `make_segmentation` per level |
| `segment_for_key` :134 | the recursive descent: each level is itself ε-bounded, so each hop is a *constant-size* search (:143-152), not a binary search over all segments |
| `search` :192 | predict, widen by ε, return the window — our `search_window` |
| `piecewise_linear_model.hpp:45` | `OptimalPiecewiseLinearModel` — O'Rourke '81 streaming convex-hull method |
| `add_point` :96, hull updates :154-190 | maintains upper/lower convex hulls of the feasible-slope region; segment closes when hulls cross |
| `make_segmentation` :276 | the greedy driver: `if (!opt.add_point(x,y)) { out(segment); start fresh }` |

ALEX — Step 5
([`~/repos/ALEX/src/core/alex_nodes.h`](https://github.com/microsoft/ALEX)):

| anchor | what it is |
|---|---|
| `class AlexDataNode` :293 | gapped array + per-node linear model; `num_keys_` :325 vs slots = the gap budget |
| `predict_position` :1448 | the model eval |
| `find_key` :1456 | predict, then `exponential_search_upper_bound` :1462 from the predicted slot — cost is O(log distance-of-model-error), no ε needed |
| `find_insert_position` :1497 | same predict-then-search on the insert path |
| :28, :474, :1513 | the gap machinery: bitmap marks gap vs key; inserts shift toward the *closest gap*, not the array end |

## Questions to answer in notes.md

1. Construct 4 points where the cone closes a segment but the hull method
   keeps going. (Hint: the cone forces every prediction line through the
   *first* point; optimal PLA doesn't.)
2. ε trades segment count against final-search width. Segments live in
   cache; the 2ε window is one or two line fetches into the data. Given
   the motivation numbers (167 ns ≈ 23 misses), predict the ns/lookup
   curve for ε ∈ {16, 64, 256} on 10M uniform keys *before* running
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
   paper's §5.5.)
5. **(cross-topic)** ALEX's gapped array + model placement vs a B-tree
   leaf with slotted-page free space (topic 2): both reserve slack to
   make inserts local. What does ALEX's *model* buy over the B-tree's
   binary search within the leaf, and when is it worth zero? (Uniform
   small leaves fit in one cache line either way.)

## References

**Papers**
- Kraska, Beutel, Chi, Dean, Polyzotis — "The Case for Learned Index
  Structures" (SIGMOD 2018,
  [arXiv:1712.01208](https://arxiv.org/abs/1712.01208)) — §1-3 (RMI),
  skim the rest
- Ferragina & Vinciguerra — "The PGM-index" (VLDB 2020,
  [pgm.di.unipi.it](https://pgm.di.unipi.it))
- Ding et al. — "ALEX: An Updatable Adaptive Learned Index" (SIGMOD
  2020, [arXiv:1905.08898](https://arxiv.org/abs/1905.08898))

**Code**
- [PGM-index](https://github.com/gvinciguerra/PGM-index)
  `include/pgm/` — `pgm_index.hpp` + `piecewise_linear_model.hpp`
- [ALEX](https://github.com/microsoft/ALEX) `src/core/` —
  `alex_nodes.h` is where the gapped-array machinery lives
