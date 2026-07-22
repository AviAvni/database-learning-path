# δ-temporal motifs: counting ordered patterns inside a time window

Topic 24 counted static triangles with a masked matrix multiply; the
previous guide showed that once edges carry timestamps, *order* is
information. This paper fuses the two: a pattern is no longer a subgraph
but an ordered sequence of edges that must all land inside a window δ —
and counting those is a new algorithmic problem. This chapter builds the
six concepts, ending at the exact window-scan operator M33's WITHIN δ
needs.

## The problem in one sentence

In a trace like Stack Overflow's ~63 million timestamped edges, "A messaged
B, then B messaged C, then C messaged A — all within one hour" is a single
pattern out of **36** possible 3-edge orderings on at most 3 nodes, and
counting its instances by enumerating triples of edges is hopeless.

## The concepts, step by step

### Step 1 — timestamped edges, and why order is information

A **temporal network** here is just a multiset of directed, timestamped
edges `(u, v, t)` — who contacted whom, when (no duration λ this time; an
edge is an instantaneous event). A static motif (a small subgraph pattern,
e.g. a triangle) treats these two histories as identical:

```
history 1:  A→B at 9:00,  B→C at 9:05     plausible information flow
history 2:  B→C at 9:00,  A→B at 9:05     B "forwarded" before receiving
```

Both condense to the static path A→B→C, but only one is a possible relay.

Why it matters: every behavioral question — forwarding, reciprocation,
who-answers-whom — lives in the *ordering*, which the static count
destroys. This is Step 2 of the previous guide (condensing lies) applied
to patterns instead of paths.

### Step 2 — the δ-temporal motif: sequence + order + window

A **δ-temporal motif** is an ordered sequence of k edge patterns on l node
placeholders — say `M = (A→B, B→A, A→B)` — and an **instance** of it is a
set of k actual edges that (a) map onto the placeholders consistently,
(b) occur in exactly the specified order, and (c) all fit in a window of
duration δ: last timestamp − first timestamp ≤ δ. Example with δ = 1 h:

```
edges between ann and bob:   ann→bob 9:00   bob→ann 9:20   ann→bob 9:50
                             ann→bob 11:00

(9:00, 9:20, 9:50)  ✓ instance of M   — right order, spans 50 min ≤ δ
(9:00, 9:20, 11:00) ✗ spans 2 h > δ
(9:20, 9:50, 11:00) ✗ order is B→A, A→B, A→B — a different motif
```

The paper fixes k = 3 edges and l ≤ 3 nodes and shows there are exactly
**36** such motifs (their grid figure — a 6 × 6 layout: the first two edges
determine a row, the third a column).

Why it matters: δ is doing real semantic work — it encodes "these events
belong to one interaction," and every count is meaningless without stating
it. This is precisely the WITHIN δ clause of capstone M33.

### Step 3 — why counting is hard: one static shape, many temporal instances

Because the same pair can carry many timestamped edges, a *single* static
subgraph instance can host an enormous number of temporal instances — and
they overlap. If ann and bob exchanged just 20 messages, there are
C(20, 3) = 1,140 3-edge subsequences to test against M for order and
window; a static triangle of three chatty nodes multiplies three such
counts. Naive enumeration of edge triples over the whole trace is
O(m³)-shaped; even per-subgraph enumeration explodes with activity.

Two structural facts rescue us: instances of a k-edge motif are
*subsequences* (not sets) of the time-sorted edge list, and the window
constraint means an edge only ever combines with edges at most δ away —
a sliding window.

Why it matters: this is a classic streaming-aggregation shape — the cost
model is "per-edge work × m", not "candidate tuples" — if you can find the
right per-edge state. Step 4 is that state.

### Step 4 — the general algorithm: gather, then one window scan

The paper's general algorithm has two phases: (1) enumerate instances of
the motif's underlying *static* subgraph H (subgraph matching, topic 24
machinery); (2) for each instance, gather the timestamped edges among its
nodes, sort by time, and count matching subsequences with one pass of a
sliding window, maintaining counts of *partial* matches. The paper's
Algorithm 1 counts all motifs at once by keying counters on label strings;
specialized to one motif, the state is counts of the motif's contiguous
fragments:

```rust
/// Count instances of one k-edge motif in a single pass over the
/// time-sorted edges of ONE static instance's node set.
/// event = (t, lab); lab says which ordered node-pair the edge uses
/// (for M = (A→B, B→A, A→B): A→B ⇒ 0, B→A ⇒ 1, so motif = [0, 1, 0]).
fn count_delta_motif(events: &[(u64, u8)], motif: &[u8], delta: u64) -> u64 {
    let k = motif.len();
    let mut cnt = vec![vec![0u64; k]; k]; // cnt[i][j]: matches of motif[i..=j]
    let (mut total, mut head) = (0u64, 0usize);
    for &(t, lab) in events {
        // 1. expire events older than t − δ. The expiring event is the
        //    OLDEST in the window, so any partial match containing it must
        //    START with it — subtract those, SHORTEST fragments first, so
        //    the inner count cnt[i+1][j] is already old-free when used.
        while events[head].0 + delta < t {
            let old = events[head].1;
            for len in 1..k {
                for i in 0..=k - len {
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
        if lab == motif[k - 1] {
            total += if k == 1 { 1 } else { cnt[0][k - 2] };
        }
        // 3. insert: extend fragments, LONGEST first, so the new event is
        //    counted at most once per match.
        for len in (1..k).rev() {
            for j in len - 1..k {
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

Per event the work is O(k²) counter updates — constant for k = 3 — so the
scan is linear in the instance's edge count and never materializes a
candidate triple.

Why it matters: correctness lives entirely in the two update orders
(expire shortest-first, insert longest-first) — get either wrong and you
double-count. The cost that remains is phase (1): static subgraph
enumeration dominates, which motivates Step 5.

### Step 5 — fast paths: 2-node and star motifs are easy, triangles are the fight

For motifs whose static shape is trivial, phase (1) collapses. **2-node
motifs**: group edges by unordered pair, run Step 4's scan per pair —
linear overall. **Star motifs** (all three edges touch one center node):
one pass over each center's incident edges with per-neighbor,
per-direction counters — again near-linear, with a correction for the
degenerate case where the two "spoke" neighbors coincide (that instance is
really a 2-node motif). **Triangle motifs** are the hard case: an edge
between u and v participates in every triangle through that pair, so
per-triangle scanning re-reads hot edges over and over. The paper adapts
the classic static trick — treat high-degree ("heavy") pairs specially and
assign each edge to the triangles it can complete — landing in the same
O(m√m) territory as static triangle listing, instead of paying "edges ×
triangles through them."

Why it matters: this mirrors topic 24 exactly — stars are the cheap
degree-local counts, triangles are where algorithmic care pays — and the
paper's scalability experiments (pull the exact speedups into notes.md)
show the specialized algorithms are what make the 63M-edge and larger
traces feasible at all.

### Step 6 — what the counts reveal: motif fingerprints of communication

A network's vector of 36 motif counts (usually normalized to fractions) is
a behavioral fingerprint. The paper's flagship contrast is **blocking**
vs **non-blocking** communication: on a phone call you cannot talk to two
people at once, so motifs where a node fires a second outgoing edge before
receiving a reply are rare in call networks — while email, which queues,
shows them freely. Reciprocation chains like `(A→B, B→A, A→B)` dominate
messaging data; on-off Q&A rhythms show up in the Stack Exchange traces.
And sweeping δ turns one count into a curve whose knees expose the natural
timescales of an interaction (seconds for SMS ping-pong, days for email
threads).

Why it matters: these analyses are exactly the query shapes a temporal
graph database gets asked — MATCH an ordered pattern WITHIN δ, GROUP BY
motif, sweep δ — so the counting operators of Steps 4–5 are not paper
curiosities; they are M33's aggregate path.

## How to read the paper (with the concepts in hand)

- **§1 (intro) — read carefully.** The motivating example and the
  blocking/non-blocking teaser; this is Steps 1 and 6 in miniature.
- **§2 (definitions) — read carefully.** The formal δ-temporal motif and
  instance definitions (Step 2) and the 36-motif grid figure. Spend real
  time on the grid — the empirical sections index everything by its rows
  and columns, and you want to be able to point at any cell and name the
  behavior it encodes.
- **§3 (algorithms) — the core.** Read the general algorithm (Step 4)
  first and check its counter-update orders against the Rust above; then
  the star section (cheap), then the triangle section slowly (Step 5) —
  the edge-to-triangle assignment argument is the paper's main algorithmic
  contribution. Skim complexity proofs on first pass.
- **§4 (experiments/analysis) — read the heatmaps carefully, skim the
  rest.** The per-dataset motif-fraction heatmaps carry the findings of
  Step 6; extract the blocking-vs-non-blocking evidence and two concrete
  speedup numbers (general vs fast algorithms) into notes.md.
- **Related work — skim**, noting how δ-motifs differ from earlier
  "time-respecting subgraph" definitions that require paths rather than
  ordered windows.

## Questions to answer in notes.md

1. Derive the 36: why exactly that many motifs with 3 edges on at most
   3 nodes and a total order? Show the counting argument.
2. In Step 4's code, why must expiry update shortest fragments first and
   insertion longest first? Construct a 3-event sequence that gets
   miscounted if either order is flipped.
3. From the heatmaps: which motif cells separate the phone/SMS (blocking)
   datasets from email (non-blocking)? Record the actual fractions the
   paper reports.
4. Capstone M33: write motif `M = (A→B, B→A, A→B)` as a time-respecting
   MATCH with WITHIN δ. Which parts does the planner get free from the
   δ constraint, and where must Step 4's window-scan operator replace
   enumerate-then-filter to avoid Step 3's C(n, 3) blowup?
5. Topic 24 tie: static triangle counting is a masked matrix multiply in
   GraphBLAS. Exactly where does the *temporal* triangle count stop being
   expressible as a matrix product, and what per-triangle state survives?
6. δ-sweep as a workload: if a user recomputes counts at 20 values of δ,
   what does topic 30's time-bucketed storage (M30) let you reuse across
   sweeps, and what must be recomputed?

## Done when

You can derive the 36-motif count, hand-trace Step 4's window scan over a
five-event sequence without miscounting, explain in one sentence each why
stars are cheap and triangles are hard, and state which M33 query shape
each of the paper's two algorithm families (general vs specialized) maps
onto.

## References

**Papers**
- Paranjape, Benson, Leskovec — "Motifs in Temporal Networks" (WSDM
  2017) — [arXiv](https://arxiv.org/abs/1612.09259) /
  [PDF](https://arxiv.org/pdf/1612.09259) — ~10 pages, ~2.5 h: definitions
  and the 36-grid carefully, the general algorithm against Step 4's code,
  the triangle section slowly, heatmaps for the findings; skim proofs
