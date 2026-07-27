# Topic 41 — On-Chain & Crypto Analytics

Fourth of six graph use-case deep dives: a public ledger is a graph
database nobody designed and everybody queries. It records
*transactions*, not people, so every question is an inference.
**Meiklejohn et al.** (IMC'13) gave the field its two clustering
heuristics — one keyed on the protocol, which never lies, and one keyed
on a habit, which fails catastrophically. **Anderson et al.**
(*Bitcoin Redux*, WEIS'18) showed that the industry-default taint rule
smears a single theft across 93% of all addresses, and that an 1816
English court case fixes it. **BlockSci** (USENIX Sec'20) is this
topic's database paper: append-only data means ACID is unnecessary,
which makes the right engine an in-memory analytical one — and its
Table 3 benchmarks that claim against Neo4j, Memgraph and RedisGraph.
**Weber et al.** (KDD'19) contribute the labelled data set, and the
uncomfortable finding that Random Forest beats the GCN.

## The problem, measured (bench lane 1, provided — runs today)

```
   20400 transactions, 40400 outputs, 30342 addresses, 400 entities;
   ONE stolen coinbase, worth 0.25% of all the money on the chain.

   haircut tainting, at the end of the chain:
     tainted UTXOs        3657 of   3734   (97.9%)
     tainted addresses    3553 of   3627   (98.0%)
     tainted value      1000000 of 400000000   (the theft was 1000000)
     of those UTXOs: 658 are <0.1% tainted, 2997 are 0.1-5%, 2 are >5%
```

Haircut does not invent money — the total is conserved to the satoshi.
It just spreads it so thin that "is this coin tainted?" stops meaning
anything: 98% of everybody is holding a trace, and two UTXOs in the
whole chain hold a share worth arguing about. Anderson et al. measured
the real version, tracing forward from real thefts to 2016:

| theft | haircut taints | FIFO taints |
|---|---|---|
| Linode 2012, 46,653 BTC | **16,855,619 addresses (93%)** | **245,120 (1.35%)** |
| Flexcoin 2014 | 10,421,112 (57%) | 15,265 |

A rule that taints 93% of everyone is a tax, not a forensic tool.

## Clayton's Case, 1816

```
   inputs                    FIFO outputs              haircut outputs
   ┌──────┐ clean  3         ┌──────┐ D: 3 clean       D: 2/9 stolen
   ├──────┤ STOLEN 2   ==>   ├──────┤ E: 2 STOLEN  vs  E: 2/9 stolen
   ├──────┤ clean  4         ├──────┤ F: 4 clean       F: 2/9 stolen
   └──────┘                  └──────┘                  (3 tainted, and
                             (1 tainted)                the next hop
                                                        multiplies again)
```

A bank failed and nobody could say which deposits had paid for which
withdrawals. The Master of the Rolls set first-in-first-out: withdrawals
are drawn against the earliest deposits. Applied to a transaction, lay
the input satoshis end to end and cut the outputs off the front in
order. The property that makes it work is that it is **lossless** — a
satoshi is stolen or it is not, so provenance survives arbitrarily many
hops, and taint can be traced *backwards* as well as forwards. Haircut
destroys that: after two hops every number is a fraction of a fraction.

Measured lane 2, same chain, same theft:

| policy | tainted UTXOs | tainted addrs | value flagged | vs stolen |
|---|---|---|---|---|
| poison | 3690 (98.8%) | 3585 | 394,674,821 | **394.67×** |
| haircut | 3657 (97.9%) | 3553 | 1,000,000 | 1.00× |
| **fifo** | **32 (0.9%)** | 32 | 1,000,000 | 1.00× |

Poison invents money: it re-counts each descendant output's full value,
so the "stolen" total explodes with the fan-out. Haircut conserves the
total but touches everything. FIFO conserves *and* concentrates — 32
UTXOs, one of which holds 22.5% of the flagged value — and it runs at
**3.1M transactions/s**, because the whole algorithm is a queue splice.

Two things follow that are not obvious. The legal principle `nemo dat
quod non habet` ("no one gives what they do not own") means a theft
victim can pursue stolen coins through however many hands; and because
every transaction is public, passing coins through a mixer puts every
later holder *on notice*. Anderson et al.'s conclusion: feed one black
coin and nine white ones into a laundry and you do not get ten white
coins, you get ten black ones. "People designing money laundering
mechanisms have been using quite the wrong metrics of quality."

## Two heuristics, opposite risk profiles

```
   Heuristic 1 — co-spend            Heuristic 2 — one-time change
   ────────────────────────          ─────────────────────────────
   inputs A, B in one tx             Def 4.3, ALL FOUR:
     ⟹ same signer holds              1. first appearance of pk
        both private keys             2. not a coin generation
   transitive ⟹ union-find            3. no output is also an input
   a PROTOCOL property:               4. EXACTLY ONE output meets (1)
     cannot be faked                 an IDIOM OF USE: can be wrong,
   12,056,684 keys → 5,579,176         and being wrong once is
     clusters (2013 chain)             permanent
```

Lane 3 sweeps how often a wallet reuses an address for change:

```
   change reuse   H1 clusters  H1 prec  H1 rec | H1+2 clusters  prec   rec   largest
           0.00          5638    1.000   0.041 |          2346  1.000 0.397      93 (1%)
           0.01          5477    1.000   0.044 |          2268  0.661 0.415     366 (3%)
           0.02          5458    1.000   0.044 |          2218  0.502 0.426     476 (4%)
           0.05          5098    1.000   0.054 |          1926  0.089 0.445    1894 (16%)
           0.10          4719    1.000   0.073 |          1639  0.009 0.559    7991 (71%)
           0.20          4128    1.000   0.106 |          1375  0.008 0.630    8275 (79%)
```

Heuristic 1 holds **precision 1.000 at every reuse rate** — you cannot
be co-spent with someone whose key you do not hold. Heuristic 2 buys
real recall (0.041 → 0.397) and one reused change address in a hundred
already costs a third of its precision; at one in twenty the largest
cluster is 16% of the chain, at one in ten it is 71%. Union-find makes
every false merge transitive and permanent, which is why a *safe*
heuristic with recall 0.04 can be worth more than an *effective* one
with recall 0.45.

Meiklejohn's own refined run — after excluding the Satoshi Dice payout
pattern and waiting a week before labelling, taking the false-positive
rate from 13% to **0.17%** — still produced a super-cluster of **1.6
million public keys containing Mt. Gox, Instawallet, BitPay and Silk
Road at once**. BlockSci, on the 2019 chain, reports 809 clusters over
20,000 addresses and one with **over 17 million**, and says plainly it
is "likely a result of such a collapse".

## BlockSci: why this is not a graph-database workload

```
   append-only data  ──▶  snapshots are static  ──▶  ACID is unnecessary
                                                       │
                                                       ▼
                                        in-memory ANALYTICAL database
                                                       │
        row-based flat file, memory-mapped ◀───────────┘
        inputs + outputs stored INLINE with the tx
          (19% duplication, bought for sequential locality)
        one writer (the parser) ⟹ zero-synchronisation parallel reads
```

BlockSci's Table 3, on 25 million transactions — the one benchmark in
this book where FalkorDB's own ancestor is a published baseline:

| query | BlockSci C++ (1T) | (MT) | Neo4j w/ index | RedisGraph | Memgraph |
|---|---|---|---|---|---|
| Tx locktime > 0 | 0.31 | 0.03 | 0.05 | 1.85 | 16.44 |
| Max output value | 0.46 | 0.03 | 24.55 | 4.48 | 40.08 |
| Calculate fee | 0.57 | 0.03 | 303.69 | did not finish | 187.02 |
| Zero-conf outputs | 5.47 | 0.32 | 207.41 | 1488.93 | 59.96 |
| Locktime change | 7.57 | 0.45 | 213.59 | did not finish | 122.98 |

Seconds. "2–16× compared to the best results for graph traversal, and
hundreds of times faster for many sequential queries." Read it as a
specification, not an insult: the queries BlockSci wins by 500× are
*full scans with a predicate*, and it wins them with a memory-mapped
columnar-ish layout and no transaction machinery. The lesson for M41 is
the one topic 12 and topic 32 keep making — a graph engine that cannot
also scan will lose these queries, and the fix is a storage layout, not
a better traversal.

Scale, for calibration: 489 million transactions / 1.198 billion inputs
/ 1.302 billion outputs as of Dec 2019 fit in **50.09 GB**; parsing
takes 5.5 hours, loading ~4 minutes, and a parallel pass over every
transaction takes **0.9 seconds** on 16 vCPUs.

## Production shape

| repo / anchor | what to see |
|---|---|
| [`~/repos/RustyTaintChain`](https://github.com/TaintChain/RustyTaintChain) `src/callbacks/bootstrap_taint_fifo.rs:52` | `TaintPart { name: u16, value: u64 }` — a run of same-provenance satoshis |
| `:142` `extract_taint` | Clayton's Case in fifteen lines: pop, split the straddling run, push the remainder back |
| `:174` `combine_taints` | merging two provenance queues, counting collisions between crime sources |
| `:250` `reduce_taint` | run-length coalescing, so a queue does not fragment forever |
| `:79` `TaintFifo` | the whole state: a UTXO map plus per-address taint queues |
| [`~/repos/BlockSci`](https://github.com/citp/BlockSci) | the parser, the memory-mapped transaction table, the union-find address linker |

## Reading guides

1. [reading-fistful-of-bitcoins.md](reading-fistful-of-bitcoins.md) — Meiklejohn IMC'13: the two heuristics, Definition 4.3, the false-positive ladder, the super-cluster.
2. [reading-bitcoin-redux.md](reading-bitcoin-redux.md) — Anderson et al. WEIS'18 with the RustyTaintChain code read: `nemo dat`, Clayton's Case, poison/haircut/FIFO.
3. [reading-blocksci.md](reading-blocksci.md) — BlockSci USENIX Sec'20: append-only ⟹ no ACID, the memory-mapped layout, and the graph-database comparison.
4. [reading-elliptic-aml.md](reading-elliptic-aml.md) — Weber et al. KDD'19: the Elliptic data set, why Random Forest beat the GCN, and the dark-market shutdown.

## Experiments

```
cd experiments
cargo test              # 3 provided tests pass; 10 fix the contract for your stubs
cargo run --release --bin chain_bench
```

- `chain.rs` (PROVIDED) — a synthetic UTXO chain *with ground truth*,
  which the real blockchain does not come with: `address_entity` maps
  every address to its controlling entity and one coinbase output is
  marked stolen. Co-spending, change addresses and address reuse are all
  planted, because they are what the heuristics key on. No fees, so
  taint conservation is exactly testable.
- `taint.rs` — `haircut` PROVIDED; `poison`, `extract_taint` and `fifo`
  are stubs. `extract_taint` is the interesting one and it is fifteen
  lines.
- `clustering.rs` — `UnionFind` and the pair-precision/recall scorer are
  PROVIDED; `multi_input_clusters`, `change_output` (Definition 4.3) and
  `full_clusters` are stubs.

Bench lanes: 1 = haircut diffusion (provided, above). 2 = the three
policies (reference: poison 394.67× / haircut 1.00× over 97.9% of UTXOs
/ FIFO 1.00× over 0.9%, at 3.1M tx/s). 3 = the clustering collapse curve
(reference: co-spend precision 1.000 throughout; change-heuristic
precision 1.000 → 0.089 → 0.009 and largest cluster 1% → 16% → 71% as
change reuse goes 0 → 5% → 10%).

## Exercises

1. Implement the stubs until all 13 tests pass and lanes 2–3 print.
2. **Two thieves.** `TaintPart` carries a `u16` name, not a boolean, for
   a reason. Plant a second theft, trace both, and implement
   RustyTaintChain's `combine_taints` collision counting: how often does
   one output hold satoshis from two different crimes, and what does
   that do to a blacklist's usability?
3. **Queue fragmentation.** Measure the length of the longest
   `VecDeque<TaintPart>` as the chain grows, then implement
   `reduce_taint` (coalesce adjacent runs with the same name) and
   measure again. Where would it grow without bound in a real chain, and
   what does that imply for storing taint as a graph property?
4. **Backwards.** FIFO is lossless, so provenance can be traced in
   reverse. Implement `origin_of(output, satoshi_offset)` returning the
   coinbase that minted it, and verify against the forward trace. Try
   the same with haircut and explain in two sentences why you cannot.
5. **Buy precision with latency.** Meiklejohn's false-positive rate goes
   13% → 1% → 0.28% → 0.17% by excluding a payout pattern and then
   waiting a day, then a week, before labelling a change address.
   Implement the delay: only apply Heuristic 2 to a transaction once `k`
   further transactions have been observed, and plot precision and
   recall against `k`. What is the exchange rate?
6. **Disable co-spending.** Set `max_inputs: 1` and re-run lane 3.
   Heuristic 1 now finds nothing. How much of the change heuristic's
   apparent value was actually Heuristic 1's, and what does that say
   about reporting the two together?
7. **The BlockSci layout.** Re-implement `Chain` with inputs and outputs
   stored inline in one flat `Vec<u8>` using BlockSci's Figure 2 record
   (32-bit ids, 60-bit values, 4-bit address types), memory-map it, and
   re-time lane 2. How much of BlockSci's advantage is the layout and
   how much is the absence of transaction machinery?

## Cross-topic threads

- **Topic 39 (fraud & identity graphs)**: address clustering *is*
  entity resolution — same union-find, same pair precision/recall, same
  "one false merge is permanent" hazard. The difference is that
  Fellegi–Sunter learns its weights from data while Heuristic 2 is
  hand-written, and lane 3 measures what that costs.
- **Topic 40 (attack graphs)**: both topics score a graph an adversary
  reads. There, camouflage; here, mixers and fresh addresses. The
  defensive answer is the same shape — pick a measure the adversary
  cannot move (co-spending; FIFO provenance) over one they can.
- **Topic 12 (columnar storage)**: BlockSci's 19%-duplication inline
  layout traded for sequential locality is the same argument as
  denormalizing a fact table, and its 500× win over graph databases on
  scan queries is the argument for topic 32's HTAP split.
- **Topic 1 (RUM conjecture)**: the taint policies are a RUM triangle in
  disguise — poison is cheap and useless, haircut is exact in total and
  useless in detail, FIFO costs a queue per UTXO and is the only one
  that answers the question.
- **Topic 25 (graph ML)**: Elliptic is the cautionary data set. Random
  Forest on hand-built aggregate features beats a GCN on the same graph
  (F1 0.796 vs 0.628), and every model collapses when the dark market
  shuts down at time step 43.
- **Topic 33 (temporal graphs)**: Elliptic's 49 time steps have *no
  edges between steps* — a modelling choice that deletes exactly the
  temporal structure topic 33 is about. Ask what it costs.

## Capstone M41 — provenance and identity on the Rust graph engine

- **FIFO taint as an incremental procedure** over M31's storage: a
  `VecDeque<TaintPart>` per UTXO in the property layer, spliced on each
  transaction write rather than recomputed, with run-length coalescing
  so queues stay bounded.
- **Address clustering as a maintained union-find**, cluster ids exposed
  as an index — the M39 machinery re-pointed at a different domain.
- **A BlockSci-shaped columnar transaction store** alongside the
  property graph, so the two layouts can be compared on the same
  queries.
- Deliverable numbers: FIFO throughput (tx/s) on a 10M-transaction
  synthetic vs `chain_bench` lane 2; incremental-vs-recompute cost per
  write; cluster-id lookup latency at 10M addresses; and the
  sequential-scan gap between the columnar store and the property graph
  on BlockSci's Table 3 queries.
