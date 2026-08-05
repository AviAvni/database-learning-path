# Time-respecting paths: when "shortest" splits four ways

Topic 8 gave every record a `begin_ts`/`end_ts` interval; topic 30's capstone
stored a graph you can time-travel. This paper asks the question both left
open: once the *edges themselves* carry timestamps, what is a path? The
answer breaks static-graph intuition twice — reachability stops being
transitive and Dijkstra's greedy invariant dies — and this chapter builds the
seven concepts one at a time before handing you a section-by-section reading
lens.

This is a guide to a **paper**, not a repo, so its anchors are the paper's own
section, definition, theorem and table numbers: **Wu, Cheng, Huang, Ke, Lu,
Xu, "Path Problems in Temporal Graphs," PVLDB 7(9), 2014**
([PDF](http://www.vldb.org/pvldb/vol7/p721-wu.pdf)). Every definition and
complexity below was checked against that PDF; the one Rust block is an
illustration pinned to the topic's own `experiments/` crate, marked as such.

## The problem in one sentence

A temporal edge is **four numbers, not two** — who, whom, *when*, and *how
long* — and if you collapse the last two away into a static graph, it will
happily report paths whose connecting edge departed *before* you could
arrive, while the single notion of "best path" you knew splits into four
that need four different algorithms.

## The concepts, step by step

### Step 1 — the temporal edge: a road that exists at one moment

> **In:** nothing yet — this step fixes the notation (`t`, `λ`, `π`, `M`)
> every later step's complexity bound is written in.
> **Out:** the quadruple `(u, v, t, λ)` and the input size `M`, the units
> Steps 2–7 consume.

A **temporal edge** is a quadruple `(u, v, t, λ)`: you may leave `u` toward
`v` only at **start time** `t` (the instant the edge is usable), and the
crossing takes **traversal time** `λ` (how long the hop itself lasts), so you
arrive at `v` at `t + λ`. Think of a flight: SFO→JFK departing 09:00 with
λ = 5 h — the "edge" is useless at 09:01. The paper writes this exactly:
a temporal edge is `(vi, vi+1, ti, λi) ∈ E` (§2, Definition of a temporal
graph).

The same vertex pair can carry many temporal edges (the 14:00 flight, the
19:00 flight); the paper writes **`π(u, v)`** for that **multiplicity** — the
number of temporal edges from `u` to `v` — and **`M = |E|`** for the total
number of temporal edges, versus **`m = |Es|`** for the edges of the
*condensed* static graph (Step 2) and **`n = |V|`** for the vertices (§2).

Why it matters: `M` counts *events*, not relationships. A social network
with 1M static edges and daily interactions for a year has `M ≈ 365M`
temporal edges. Every complexity bound below is in `M` (never `m`), and every
storage decision in M33 is about where those `(t, λ)` pairs live.

### Step 2 — condensing lies: reachability is not transitive

> **In:** the temporal edges of Step 1.
> **Out:** the **condensed graph** and the proof that the reachability it
> reports is wrong — the fact the whole topic's measured headline counts.

The obvious move — drop the timestamps, keep one static edge per connected
pair — gives the **condensed graph** `Gs` (the paper's term: all temporal
edges between the same pair collapse to one static edge, §2). It gives *wrong*
answers, not just imprecise ones, and the cleanest way to see why is that
**temporal reachability is not transitive**: write `u ⇝ w` for "a
time-respecting path runs from `u` to `w`"; then `a ⇝ b` and `b ⇝ c` do **not**
imply `a ⇝ c`. A concrete three-node counterexample (all λ = 1):

```
temporal:   a ──(t=2)──► b ──(t=1)──► c

    a ⇝ b :  take (a,b,2,1), arrive b at time 3.                   TRUE
    b ⇝ c :  from b, take (b,c,1,1), arrive c at time 2.           TRUE
    a ⇝ c :  to chain them you must be at b BEFORE t=1 to board
             b→c, but the only way into b arrives at time 3 > 1.   FALSE

condensed:  a ────────► b ────────► c   → says "c reachable from a" — a LIE
```

`a ⇝ b` holds, `b ⇝ c` holds, `a ⇝ c` fails: reachability composed across `b`
does not survive, because `b`'s onward contact departed at t = 1 but the path
into `b` does not arrive until t = 3. Static condensation silently assumes the
transitivity that temporal graphs lack, so it counts `(a, c)` as reachable.

The paper's Fig 1 makes the same point on a larger example and adds a second
lie: even when the destination *is* reachable, the condensed graph's
hop-count or weight-sum "shortest path" can name a route no time-respecting
traversal can follow.

Why it matters: this non-transitivity is exactly what this topic *measured*.
On the sparse contact graph, static reachability reports **25,031** reachable
pairs where time-respecting paths number **137** — **99.5% false positives**
([FINDINGS.md](../../FINDINGS.md) row 33; the full density sweep is in
[README.md](README.md)). An `AT TIME t` snapshot view (capstone M33) is a
condensed graph of the edges alive at `t`: the right tool for "what did the
graph look like," and provably the *wrong* tool for "what could flow through
it."

### Step 3 — the temporal path and its four minima

> **In:** the temporal edges (Step 1), now to be *chained* legally.
> **Out:** four scalar objectives — earliest-arrival, latest-departure,
> fastest, shortest — each of which Steps 5–7 compute with a different
> algorithm.

A **temporal path** (also **time-respecting path**) is a sequence of temporal
edges where each edge departs no earlier than the previous one arrives. The
paper states it as: for consecutive edges on the path,
`(ti + λi) ≤ ti+1` (§2). Two quantities are read off any path `P`:
its **starting time** `start(P) = t1`, its **ending time**
`end(P) = tk + λk` (departure plus traversal of the last edge), and from them
its **duration** `dura(P) = end(P) − start(P)` and its **distance**
`dist(P) = Σ λi` (the sum of traversal times) (§2, Definition 1 preamble).

A query fixes a **time window** `[tα, tω]`: consider only paths with
`start(P) ≥ tα` and `end(P) ≤ tω`. Within that set the paper defines
**four minimum temporal paths** (§3, Definition 1), quoted as it states them:

- **Earliest-arrival path** — minimizes `end(P)`. (Called the **foremost**
  path in the earlier Bui-Xuan–Ferreira–Jarry lineage the paper cites, ref
  [21]; "earliest-arrival" and "foremost" name the same objective.)
- **Latest-departure path** — maximizes `start(P)` subject to arriving by `tω`.
- **Fastest path** — minimizes `dura(P) = end(P) − start(P)`.
- **Shortest path** — minimizes `dist(P) = Σ λi`.

Worked on one graph — source `a`, target `c`, window `[0, 10]`:

```
edges (u, v, t, λ):   (a, b, 1, 4)   depart 1, arrive 5
                      (b, c, 6, 1)   depart 6, arrive 7
                      (a, c, 8, 1)   depart 8, arrive 9
```

There are two temporal paths from `a` to `c`. Compute each objective on both,
by hand, then read off the winner:

| path | start | end | dura = end − start | dist = Σλ |
|---|---|---|---|---|
| `a→b→c` = ⟨(a,b,1,4),(b,c,6,1)⟩ | 1 | 6 + 1 = 7 | 7 − 1 = 6 | 4 + 1 = 5 |
| `a→c` = ⟨(a,c,8,1)⟩ | 8 | 8 + 1 = 9 | 9 − 8 = 1 | 1 |

| minimum | objective | winner | value |
|---|---|---|---|
| **earliest-arrival** | min `end(P)` | `a→b→c` | end = 7 (< 9) |
| **latest-departure** | max `start(P)` | `a→c` | start = 8 (> 1) |
| **fastest** | min `dura(P)` | `a→c` | 9 − 8 = 1 (< 6) |
| **shortest** | min `dist(P)` | `a→c` | Σλ = 1 (< 5) |

In a static graph all four collapse into "shortest." Here the
earliest-arrival route (`a→b→c`, arriving 7) is *neither* fastest nor
shortest, and waiting for the late direct edge `a→c` wins the other three
criteria. Three of the four "best paths" disagree with the fourth on one tiny
graph.

Why it matters: these are four distinct path *functions* for M33. A query
planner must know which one the user asked for, because — as the table proves
— no single answer serves all four.

### Step 4 — greedy dies: a subpath of a shortest path isn't shortest

> **In:** the shortest-path objective from Step 3.
> **Out:** the counterexample that forbids Dijkstra's "settle once" step, and
> so forces either the dominance lists of Step 6 or the transformation of
> Step 7.

Dijkstra's algorithm rests on the **subpath optimality** invariant: any prefix
of a shortest path is itself a shortest path to its endpoint, so a vertex can
be **settled** (fixed at its best-known distance, never revisited) once.
Temporal edges break it — a cheap prefix can arrive *too late* to catch the
connecting edge:

```
(a, b, 0, 5)   the slow prefix:  arrive b at 5, cost Σλ = 5
(a, b, 8, 1)   the cheap prefix: arrive b at 9, cost Σλ = 1   ← shortest to b
(b, c, 6, 1)   departs b at 6

shortest a→c = (a,b,0,5)+(b,c,6,1), Σλ = 5 + 1 = 6 — its prefix to b costs 5,
even though a Σλ = 1 route to b exists. The cheap route arrives b at 9,
after b→c has departed at 6, so it cannot extend to c at all.
```

So you cannot settle `b` at its best-known distance (1): the *dominated-looking*
label (cost 5, but arriving at 5 instead of 9) is the one that extends to `c`.
A label must be kept alive when it is worse in cost but better in arrival time.
The fix is either Pareto frontiers per vertex (Step 6) or restructuring the
input so greedy works again (Step 7). The paper flags this — "subpaths of a
shortest path may not be shortest" — in its abstract and §3.

Why it matters: this is the single theorem-shaped fact to carry out of the
paper. It is why you cannot bolt a timestamp filter onto topic 24's frontier
BFS/Dijkstra and call it done.

### Step 5 — the one-pass scan: earliest arrival in O(n + M)

> **In:** the earliest-arrival objective (Step 3), plus the assumption that
> edges are pre-sorted by start time.
> **Out:** one number per vertex — `arr[v]`, its earliest arrival time — from
> a single sequential pass, no priority queue.

If edges are pre-sorted by start time `t` (the paper's **edge stream**
representation, §2), earliest-arrival needs no priority queue: one sequential
pass, each edge examined exactly once (Algorithm 1). The paper proves this
runs in **O(n + M) time and O(n) space** (§4.2, Theorem for Algorithm 1). The
shape, illustrated on the topic's own stub:

```rust
// ILLUSTRATION — not quoted from Wu et al.; this is the paper's Algorithm 1
// (§4.2, earliest-arrival) with the [tα, tω] window written out. The real
// code you implement to this contract is experiments/src/temporal_reach.rs:20
// (its relax rule is stated at temporal_reach.rs:14).
fn earliest_arrival(
    stream: &[(u32, u32, u64, u64)],   // (u, v, t, λ), sorted by t ascending
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
arriving anywhere before `t` has already been recorded — **time order is the
topological order**, so one relaxation per edge suffices (this is the exact
claim README exercise 2 asks you to prove, and the `zero_lambda_chains_...`
test at `experiments/src/temporal_reach.rs:59` pins the λ = 0 tie case).
Latest-departure is the mirror image: scan the stream backwards, maintaining
the latest possible departure from each vertex (§4.3, also O(n + M)). For a
single target, stop as soon as `t ≥ arr[target]`.

Why it matters: a single forward scan over a sorted array is the best-behaved
access pattern topic 0 knows — prefetch-friendly, no pointer chasing — and it
is the shape M33's earliest-arrival path function wants. The price is the
precondition: storage must hand you edges in time order (question 3).

### Step 6 — dominance lists: fastest and shortest in one pass, plus a log

> **In:** the fastest and shortest objectives (Step 3), which Step 4 proved
> cannot be summarized by one scalar per vertex.
> **Out:** a per-vertex **dominance list** and the extra `log` factor keeping
> it costs, still over a single time-ordered pass.

Fastest and shortest need more than one number per vertex (Step 4), so the
one-pass framework keeps a **dominance list** at each vertex — a **Pareto
frontier**, the set of candidate labels none of which is better than another in
*both* coordinates. For fastest the coordinates are (departure-from-source `s`,
arrival `a`); for shortest they are (distance `d`, arrival `a`). A new label is
inserted only if nothing already in the list **dominates** it (beats or ties it
on both coordinates), and every label it dominates is evicted. The lists stay
sorted, so each edge costs a binary search.

That binary search is the whole price. The paper's bounds (§4.4, §4.5):

- **Fastest**: `O(n + M log c)` time, `O(min{n|S|, n + M})` space, where `S`
  is the set of distinct out-edge start times.
- **Shortest**: `O(n + M log dmax)` time, `O(n + M)` space, where
  `dmax = max{din(v)}` is the largest in-degree.

Both are a single time-ordered pass with a `log`-sized list operation per
edge, versus Step 5's `O(1)` per edge. Worked, to see the `log` is not free
but is small: on the topic's lane-1 graph, `n = 2000`, `M = 3999`; a shortest
run costs on the order of `n + M·log(dmax)` — with an average in-degree near
`M/n ≈ 2`, `log dmax` is a single-digit factor, so the pass stays within a
small constant of Step 5's `n + M = 5999` edge-touches rather than blowing up.

Why it matters: the memory cost moved from `O(1)` to `O(frontier size)` per
vertex — bounded in practice by the number of distinct useful departure times.
This is the same labels-not-scalars move multi-criteria route planning makes,
and it is what your M33 executor must carry per node when a query asks for
fastest rather than earliest.

### Step 7 — the transformed graph: pay O(M) space, get statics back (§5)

> **In:** the original temporal edges (Step 1) and the four objectives
> (Step 3).
> **Out:** a static **DAG** on which plain BFS/Dijkstra recompute all four
> minima — the materialized-view alternative to Steps 5–6.

The alternative to new algorithms is a **time-expanded graph** (the paper's
graph transformation, §5): replace each vertex `v` by copies `(v, t)` — one
per distinct time an edge arrives at or departs from `v` — chain the copies
forward in time with 0-weight "wait here" edges, and turn each temporal edge
`(u, v, t, λ)` into a static edge from copy `(u, t)` to copy `(v, t + λ)`:

```
        (b,3) ──wait──► (b,6)              vertex b's timeline
          ▲               │
   a──t=2─┘               └──t=6──► (c,7)  temporal edges become
        arrive b at 3     depart b at 6    static DAG edges
```

Because chaining (not all-pairs wiring) connects the copies, the result is a
**directed acyclic graph** — every edge points forward in time, so no cycle
exists — and §5.5 proves that, assuming `n < M`, **both its vertex count and
its edge count are O(M)**. So plain BFS / Dijkstra / topological-order
algorithms compute all four minima correctly again: single-source
earliest-arrival and latest-departure each cost one BFS, i.e. `O(M)` (§5.5).
The paper's Table 4 makes the blow-up concrete — for the `arxiv` trace the
transformed `|Ṽ| = 433K` and `|Ẽ| = 9759K`, both the same order as `M`.

Why it matters: this is the materialized-view option — precompute a bigger
static structure so the classic toolbox (and topic 24's frontier engines)
applies unchanged. The paper's experiments (§6) measure exactly this trade:
transformation pays construction time and a per-query window's blown-up working
set; the one-pass algorithms stream the original data. Read the experiment
tables as a build-vs-scan price list.

## How to read the paper (with the concepts in hand)

The paper is ~12 pages; the definitions and the one-pass algorithms are the
payload. Budget ~2 h.

- **§1 (intro) + Fig 1 — read carefully.** Fig 1 is Step 2 in the authors'
  example; reproduce its reachability lie in your notes before moving on.
- **§2 (definitions) — read carefully.** Temporal graph, `π(u, v)`, `M`, `m`,
  `n`, the edge-stream representation, temporal paths, `start`/`end`/`dura`/
  `dist`, and the formal four minima (Step 3, Definition 1). Nail the notation
  table — everything later leans on it.
- **§4 (one-pass algorithms) — the core; read carefully.** Algorithm 1
  (earliest-arrival) should match Step 5's Rust nearly line for line;
  latest-departure (§4.3) is its mirror. For fastest (§4.4) and shortest
  (§4.5), focus on the dominance-list bookkeeping (Step 6) — read the
  invariants, skim the proofs on first pass. Note where the subpath-property
  failure (Step 4) is invoked to justify the lists.
- **§5 (graph transformation) — read the construction, skim the proofs.**
  Check Fig 2 against Step 7's sketch; the thing to verify is *why* the size
  stays O(M) (chaining, not complete wiring — §5.5).
- **§6 (experiments) — skim with two questions:** how much faster is one-pass
  than transformation per query, and how does transformation's cost scale with
  window size? Pull two concrete numbers from the tables into notes.md.
- **Related work / conclusion — skim.** Note the lineage they cite (ref [21],
  Bui-Xuan et al.) for shortest/fastest/foremost journeys; it predates the
  one-pass framework.

## Questions to answer in notes.md

1. From Fig 1: which pairs does the condensed graph claim are reachable but
   temporally are not — and even for a truly reachable pair, which of the four
   minima does the static graph compute wrongly?
2. State precisely which invariant of Dijkstra's correctness proof the Step 4
   counterexample violates, and why keeping (distance, arrival) Pareto pairs
   restores correctness.
3. Step 5's precondition is a time-sorted edge stream. FalkorDB stores
   adjacency as GraphBLAS matrices (topic 13): what is the cheapest layout that
   yields per-window time-ordered edges — timestamped edge-list sidecar,
   per-time-bucket delta matrices (topic 30's M30), or sorting at query time?
   Sketch the cost of each for a `[tα, tω]` query.
4. Capstone M33: earliest-arrival as a path function. Rewrite Step 5's
   relaxation condition for (a) a WITHIN δ constraint (path duration ≤ δ) and
   (b) MATCH with non-decreasing timestamps but no λ — which of the four minima
   does each correspond to?
5. MVCC tie-back (topic 8): `begin_ts`/`end_ts` version intervals are
   *transaction time*; `(u, v, t, λ)` is *valid time*. Which queries from this
   paper can an AT TIME snapshot answer exactly, and which are unanswerable by
   any single snapshot no matter how it is chosen?
6. Treat §5's transformation as a materialized view of size O(M): given average
   multiplicity `π` and a query mix, when does building it beat running
   one-pass scans per query? Where is the break-even?

## Done when

Answer each before unfolding it.

- [ ] You can state the four minima and produce a graph where all four differ.

  <details><summary>Answer</summary>

  The four are Wu et al.'s Definition 1 (§3): **earliest-arrival** minimizes
  `end(P)`, **latest-departure** maximizes `start(P)`, **fastest** minimizes
  `dura(P) = end − start`, **shortest** minimizes `dist(P) = Σλ`.
  (Earliest-arrival is the objective the older literature calls *foremost*,
  ref [21].)

  Step 3's graph separates them: edges `(a,b,1,4)`, `(b,c,6,1)`, `(a,c,8,1)`,
  window `[0,10]`. The path `a→b→c` has `end = 7`, `dura = 6`, `dist = 5`; the
  direct `a→c` has `end = 9`, `dura = 1`, `dist = 1`, `start = 8`.
  Earliest-arrival picks `a→b→c` (end 7 < 9); latest-departure, fastest and
  shortest all pick `a→c` (start 8, dura 1, dist 1). One graph, the
  earliest-arrival winner losing every other criterion — the proof that four
  algorithms are genuinely needed.

  </details>

- [ ] You can reproduce the greedy counterexample from memory and say which Dijkstra invariant it kills.

  <details><summary>Answer</summary>

  The invariant is **subpath optimality**: a prefix of a shortest path is a
  shortest path, which licenses settling a vertex once and never revisiting it.

  Step 4's edges kill it: `(a,b,0,5)`, `(a,b,8,1)`, `(b,c,6,1)`. The cheapest
  route to `b` is `(a,b,8,1)` at `dist = 1`, but it arrives at time 9, after
  `b→c` departs at 6, so it extends nowhere. The shortest `a→c` is
  `(a,b,0,5)+(b,c,6,1)` with `dist = 6`, whose prefix to `b` costs 5 — a
  *non-shortest* prefix. So `b` cannot be settled at cost 1; the (cost 5,
  arrival 5) label, dominated on cost, is the one that reaches `c`. Keeping
  both coordinates as a Pareto pair (Step 6) is what preserves the label greedy
  would have discarded, restoring correctness at a `log` factor.

  </details>

- [ ] You can write the one-pass earliest-arrival scan without looking, and say why one relaxation per edge suffices.

  <details><summary>Answer</summary>

  The loop is Step 5's `earliest_arrival`: initialize `arr[src] = tα`, then for
  each `(u, v, t, λ)` in start-time order, if `t ≥ arr[u]` and
  `t + λ < arr[v]`, set `arr[v] = t + λ`. Skip edges arriving after `tω`; break
  once `t > tω`. Wu et al. prove this is `O(n + M)` time, `O(n)` space
  (§4.2, Algorithm 1).

  One relaxation per edge is enough because the stream is sorted by `t`, so
  **time order is topological order**: when the scan reaches an edge departing
  at `t`, every arrival earlier than `t` has already been written into `arr`,
  so `arr[u]` is final for the purpose of departing at `t`. There is no way a
  later edge improves an arrival that an earlier departure needed — the exact
  claim README exercise 2 asks you to prove, and the reason the crate's contract
  (`experiments/src/temporal_reach.rs:14`) forbids a fixpoint loop. The λ = 0
  case (`temporal_reach.rs:59`) works because the relax test uses `t ≥ arr[u]`,
  non-strict, so an arrival at `t` can board a departure at the same `t`.

  </details>

- [ ] You can say what storage order the one-pass scan demands from FalkorDB, and why an AT TIME view can never answer it.

  <details><summary>Answer</summary>

  It demands **edges delivered in non-decreasing start-time order** within the
  query window — the edge-stream representation (§2). FalkorDB's GraphBLAS
  adjacency is not time-ordered, so M33 must supply the order some other way: a
  timestamped edge-list sidecar, per-time-bucket delta matrices (M30), or a
  sort at query time (question 3 weighs the three).

  An `AT TIME t` view cannot answer earliest-arrival because it is a *condensed
  graph of one instant* — it discards the very `(t, λ)` ordering the scan needs,
  and Step 2 proved reachability across such a condensation is not even
  transitive: `a ⇝ b` and `b ⇝ c` there do not imply `a ⇝ c`. This is the
  topic's measured 99.5% false-positive result ([FINDINGS.md](../../FINDINGS.md)
  row 33): no single snapshot, however chosen, holds the cross-time information
  a time-respecting path is made of.

  </details>

- [ ] You can say when materializing §5's transformed graph beats streaming the one-pass scans.

  <details><summary>Answer</summary>

  Building the time-expanded graph costs `O(M)` space and construction time
  (§5.5) but then answers each single-source query with one plain BFS/Dijkstra
  over a static DAG, reusing the structure across queries. The one-pass scans
  (Steps 5–6) touch the original stream once *per query* and keep only `O(n)`
  to `O(n + M)` transient state.

  So the transformation wins when the same window is queried many times — the
  `O(M)` build amortizes over a query batch — and loses on a single ad-hoc
  query, where you pay the full build to run one BFS. The break-even is roughly
  "build cost / per-query scan cost" queries against the same window; question 6
  works it against the multiplicity `π` and a query mix. It is precisely
  topic 5's checkpoint-vs-redo trade again: materialize once and replay cheaply,
  or stream every time.

  </details>

## References

**Papers**
- Wu, Cheng, Huang, Ke, Lu, Xu — "Path Problems in Temporal Graphs"
  (PVLDB Vol 7, No 9, 2014) —
  [PDF](http://www.vldb.org/pvldb/vol7/p721-wu.pdf) — ~12 pages, ~2 h: read
  §1–§2 and the §4 one-pass algorithms carefully, the §5 construction once, and
  skim the §6 experiments for the one-pass vs transformation gap. Anchors used
  above: §2 (notation, temporal path, edge stream), §3 Definition 1 (four
  minima), §4.2/§4.3 (earliest-arrival / latest-departure, O(n+M)),
  §4.4/§4.5 (fastest / shortest, the `log` factor), §5.5 + Table 4
  (transformation size O(M)).
- Bui-Xuan, Ferreira, Jarry — "Computing shortest, fastest, and foremost
  journeys in dynamic networks" (Int. J. Found. Comput. Sci. 14(2), 2003) —
  ref [21], the source of the "foremost" = earliest-arrival naming.

**Code**
- This topic's `experiments/src/temporal_reach.rs` (`earliest_arrival` stub,
  contract at `:14`, tests at `:33`) — the one-pass scan you implement; bench
  lane 2 times it against the fixpoint oracle in `events.rs`.

**Related guides**
- [reading-temporal-motifs.md](reading-temporal-motifs.md) — δ-temporal
  motifs, where ordering (not reachability) is the information.
- [reading-aeong.md](reading-aeong.md) and
  [reading-raphtory.md](reading-raphtory.md) — the storage engines that must
  hand these algorithms edges in time order.
