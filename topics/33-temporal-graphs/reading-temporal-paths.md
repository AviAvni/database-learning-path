# Time-respecting paths: when "shortest" splits four ways

Topic 8 gave every record a `begin_ts`/`end_ts` interval; topic 30's capstone
stored a graph you can time-travel. This paper asks the question both left
open: once the *edges themselves* carry timestamps, what is a path? The
answer breaks static-graph intuition twice — reachability changes and
Dijkstra's greedy invariant dies — and this chapter builds the seven
concepts one at a time before handing you a section-by-section reading lens.

## The problem in one sentence

A temporal edge is **four numbers, not two** — who, whom, *when*, and *how
long* — and if you collapse the last two away into a static graph, it will
happily report paths whose connecting edge departed *before* you could
arrive, while the single notion of "best path" you knew splits into four
that need four different algorithms.

## The concepts, step by step

### Step 1 — the temporal edge: a road that exists at one moment

A **temporal edge** is a quadruple `(u, v, t, λ)`: you may leave `u` toward
`v` only at **start time** `t`, and the crossing takes **traversal time**
`λ`, so you arrive at `v` at `t + λ`. Think of a flight: SFO→JFK departing
09:00 with λ = 5 h — the "edge" is useless at 09:01. The same vertex pair
can carry many temporal edges (the 14:00 flight, the 19:00 flight); the
paper writes `π(u, v)` for that multiplicity, and `M` for the total number
of temporal edges — the real size of the input.

Why it matters: `M` counts *events*, not relationships. A social network
with 1M static edges and daily interactions for a year has `M ≈ 365M`
temporal edges. Every complexity bound below is in `M`, and every storage
decision in M33 is about where those `(t, λ)` pairs live.

### Step 2 — condensing lies: the static view over-reports reachability

The obvious move — drop the timestamps, keep one static edge per connected
pair (the **condensed graph**) — gives wrong answers, not just imprecise
ones. Watch a 3-node graph (all λ = 1):

```
temporal:   a ──(t=2)──► b ──(t=1)──► c

condensed:  a ────────► b ────────► c        "c reachable from a" — FALSE

reality: you arrive at b at time 3, but the only b→c edge
         departed at time 1. It's gone. c is unreachable from a.
```

The paper's Fig 1 makes exactly this point on a slightly larger example,
and adds a second lie: even when the destination *is* reachable, the
condensed graph's hop-count or weight-sum "shortest path" can name a route
that no time-respecting traversal can follow.

Why it matters: an `AT TIME t` snapshot view (capstone M33) is a condensed
graph of the edges alive at `t`. It is the right tool for "what did the
graph look like" and provably the *wrong* tool for "what could flow through
it" — no single snapshot can answer a cross-time reachability question.

### Step 3 — the temporal path and its four minima

A **temporal path** (also: time-respecting path) is a sequence of temporal
edges where each edge departs no earlier than the previous one arrives:
`tᵢ + λᵢ ≤ tᵢ₊₁` — timestamps non-decreasing along the path, exactly M33's
MATCH semantics. Queries fix a time window `[tα, tω]` (depart no earlier
than `tα`, arrive no later than `tω`). Now "best" splits four ways. One
graph, source `a`, target `c`, window `[0, 10]`:

```
edges (u, v, t, λ):   (a, b, 1, 4)   depart 1, arrive 5
                      (b, c, 6, 1)   depart 6, arrive 7
                      (a, c, 8, 1)   depart 8, arrive 9
```

| minimum | optimizes | winner here | value |
|---|---|---|---|
| **earliest-arrival** | min arrival time | a→b→c | arrives 7 |
| **latest-departure** | max start time (arriving by tω) | a→c | departs 8 |
| **fastest** | min duration (arrival − departure) | a→c | 9 − 8 = 1 |
| **shortest** | min Σλ (total traversal time) | a→c | Σλ = 1 |

In a static graph all four collapse into "shortest". Here the
earliest-arrival route is *neither* fastest nor shortest — waiting for the
late direct edge wins three of the four criteria.

Why it matters: these are four distinct path *functions* for M33; a query
planner must know which one the user asked for, because no single answer
serves all four.

### Step 4 — greedy dies: a subpath of a shortest path isn't shortest

Dijkstra's algorithm rests on one invariant: any prefix of a shortest path
is itself a shortest path, so a vertex can be "settled" once. Temporal
edges break it — a cheap prefix can arrive *too late* to catch the
connecting edge:

```
(a, b, 0, 5)   the slow prefix:  arrive b at 5, cost Σλ = 5
(a, b, 8, 1)   the cheap prefix: arrive b at 9, cost Σλ = 1   ← shortest to b
(b, c, 6, 1)   departs b at 6

shortest a→c = (a,b,0,5)+(b,c,6,1), Σλ = 6 — its prefix to b costs 5,
even though a Σλ = 1 route to b exists. The cheap route misses the bus.
```

So you cannot settle `b` with its best-known distance; a *dominated-looking*
label (higher cost, earlier arrival) must be kept alive. The fix is either
Pareto frontiers per vertex (Step 6) or restructuring the input so greedy
works again (Steps 5 and 7).

Why it matters: this is the single theorem-shaped fact to carry out of the
paper — it is why you can't bolt a timestamp filter onto topic 24's
frontier BFS/Dijkstra and call it done.

### Step 5 — the one-pass scan: earliest arrival in O(n + M)

If edges are pre-sorted by start time `t` (the paper's **edge stream**
representation), earliest-arrival needs no priority queue at all — one
sequential pass, each edge examined exactly once:

```rust
/// One-pass earliest-arrival over a time-sorted edge stream.
/// Returns the earliest arrival time at every vertex within [tα, tω].
fn earliest_arrival(
    stream: &[(u32, u32, u64, u64)],   // (u, v, t, λ), sorted by t
    src: usize, n: usize,
    t_alpha: u64, t_omega: u64,
) -> Vec<u64> {
    let mut arr = vec![u64::MAX; n];
    arr[src] = t_alpha;                    // "at" the source from tα on
    for &(u, v, t, lam) in stream {
        if t + lam > t_omega { if t > t_omega { break; } continue; }
        // depart u only if we've already arrived there by time t;
        // relax v if this edge gets us there sooner
        if t >= arr[u as usize] && t + lam < arr[v as usize] {
            arr[v as usize] = t + lam;     // each edge relaxed ONCE
        }
    }
    arr   // O(n + M): no queue, no revisits, pure sequential scan
}
```

Why it works: by the time the stream reaches start time `t`, every way of
arriving anywhere before `t` has already been recorded — time order *is*
the topological order. Latest-departure is the mirror image: scan the
stream backwards, maintaining the latest possible departure from each
vertex. For a single target, stop as soon as `t ≥ arr[target]`.

Why it matters: a single forward scan over a sorted array is the
best-behaved access pattern topic 0 knows — prefetch-friendly, no pointer
chasing — and it's the shape M33's earliest-arrival path function wants.
The price is the precondition: storage must hand you edges in time order
(question 3).

### Step 6 — dominance lists: fastest and shortest in one pass, plus a log

Fastest and shortest can't be summarized by one number per vertex (Step 4),
so the one-pass framework keeps a small **dominance list** (Pareto
frontier — set of candidates none of which is better in both coordinates)
at each vertex: for fastest, pairs of (departure-from-source `s`, arrival
`a`); for shortest, pairs of (distance `d`, arrival `a`). A new pair is
inserted only if nothing in the list dominates it, and pairs it dominates
are evicted; the lists stay sorted, so each edge costs a binary search — a
log factor over Step 5, still a single time-ordered pass.

Why it matters: the memory cost moved from O(1) to O(frontier size) per
vertex — bounded in practice by the number of distinct useful departure
times. This is the same labels-not-scalars move that multi-criteria route
planning makes, and it's what your M33 executor must carry per node when a
query asks for fastest rather than earliest.

### Step 7 — the transformed graph: pay O(M) space, get statics back (§5)

The alternative to new algorithms is a **time-expanded graph**: replace
each vertex `v` by copies `(v, time-point)` — one per distinct time an edge
arrives at or departs from `v` — chain the copies forward with 0-weight
"wait here" edges, and turn each temporal edge `(u, v, t, λ)` into a static
edge from copy `(u, t)` to copy `(v, t + λ)`:

```
        (b,3) ──wait──► (b,6)              vertex b's timeline
          ▲               │
   a──t=2─┘               └──t=6──► (c,7)  temporal edges become
        arrive b at 3     depart b at 6    static DAG edges
```

Because chaining (not all-pairs wiring) connects the copies, the result has
O(M) vertices *and* O(M) edges, and it is a DAG (edges only go forward in
time) — so plain BFS/Dijkstra/topological-order algorithms compute all four
minima correctly again.

Why it matters: this is the materialized-view option — precompute a bigger
static structure so the classic toolbox (and topic 24's frontier engines)
applies unchanged. The paper's experiments measure exactly this trade:
transformation pays construction time and a blown-up working set per query
window; the one-pass algorithms stream the original data. Read the
experiment tables as a build-vs-scan price list.

## How to read the paper (with the concepts in hand)

The paper is ~12 pages; the definitions and the one-pass algorithms are the
payload.

- **§1 (intro) + Fig 1 — read carefully.** Fig 1 is Step 2 in the authors'
  example; reproduce its reachability lie in your notes before moving on.
- **§2 (definitions) — read carefully.** Temporal graph, `π(u, v)`, `M`,
  the edge-stream representation, temporal paths, and the formal statements
  of the four minima (Step 3). Nail the notation table — everything later
  leans on it.
- **§3–§4 (one-pass algorithms) — the core; read carefully.** The
  earliest-arrival pseudocode should match Step 5's Rust nearly line for
  line; latest-departure is its mirror. For fastest and shortest, focus on
  the dominance-list bookkeeping (Step 6) — read the invariants, skim the
  proofs on first pass. Note where the subpath-property failure (Step 4)
  is invoked to justify the lists.
- **§5 (graph transformation) — read the construction, skim the proofs.**
  Check the figure against Step 7's sketch; the thing to verify is *why*
  the size stays O(M) (chaining, not complete wiring).
- **§6 (experiments) — skim with two questions:** how much faster is
  one-pass than transformation per query, and how does transformation's
  cost scale with window size? Pull two concrete numbers from the tables
  into notes.md.
- **Related work / conclusion — skim.** Note the lineage they cite for the
  transformation idea; it predates the one-pass framework.

## Questions to answer in notes.md

1. From Fig 1: which pairs does the condensed graph claim are reachable but
   temporally are not — and even for a truly reachable pair, which of the
   four minima does the static graph compute wrongly?
2. State precisely which invariant of Dijkstra's correctness proof the
   Step 4 counterexample violates, and why keeping (distance, arrival)
   Pareto pairs restores correctness.
3. Step 5's precondition is a time-sorted edge stream. FalkorDB stores
   adjacency as GraphBLAS matrices (topic 13): what is the cheapest layout
   that yields per-window time-ordered edges — timestamped edge-list
   sidecar, per-time-bucket delta matrices (topic 30's M30), or sorting at
   query time? Sketch the cost of each for a `[tα, tω]` query.
4. Capstone M33: earliest-arrival as a path function. Rewrite Step 5's
   relaxation condition for (a) a WITHIN δ constraint (path duration ≤ δ)
   and (b) MATCH with non-decreasing timestamps but no λ — which of the
   four minima does each correspond to?
5. MVCC tie-back (topic 8): `begin_ts`/`end_ts` version intervals are
   *transaction time*; `(u, v, t, λ)` is *valid time*. Which queries from
   this paper can an AT TIME snapshot answer exactly, and which are
   unanswerable by any single snapshot no matter how it's chosen?
6. Treat §5's transformation as a materialized view of size O(M): given
   average multiplicity π and a query mix, when does building it beat
   running one-pass scans per query? Where's the break-even?

## Done when

You can state the four minima and produce a graph where all four differ;
you can reproduce the greedy counterexample from memory and say which
Dijkstra invariant it kills; you can write the one-pass earliest-arrival
scan without looking; and you can say, in one sentence each, what storage
order it demands from FalkorDB and why an AT TIME view can never answer it.

## References

**Papers**
- Wu, Cheng, Huang, Ke, Lu, Xu — "Path Problems in Temporal Graphs"
  (PVLDB Vol 7, No 9, 2014) —
  [PDF](http://www.vldb.org/pvldb/vol7/p721-wu.pdf) — ~12 pages, ~2 h:
  read §1–§2 and the one-pass algorithms carefully, the §5 construction
  once, and skim the experiments for the one-pass vs transformation gap
