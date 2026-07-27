# Topic 41 notes — On-chain & crypto analytics

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| lane 1: addresses vs entities | 10–50× | **30,342 addresses for 400 entities (76×)** |
| lane 1: haircut-tainted UTXOs from one theft | most of them | **3657 of 3734 (97.9%)** |
| lane 1: haircut-tainted addresses | ~same | **3553 of 3627 (98.0%)** |
| lane 1: how much taint is meaningful | thin | 658 UTXOs <0.1%, 2997 at 0.1–5%, **only 2 above 5%** |
| lane 2: poison total vs stolen | 10–50× | (stub — reference: **394.67×**) |
| lane 2: haircut total vs stolen | 1.00× (conserved) | (stub — reference: **1.00×**, over 97.9% of UTXOs) |
| lane 2: FIFO total vs stolen | 1.00× | (stub — reference: **1.00×**, over **0.9%** of UTXOs — 32 of 3734) |
| lane 2: FIFO concentration | most in a few | (stub — reference: top UTXO holds **22.5%** of flagged value) |
| lane 2: FIFO throughput | ~1M tx/s | (stub — reference: 20,400 tx in **6.6 ms = 3.1M tx/s**) |
| lane 3: co-spend precision | 1.000 (protocol property) | (stub — reference: **1.000 at every reuse rate**) |
| lane 3: co-spend recall | low | (stub — reference: **0.041**; 12,186 addrs → 5,638 clusters ≈ 2.2 addrs/cluster) |
| lane 3: change heuristic recall gain | +0.2? | (stub — reference: 0.041 → **0.397**) |
| lane 3: precision at 1% change reuse | small dent | (stub — reference: **1.000 → 0.661**) |
| lane 3: precision at 5% / 10% | collapse | (stub — reference: **0.089 / 0.009**) |
| lane 3: largest cluster at 5% / 10% | big | (stub — reference: **1894 (16%) / 7991 (71%)** of all addresses) |

Two mechanics worth memorizing.

**FIFO is lossless, and that is the whole argument.** Haircut and FIFO
both conserve the stolen total exactly — neither invents money. The
difference is that haircut turns "stolen" into a real number between 0
and 1 that gets multiplied at every merge, so after two hops nothing can
be reversed and everybody holds a trace. FIFO keeps a satoshi stolen or
not-stolen, so provenance survives arbitrarily many hops *and can be
traced backwards*. Same conservation law, 114× narrower answer
(32 UTXOs vs 3657), and the real chain agrees: Linode taints 1.35% of
addresses under FIFO and 93% under haircut.

**A false merge is permanent; a missed merge is not.** Co-spending is a
property of the protocol — you cannot be co-spent with someone whose key
you do not hold — so Heuristic 1 has precision 1.000 by construction and
keeps it at every parameter setting. The change heuristic keys on a
habit, so it can be wrong, and because union-find is transitive one
error fuses two whole components. That asymmetry is why recall 0.041 at
precision 1.000 can be worth more than recall 0.397 at precision 0.089.

## Guide-question checklist

- [ ] reading-fistful-of-bitcoins.md Q1–Q5
- [ ] reading-bitcoin-redux.md Q1–Q5
- [ ] reading-blocksci.md Q1–Q5
- [ ] reading-elliptic-aml.md Q1–Q5

## Paper numbers worth keeping

| fact | source |
|---|---|
| 2013 chain: 231,207 blocks, 16,086,073 txs, **12,056,684 distinct public keys** | Meiklejohn §2.3 |
| Heuristic 1: 12,056,684 keys → **5,579,176 clusters** (≤ 6,595,564 users, "a large upper bound") | Meiklejohn §4.3 |
| Heuristic 2 false positives: **13% → 1% → 0.28% (a day) → 0.17% / 7,382 (a week)** | Meiklejohn §4.5 |
| refined run still yields a **1.6M-key super-cluster** with Mt. Gox + Instawallet + BitPay + Silk Road | Meiklejohn §4.5 |
| refined Heuristic 2: 3,384,179 clusters; 2,197 named, covering >1.8M addresses = **1,600× manual tagging** | Meiklejohn §4.5 |
| 23% of transactions used a self-change address (why Definition 4.3 condition 3 exists) | Meiklejohn §4.3 |
| Satoshi Dice was ~**60% of all Bitcoin activity**; 21% of bets (896,864 / 4,127,979) at the 0.01 BTC minimum | Meiklejohn §5.1 |
| Linode 2012 (46,653 BTC): haircut taints **16,855,619 addresses (93%)**, FIFO **245,120 (1.35%)** | Bitcoin Redux §3.3 |
| Flexcoin 2014: haircut **10,421,112 (57%)**, FIFO **15,265** | Bitcoin Redux §3.3 |
| Mt. Gox lost **744,000 BTC**; 6–9% of all issued bitcoin stolen at least once; 10–20% by value may be crime proceeds | Bitcoin Redux §2.1, §2.2 |
| Clayton's Case (1816): withdrawals drawn against earliest deposits; FIFO taint is **lossless**, so traceable backwards | Bitcoin Redux §3.2–3.3 |
| "one black coin and nine white coins into a laundry isn't ten white coins, but ten black ones" | Bitcoin Redux §2.2 |
| Bitcoin Dec 2019: **489M txs / 1.198B inputs / 1.302B outputs in 50.09 GB**; 260 GB on disk | BlockSci §2.6.5, §1 |
| inline input/output layout costs **19% space**, bought for sequential locality | BlockSci §2.3 |
| **88% of inputs** spend outputs from the last 4000 blocks; **8.6% of addresses** used >once = **51% of occurrences** | BlockSci §2.2 |
| parse 5.5 h, load ~4 min, **parallel pass over every tx = 0.9 s** on 16 vCPUs | BlockSci §2.6 |
| clustering: **474M clusters**, 380M singletons, 809 over 20k addresses, **one supercluster >17M** | BlockSci §2.4 |
| Table 3 (25M txs): BlockSci 0.57 s vs Neo4j 303.69 / RedisGraph DNF / Memgraph 187.02 on `calculate fee` | BlockSci Table 3 |
| fluent DSL is **7–11× faster** than the helper method, **3–5× slower** than hand C++ | BlockSci §2.5, Table 2 |
| Elliptic: **203,769 nodes / 234,355 edges / 166 features**; 2% illicit, 21% licit, 49 time steps, **no cross-step edges** | Weber §2.1 |
| **Random Forest illicit F1 0.788 (0.796 with GCN embeddings) beats GCN 0.628**, Skip-GCN 0.705, EvolveGCN 0.720 | Weber Table 1–2 |
| the dark market shutdown at time step 43 breaks every model, **even retrained each step** | Weber §4 |

## Cross-topic threads (worked)

- **Topic 39 ↔ 41**: address clustering *is* entity resolution. Same
  union-find, same pair precision/recall scorer, same "one false merge
  is permanent" hazard. The difference is that Fellegi–Sunter *learns*
  its weights from unlabelled data with EM while Definition 4.3 is four
  hand-written conditions — and lane 3 measures exactly what the
  hand-written version costs when its assumption (fresh change address)
  is violated 5% of the time.
- **Topic 40 ↔ 41**: both score a graph an adversary reads. FRAUDAR
  (39) answered camouflage by picking a metric the fraudster cannot
  move; here the equivalent move is preferring co-spending (protocol) to
  change detection (habit), and FIFO (lossless) to haircut (diffusing).
  Elliptic shows what happens when there is no such move available: the
  dark market shuts and every model dies.
- **Topic 12 ↔ 41**: BlockSci's inline layout with 19% duplication,
  32-bit ids and 60-bit packed values is the columnar argument. Its
  500× win over graph databases on `calculate fee` is a *scan* win, not
  a traversal win — read Table 3 by row and the losses are all full
  scans with arithmetic.
- **Topic 32 ↔ 41**: which makes Table 3 an HTAP brief. A graph engine
  keeps the point lookups (Neo4j-with-index beats single-threaded
  BlockSci on `Tx locktime > 0`) and needs a columnar side-store for
  the scans. That is M41's third deliverable.
- **Topic 1 ↔ 41**: the three taint policies are a RUM triangle. Poison
  is O(1) state and useless; haircut is one float per output and exact
  in total, useless in detail; FIFO is a queue per UTXO — the only one
  that answers the question, and the only one with an unbounded space
  term (hence `reduce_taint`).
- **Topic 33 ↔ 41**: Elliptic's 49 time steps have no edges between
  them, which deletes time-respecting paths by construction. EvolveGCN's
  small gain over GCN is the direct consequence.
- **Topic 25 ↔ 41**: the cautionary result. A hand-built one-hop
  aggregation fed to Random Forest beat a learned two-layer GCN on the
  same graph. Build the aggregates and measure before reaching for a GNN
  — and note that the *best* configuration used the GCN as a feature
  extractor rather than a classifier.
- **Topic 36 ↔ 41**: BlockSci's "infinite COST" conjecture rests on
  blockchain data being graph-structured and therefore hard to
  partition. That is topic 36's vertex-cut problem, used as an argument
  for scaling vertically instead.

## Open questions

- `reduce_taint` bounds queue fragmentation by coalescing adjacent
  same-name runs, but a chain that alternates provenance at every hop
  defeats it. What is the actual worst case on a real chain, and does it
  make FIFO taint storable as a graph property or not? (Exercise 3.)
- Meiklejohn buys precision with latency (13% → 0.17% by waiting a
  week). Is there an online version — a confidence that decays as
  evidence arrives — or is the delay fundamental? (Exercise 5.)
- The Elliptic labels were produced partly by address clustering, and
  lane 3 shows clustering can collapse. Nobody in the literature seems
  to have measured how much label noise that introduces.
- Bitcoin Redux's own conclusion is that most victim funds never touched
  the chain at all (hosted wallets, off-chain settlement). What fraction
  of on-chain analysis conclusions survive that, and is there a
  published estimate?
