# FlowScope: laundering is dense flow, not a dense block

FRAUDAR (topic exercise 1-4) hunts for a dense bipartite block — many edges crammed
between one set of rows and one set of columns. Money laundering deliberately avoids
looking like that: dirty money fans out of a few source accounts, hops through mule
accounts that keep almost nothing, and converges into a few destinations. No single
hop is unusually dense, so block detectors stay quiet. FlowScope's move is to change
the objective from "dense subgraph" to "high-throughput, balanced, multi-step flow" —
and then reuse the same near-greedy peeling machinery FRAUDAR made famous.

## The problem in one sentence

**Find the small set of source, mule, and destination accounts through which the largest volume of money actually flows — where every mule's inflow roughly equals its outflow — even though no single transfer edge or account pair exceeds a threshold.**

## The concepts, step by step

### Step 1 — Layering: why laundering is shaped like a flow

> **In:** nothing yet — a stream of account-to-account transfers under per-account and per-pair reporting thresholds.
> **Out:** the observation that evasion forces a high-volume, balanced, multi-step flow: few sources, pass-through mules, few destinations.

Regulators impose per-account and per-pair reporting thresholds. Launderers respond
with layering: split the dirty amount into many transfers and route them through
middle ("mule") accounts that retain almost nothing. The result is a high-volume,
balanced, multi-step flow: few sources, a layer (or several) of mules, few
destinations. The evasion tactic itself is the signature — the volume is enormous
in aggregate, but only visible when you require it to pass *through* the middle.

```
  sources X          mules W            destinations Y
   x1 --200--> w1 --195--> y1
   x1 --150--> w2 --148--> y1        fan-out ... pass-through ... fan-in
   x2 --300--> w3 --297--> y2
   x2 --100--> w1  (w1 out: 195+100 ~ in)
   few accounts | keep ~nothing |  few accounts
```

### Step 2 — Why dense-block methods miss it

> **In:** the layered-flow shape from Step 1.
> **Out:** why any per-hop dense-block score is blind — the anomaly is conjunctive, coupled through the mules.

FRAUDAR-style detectors score one bipartite block: rows X against columns Y, edges
weighted by column degree. In a laundering ring, the X→W hop alone is not dense —
each mule receives from only a few sources; the W→Y hop alone is equally bland.
The anomaly is *conjunctive*: money that enters a mule must also leave it, toward
the same small destination set. Score each hop independently and the signal
vanishes into the background of normal banking traffic. FlowScope therefore scores
the two (or more) hops jointly, coupled through the mules.

```
  FRAUDAR view:                  FlowScope view:
  [ X | Y ] one dense block?     [ X ]-->[ W ]-->[ Y ]
   no hop is dense -> miss        volume must FLOW THROUGH W -> hit
```

### Step 3 — The k-partite transfer graph

> **In:** the conjunctive-signal requirement from Step 2.
> **Out:** the k-partite transfer graph (X → W → Y, k=3 in the paper) and the "pick a subset S, score it, optimize" template.

Model transfers as a k-partite graph: sources X in the first partite, one or more
middle layers W, destinations Y in the last. The paper works out k=3 (X → W → Y)
in full and generalizes to more middle layers for deeper laundering chains. An
account can in principle appear in multiple roles; the detector's job is to select
a subset S spanning all partites that maximizes an anomalousness score. This is the
same "pick a subgraph, score it, optimize" template as dense-block mining — only
the score changes.

```
      X (partite 1)        W (partites 2..k-1)      Y (partite k)
   { x1, x2, ... }  --->  { w1, w2, w3, ... }  --->  { y1, y2 }
   k=3 in the paper; deeper chains:  X --> W1 --> W2 --> Y
```

### Step 4 — Throughput f, imbalance q, and the score g(S)

> **In:** a candidate subgraph S spanning all partites (Step 3).
> **Out:** the score g(S), built from each mule's throughput f_i = min(in, out) and imbalance q_i = max(in, out).

For each middle account i inside a candidate subgraph S, define
f_i = min(inflow, outflow) — the money that genuinely flows through — and
q_i = max(inflow, outflow). The subgraph score is the size-normalized sum

```
  g(S) = (1/|S|) * SUM_i [ (1+lambda) * f_i  -  lambda * q_i ]

  mule ledger A:  in 100, out  98  ->  f= 98, q=100   high g contribution
  mule ledger B:  in 100, out  10  ->  f= 10, q=100   parked money: q >> f
  mule ledger C:  in 100+30camo,
                  out  98          ->  f= 98, q=130   camo raised q, not f
```

lambda is the imbalance-penalty weight (lambda = 4 in the paper's experiments).
Parking money (big in, small out) and camouflage transfers both inflate q without
inflating f, so they *reduce* the score — robustness to camouflage is built into
the metric rather than bolted on.

### Step 5 — Near-greedy peeling with a flow-aware heap key

> **In:** the metric g(S) from Step 4 to maximize.
> **Out:** the near-greedy peel with the Eq. (5) priority key, returning ˆS under Theorem 1's bound g(ˆS) ≥ (|M'|/|S'|)·(g(S*) − λε).

The optimizer is FRAUDAR's near-greedy peel: start from the full graph, repeatedly
remove the node whose removal hurts g least, remember the best S seen, return it.
A priority tree keyed by Eq. (5) makes the whole loop near-linear in edges. The key is
role-dependent: a middle node v_i ∈ M is keyed by `w_i = f_i − (λ/(1+λ)) q_i` —
proportional to its g-contribution `(1+λ) f_i − λ q_i`, so the argmin peel order is
identical — while a source or destination node is keyed by its plain degree d_i.
Peeling a source or destination changes the inflow/outflow — hence f and q, hence the
keys — of its middle-layer neighbors. The paper proves an approximation bound
(Theorem 1): `g(ˆS) ≥ (|M'|/|S'|)·(g(S*) − λε)`, where ε is the largest camouflage
volume a laundering account exchanges with honest accounts. It is FRAUDAR's "first
optimal node removed" proof technique, but the constant is |M'|/|S'| (bounded below by
the mule count) and the slack is λε — not a flat ½.

```
  while nodes remain:
      v = pop-min(heap)               # least marginal contribution to g
      remove v; for each middle-layer neighbor m of v:
          recompute in(m), out(m) -> f_m, q_m -> update key(m)
      if g(current S) > g(best): best = current S
  return best
```

### Step 6 — Why camouflage is self-defeating here

> **In:** the metric and peel from Steps 4–5.
> **Out:** the argument that any camouflage transfer raises some mule's q without raising f, so it lowers g — no column weights needed.

FRAUDAR resists camouflage via column weighting: camo edges land on honest
high-degree columns and earn little. FlowScope needs no column weights at all.
Any extra transfer a launderer adds to look normal lands on one side of some
mule's ledger, raising that mule's q while leaving f untouched — and the
lambda-weighted penalty drags g down. The attacker's only safe strategy is to keep
every mule perfectly balanced and dedicated, which is exactly the pattern the
metric is maximized by, i.e. exactly what gets detected. Compare: same adversarial
robustness goal, two very different mechanisms (reweighting vs metric shape).

### Step 7 — Evidence: CBank and CFD

> **In:** FlowScope run on the CBank and CFD datasets.
> **Out:** FAUC 0.761/0.843 on CBank (vs FRAUDAR 0.529/0.704) and F1 ≥ 0.9 down to $76M injected vs FRAUDAR's $180M.

CBank is a real bank dataset: 6.13M accounts and 43.98M transfer records, with a
labeled real laundering ring of 4 sources, 12 mules, and 2 destinations (Fig. 1). Its
central mule v5 alone passes ≈452.1M yuan through — inflow ≈ outflow, so q5 − f5 ≈ 0
and almost nothing is left in balance (Example 1); that near-zero residue is exactly
what g(S) rewards. On the two CBank injection settings FlowScope scores FAUC 0.761 and
0.843 versus FRAUDAR's 0.529 and 0.704 (7:5:3 A:M:C ratio). In the injection
experiments FlowScope holds F1 at 0.9 or above down to injected volumes of $76 million,
where FRAUDAR needs $180 million (paper units: million $) — FlowScope detects
laundering at less than half the volume. On the Czech Financial Dataset (CFD) it
reaches FAUC 0.970 and 0.900. The practical reading: the flow objective buys
sensitivity, not just elegance.

### Step 8 — Database-engineer lens: peeling, k-core, and streaming

> **In:** the static-snapshot peel from Steps 5–7.
> **Out:** the mapping to k-core peeling machinery and the open streaming/temporal-window gap for production AML.

The peel is the same degree-ordered elimination family as k-core decomposition
(topic 18) — a lazy min-heap over a per-node key, with neighbor updates on each
removal; everything you know about making k-core fast (bucketed keys, cache-aware
adjacency) transfers. The gap to production AML: monitoring is a streaming,
temporal problem — transfers arrive in windows, rings live for weeks — while the
paper scores one static snapshot. How you would maintain f_i/q_i incrementally
under a sliding window is a genuinely open engineering question worth a note.

## How to read the paper (with the concepts in hand)

It is a short AAAI paper; one careful pass suffices if you enter with the metric
(Step 4) and the peel (Step 5) already firm. Map sections to steps as follows.

- **Section 1 (Introduction)** — the fan-out/pass-through/fan-in story and why
  thresholds force layering. You have this from Steps 1-2; skim for the figures.
- **Section 2 (Related work)** — positions FlowScope against dense-block mining
  (FRAUDAR and kin). Read with Step 2's "conjunctive signal" framing in mind.
- **Section 3 (Problem formulation)** — the k-partite model, f_i, q_i, and g(S).
  This is Steps 3-4; check that the camouflage argument matches Step 6 before
  moving on, since everything downstream leans on it.
- **Section 4 (Proposed method)** — the near-greedy peel, priority-queue
  implementation, complexity, and the approximation guarantee. Map each paragraph
  onto Step 5's loop; note where the multi-middle-layer (k greater than 3)
  generalization changes the bookkeeping.
- **Section 5 (Experiments)** — CBank, CFD, injection protocol, FAUC and F1
  curves. Step 7 has the headline numbers; focus your reading on the injection
  methodology (what volume/density is injected where) since that defines what
  "detectable" means.
- **Conclusion** — short; read against Step 8 and ask what a temporal FlowScope
  would need.

## Questions to answer in notes.md

1. Why does a per-hop dense-block detector fundamentally miss a balanced laundering
   flow, even with perfect data — what property of g(S) captures the conjunctive
   signal that per-hop density cannot?
2. Walk one mule through the metric: with in=100, out=98, lambda=4, what is its
   contribution to the numerator of g, and how does adding 30 of camouflage inflow
   change it?
3. In the k=3 peel, exactly which heap keys must be updated when a *source* node is
   removed, and what graph structures do you need to make that update O(degree)?
4. FRAUDAR resists camouflage by column-degree weighting; FlowScope by the
   min/max metric. Which mechanism survives an adversary who can also *balance*
   camouflage (equal fake in and fake out per mule), and why?
5. What breaks if you run FlowScope on a 30-day window snapshot while the ring
   spreads transfers over 90 days — and what incremental state would a streaming
   variant need to maintain f_i and q_i under a sliding window?

## Done when

Answer each before unfolding it.

- [ ] You can write g(S) from memory and explain why parking and camouflage both
      lower it via q without touching f.

  <details><summary>Answer</summary>

  `g(S) = (1/|S|) Σ_{mules i} [(1+λ) f_i − λ q_i]`, with `f_i = min(inflow, outflow)`,
  `q_i = max(inflow, outflow)`, and λ = 4 (Eq. 4). Each mule contributes
  `(1+λ) f_i − λ q_i`. A balanced mule (in 100, out 98) contributes
  `5·98 − 4·100 = +90`.

  Parking money (in 100, out 10) gives f = 10, q = 100 →
  `5·10 − 4·100 = −350`. Camouflage (in 130, out 98) gives f = 98, q = 130 →
  `5·98 − 4·130 = −30`. Both raise q while f stays capped by the smaller side, so
  the penalty `−λ(q − f)` drags the contribution down — the metric is maximized
  exactly by dedicated, perfectly balanced mules, which is what a real ring looks
  like.

  </details>

- [ ] You can state the peel loop and the exact heap-key delta from fraudar.rs's
      bipartite version to the k=3 flow version (exercise 5).

  <details><summary>Answer</summary>

  The loop is FRAUDAR's near-greedy peel: start from all nodes, pop the
  minimum-key node, remove it, update its neighbors' keys, and track the best g
  seen. The delta is the key (Eq. 5): fraudar.rs keys every node by weighted
  degree, whereas the flow version keys a *middle* node by
  `f_i − (λ/(1+λ)) q_i` (proportional to its g-contribution, so the same peel
  order) and a *source/destination* node by its plain degree d_i.

  Removals now propagate through the coupling: peeling a source changes its
  mules' inflow, hence their f and q, hence their keys — a two-hop update the
  bipartite version never performs. That is the whole engineering delta exercise
  5 asks you to write as pseudocode.

  </details>

- [ ] You can quote the CBank sensitivity result (F1 at 0.9 or above down to $76M
      injected vs FRAUDAR's $180M) and say what the injection protocol measures.

  <details><summary>Answer</summary>

  FlowScope holds F1 ≥ 0.9 down to $76 million of injected laundering volume,
  where FRAUDAR needs $180 million (paper table, million $) — under half the
  volume. The injection protocol plants a synthetic ring of known A:M:C ratio
  (e.g. 7:5:3) and volume into the real transfer graph, then sweeps either the
  injected money volume or the injected account count and records the lowest
  setting at which F1 stays ≥ 0.9.

  It measures the faintest ring a detector can still recover against real
  background banking traffic — a sensitivity floor, not a headline accuracy.

  </details>

- [ ] You have a written position on the static-snapshot vs streaming-window gap
      for production AML.

  <details><summary>Answer</summary>

  The paper scores one static snapshot; production AML is streaming and
  temporal — transfers arrive in windows, rings persist for weeks. A streaming
  variant would maintain each mule's inflow/outflow (hence f_i and q_i)
  incrementally under a sliding window, re-keying only the mules touched by an
  arriving or expiring transfer, and re-peel incrementally instead of from
  scratch.

  The open question is bounding how far a single transfer can move g, so you know
  when a re-peel is actually needed. Your notes should take a position on window
  length versus ring lifetime — too short a window and a slow ring never
  accumulates detectable throughput.

  </details>

## References

- Li, X., Liu, S., Li, Z., Han, X., Shi, C., Hooi, B., Huang, H., Cheng, X.
  "FlowScope: Spotting Money Laundering Based on Graphs." AAAI 2020.
- Hooi, B., Song, H. A., Beutel, A., Shah, N., Shin, K., Faloutsos, C.
  "FRAUDAR: Bounding Graph Fraud in the Face of Camouflage." KDD 2016 — the
  predecessor whose peeling framework and guarantee style FlowScope inherits.
- Local: `topics/39-fraud-identity-graphs/experiments/src/fraudar.rs` — the
  bipartite greedy peel (lazy min-heap over weighted degrees) that exercise 5
  extends to the k=3 flow objective. There is no FlowScope stub module; the
  deliverable is the pseudocode delta described in exercise 5.
- Cross-topic: k-core decomposition and degree-ordered peeling in topic 18
  (`topics/18-gpu-graph-analytics/`), the same elimination family.
