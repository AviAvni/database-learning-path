# δ-temporal motifs: counting ordered patterns inside a time window

Topic 24 counted static triangles with a masked matrix multiply; the previous
guide showed that once edges carry timestamps, *order* is information. This
paper fuses the two: a pattern is no longer a subgraph but an ordered sequence
of edges that must all land inside a window δ — and counting those is a new
algorithmic problem. This chapter builds the six concepts, ending at the exact
window-scan operator M33's WITHIN δ needs.

This is a guide to a **paper**, so its anchors are the paper's own section,
figure, algorithm and theorem numbers: **Paranjape, Benson, Leskovec, "Motifs
in Temporal Networks," WSDM 2017**
([arXiv:1612.09259](https://arxiv.org/abs/1612.09259)). Every count and
complexity below was checked against that PDF; the one Rust block is an
illustration, marked as such. One notation warning up front: the paper writes
**`k` for the number of *nodes*** and **`l` for the number of *edges*** in a
motif — this guide uses those letters the paper's way throughout.

## The problem in one sentence

In a trace like the SNAP `sx-stackoverflow` dataset the paper released
(2,601,977 nodes, **63,497,050 timestamped edges**), "A messaged B, then B
messaged C, then C messaged A — all within one hour" is a single pattern out
of the **36** possible 3-edge orderings on at most 3 nodes, and counting its
instances by enumerating triples of edges is hopeless.

## The concepts, step by step

### Step 1 — timestamped edges, and why order is information

> **In:** nothing yet — this step fixes what a temporal edge is here (no
> duration λ, unlike the paths guide) and why a static count throws away the
> answer.
> **Out:** the multiset of `(u, v, t)` events every later step consumes, and
> the observation that reordering them changes their meaning.

A **temporal network** here is a multiset of directed, timestamped edges
`(u, v, t)` — who contacted whom, when. The paper's own definition: a temporal
graph `T` on node set `V` is a collection of tuples `(ui, vi, ti)`,
`i = 1..m`, each `ti` a timestamp in ℝ (§2). Note there is **no traversal time
λ** — an edge is an *instantaneous event*, not a road you spend time on (this
is the sharpest difference from the temporal-paths guide's `(u, v, t, λ)`). The
paper assumes the `ti` are **unique**, so the edges are strictly ordered
(§2 — an assumption for clean presentation, adaptable to ties).

A **static motif** (a small subgraph pattern, e.g. a triangle) treats these two
histories as identical:

```
history 1:  A→B at 9:00,  B→C at 9:05     plausible information flow
history 2:  B→C at 9:00,  A→B at 9:05     B "forwarded" before receiving
```

Both condense to the static path A→B→C, but only one is a possible relay.

Why it matters: every behavioral question — forwarding, reciprocation,
who-answers-whom — lives in the *ordering*, which the static count destroys.
This is the previous guide's "condensing lies" applied to patterns instead of
paths.

### Step 2 — the δ-temporal motif: sequence + order + window

> **In:** the `(u, v, t)` events of Step 1.
> **Out:** the formal object being counted — a `k`-node, `l`-edge motif and its
> **instances** — plus the number 36 that indexes the whole empirical paper.

The paper's definition, quoted (§2): a **`k`-node, `l`-edge δ-temporal motif**
is a sequence of `l` edges `M = (u1, v1, t1), …, (ul, vl, tl)` that are
**time-ordered within a δ duration**, i.e. `t1 < t2 < … < tl` and
`tl − t1 ≤ δ`. Here **`δ`** is the window: the span from first to last edge may
not exceed it. An **instance** of `M` is a set of `l` actual edges that
(a) map onto the placeholders consistently, (b) occur in exactly the specified
order, and (c) satisfy `tl − t1 ≤ δ`. Example with δ = 1 h:

```
edges between ann and bob:   ann→bob 9:00   bob→ann 9:20   ann→bob 9:50
                             ann→bob 11:00

M = (A→B, B→A, A→B):
(9:00, 9:20, 9:50)  ✓ instance of M   — right order, spans 50 min ≤ δ
(9:00, 9:20, 11:00) ✗ spans 2 h > δ
(9:20, 9:50, 11:00) ✗ order is B→A, A→B, A→B — a different motif
```

The paper fixes attention on `l = 3` edges and `k ≤ 3` nodes and shows there
are exactly **36** such motifs (Fig 3). The count decomposes cleanly (Fig 3's
own colours): **4** two-node motifs + **8** triangle motifs + **24** star
motifs = 36. Fig 3 lays them in a 6 × 6 grid: the first edge is fixed (green
node → orange node), the **second edge indexes the row**, the **third edge the
column**. (Why 6 per axis: with the first edge fixed as node 1 → node 2, a
following edge is either *between the two existing nodes* — 2 directions — or
*between an existing node and a new third node* — 2 existing nodes × 2
directions = 4, for 2 + 4 = 6 choices. Q1 asks you to finish this argument.)

Why it matters: δ is doing real semantic work — it encodes "these events belong
to one interaction," and every count is meaningless without stating it. This is
precisely the WITHIN δ clause of capstone M33.

### Step 3 — why counting is hard: one static shape, many temporal instances

> **In:** the motif `M` and window δ from Step 2.
> **Out:** the cost model that rules out naive enumeration and demands the
> per-event state of Step 4.

Because the same pair can carry many timestamped edges, a *single* static
subgraph instance can host an enormous number of temporal instances — and they
overlap. If ann and bob exchanged just 20 messages, there are
**C(20, 3) = 20·19·18 / 6 = 1,140** 3-edge subsequences to test against `M` for
order and window; a static triangle of three chatty nodes multiplies three such
counts. Naive enumeration of edge triples over the whole trace is
`O(m³)`-shaped; even per-subgraph enumeration explodes with activity.

Two structural facts rescue us: instances of an `l`-edge motif are
*subsequences* (not arbitrary sets) of the time-sorted edge list, and the
window constraint means an edge only ever combines with edges at most δ away —
a **sliding window**.

Why it matters: this is a classic streaming-aggregation shape — the cost model
becomes "per-edge work × m," not "candidate tuples" — *if* you can find the
right per-edge state. Step 4 is that state.

### Step 4 — the general algorithm: gather, then one window scan

> **In:** the time-sorted edges among one static subgraph's nodes (Step 3).
> **Out:** a single motif-instance count, produced by one sliding-window pass
> that maintains counts of *partial* matches.

The paper's general algorithm (§4.1, Algorithm 1) has two phases: (1) enumerate
instances of the motif's underlying *static* subgraph `H` (subgraph matching,
topic 24 machinery); (2) for each instance, gather the timestamped edges among
its nodes, sort by time, and count matching subsequences with one pass of a
sliding window, maintaining counts of *partial* matches. Algorithm 1 counts
**all** motifs at once by keying counters on label strings; the paper notes
there are `O(l²)` contiguous subsequences of an `l`-edge motif (§4.1).
Specialized to one motif, the state is exactly those `O(l²)` counters of the
motif's contiguous fragments:

```rust
// ILLUSTRATION — not quoted from Paranjape et al. This specializes the paper's
// Algorithm 1 (§4.1) to a single motif; the paper's own version keys counters
// on label strings to count all 36 at once (its Fig 2 traces the counters).
// The nearest single-pass-over-time-sorted-events code in this repo is
// experiments/src/temporal_reach.rs:20 (same streaming shape, different state).
//
// event = (t, lab); lab says which ordered node-pair the edge uses
// (for M = (A→B, B→A, A→B): A→B ⇒ 0, B→A ⇒ 1, so motif = [0, 1, 0]).
fn count_delta_motif(events: &[(u64, u8)], motif: &[u8], delta: u64) -> u64 {
    let l = motif.len();
    let mut cnt = vec![vec![0u64; l]; l]; // cnt[i][j]: matches of motif[i..=j]
    let (mut total, mut head) = (0u64, 0usize);
    for &(t, lab) in events {
        // 1. expire events older than t − δ. The expiring event is the
        //    OLDEST in the window, so any partial match containing it must
        //    START with it — subtract those, SHORTEST fragments first, so
        //    the inner count cnt[i+1][j] is already old-free when used.
        while events[head].0 + delta < t {
            let old = events[head].1;
            for len in 1..l {
                for i in 0..=l - len {
                    let j = i + len - 1;
                    if motif[i] == old {
                        cnt[i][j] -= if len == 1 { 1 } else { cnt[i + 1][j] };
                    }
                }
            }
            head += 1;
        }
        // 2. bank completions BEFORE inserting: the new event can only
        //    ever be the LAST edge of a full match. `total` never expires.
        if lab == motif[l - 1] {
            total += if l == 1 { 1 } else { cnt[0][l - 2] };
        }
        // 3. insert: extend fragments, LONGEST first, so the new event is
        //    counted at most once per match.
        for len in (1..l).rev() {
            for j in len - 1..l {
                let i = j + 1 - len;
                if motif[j] == lab {
                    cnt[i][j] += if len == 1 { 1 } else { cnt[i][j - 1] };
                }
            }
        }
    }
    total
}
```

Per event the work is the `O(l²)` counter updates — for `l = 3` that is a
fixed 3×3 grid, a constant — so the scan is linear in the instance's edge count
and never materializes a candidate triple. The paper states the matching
2-node bound as `O(2lm)`, linear in `m` and optimal up to constants (§4.1).

Why it matters: correctness lives entirely in the two update orders (expire
shortest-first, insert longest-first) — get either wrong and you double-count.
The cost that remains is phase (1): static subgraph enumeration dominates,
which motivates Step 5.

### Step 5 — fast paths: 2-node and star motifs are easy, triangles are the fight

> **In:** phase (1)'s subgraph-enumeration cost from Step 4.
> **Out:** three specialized bounds — the reason the 63M-edge traces are
> feasible at all.

For motifs whose static shape is trivial, phase (1) collapses:

- **2-node motifs**: group edges by unordered pair, run Step 4's scan per pair.
  Linear overall, `O(m)` — the paper calls this optimal up to constants (§4.1).
- **Star motifs** (all three edges touch one center node): a dynamic program
  over each center's incident edges, keyed by neighbor and direction, with a
  correction that *subtracts* the 2-node counts for the degenerate case where
  the two "spoke" neighbors coincide (that instance is really a 2-node motif).
  Also **`O(m)`**, linear in the input (§4.2).
- **Triangle motifs** are the hard case: an edge between `u` and `v`
  participates in every triangle through that pair, so per-triangle scanning
  re-reads hot edges. The paper's fast algorithm (§4.2, Alg 5) assigns each edge
  to the triangles it can complete and runs in **`O(TriEnum + m√τ)`**, where
  `TriEnum` is the time to list all static triangles, `m` is the number of
  temporal edges, and **`τ` is the number of static triangles** in the induced
  graph. That is a genuine reduction from the naive per-triangle `O(mτ)` down to
  `O(m√τ)` (§4.2, Theorem).

Worked, to feel the reduction: on a graph with `m = 10⁶` temporal edges and
`τ = 10⁴` static triangles, the naive `O(mτ) = 10¹⁰`; the fast `O(m√τ)`
replaces `τ = 10⁴` with `√τ = 10²`, giving `10⁸` — a **100×** cut, which is why
the paper reports its fast temporal-triangle counter is **up to 56.5×** faster
than a competitive baseline in practice (abstract / §5).

Why it matters: this mirrors topic 24 exactly — stars are the cheap
degree-local counts, triangles are where algorithmic care pays. The paper's
scalability experiments (§5; pull the exact per-dataset speedups into notes.md)
show the specialized algorithms are what make the 63M-edge and larger traces
feasible.

### Step 6 — what the counts reveal: motif fingerprints of communication

> **In:** the per-motif counts produced by Steps 4–5.
> **Out:** the 36-vector "fingerprint" and the query shapes M33 must serve.

A network's vector of 36 motif counts (usually normalized to fractions) is a
behavioral fingerprint. The paper's flagship contrast is **blocking** vs
**non-blocking** communication: on a phone call you cannot talk to two people
at once, so motifs where a node fires a second outgoing edge before receiving a
reply are rare in call networks — while email, which queues, shows them freely
(§5, the switching analysis of Fig 7 finds switching *least* common on Stack
Overflow, *most* common in email). Reciprocation chains like `(A→B, B→A, A→B)`
dominate messaging data; on-off Q&A rhythms show up in the Stack Exchange
traces. And sweeping δ turns one count into a curve whose knees expose the
natural timescales of an interaction (the paper finds certain Stack Overflow
Q&A patterns need ≥ 30 minutes to develop, §5).

Why it matters: these analyses are exactly the query shapes a temporal graph
database gets asked — MATCH an ordered pattern WITHIN δ, GROUP BY motif, sweep
δ — so the counting operators of Steps 4–5 are not paper curiosities; they are
M33's aggregate path.

## How to read the paper (with the concepts in hand)

~10 pages, budget ~2.5 h.

- **§1 (intro) — read carefully.** The motivating example and the
  blocking/non-blocking teaser; this is Steps 1 and 6 in miniature.
- **§2 (definitions) — read carefully.** The formal `k`-node, `l`-edge
  δ-temporal motif and instance definitions (Step 2) and the 36-motif grid
  (Fig 3). Spend real time on Fig 3 — the empirical sections index everything by
  its rows and columns, and you want to point at any cell and name the behavior
  it encodes.
- **§4 (algorithms) — the core.** Read the general algorithm §4.1 (Step 4)
  first and check its counter-update orders against the Rust above; then the
  star section (cheap, §4.2), then the triangle section slowly (Step 5) — the
  edge-to-triangle assignment argument and its `O(m√τ)` bound are the paper's
  main algorithmic contribution. Skim complexity proofs on first pass.
- **§5 (experiments/analysis) — read the heatmaps carefully, skim the rest.**
  The per-dataset motif-fraction heatmaps (Fig 5) carry the findings of Step 6;
  extract the blocking-vs-non-blocking evidence and two concrete speedup numbers
  (general vs fast algorithms) into notes.md.
- **Related work — skim**, noting how δ-motifs differ from earlier
  "time-respecting subgraph" definitions that require paths rather than ordered
  windows.

## Questions to answer in notes.md

1. Derive the 36: why exactly that many motifs with `l = 3` edges on `k ≤ 3`
   nodes and a total order? Show the counting argument (the 4 + 8 + 24
   decomposition, or the 6 × 6 grid).
2. In Step 4's code, why must expiry update shortest fragments first and
   insertion longest first? Construct a 3-event sequence that gets miscounted if
   either order is flipped.
3. From the heatmaps (Fig 5): which motif cells separate the phone/SMS
   (blocking) datasets from email (non-blocking)? Record the actual fractions
   the paper reports.
4. Capstone M33: write motif `M = (A→B, B→A, A→B)` as a time-respecting MATCH
   with WITHIN δ. Which parts does the planner get free from the δ constraint,
   and where must Step 4's window-scan operator replace enumerate-then-filter to
   avoid Step 3's C(n, 3) blowup?
5. Topic 24 tie: static triangle counting is a masked matrix multiply in
   GraphBLAS. Exactly where does the *temporal* triangle count stop being
   expressible as a matrix product, and what per-triangle state survives?
6. δ-sweep as a workload: if a user recomputes counts at 20 values of δ, what
   does topic 30's time-bucketed storage (M30) let you reuse across sweeps, and
   what must be recomputed?

## Done when

Answer each before unfolding it.

- [ ] You can derive the 36-motif count.

  <details><summary>Answer</summary>

  Count `l = 3`-edge motifs on `k ≤ 3` nodes, up to relabeling nodes by order
  of first appearance, and decompose by node count (Fig 3's colours):

  - **2-node** (nodes A, B only): the first edge is fixed A→B by the
    first-appearance convention, leaving 2 directions each for edges 2 and 3 →
    `2 × 2 = 4` motifs.
  - **Triangle** (3 distinct nodes, each of the 3 edges on a different pair):
    `8` motifs.
  - **Star** (a center plus two spokes, all 3 edges incident to the center):
    the paper groups these into 3 classes (pre / post / mid) of `2³ = 8` each →
    `24` motifs.

  `4 + 8 + 24 = 36`, exactly the 6 × 6 grid the paper indexes by second edge
  (row) and third edge (column) with the first edge fixed (§2, Fig 3).

  </details>

- [ ] You can hand-trace Step 4's window scan over a five-event sequence without miscounting.

  <details><summary>Answer</summary>

  Take `M = [0, 1, 0]` (i.e. A→B, B→A, A→B), δ large enough that nothing
  expires, and events `(t, lab)` = `(1,0), (2,1), (3,0), (4,1), (5,0)`. Process
  left to right, banking completions *before* inserting (Step 4, phase 2):

  - `(1,0)`: `lab 0 == motif[2] 0`, but `cnt[0][1] = 0`, so bank 0. Insert
    extends fragment `[0]`: `cnt[0][0] = 1`.
  - `(2,1)`: `lab 1 ≠ motif[2] 0`, bank nothing. Insert: `motif[1] = 1`, so
    `cnt[0][1] += cnt[0][0] = 1`.
  - `(3,0)`: `lab 0 == motif[2]`, bank `cnt[0][1] = 1` → `total = 1`. Insert:
    `cnt[0][0] += 1 = 2`.
  - `(4,1)`: bank nothing. Insert: `cnt[0][1] += cnt[0][0] = 2` → `cnt[0][1] = 3`.
  - `(5,0)`: bank `cnt[0][1] = 3` → `total = 4`.

  Four instances: the A→B at position 5 completes with each earlier
  (A→B, B→A) prefix, and the A→B at position 3 completed one earlier. Banking
  before inserting is what stops event `(5,0)` from pairing with itself; the
  shortest-first expiry (unused here) is what keeps the subtraction consistent
  when δ is finite.

  </details>

- [ ] You can explain in one sentence each why stars are cheap and triangles are hard.

  <details><summary>Answer</summary>

  **Stars are cheap** because every edge of a 3-node star touches the center, so
  a single dynamic-programming pass over each center's incident edges — keyed by
  neighbor and direction, minus the 2-node-motif correction for coincident
  spokes — counts them in `O(m)`, linear in the input (§4.2).

  **Triangles are hard** because an edge between `u` and `v` lies on every
  triangle through that pair, so scanning per triangle re-reads hot edges; the
  paper's fix assigns each edge to the triangles it can complete, cutting the
  naive `O(mτ)` to `O(TriEnum + m√τ)` where `τ` is the static-triangle count
  (§4.2) — the `√τ` is the whole reason a 63M-edge trace finishes.

  </details>

- [ ] You can state which M33 query shape each of the paper's two algorithm families maps onto.

  <details><summary>Answer</summary>

  The **general algorithm** (§4.1, Step 4) counts any single `M` by gathering
  the edges among a matched static subgraph and running the `O(l²)`-state
  sliding-window scan — it maps onto M33's *"MATCH this specific ordered pattern
  WITHIN δ"*, where the planner has already pinned the shape and only the
  window scan remains.

  The **specialized family** (§4.2, Step 5 — 2-node, star, triangle) counts
  *all* motifs of a class at once with per-class bounds (`O(m)` for 2-node and
  stars, `O(m√τ)` for triangles) — it maps onto M33's *"GROUP BY motif over the
  whole trace"* aggregate, where enumerate-then-filter would pay Step 3's
  `C(n,3)` blow-up and the specialized counters avoid it.

  </details>

## References

**Papers**
- Paranjape, Benson, Leskovec — "Motifs in Temporal Networks" (WSDM 2017) —
  [arXiv](https://arxiv.org/abs/1612.09259) /
  [PDF](https://arxiv.org/pdf/1612.09259) — ~10 pages, ~2.5 h: read the §2
  definitions and Fig 3 carefully, the §4.1 general algorithm against Step 4's
  code, the §4.2 triangle section slowly, and the §5 heatmaps for the findings;
  skim proofs. Anchors used above: §2 + Fig 3 (definition, the 36 motifs),
  §4.1 (Algorithm 1, `O(l²)` counters, `O(2lm)` 2-node bound), §4.2 (star `O(m)`,
  triangle `O(TriEnum + m√τ)`, up to 56.5× speedup), §5 (blocking vs
  non-blocking, δ-sweep).
- Dataset: the SNAP `sx-stackoverflow` temporal network the paper released —
  2,601,977 nodes, 63,497,050 temporal edges, 2,774-day span
  ([snap.stanford.edu/data/sx-stackoverflow](https://snap.stanford.edu/data/sx-stackoverflow.html)).

**Related guides**
- [reading-temporal-paths.md](reading-temporal-paths.md) — where *reachability*
  (not ordering) is the information; the two guides are the two ways timestamps
  change a graph question.
- [README.md](README.md) — the topic's measured false-positive headline; the
  motif count is the *aggregate* companion to that path query.
