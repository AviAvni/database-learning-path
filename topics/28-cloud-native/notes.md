# Topic 28 — Notes & measurements

Machine: Apple M3 Pro, macOS. `cargo run --release --bin tier_bench`
(4M keys / ~23.5K 4 KiB blocks; 200K zipf(0.99) point reads; latencies are
*simulated & charged*, not slept — deterministic under seeds).
Date: 2026-07-10.

## Measured baselines (provided lanes — the ladder, priced)

| lane | p50 | p95 | p99 | mean |
|---|---|---|---|---|
| local NVMe | 0.10 ms | 0.12 ms | 0.12 ms | 0.10 ms |
| raw S3 (no cache) | 14.17 ms | 27.18 ms | 112.99 ms | 17.07 ms |

140× at the median, **940× at p99** (2% stragglers dominate the tail).
That's the gap the whole topic exists to close.

## Predictions BEFORE implementing the stubs

| stub lane | prediction | reasoning |
|---|---|---|
| S3 + LRU cache (3000 blocks ≈ 1/8) | hit rate 65-80%; mean ~3-6 ms, ~3-5× vs raw S3; p50 collapses to 2 µs, p99 stays ~S3 p95 | zipf(0.99) mass concentrates hard; adjacent hot keys share blocks (170 keys/block) boosting hits; but p99 is governed by misses, which caching can't fix — only hedging can |
| hedged at p95 | p99 from ~113 ms to ~30-35 ms (>3×); hedge rate ≈ 5% | deadline = p95 by construction fires ~5%; a straggler's rescue costs p95 + fresh sample (median ~14 ms) ≈ 40 ms worst-typical |
| CoW branching | build ms-scale; tip reads < 1 µs/read despite 64-hop worst case | HashMap probe per hop; most pages resolve at ROOT after ~1 hop... no wait — pages 0-999 are re-written on EVERY branch, so hot pages resolve at the tip in 1 probe; the 64-hop walk only bites pages ≥ 1000. Expect bimodal cost hidden in the mean |

Honest flag: the LRU O(n) eviction scan (3000 entries × ~50-70K misses ≈
2×10⁸ scans) may make the cache lane's *wall* time visible even though
simulated latency is what's reported. If it does, that's the lesson: real
caches (quickwit's memory_sized_cache linked-LRU, S3-FIFO) exist because
eviction is on the miss path.

## Measured (stub lanes) — TODO after implementation

| lane | measured | prediction hit? |
|---|---|---|
| S3 + LRU cache | — | — |
| hedged GETs | — | — |
| CoW branching | — | — |

## Questions to answer while reading (from the guides)

- [ ] Aurora Q2: how monotonic LSN + VDL replaces 2PC across protection groups.
- [ ] Aurora Q4: what storage must support to apply graph deltas (compute-in-storage vs S3-behind-pageserver).
- [ ] Socrates Q2: map XLOG landing zone → topic 5 WAL lifecycle → Neon components.
- [ ] Socrates Q4: does M28 need a page-server tier, or is local-cache-over-S3 enough pre-replicas? (write the paragraph)
- [ ] Snowflake Q2: why SI over file lists suffices for a warehouse but not OLTP.
- [ ] S3'08 Q1: the three blockers and which got fixed (strong consistency 2020, CAS 2024) vs routed around (immutability).
- [ ] Neon Q2: state the branch-aware GC retain rule in one sentence.
- [ ] Neon Q4: when M28 branches need "image layers" (materialized matrix snapshots) to cap ancestor walks.
- [ ] SlateDB Q1: the durable-commit latency floor on S3-only, and why landing zones exist.
- [ ] SlateDB Q2: CAS fencing vs Raft leases — detection laziness as the price of lease-freedom.
- [ ] Quickwit Q4: what goes in a graph snapshot object's hotcache footer.

## Cross-topic threads

- Topic 4: Neon pageserver = LSM over (page, LSN); delta layers = level
  files, image layers = compaction output, GC = tombstone horizon. Fourth
  appearance of the shape (LSM, GIN pending list, arrangements, layer map).
- Topic 5: the WAL rule promoted to architecture — Aurora ships ONLY redo;
  walredo runs REDO on the read path; safekeepers/XLOG = the durable tail;
  SlateDB's AwaitDurable = fsync trade at 100 ms scale.
- Topic 6: local cache tier = the buffer pool reborn; same LRU-vs-scan
  questions, but a miss now costs 15 ms *and money*, so admission policy
  (quickwit split_cache) matters more than eviction.
- Topic 15: safekeeper quorum = Raft-shaped; SlateDB fencing epochs = CAS
  on S3 instead of leases — consensus outsourced to the store's
  conditional PUT.
- Topic 26: Snowflake prunes with min/max zone maps = BRIN's one-sided
  filter at cloud scale.
- Topic 27: log-is-the-database = Kafka's thesis (reading-kafka-log.md);
  Materialize persist = this topic applied to IVM state; tables/pages as
  "caches of log prefixes" is the same sentence in both.

## Capstone M-log (M28, per PLAN)

Target: tiered storage backend — hot data local, SSTs on object storage —
plus instant graph snapshots/branches.

- Tiering: the M4 LSM's levels ≥ L1 move to object storage as immutable
  SSTs; L0 + WAL stay local (the landing-zone lesson — never pay S3
  latency on the commit path). Local NVMe block cache in front, slatedb
  part-cache shaped, sized in blocks not objects.
- Manifest: single CAS-updated object listing live SSTs + epoch numbers;
  writer/compactor fencing exactly as slatedb (fence.rs:105). No lease
  service.
- Branching: snapshot = pin a manifest version (O(1)); branch = new
  manifest referencing parent SSTs + private delta chain. Read path =
  branch.rs's ancestry walk at SST-list granularity, NOT page granularity
  — graphs want whole-matrix versioning first (delta matrices already
  give per-tick versions, topic 27).
- The Neon trick deferred: materialize matrix snapshots into long-lived
  branches ("image layers") only when ancestor walks show up in profiles.
- Hedging: wrap the object-store client with a p95 deadline policy from
  day one (quickwit's citation: AWS recommends it) — it's ~30 lines and
  bounds the p99 story.
- NOT in v1: page-server tier (single writer + read-locally covers it
  until M15 replicas want GetPage@LSN semantics); compute-applied deltas
  in storage (Aurora Q4) — requires custom storage fleet.

## Infra notes

- Provided lanes always print; stub lanes are wrapped in catch_unwind and
  print `[stub — implement …]`.
- 6 provided tests pass (block layout roundtrip, S3 latency
  shape/determinism, tier gap, zipf skew, percentile edges, branching
  copies nothing); 13 stub tests fail as todo!() panics (cache 5, hedge 3,
  branch 5).
- All latency is virtual: `LatencyModel::sample_micros` charges cost;
  `Fixed` scripts latencies for exact hedge arithmetic tests
  (50ms primary / 1ms backup / 10ms deadline ⇒ 11ms — the contract).
- Zipf sampler precomputes a CDF (32 MB for 4M keys) — fine, one-time.
- `BranchStore` LSNs are globally monotonic across branches, so parent
  writes after a branch point are invisible to children by comparison
  alone — no per-branch clocks needed (single writer, the M28 luxury).

## Done when

- [ ] All 19 tests green (`cargo test --release`).
- [ ] tier_bench stub lanes filled in the table above; the p99-vs-p50
  split confirmed (cache fixes the median, hedging fixes the tail — say
  it from memory).
- [ ] Can draw the Neon data flow (compute / safekeepers / pageserver /
  S3) and name what each tier is durable for.
- [ ] One paragraph: why S3 conditional PUT (CAS) made lease-free
  single-writer databases possible, and where FalkorDB would use it.
- [ ] M28 design sketch reviewed against Socrates Q4 and Neon Q4 answers.
