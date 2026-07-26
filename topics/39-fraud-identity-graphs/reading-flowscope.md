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

The optimizer is FRAUDAR's near-greedy peel: start from the full graph, repeatedly
remove the node whose removal hurts g least, remember the best S seen, return it.
A priority queue keyed on each node's marginal contribution to g makes the whole
loop near-linear in edges, and the paper carries over a FRAUDAR-style approximation
guarantee for the returned subgraph. The delta from FRAUDAR: a middle node's key is
its (1+lambda) f_i − lambda q_i term, and peeling a source or destination changes
the inflow/outflow — hence f and q, hence the keys — of its middle-layer neighbors.

```
  while nodes remain:
      v = pop-min(heap)               # least marginal contribution to g
      remove v; for each middle-layer neighbor m of v:
          recompute in(m), out(m) -> f_m, q_m -> update key(m)
      if g(current S) > g(best): best = current S
  return best
```

### Step 6 — Why camouflage is self-defeating here

FRAUDAR resists camouflage via column weighting: camo edges land on honest
high-degree columns and earn little. FlowScope needs no column weights at all.
Any extra transfer a launderer adds to look normal lands on one side of some
mule's ledger, raising that mule's q while leaving f untouched — and the
lambda-weighted penalty drags g down. The attacker's only safe strategy is to keep
every mule perfectly balanced and dedicated, which is exactly the pattern the
metric is maximized by, i.e. exactly what gets detected. Compare: same adversarial
robustness goal, two very different mechanisms (reweighting vs metric shape).

### Step 7 — Evidence: CBank and CFD

CBank is a real bank dataset: 6.13M accounts and 43.98M transfer records, with a
labeled real laundering ring — 4 sources, 12 mules, 2 destinations moving about
452M yuan. On the two CBank injection settings FlowScope scores FAUC 0.761 and
0.843 versus FRAUDAR's 0.529 and 0.704. In injection experiments FlowScope holds
F1 at 0.9 or above down to injected laundering volumes of 76M yuan, where FRAUDAR
needs 180M — FlowScope detects laundering at less than half the volume. On the
Czech Financial Dataset (CFD) it reaches FAUC 0.970 and 0.900. The practical
reading: the flow objective buys sensitivity, not just elegance.

### Step 8 — Database-engineer lens: peeling, k-core, and streaming

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

- [ ] You can write g(S) from memory and explain why parking and camouflage both
      lower it via q without touching f.
- [ ] You can state the peel loop and the exact heap-key delta from fraudar.rs's
      bipartite version to the k=3 flow version (exercise 5).
- [ ] You can quote the CBank sensitivity result (F1 at 0.9 or above down to 76M
      yuan vs FRAUDAR's 180M) and say what the injection protocol measures.
- [ ] You have a written position on the static-snapshot vs streaming-window gap
      for production AML.

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
