# Topic 49 notes — open file & table formats

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin table_bench`. Seed 49. Lanes 1a and 1b are counts
on an in-memory metered object store and reproduce exactly on any machine;
the CPU timings in 1a and 1c are local, one run, taken while other agents were
benchmarking on the same machine — `sysctl -n vm.loadavg` read
`{ 7.68 8.33 8.54 }` before the run and `{ 14.50 9.70 9.02 }` after. Two
earlier runs at load 6.5 and 10.9 gave timings within ±20% of these and
identical counts.

### Lane 1a — planning a scan after n one-file commits (100 commits per day)

| after n | layout | GETs | bytes read | LISTs | rounds | files |
|---:|---|---:|---:|---:|---:|---:|
| 10 | Iceberg, fast append | 13 | 15,201 | 0 | 4 | 10 |
| 10 | Iceberg, merge append (min 100) | 13 | 15,201 | 0 | 4 | 10 |
| 10 | Delta, no checkpoints | 11 | 5,763 | 1 | 3 | 10 |
| 10 | Delta, checkpoint every 10 | 11 | 5,763 | 1 | 3 | 10 |
| 100 | Iceberg, fast append | 103 | 146,089 | 0 | 4 | 100 |
| 100 | Iceberg, merge append | 4 | 97,845 | 0 | 4 | 100 |
| 100 | Delta, no checkpoints | 101 | 52,591 | 1 | 3 | 100 |
| 100 | Delta, checkpoint every 10 | 11 | 41,331 | 1 | 3 | 100 |
| 1,000 | Iceberg, fast append | 1,003 | 1,373,885 | 0 | 4 | 1,000 |
| 1,000 | Iceberg, merge append | 13 | 889,448 | 0 | 4 | 1,000 |
| 1,000 | Delta, no checkpoints | 1,001 | 522,593 | 1 | 3 | 1,000 |
| 1,000 | Delta, checkpoint every 10 | 11 | 399,735 | 1 | 3 | 1,000 |

At 1,000 commits:

| query | layout | GETs | bytes read | LISTs | rounds | files |
|---|---|---:|---:|---:|---:|---:|
| one day (day 3) | Iceberg, fast append (pruned) | 103 | 845,154 | 0 | 4 | 100 |
| one day | Iceberg, merge append (pruned) | 4 | 884,139 | 0 | 4 | 100 |
| one day | Delta, checkpoint every 10 | 11 | 399,735 | 1 | 3 | 100 |
| time travel to v500 | Iceberg | 503 | 877,504 | 0 | 4 | 500 |
| time travel to v500 | Delta, no checkpoints | 500 | 261,392 | 1 | 2 | 500 |
| time travel to v500 | Delta, checkpoint every 10 | 10 | 200,508 | 1 | 2 | 500 |

- **Counts, not encodings, are the headline.** 1,003 = catalog + table
  metadata + manifest list + 1,000 manifests. 11 = `_last_checkpoint` +
  checkpoint 990 + commits 991–999. Delta without checkpoints is 1,001 because
  the `_last_checkpoint` GET is paid even when it misses.
- **Merge append** fires at commits 100, 199, 298, …, 991 (a merged manifest
  plus 99 fast ones reach 100), so at 1,000 the list holds 10 manifests: 13
  GETs; the worst case just before a merge is 3 + 99. It is 4 at 100 commits
  because commit 100 had just merged.
- **The one-day query** separates the shapes. The manifest list's partition
  summaries let fast append skip 900 of 1,000 manifests; Delta reads the same
  11 objects and 399,735 bytes as a full scan, since partition values live
  inside the actions and are only known after replay. The merged manifest
  contains all days, so merge append reads it whole (884,139 bytes).
- **Time travel without `_last_checkpoint`** costs Delta a LIST from version 0
  and one fewer round (no hint to read first); the checkpoint ≤ 499 is 490.
  Iceberg's cost is the snapshot's own manifest list plus its 500 manifests,
  and the current table-metadata file, which holds all 1,000 snapshots.
- Sanity line (printed): every planner returned exactly the committed files
  for every query above.

Local CPU to plan the latest scan (in-memory, JSON parse + replay, median of
7): Iceberg 6,114 µs; Delta no checkpoints 3,371 µs; Delta every 10 1,949 µs;
after maintenance Iceberg 2,337 µs, Delta 1,867 µs. Parsing is not the cost
that matters on a real store — 1,003 requests are — but it shows the
metadata.json (1,000 snapshots) is not free to parse either.

### Lane 1b — what the commits wrote, and maintenance

| metadata written by 1,000 commits | PUTs | bytes | largest commit | objects now |
|---|---:|---:|---:|---:|
| Iceberg, fast append | 4,000 | 398,749,323 | 786,993 | 3,001 |
| Iceberg, merge append (min 100) | 4,010 | 219,263,669 | 877,726 | 3,001 |
| Delta, no checkpoints | 1,000 | 522,593 | 1,094 | 1,000 |
| Delta, checkpoint every 10 | 1,198 | 20,307,084 | 395,541 | 1,100 |

Iceberg bytes stored after 1,000 commits, by kind:

| append | table metadata | manifest lists | manifests |
|---|---:|---:|---:|
| fast | 195,826,990 | 202,285,853 | 587,480 |
| merge | 195,826,990 | 20,061,104 | 3,320,693 |

- 1 + 2 + … + 1,000 = 500,500 entries per rewritten kind:
  195,826,990 / 500,500 = 391.3 B per snapshot entry; 202,285,853 / 500,500 =
  404.2 B per manifest-list entry. Fast append's four kinds plus the catalog
  pointer (1,000 × 49 B) sum to the total exactly.
- Merge append cuts the manifest-list term 10× (lists stay ≤ 100 long) but
  not the table-metadata term: every snapshot is still in every root file.
  Only expiry removes that term.
- Delta's checkpoints write Σ_{j=1..99} (10j + 1) = 49,599 `add` entries:
  (20,307,084 − 522,593) / 49,599 = 398.9 B each, against 399,735 / 1,000 =
  399.7 B per entry in the final checkpoint. Checkpoints multiply the log's
  write volume 38.9× and cut a reader's requests 91× (1,001 → 11) but its
  bytes only 1.31× (522,593 → 399,735).
- The largest Delta commit without checkpoints (1,094 B) is version 0, which
  carries `protocol` and `metaData`.

After maintenance at 1,000 commits:

| state | GETs | bytes read | LISTs | rounds |
|---|---:|---:|---:|---:|
| Iceberg + rewrite manifests | 4 | 885,412 | 0 | 4 |
| + expire snapshots, keep 1 | 4 | 514,082 | 0 | 4 |
| Delta + checkpoint at 999 | 2 | 398,620 | 1 | 3 |

After expiry, time travel to version 500 reports "gone (the snapshot was
expired)".

### Lane 1c — Parquet footer, 10 of C float columns

| columns | footer bytes | B/column | full µs | skip-stats µs | + cached schema µs | 10-column share |
|---:|---:|---:|---:|---:|---:|---:|
| 100 | 10,843 | 108.4 | 36 | 41 | 23 | 10.00% |
| 1,000 | 113,461 | 113.5 | 353 | 355 | 231 | 1.00% |
| 10,000 | 1,157,462 | 115.7 | 3,828 | 3,868 | 2,436 | 0.10% |
| 50,000 | 6,027,910 | 120.6 | 19,484 | 20,037 | 12,768 | 0.02% |

- Bytes per column grow slowly (108 → 121) because column names and offsets
  get longer varints; the footer is linear in total columns.
- Full decode ≈ 390 ns per column at 50,000 (19,484 µs / 50,000).
- **Skip-stats is not faster** at any width (3,868 vs 3,828 µs at 10,000).
  The skipped fields are still walked by the Thrift skip routine, and the
  policy check per column is a `HashSet` lookup
  ([reading-parquet-footer.md](reading-parquet-footer.md)). This is the
  negative result of the lane.
- A cached schema saves 34–37% (12,768 / 19,484 = 0.66; 2,436 / 3,828 = 0.64),
  because building the schema descriptor allocates per column.

### Lane 2 — reference numbers for the `run_occ` stub

From a reference implementation (seeded scheduler choosing one writer step at
a time; begin = `Delta::plan`; commit = `put_if_absent(base + 1)`; on conflict
LIST after `base`, GET each winner, abort if a winner removed one of our
inputs, else rebase), run before the function was stubbed. 25 operations per
writer, 40 initial files:

| writers (append / compact / delete) | committed | conflicts | aborted | skipped | conflicts per commit | validation reads |
|---|---:|---:|---:|---:|---:|---:|
| 1 / 0 / 0 | 25 | 0 | 0 | 0 | 0.00 | 0 |
| 2 / 0 / 0 | 50 | 26 | 0 | 0 | 0.52 | 34 |
| 4 / 0 / 0 | 100 | 107 | 0 | 0 | 1.07 | 196 |
| 8 / 0 / 0 | 200 | 408 | 0 | 0 | 2.04 | 1,000 |
| 16 / 0 / 0 | 400 | 1,318 | 0 | 0 | 3.29 | 4,424 |
| 0 / 2 / 0 | 10 | 4 | 4 | 36 | 0.40 | 4 |
| 4 / 2 / 0 | 125 | 171 | 12 | 13 | 1.37 | 346 |
| 4 / 2 / 2 | 160 | 272 | 26 | 14 | 1.70 | 621 |

- Blind appends never abort (Iceberg spec: an append "can always be
  applied"), but each conflict costs a LIST and a GET per winner: 4,424
  validation reads for 400 commits at 16 writers, 11 per commit.
- Two compactors: 40 small files support exactly 10 compactions of 4; the
  other 36 attempts found fewer than 4 small files and were skipped, and 4
  lost a race over the same inputs and aborted. The audit passed in every row.
- The stub tests use 12 operations per writer and check the invariants
  (contiguous versions, committed + aborted + skipped = attempted, no double
  remove, conflicts growing with writers, seeded reproducibility).

### Lane 3 — reference numbers for the `Eoq` stub

600 commits, reads at a seeded random rate, cost in millions of
byte-equivalents (bytes + 250,000 per request):

| reads/commit | never | every 10 | best fixed (k) | ski rental | EOQ (reference) |
|---:|---:|---:|---:|---:|---:|
| 0.02 | 1,358.8 | 219.3 | 215.7 (20) | 305.3 (14 cp) | 221.5 (39 cp) |
| 0.2 | 9,811.6 | 434.7 | 393.4 (5) | 448.6 (79 cp) | 388.0 (110 cp) |
| 2 | 91,089.0 | 2,577.8 | 1,530.0 (2) | 1,617.5 (200 cp) | 1,530.0 (300 cp) |

- EOQ lands within 2.7% of the best fixed interval at every rate (221.5 vs
  215.7), slightly *beats* it at 0.2 (388.0 vs 393.4: it can use different
  intervals as the table grows), and ties at 2. It does not beat every-10 on
  the rarely read table (221.5 vs 219.3): ten is a good constant there.
- Ski rental is 1.42× the best fixed interval at 0.02. A first version of
  this lane spaced reads exactly every 1/rate commits; fixed intervals that
  divided the spacing then checkpointed just before every read and looked
  unbeatable (k = 50 "won" at 0.02). The reads were made random to remove
  that aliasing.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 2: conflicts per commit at 2 appenders | | |
| lane 2: conflicts per commit at 16 appenders | | |
| lane 2: aborts among 16 appenders | | |
| lane 2: validation reads per commit at 16 appenders | | |
| lane 2: aborts with 2 compactors and 40 initial files | | |
| lane 3: EOQ checkpoints at 0.02 / 0.2 / 2 reads per commit | | |
| lane 3: EOQ cost vs best fixed interval at 0.2 reads per commit | | |
| exercise 3: Iceberg bytes written with expiry every 100 commits | | |

<details><summary>The author's expectations going in, and how they fared</summary>

These are the expectations the lanes were designed around, as recorded in the
code's own first drafts — not numeric guesses written in advance.

- **Lane 1b:** the Iceberg writer would be quadratic because the manifest
  list is rewritten every commit. True, but only half the story: the table
  metadata file (every snapshot in every root file) is the other half,
  195.8 MB against 202.3 MB.
- **Lane 1c:** `footer.rs`'s module docs predicted that skipping statistics
  would still walk every column chunk, so the cost stays linear. It does — and
  the skip turned out to save nothing at all (1% slower), which was not
  expected.
- **Lane 3:** the lane was first written with ski rental as the exercise, on
  the strength of its 2-competitive bound. It stays within 2× of the best
  fixed interval (1.42× worst), but it is the worst non-trivial policy on a
  rarely read table, so it became the provided negative result and EOQ the
  exercise.
- **Lane 3, first draft:** reads spaced exactly every 1/rate commits made
  fixed intervals look unbeatable (aliasing, above); random arrivals were the
  fix.

</details>

## Paper numbers worth keeping

- **F3** (Zeng et al., Proc. ACM Manag. Data 3(4) Art. 245, 2025). ~300
  public Parquet files mostly use v1 features (§2.2, Fig. 1). IOUnit 8 MB for
  S3 (§5.1). Dictionary scope: 13% of Public BI columns favour local, 35%
  global, 52% in between; near-best 67% / 48% / 88% (§5.2). Table 3: Global
  92.2% CR at 137% encode time; GLBest SR 1% 90.4% / 225%; SR 100% 89.7% /
  252%; 2,080 columns (§7.6). Table 2 average chunk sizes (Laion) url: F3
  8.8 MB, Parquet 75 MB, ORC 1.0 MB, Lance 11.2 MB, Nimble 1.0 MB. Wasm: the
  intro's 10–30%; Fig. 15 annotations 10.0% (core) to 29.2% (log), against the
  prose's "9.6% … up to 25% on classic"; Fig. 13 string decoding 46.6% (built
  in) and 52.0% (zstd). Fig. 9's F3-vs-Parquet gap reads ≈3–6× off the log
  axis, not "an order of magnitude".
- **LakeVilla** (Götz, Ritter, El-Hindi, Giceva, PVLDB 19(11), 2026). Table 4
  (TPC-C, 1 warehouse, 4 clients, MinIO, 1 h): Delta 0.00 txns/min at 100%
  aborts, Nessie 0.00 / 100%, DuckLake 0.20 / 99.98%, LakeVilla 2.81 / 21.92%,
  LV[Write] 2.79 / 29.42% (the prose says "ca. 20%" for both), LV[Read] 0.62 /
  78.10%. YCSB: aborts < 1%, p50 7,672 ms, p99 up to 9,411 ms (§8.2). Freshness
  117 ms average, ~600 ms max (§8.3). AWS Time Sync 40–100 ms (§4.1).
- **Delta Lake** (Armbrust et al., PVLDB 13(12), 2020). LIST ≤ 1,000 keys per
  call, tens to hundreds of ms (§2.1); 5–10 ms base latency, 50–100 MB/s (§2.3);
  checkpoint every 10 transactions (§3.1.3); several write transactions per
  second (§3.4); Fig. 4: Delta 108 s at 1 M partitions (17 s cached), Hive
  > 1 h at 10 k, Presto > 1 h at 100 k, Databricks Parquet listing 450 s at
  100 k (§6.1).
- **Iceberg** (spec @ 47677ab): v1–v3 adopted, v4 under development; Java
  `commit.manifest.min-count-to-merge` = 100, target 8 MB. iceberg-rust:
  `write.metadata.previous-versions-max` = 100. **delta-rs** checkpoint
  interval default 100 (`table/config.rs:269-274`), not 10.

## Cross-topic threads

- Topic 28's M28 branching is M49's starting point: a branch becomes a named
  reference to a manifest, committed with the same conditional PUT.
- Topic 5's checkpoint interval and lane 3 are the same question; EOQ applies
  to any log whose replay cost readers pay.
- Topic 12's encodings sit inside the files lane 1c's footer indexes.

## Open questions

- delta-rs's conflict checker ignores `remove` actions written with
  `dataChange: false` when checking for concurrent deletes, and compaction
  writes exactly those (reading-delta-rs.md). Two concurrent compactions of
  the same file would both commit. Not run here — build the scenario on lane 2
  and see whether the audit catches it.
- iceberg-rust's default duplicate-file check reads every live manifest on
  every append (reading-iceberg-rust.md): 499,500 manifest GETs over lane 1's
  1,000 commits by arithmetic. Lane 1's writer does not charge them. Should it?
- What would a DuckLake planner's "rounds" column be with a catalog database
  one network hop away, and what does that move from the object store to the
  database's commit rate?

## Implementation log

- [ ] `occ::run_occ` — six tests green; lane 2 rows match the reference
- [ ] `policy::Eoq` — four tests green; lane 3's EOQ column matches
- [ ] prediction table reconciled
- [ ] exercise 3 (expiry cadence) and exercise 4 (DuckLake planner)
