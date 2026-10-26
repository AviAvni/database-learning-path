# Gorder, replicated: vertex order as a cache optimisation, and the cheap orders that nearly match it

The other guides here measure a vertex order in **bits**. This one measures it
in **cache misses and seconds** on unmodified algorithms — the claim a graph
engine cares about, since nothing else about the algorithm changes. The source
is a replication, which is better evidence than the original paper alone: it
re-implements ten orderings, runs nine algorithms on nine real graphs with
hardware cache counters, and reports where the original's ranking holds and
where it does not.

**"[Re] Speedup Graph Processing by Graph Ordering"** — Fabrice Lécuyer,
Maximilien Danisch, Lionel Tabourier. ReScience C 7.1 (#3), 2021
([pdf](https://zenodo.org/records/4836230), DOI 10.5281/zenodo.4836230; code
`github.com/lecfab/rescience-gorder`). It replicates **"Speedup Graph
Processing by Graph Ordering"**, Hao Wei, Jeffrey Xu Yu, Can Lu and Xuemin Lin,
SIGMOD 2016, which defines **Gorder**. The SIGMOD paper itself was not
obtained; everything below about Gorder is as the replication states it
(§2.3, §3), and numbers are the replication's. Section, table and figure
numbers are the replication's.

## The problem in one sentence

When an algorithm reads the neighbours of a vertex, how should the vertices be
numbered so that vertices read close together in time sit close together in
memory — and how much does it matter, and what does the best ordering cost to
compute?

## The concepts, step by step

### Step 1 — the mechanism: cache lines and the gather

> **In:** a graph in CSR and a per-vertex value array. **Out:** why vertex
> numbering changes the running time of an algorithm that never mentions it.

A **cache line** is the unit the CPU fetches from memory: 64 bytes on x86
(128 on Apple's M-series). An algorithm that visits vertex `v` and reads
`value[u]` for each neighbour `u` performs a **gather**: the reads land at
whatever addresses the neighbours' ids dictate. If the neighbours' ids are
close, their values share cache lines and one fetch serves several reads; if
they are scattered, each read fetches a line to use 8 bytes of it.

The replication's Figure 1 shows the effect on `sdarc` (95 M vertices, 1.94 G
edges): CPU execution time is "almost identical" under the original order and
under Gorder, while time spent *waiting for data* falls by a factor of 3 — "the
total runtime is reduced by 15 to 50% on sdarc" (§3.5). The paper's motivation
(quoted by the replication) is that cache stalls can reach 70% of the total
computation time.

Latency is why: the replication quotes (from a 4 GHz Skylake) about 1 ns for an
L1 hit and about 62 ns for a trip to RAM (footnote 5: 42 cycles + 51 ns): a
factor of 60. A neighbour list that stops sharing lines pays that factor on
every read.

### Step 2 — Gorder: a score for "should sit nearby", maximised greedily

> **In:** the graph. **Out:** the objective Gorder maximises, worked on three
> vertices, and the greedy procedure (replication §2.3).

For vertices `u` and `v` define a proximity score `S(u, v) = Ss(u, v) +
Sn(u, v)`: `Ss` is the number of **common in-neighbours** (vertices that point
to both: *siblings*), `Sn` the number of edges between `u` and `v` (0, 1 or 2,
counting both directions). Given an ordering `π` and a **window** `w`, the
quality is `F(π) = Σ S(u, v)` over all pairs with `0 < π(u) − π(v) ≤ w`: only
pairs placed within `w` of each other count, since only those can share a cache
line or stay in cache together.

Worked on three vertices with edges `A→B`, `A→C`, `B→C`, window `w = 1` (only
adjacent positions count):

| pair | edges between (`Sn`) | common in-neighbours (`Ss`) | `S` |
|---|---|---|---|
| (A, B) | A→B: 1 | in(A) = {} so 0 | 1 |
| (B, C) | B→C: 1 | in(B) = {A}, in(C) = {A, B}: common {A} = 1 | 2 |
| (A, C) | A→C: 1 | in(A) = {} so 0 | 1 |

Ordering `(A, B, C)`: `F = S(A,B) + S(B,C) = 1 + 2 = 3`. Ordering
`(B, A, C)`: `F = S(B,A) + S(A,C) = 1 + 1 = 2`. Gorder prefers the first.

Maximising `F` is NP-hard (the original proves it); Gorder is a **greedy**
heuristic: place vertices one at a time, always the not-yet-placed vertex with
the highest total score against the last `w` placed vertices, maintained in a
**unit heap** — a structure the replication describes as "a linked list and
pointers to different positions", used because scores change by ±1. The window
is `w = 5` in the original (chosen on PageRank on `flickr`, where the eight
windows 1–8 differ by only 3%); the replication's Figure 4 finds a plateau at
`w = 64 … 2048` that is a further 3% faster but costs more to compute, since
each candidate must be scored against the whole window. The original proves
the heuristic is a `1/(2w)`-approximation, so a bigger `w` loosens the
guarantee.

### Step 3 — the field of cheaper orderings

> **In:** Step 2's goal. **Out:** the ten orderings compared and what each one
> is (replication §2.3), so a row in a table means something.

| ordering | what it does |
|---|---|
| Original | the ids the dataset shipped with |
| Random | a shuffle (added by the replication; the worst case) |
| MinLA / MinLogA | simulated annealing on `Σ|π(u)−π(v)|` / `Σ log|π(u)−π(v)|` |
| RCM | reverse Cuthill–McKee: a BFS that visits small-degree vertices first, reversed |
| InDegSort | sort by descending in-degree |
| ChDFS | a depth-first traversal, ids in visit order |
| SlashBurn | iteratively peel off hubs; the replication implements a simplified version |
| LDG | linear deterministic greedy partitioning into bins of 64 |
| Gorder | Step 2 |

MinLA is the same family as Dhulipala et al.'s LogGap objective
([reading-recursive-bisection.md](reading-recursive-bisection.md), Step 1),
optimised by annealing rather than bisection. The replication "was not able to
find a combination … that outperforms a simple local search", so its MinLA and
MinLogA results question themselves (§2.3, §4).

### Step 4 — the result, with the cache counters

> **In:** Steps 1–3. **Out:** what held up, with the two datasets whose cache
> counters the replication prints.

Overall (§3.3–§3.4): over 81 series (9 algorithms × 9 datasets) "Gorder is the
best ordering method in half of the experiments, and second-best in most other
cases" — first in **40 of 81**. The speed-up factor "reaches up to 2.5 compared
to default for Diameter on `sdarc` and 3.7 compared to random for PageRank on
`wiki`". Random is "always the worst performer except for 6 experiments where
it is second-worst".

Cache counters for PageRank (Table 3), `sdarc` (1.94 G edges); *L1 miss rate*
is the share of reads not served from L1, *L3 refs* the reads that reached the
last level (units of 10⁹), *cache miss rate* the share that went to RAM:

| ordering | L1 miss rate | L3 refs (10⁹) | cache miss rate |
|---|---|---|---|
| Original | 19.0% | 303 | 6.8% |
| Random | 23.4% | 397 | 9.0% |
| InDegSort | 15.0% | 198 | 6.0% |
| ChDFS | 11.8% | 153 | 4.3% |
| RCM | 11.0% | 139 | 3.7% |
| Gorder | 9.3% | 104 | 3.1% |

Reading it: L1 references are nearly identical across orderings (1.78–1.89 ·
10¹²) — the algorithm does the same work — while RAM misses vary threefold
(Gorder 3.1%, Random 9.0%). "The ranking for cache-miss rates matches the
ranking for runtime", so the speed-up "is indeed due to cache-miss reduction".
The Original order beating Random by 2.2 points of RAM miss rate is the
replication's own illustration of "the way in which datasets are constructed
tends to give close indices to nodes that are in the same neighbourhood" (§3.3).

### Step 5 — the cost, and the cheap rivals

> **In:** Step 4. **Out:** the replication's qualification of the original
> paper's conclusion, which is the most useful sentence in it for an engine.

Computing the orderings (Table 2, seconds; the replication's cluster is 2–5×
slower than the original's machine):

| ordering | `pokec` (31 M edges) | `sdarc` (1.94 G edges) |
|---|---|---|
| InDegSort | 0.8 | 85 |
| ChDFS | 1 | 76 |
| RCM | 3 | 406 |
| LDG | 6 | 798 |
| Gorder, `w = 5` | 79 | **32,488** (9 hours) |

"Gorder does not scale linearly: the edges processed per second decrease from
380k for `pokec` to 60k for `sdarc`." And the ranking changes against the
original paper: "The only important difference with the original paper is that
RCM and ChDFS follow closely behind Gorder and even outperform it in half of the
experiments" — in the replication RCM is first in 24 series and ChDFS in 16
(Figure 6a), whereas in the original Gorder was outranked once. ChDFS is the
best ordering for DFS "due to the close relation between these two processes:
the algorithm explores the graph in the exact same way as the ordering is
created"; RCM is the best for BFS, SP and Diameter. The replication suspects the
original avoided this by shuffling the DFS neighbour order.

Its conclusion, in one sentence: "it is a matter of balance between this long
overhead time and the substantial speedup for subsequent graph algorithms" —
and, citing Balaji & Lucia, the overhead "can only be amortised if algorithms
are run thousands of times", which for a database means the ordering must be
recomputed or maintained as the graph changes, which Gorder does not support.

### Step 6 — what this means for a vertex-id design

> **In:** Steps 1–5. **Out:** the three questions lane 1 and lane 2 let you
> answer on a graph you can generate.

1. **Is the id order you have already good?** The replication's Original
   beats MinLA and MinLogA. Graphs with crawl-ordered ids behave this way;
   graphs numbered by arrival or by hash do not. Lane 1 has both: `planted
   (oracle)` is a good original order, `random (as loaded)` a bad one.
2. **How much does a *cheap* order recover?** InDegSort and BFS are cheap;
   Table 3 puts InDegSort between Original and RCM. Lane 1 measures the same
   three cheap orders (in-degree, BFS, plus random) against the oracle.
3. **Does an expensive order pay?** Bisection (lane 2) costs minutes; it
   plays the role Gorder plays here: the best size, at the highest preprocessing
   cost. The replication's lesson says to ask for the amortisation count before
   adopting it.

One caveat belongs next to Step 4: the counters come from a cluster of Xeon
E5-4650L processors with a 20 MB L3 cache, on graphs of up to 34 GB — the regime
where orderings matter because the value array does not fit. On a graph whose
value array fits in cache the gap closes, which is why lane 1 uses 6 M
vertices: a 48 MB value array against the 16 MB L2 of an M3 Pro performance
cluster.

## How to read the replication (with the concepts in hand)

Read §2.3 for the definitions (the formula for `F` is there, with the window
and the unit heap), §3.2's Table 2 for cost, §3.3's Figure 5 for speed-ups,
§3.5's Table 3 for the counters, §4 for the qualification. Figure 6 is the
rankings summary. The code
(`github.com/lecfab/rescience-gorder`) was not read.

## Where each step lives in this topic

| step | here |
|---|---|
| gather and cache lines | `gapcode::simulate_gather_misses`, lane 1 `miss %` |
| cheap orders | `order.rs` |
| bisection as the "expensive order" | `bisect.rs` (lane 2) |
| scan and BFS timings | `measure.rs`, lane 1 `CSR ns/e`, `BFS ms` |

## Questions (answer in notes.md)

1. Compute `F` for the order `(A, C, B)` on the worked three-vertex graph with
   `w = 1`, and with `w = 2`.
2. On `sdarc`, how many times higher is Random's RAM-miss rate than Gorder's?
   How many times higher is its L3 traffic?
3. Why does ChDFS win on DFS in the replication, and what does that say about
   evaluating an ordering with the algorithm it was derived from?
4. Your lane-1 table has an `in-degree sorted` row. Which row of Table 3 is it
   comparable to, and what does the replication say about it?

## Done when

Answer each before unfolding it.

- [ ] You can compute Gorder's score for a small ordering.

  <details><summary>Answer</summary>

  S(u, v) = common in-neighbours + edges between u and v; F sums S over pairs
  placed within w positions. For edges A→B, A→C, B→C and w = 1: S(A,B) = 1,
  S(B,C) = 1 + 1 = 2, S(A,C) = 1, so (A, B, C) scores 3 and (B, A, C) scores
  1 + 1 = 2.

  </details>

- [ ] You can say what the replication confirmed and what it qualified.

  <details><summary>Answer</summary>

  Confirmed: Gorder is the best overall ordering (first in 40 of 81 series),
  with speed-ups up to 2.5× over the original order and 3.7× over random, and
  the gain comes from cache stalls (RAM miss rate 3.1% against 6.8% on sdarc
  PageRank; CPU time unchanged). Qualified: RCM and ChDFS, computed in seconds
  to minutes, are first in 24 and 16 series and follow Gorder closely, while
  Gorder takes 32,488 s (9 hours) on sdarc.

  </details>

- [ ] You can explain why Original beat MinLA and MinLogA.

  <details><summary>Answer</summary>

  The datasets' ids already place neighbours close (web pages sorted by URL,
  for example). Annealing, hard to tune, did not improve on a local search and
  did not beat that order; the replication says its own tuning is unreliable.
  Orders only help relative to the order you start with.

  </details>

- [ ] You can say what the original paper's L1/L3/RAM chain means for a
  random-access workload.

  <details><summary>Answer</summary>

  The same number of L1 references, different miss rates at each level; each
  further level is roughly 4× slower and RAM is about 60× slower than L1. A
  3× lower RAM-miss rate on the same reference count is where the 15–50%
  runtime reduction comes from.

  </details>

## References

- Lécuyer, Danisch & Tabourier, *[Re] Speedup Graph Processing by Graph
  Ordering*, ReScience C 7.1 (#3), 2021 — §2.3, §3.2 (Table 2), §3.3
  (Figures 5–6), §3.5 (Table 3), §4.
- Wei, Yu, Lu & Lin, *Speedup Graph Processing by Graph Ordering*, SIGMOD 2016,
  DOI 10.1145/2882903.2915220 — the original; not obtained.
- Balaji & Lucia, *When is Graph Reordering an Optimization?*, IISWC 2018 —
  cited by the replication for the amortisation point; not read.
- Arai et al., *Rabbit Order* (IPDPS 2016) — another cheap, community-based
  ordering; not obtained.
