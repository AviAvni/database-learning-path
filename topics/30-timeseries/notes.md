# Topic 30 — Notes

## Measured: the baseline to beat (lane 1, provided)

1M samples, 10s scrape interval ±100ms jitter, `cargo run --release --bin tsdb_bench`:

| shape | delta+varint B/sample | decode Msamples/s |
|---|---|---|
| constant | 11.00 | 292 |
| gauge | 11.00 | 268 |
| counter | 11.00 | 303 |
| random | 11.00 | 333 |

The flat 11.00 is the finding: timestamps compress to ~3 B (varint of
jittered deltas) but the raw 8-byte values dominate and are *shape-blind*.
Whatever the codec does about timestamps is a rounding error until it
attacks the value bytes — hence XOR floats.

## Predictions (before implementing the stubs)

| lane | prediction | reasoning |
|---|---|---|
| gorilla constant | ~0.3 B/sample | steady state 1+1 bits, jitter pushes some ts to 9-bit bucket |
| gorilla gauge | 3-5 B/sample | random walk: XOR shares exponent/sign, mantissa noise costs ~30-45 meaningful bits |
| gorilla counter | 4-6 B/sample | value *changes* every sample by a varying integer — XOR of shifting mantissas is wide |
| gorilla random | 9-10 B/sample | entropy floor + control-bit overhead (test demands >8) |
| gorilla decode | 100-200 Msamples/s | bit-at-a-time reader; slower than the byte-aligned varint baseline's ~300 |
| ooo tax 0%→50% | ingest barely moves; flush grows ~k log k | append is O(1) either path; sort of the OOO fraction dominates flush |
| index intersect hot∧rare | <1 µs | shortest-list-first makes the unique instance label do all the work |
| index build 100K series | tens of ms | 400K postings pushes into a HashMap |

Record actuals next to these after implementing.

## Things that surprised me while designing the experiments

- The delta+varint baseline decoding at ~300 Msamples/s is *fast* —
  byte-aligned codecs have a real throughput edge over bit-packed
  Gorilla. Production systems know this: VM chose varint batches
  (nearest_delta2), and Parquet's encodings are byte/word-aligned. The
  ratio-vs-decode-speed trade is the actual design axis, not just ratio.
- Prometheus's dod buckets (14/17/20 bits) differ from the paper's
  (7/9/12) — bucket boundaries are workload parameters. Encoding the
  paper's table verbatim into a test would have been wrong; the tests
  pin roundtrip + ratio *bounds* instead.
- The OOO design is the same watermark idea as topic 27's streaming:
  bounded disorder, quarantined buffer, merge at seal time,
  refuse-too-late. TSDBs and stream processors converged independently.
- `ErrTooOldSample` — a database that *refuses writes by policy* is rare;
  the alternative (resort history forever) is worse. Good API honesty.

## Guide questions (work through per reading guide)

- [ ] reading-gorilla.md — 6 questions (dod-vs-xor asymmetry; window reuse trade; 120-sample chunks; counters vs gauges; entropy-floor bit accounting; M30 non-numeric payloads)
- [ ] reading-prometheus-tsdb.md — 6 questions (one-WAL-many-series; head/block overlap; regex postings; OOO read cost; retention anomaly; M30 label-vs-payload for adjacency)
- [ ] reading-victoriametrics-influx.md — 6 questions (decimal scaling breakage; partition sizing; tagFilters invalidation; buffer/Parquet consistency; how-much-was-sorting; M30 custom-chunks vs Parquet)
- [ ] reading-monarch-btrdb.md — 6 questions (durability trade; distribution values; BtrDB cost derivation; CoW at 10M streams; essential-vs-incidental labels; M30 aggregate tree over changelog)

## Cross-topic threads

- Topic 4: TSDB = LSM keyed by time; retention = drop the oldest level —
  the cheapest delete in databases.
- Topic 5: prometheus head WAL + checkpoint-on-block-cut is the WAL
  lifecycle verbatim.
- Topic 12/28: InfluxDB 3 dissolves the TSDB into Parquet + object store +
  DataFusion — a columnar store with a time-partitioned catalog.
- Topic 23: MemPostings is an inverted index; high cardinality =
  unbounded vocabulary; selector = boolean term query.
- Topic 27: OOO window = watermark; BtrDB changed-ranges = IVM input.

## Capstone M30 log

- Temporal graph = history chunks per (entity, attribute): edge
  existence intervals [added_ts, removed_ts) + property value series.
  Entity id is the series key; the M23/M26 index infrastructure serves
  the "label selector" role over entity properties.
- `MATCH ... AT TIME t`: snapshot = for each touched entity, latest
  history record ≤ t — exactly `latest_write_before` from topic 29's
  kv.rs, so M29's MVCC read path generalizes to time-travel if commit_ts
  is wall-clock-ish (HLC helps here).
- Storage split by age (the VM-vs-IOx question resolved per tier): hot
  recent history in delta-matrix-adjacent chunks (custom, fast), cold
  history as Parquet on object store via M28 — the same data ages
  through formats.
- Gorilla dod survives for timestamp columns of the changelog; property
  values need dictionary + RLE instead of XOR (mostly non-float).
- Rollups: edges-added-per-interval aggregate tree (BtrDB-shaped) over
  the M27 changelog enables "graph evolution" dashboards without
  scanning history.

## Infra notes

- Crate: `timeseries-experiments`; gen/bits/baseline PROVIDED (7 tests
  pass), gorilla.rs / head.rs / index.rs stubs — 15 tests fail as
  `todo!()` panics.
- tsdb_bench lane 1 always prints (numbers above); lanes 2-4 armed
  behind catch_unwind until the stubs are implemented.

## Done when

- [ ] `gorilla.rs`: all 6 tests green — bit-exact roundtrip incl. NaN
      patterns and bucket edges, constant ≤ ~2 bits/sample, gauge beats
      raw 3×, random *fails* to compress (>8 B/sample).
- [ ] `head.rs`: all 4 tests green — window boundaries exact, TooOld
      never stored, flush sorted + LWW, output feeds the encoder.
- [ ] `index.rs`: all 5 tests green — brute-force match, sorted results,
      hot∧rare narrows to one, cardinality bomb counted.
- [ ] tsdb_bench full run: prediction table above filled with actuals;
      the ratio-vs-decode-speed trade quantified against the baseline.
- [ ] Can explain, without notes: why 11.00 B/sample is shape-blind and
      what XOR does about it; why OOO gets a bounded window instead of
      either extreme (reject all / absorb all); why high cardinality is
      an *index* problem, not a data-volume problem.
