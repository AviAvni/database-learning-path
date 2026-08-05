# Topic 36 notes — sharding, partitioning & rebalancing

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| mod-N movement 4→5 | 80% (closed form N/(N+1)) | **80.0% exactly** (identity keys, 100k, multiple of 20) |
| mod-N movement 16→17 | 94.1% | **94.1%** (hashed keys, 1M) |
| Zipf(1.0) hottest of 16 hash shards | rank-0 key ≈ 1/H₁₀₀₀₀ ≈ 10.2% + shard's uniform share | **14.7%** (2.4× the 6.25% ideal) |
| Zipf(1.2) hottest shard | worse | **23.8%** (3.8× ideal) |
| lane 2: ring 4→5 movement, 128 vnodes | ≈ 20% | (stub — implement hashring.rs) |
| lane 3: greedy vs random edge-cut, k=8 community | random 87.5%, greedy well under 52.5% | (stub — implement partitioner.rs) |

The lane-1 mechanics, worth memorizing: `k mod N == k mod N+1` iff
`k mod N(N+1)` is below N (CRT), so mod-N growth moves N/(N+1) of keys
— 80% at 4→5, 94% at 16→17, *worse as you grow*. Consistent hashing is
the mirror image: 1/(N+1), because only the arcs claimed by the new
node's points change owners. And the Zipf row is a proof, not an
accident: a hash function maps one key to one shard, so no hash choice
can spread the rank-0 key's ~10% of traffic. Splitting *between* keys
(range partitioning) or replicating the hot key are the only outs.

## Guide-question checklist

- [ ] reading-dynamo.md Q1–Q5
- [ ] reading-powergraph.md Q1–Q5
- [ ] reading-redis-cluster.md Q1–Q5
- [ ] reading-cockroach-rebalancing.md Q1–Q5

## Cross-topic threads (worked)

- Topic 35 ↔ 36: a rebalance is offered load — an unthrottled slot
  migration is a metastable trigger. Cockroach paces allocator actions;
  M36's migration runs at the admission layer's lowest priority.
- Topic 29 ↔ 36: hash tags are the placement-side answer to cross-shard
  transaction cost — co-locate what commits together.
- Topic 28 ↔ 36: Dynamo strategy 3 (fixed partitions, transfer as
  files) = the same design pressure that makes LSM tiering ship sealed
  SSTs to object storage.

## Capstone M36 log

- Surface: `slot = hash(vertex_key) & 0x3FFF` with hash tags; edges
  stored with source vertex; MOVED/ASK-style redirects; per-slot
  migration state machine, dual-routing during moves.
- Targets: movement ≈ 1/(N+1) on growth; edge-cut and replication
  factor beat random placement on power-law; p99 during live migration
  within 2× steady state.
- Order of work: slot map + redirect protocol first, then migration
  state machine, then greedy placement for high-degree vertices.

## Infra notes

- No new clones: redis and cockroach already under ~/repos.
- Redis anchors verified by grep this session: cluster.h:23
  (CLUSTER_SLOTS = 1<<14), :59 (keyHashSlot, hash-tag carve-out);
  cluster.c:35 (patternHashSlot), :1191 (getNodeByQuery), :1397
  (CLUSTER_REDIR_ASK), :1432 (CLUSTER_REDIR_MOVED), :1443
  (clusterRedirectClient), :1680 (askingCommand); cluster_legacy.h:343/:344
  (migrating_slots_to / importing_slots_from); cluster_legacy.c:6072-6075
  (SETSLOT MIGRATING/IMPORTING/STABLE/NODE).
- Cockroach anchors verified: zonepb/zone.go:257 (RangeMaxBytes 512<<20);
  replica_split_load.go:34 (SplitByLoadQPSThreshold 2500), :52
  (SplitByLoadCPUThreshold 500ms); split_queue.go:145
  (shouldSplitRange), :194 (shouldQueue); merge_queue.go:138
  (shouldQueue); split/decider.go:155 (Decider), :222 (Record), :329
  (RecordMax); allocatorimpl/allocator.go:125-127 (AllocatorAction);
  store_rebalancer.go:114 (StoreRebalancer), :218 (RebalanceMode).
- Papers verified from PDFs: Dynamo (SOSP'07, `.cache/papers/dynamo-sosp07.txt`, §4 +
  §6.1-6.3) — MD5→128-bit ring, tokens/vnodes, N/R/W with common
  (3,2,2), sloppy quorum + hinted handoff, per-range Merkle trees,
  strategies 1/2/3 (strategy-1 bootstrap "almost a day", strategy 3 =
  Q/S tokens, metadata 3 orders smaller, partition-as-file), imbalance
  ratio 20% low load vs 10% high (15% threshold), 99.94% of reads see
  one version; PowerGraph (OSDI'12, `.cache/papers/powergraph-osdi12.txt`,
  pp. 1-8) — GAS, α≈2 natural graphs (the paper assigns no numeric α
  to any real graph; 1.65/1.7/1.8/2.0 are Fig 6 synthetic-curve
  labels), Twitter's in-degree tail heavier than its out-degree
  (Fig 1), 1% of vertices ~
  half the edges, Thm 5.1 (random cut 1−1/p), Thm 5.2 (replication
  from degree distribution), Thm 5.3 (vertex-cut ≤ ghosts of any
  edge-cut), greedy Cases 1-4, coordinated vs oblivious, Table 1
  (Twitter 41M/1.4B α=1.8).
- Crate: 4 provided tests green (placement.rs — exact 80%, hashed
  ~80%, Zipf hot shard; graphs.rs — random cut ≈ 0.875 = Thm 5.1).
  6 stub tests fix contracts for hashring.rs (3) and partitioner.rs (3).
  Lanes 2-3 print `[stub …]` banners via catch_unwind until implemented.
  Zero warnings on the pristine crate.

## Done when

- [ ] All 10 tests pass; lanes 2-3 print real numbers.
- [ ] Exercise 2 proof written (CRT argument) and checked against the
      bench for N = 4, 5, 8, 16.
- [ ] Vnode-balance table extended (exercise 3): vnodes for
      max/mean ≤ 1.05 recorded here.
- [ ] Hot-key salting experiment (exercise 4): share vs R table.
- [ ] Edge-cut vs replication comparison (exercise 5) with the
      Thm 5.2 computation on the generated degree sequence.
- [ ] All 20 guide questions answered in writing.
- [ ] M36 sketch (exercise 6) upgraded to a design note: unit of
      migration chosen and justified.
