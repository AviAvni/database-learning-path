# Topic 40 notes — Security & attack graphs

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| lane 1: `MemberOf` closure to tier zero | 5 direct + a nested group | **8**, and it never moves |
| lane 1: attack-path reachable, no session data | tens (only the over-privileged group) | **39 of 2000 (1.9%)** |
| lane 1: attack-path reachable, 100 sessions | a big jump, maybe half | **1969 (98.5%)** — the cascade is near-total |
| lane 1: sessions needed to saturate | thousands | **500** (2000 users, 100.0%) |
| lane 1: two DA tokens on workstations | a few hundred users | **8 → 2000** with one token |
| lane 1: shortest attack-path length | 3–4 hops | **mean 6.03, worst 8** |
| lane 2: dominator pass vs \|V\| re-runs, 3400 nodes | 100×+ | (stub — reference: **0.8 ms vs 543 ms ≈ 600×**, exact match on every node) |
| lane 2: tiered directory, top choke point | most of the exposure | (stub — reference: **1992 of 2000 users, 99.6%**; greedy 2000 → 8 → 5) |
| lane 2: flat directory, top choke point | smaller, still positive | (stub — reference: **none — every single-node blast radius is 0**) |
| lane 2: flat directory, planned gateway cut | monotone decline | (stub — reference: **2000 → 2000 → 2000 → 2000 → 2000 → 8** — nothing until the last piece) |
| lane 3: pointer-chase tuple reads, depth 2 → 32 | linear in depth×width | (stub — reference: **19 → 559**, 0.46 → 11.28 µs) |
| lane 3: Leopard probes, depth 2 → 32 | flat | (stub — reference: **4 → 12 probes, ~0.01 µs** — flat in depth) |
| lane 3: index space tax at depth 32 | 2–3× | (stub — reference: 6672 tuples → **11393 entries = 1.7×**, closure quadratic in depth) |
| lane 3: galloping vs merge, lopsided sets | 100× | (stub — reference: **>1000×** on a 1-vs-500,000 pair) |

Two mechanics worth memorizing.

**Exposure is a collection-time measurement, not a privilege
measurement.** Twenty over-privileged users expose 39 people with no
session data and 1969 with a hundred sessions, because each newly
exposed user's own sessions drag in everyone who is local admin on
those machines, and that cascades. Any report of "% of principals with
a path to Tier Zero" is meaningless without the collection window
attached.

**Tiering is what makes a graph have choke points.** The tiered and
flat directories in lane 2 have *identical* exposure (2000 users). The
tiered one has a group whose removal frees 1992 of them; the flat one
has no node whose removal frees a single user, and cutting the whole
gateway set one node at a time shows no progress at all until the last
piece lands. Remediation is a set problem on a flat directory, and the
dominator pass returning all zeros is the report — not a failure of the
analysis.

## Guide-question checklist

- [ ] reading-bloodhound.md Q1–Q5
- [ ] reading-attack-graph-monotonicity.md Q1–Q5
- [ ] reading-zanzibar.md Q1–Q5
- [ ] reading-sleuth.md Q1–Q5

## Paper numbers worth keeping

| fact | source |
|---|---|
| 5 hosts / 8 exploits → 5,948 nodes, 68,364 edges, 2 h, 229-bit state space | Ammann CCS'02 §1 (quoting Sheyner) |
| monotone encoding of the same problem: **at most 229 nodes** | Ammann CCS'02 §1 |
| `markAttributes` is O(\|A\|²·\|E\|), ≤ \|A\| layers | Ammann CCS'02 §2.1 |
| finding a *minimum* attack is NP-complete; *minimal* is easy | Ammann CCS'02 §2.2 |
| 3-host example: 60 attributes, 30 instantiated exploits, only 8 attributes ever change | Ammann CCS'02 §3 |
| Sheyner's tool: 10 hosts × 5 vulns → ~15 min, **10 million edges** | MulVAL CCS'06 §1 |
| MulVAL: O(N²) derivation steps, O(N²) graph, O(N² log N) build; **1000 hosts** on a Pentium 4 | MulVAL CCS'06 §5–6 |
| Zanzibar: **>2 trillion tuples**, ~100 TB, >1,500 namespaces, median namespace ~15,000 tuples | Zanzibar §4 |
| >10M client QPS; Check peak **4.2M QPS**, Read 8.2M, Expand 760K, Write 25K | Zanzibar §4 |
| Check Safe p50/p95/p99 = **3.0 / 9.46 / 15.0 ms**; Recent 2.86 / 60.0 / 76.3; Write 127 / 233 / 401 | Zanzibar Table 2 |
| >99.999% availability for 3 years = **<2 min global downtime per quarter** | Zanzibar §4.3 |
| Leopard: **1.56M QPS median**, <150 µs median / <1 ms p99; incremental layer ~500 updates/s | Zanzibar §4.4 |
| Check cache: **10% hit** on the delegate + 12% from the lock table — prevents **500K RPC/s** of hot spots | Zanzibar §4.4 |
| Timestamp quantization to **1 or 10 s** so cache keys collide | Zanzibar §3.2.5 |
| One tuple change can yield **tens of thousands** of Leopard index events | Zanzibar §3.2.4 |
| SLEUTH: **<10 bytes/event** vs ~250 B/edge (Neo4j-class) and ~3 KB (STINGER/NetworkX) | SLEUTH §2 |
| 38M events in **329 MB**; 79 h of audit data in **14 s** at 84 MB | SLEUTH §1.1, §6.7 |
| L-2: **38.5M events → 130** in the scenario graph (297,100× total reduction) | SLEUTH Table 11 |
| split code/data t-tags: **1305×** vs 4.68× for a single t-tag (forward analysis) | SLEUTH Table 11 |
| 174 entities correctly identified, **0 incorrectly, 2 missed** across 8 campaigns | SLEUTH Table 7 |
| >99.9% of audit events were benign activity | SLEUTH §6.3 |
| BloodHound: **104** node/edge kinds, **63** traversable, **31** derived by post-processing | `graphschema/ad/ad.go` |

## Cross-topic threads (worked)

- **Topic 26/23 ↔ 40**: BloodHound holds principal sets as roaring
  bitmaps (`cardinality.Duplex[uint64]`, `analysis/ad/post.go:244`) and
  uses `CheckedAdd` on a thread-safe one as a parallel BFS visited set
  (`membership.go:81`). Leopard's membership test is the galloping
  intersect from the roaring guide. Search postings lists and
  authorization sets are the same structure with different labels.
- **Topic 27 ↔ 40**: two independent instances of the
  recompute-vs-maintain question. BloodHound's 31 derived edge kinds
  are a materialized view refreshed in bulk after every ingest
  (`analysis.go:346`); Leopard's closure is a materialized view
  refreshed offline plus a Watch-fed incremental layer. And MulVAL's
  attack graph literally *is* the derivation graph of a tabled Datalog
  query — semi-naive evaluation with provenance.
- **Topic 1 ↔ 40**: lane 3 is a RUM triangle. Reads go from 559 tuple
  reads to 12 probes; the cost is 1.7× space growing quadratically in
  nesting depth, plus a maintenance path where one edge change can
  cascade into tens of thousands of index updates.
- **Topic 37 ↔ 40**: SpiceDB's dispatcher is topic 37's scatter-gather
  with a concurrency bound (`defaultConcurrencyLimit = 50`), a cache in
  front keyed on a canonicalized expression, and a singleflight lock
  table. Zanzibar hedges to Spanner and Leopard but explicitly *not*
  between its own servers — hedging the expensive checks would make the
  tail worse, which is the sharpest caveat on topic 37's hedging story.
- **Topic 18 ↔ 40**: the dominator pass and the reachability sweep are
  CSR traversals; M40's choke-point procedure reads the same CSR as
  M39's peel.
- **Topic 12 ↔ 40**: SLEUTH's <10 bytes/event encoding is the columnar
  argument — variable-length domain-specific encodings, delta
  timestamps, dictionary-narrow identifiers — arriving at 25–300×
  against a general-purpose graph store.
- **Topic 33 ↔ 40**: a provenance graph is a contact sequence, and a
  causal dependency is a time-respecting path. SLEUTH's backward
  analysis is a temporal reachability query with tag-derived costs.
- **Topic 39 ↔ 40**: both topics score a graph against an adversary who
  reads the score. FRAUDAR's answer is a metric camouflage cannot move;
  the attack-graph answer is that you cannot lower exposure by scoring
  harder, only by removing edges — and lane 2 shows the flat directory
  where even that has no single-node move.

## Open questions

- Lane 2 prices *node* removals exactly. Edge removals are the actual
  remediation ("remove this group from that group"). Exercise 2 does it
  by edge subdivision — does that stay exact, and what does it cost?
- The flat directory has no single-node choke point. Is finding the
  minimum *set* whose removal drops exposure below a threshold the
  NP-complete problem Ammann's `findMinimal` sidesteps, or a different
  one?
- Leopard cannot precompute a closure when membership is conditional.
  SpiceDB's caveats make exactly that situation ordinary
  (`membershipset.go` combines caveat expressions, not booleans). What
  is the right index for conditional reachability?
