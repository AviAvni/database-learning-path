# Topic 39 notes — Fraud rings & identity graphs

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| lane 1: degree-rank at camo 0 | ~0 (power users out-review a 100-edge bot) | **0.00** (rises with camo: 0.28/0.60/0.76 at 0.5/1/2) |
| lane 1: obscurity-rank at camo 0 / 2 | good, then dead (row score) | **0.52 → 0.00** (dead already at camo 0.5) |
| lane 2: log-weighted F under camo | 1.0 everywhere (Theorem 3) | (stub — reference: **1.00/1.00/1.00/1.00** at camo 0/0.5/1/2) |
| lane 2: unweighted F at camo 2 | collapses into popular core | (stub — reference: **0.65**; degrades 1.00/0.95/0.69/0.65) |
| lane 2: peel 100k×50k, ~1M edges | sub-second (O(E log V)) | (stub — reference: **1,019,984 edges in ~0.2 s**, F = 1.00) |
| lane 3: u from random pairs | ≈ 1/pool = [.005 .002 .0003 .005 .0005] | (stub — reference: **[0.0052 0.0021 0.0003 0.0051 0.0006]**) |
| lane 3: EM m vs analytic (1−t)² | [0.81 0.87 0.94 0.77 0.90] | (stub — reference: **[0.80 0.86 0.94 0.78 0.90]**, p = 0.184) |
| lane 3: blocking savings, 15k records | ~100× | (stub — reference: 112,492,500 → 271,012 = **415×**) |
| lane 3: link at 12 bits | precision ≥ 0.95, recall ≥ 0.9 | (stub — reference: **0.989 / 0.992**, 48 ms end-to-end) |

The lane-1 mechanic, worth memorizing: degree-rank and obscurity-rank
are both functions of the fraudster's own *row*, and he controls his
row — at camo ≈ 0.5/fraud-edge he slips between them (0.28 and 0.00).
FRAUDAR scores *columns*: camouflage lands on honest popular columns
(weight ~1/log of a big degree ≈ 0) and the fraud block's own columns
never receive camouflage, so g(block) is byte-identical at camo 0 and
camo 2 — Theorem 3, verified by exercise 2's hand-derivation.

## Guide-question checklist

- [ ] reading-fraudar.md Q1–Q5
- [ ] reading-fellegi-sunter.md Q1–Q5
- [ ] reading-splink.md Q1–Q5
- [ ] reading-flowscope.md Q1–Q5

## Cross-topic threads (worked)

- Topic 18 ↔ 39: greedy peeling is degree-ordered vertex elimination —
  the same schedule as k-core decomposition; the lazy min-heap is a CPU
  stand-in for topic 18's bucketed frontier, and M39 reads weighted
  degrees straight off M31's CSR.
- Topic 36 ↔ 39: blocking keys are hash-partition keys. Two blocking
  passes = two shuffles; splink literally runs them as SQL self-joins
  on Spark (`blocking.py:747`), so the 415× table is a
  data-movement number, not just a comparison count.
- Topic 35/32 ↔ 39: the FRAUDAR peel is an offline batch scan (0.2 s
  per million edges, hours at Twitter scale); the identity lookup —
  cluster id per incoming record — is the online path that must stay in
  single-digit ms. Same OLAP/OLTP split as topic 32's HTAP.
- Topic 38 ↔ 39: GraphRAG-SDK's resolution stack (exact → fuzzy →
  embedding → LLM) is a Fellegi–Sunter decision rule with fancier
  comparators — each stage is a graded agreement level feeding a
  learned match weight.

## Capstone M39 log

- Surface: dense-block scan as a graph procedure over M31's storage
  (weighted degrees from topic-18 CSR, lazy-heap peel, returns
  (users, objects, g)); identity resolution at write time (blocking
  keys as indexed properties, FS match weights in the property layer,
  incremental union-find cluster ids on insert).
- Targets: peel throughput on a 10M-edge synthetic vs fraud_bench
  lane 2 (~5M edges/s); per-insert resolution latency at 1M records
  with two blocking indexes; precision/recall vs lane 3's 0.989/0.992.
- Order of work: peel procedure first (pure read path over CSR), then
  blocking indexes, then incremental union-find (touches write path).

## Infra notes

- Papers read in full from PDFs: /tmp/fraudar.pdf (KDD'16),
  /tmp/flowscope.pdf (AAAI'20), /tmp/winkler-rl.pdf (Winkler 2006
  survey, pp. 1–22).
- FRAUDAR facts: metric family g(S) = f(S)/|S|, f sums in-block edge
  weights; column weight 1/log(d_j + c), c = 5. Axioms 1–4 (node
  suspiciousness, edge suspiciousness, size, concentration). Greedy peel =
  "exonerate the least suspicious," O(|E| log |V|) with a priority
  tree. Theorem 2: g(returned) ≥ g_OPT/2. Theorem 3: column weights
  are camouflage-resistant — camo edges land on honest columns, the
  block's own edges and column degrees never change. Paper numbers:
  F > 0.95 injecting 200×200 blocks into real review graphs under all
  four camouflage attacks; Twitter 41.7M users / 1.47B edges →
  4031×4313 block at 68% density, 57% of sampled block users
  hand-labeled fraudulent (vs 12–25% control).
- FlowScope facts: laundering = dense multi-step flow on a k-partite
  transfer graph X → W → Y; per-middle-account f_i = min(in, out),
  q_i = max(in, out); g = (1/|S|) Σ [(1+λ)f_i − λq_i], λ = 4 — parking
  money or camouflage transfers *lower* the score. Same near-greedy
  peel and guarantee style. CBank 6.13M accounts / 43.98M transfers,
  labeled ring 4 sources / 12 mules / 2 destinations, central mule v5
  alone ≈ 452.1M yuan:
  FAUC 0.761/0.843 vs FRAUDAR 0.529/0.704; holds F1 ≥ 0.9 down to $76M
  injected volume vs FRAUDAR's 180M. Covered as guide + exercise 5
  (no stub module — the peel machinery is fraudar.rs's).
- Winkler/FS facts: R = P(γ|M)/P(γ|U), thresholds T_λ/T_μ with a
  clerical-review band; log2 R decomposes per field under conditional
  independence (+log2(m/u) agree, +log2((1−m)/(1−u)) disagree). 1990
  census: clerical review 3000 people × 3 months → 200 × 6 weeks.
  String comparators: exact matching misses >25% of true census
  matches; Jaro, then Winkler's prefix boost. EM for (p, m, u) is
  Winkler 1988. Multi-pass blocking: Winkler 2004 takes 10¹⁷ pairs to
  10¹² with 11 criteria while keeping 99.5% of matches; BigMatch does
  100M × 4B at ~100k pairs/s with 10 passes simultaneously.
- The EM degeneracy (discovered building the crate, the reason splink's
  API is shaped the way it is): a fixed-u EM over the *unioned* blocked
  candidates fits p → 1.0 (true 0.126) — every candidate agrees on a
  blocking key by construction, class U's tiny u values cannot explain
  that, so the free-m class M absorbs everything. Fix = splink's:
  one EM session per blocking pass, each EXCLUDING its blocking field;
  the excluded field's m comes from the other pass; doubly-estimated
  fields averaged. Exercise 3 reproduces the degeneracy on purpose.
- splink anchors (cloned ~/repos/splink @ 04189f5) verified by
  grep/read this session — full table in README, headline set:
  linker.py:66; training.py:163 (u by random sampling) / :231 (EM per
  blocking rule); expectation_maximisation.py:225 (E `:268`, M `:45`/`:278` in
  `maximisation_step:193`);
  comparison_level.py:148 (m `:190`, u `:191`, weight `:426`,
  tf-adjustment `:667`); predict.py:203 (prior + weights →
  1/(1+2^(−mw))); blocking.py:747; clustering.py:43 →
  connected_components.py:121; dialects.py:24 (DuckDB/Spark/SQLite/
  PostgreSQL at :270/:402/:532/:573).
- Crate: 3 provided tests green (review_graph.rs — instance shape
  20×80 block / 1600 fraud edges / camo ratio 0.8–1.05; degree-rank
  camo-0 precision < 0.3; obscurity 0.75 at camo 0 → < 0.3 at camo 2).
  6 stub tests fix contracts for fraudar.rs (log-weighted F ≥ 0.9 with
  and without camo 2; unweighted F < 0.7 at camo 2 — measured 0.643;
  g(returned) ≥ g(planted)/2) and er.rs (u within 0.005+expect of
  1/pool; per-pass EM p and m within 0.05 of the labeled empirical
  values with the blocked field NaN; match-weight gap > 20 bits;
  blocking ≥ 20×; precision ≥ 0.95 / recall ≥ 0.9 at 12 bits).
  Reference solution verified 9/9 then reverted; pristine crate:
  0 warnings, bench prints lane 1 + `[stub …]` banners via
  catch_unwind.
- Design margins that make the contracts honest: block density 1.0 is
  required — at 0.9 the log-weighted peel already loses the block to
  the block∪core union (F = 0.702; probe: union g 4.28 vs block 4.17
  at 0.8); camo 4 also breaks it (F = 0.619). The 20×80 wide-short
  block caps per-column camouflage at 20 edges (a user reviews an
  object once), keeping camo columns' weighted degree ≈ 3.06 below the
  block's g = 4.97. Threshold 12 bits clears the three two-field
  coincidence patterns (dob+city ≈ 10.3, dob+first ≈ 10.6, last+phone
  ≈ 10.95 bits); at 8 bits union-find chaining drags precision to
  0.85 — exercise 4's sweep maps the cliffs.

## Done when

- [ ] All 9 tests pass; lanes 2–3 print real numbers.
- [ ] Exercise 2 hand-derivation: Theorem 3 on lane 2's numbers —
      f(block) and every block column weight unchanged by camo.
- [ ] Exercise 3: the p → 1 EM degeneracy reproduced and explained in
      two sentences.
- [ ] Exercise 4: threshold sweep {6..16} bits recorded, each precision
      cliff matched to its coincidence pattern.
- [ ] Exercise 5: FlowScope's k-partite peel delta written as
      pseudocode (heap key = marginal (1+λ)f − λq contribution).
- [ ] Exercise 6: Zipf names re-measured; tf-adjustment's role stated.
- [ ] All 20 guide questions answered in writing.
- [ ] M39 sketch upgraded to a design note.
