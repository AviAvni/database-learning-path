# Topic 30 — Notes

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

1M samples, 10s scrape interval ±100ms jitter, `cargo run --release --bin tsdb_bench`:

```
=== tsdb_bench: 1000000 samples/series shape, 10s scrape interval ===

-- baselines (provided): raw = 16.00 B/sample --
timestamps (all shapes): delta+varint 3.00 B/sample
 constant: values raw 8.00 | values xor-bytes 0.50 B | total raw-values 11.00 / xor-bytes 3.50 B/sample | decode 214 Msamples/s
    gauge: values raw 8.00 | values xor-bytes 6.80 B | total raw-values 11.00 / xor-bytes 9.80 B/sample | decode 132 Msamples/s
  counter: values raw 8.00 | values xor-bytes 6.07 B | total raw-values 11.00 / xor-bytes 9.07 B/sample | decode 124 Msamples/s
   random: values raw 8.00 | values xor-bytes 7.48 B | total raw-values 11.00 / xor-bytes 10.48 B/sample | decode 136 Msamples/s

-- gorilla codec (stub lane) --
gorilla lane: [stub — implement gorilla.rs]

-- float value codecs (provided; gorilla column is the stub) --
            shape xor-bytes   gorilla       ALP |   exc%  bits raw-vecs modal e,f | xor dec M/s ALP dec M/s
      decimal 2dp      6.79    [stub]      1.50 |   0.1%  11.8    0/977     14,12 |         164         590
      decimal 6dp      6.82    [stub]      3.25 |   1.0%  25.1    0/977      14,8 |         168         589
          counter      6.07    [stub]      1.95 |   0.3%  15.3    0/977     14,14 |         174         647
         constant      0.50    [stub]      0.01 |   0.0%   0.0    0/977     18,18 |         499        1666
 walk (f64 arith)      6.80    [stub]      8.00 |  95.1%  57.9  926/977      16,0 |         171        2614
   random doubles      7.48    [stub]      8.00 | 100.0%   0.0  977/977         - |         206        2405

-- out-of-order ingestion tax (stub lane) --
ooo lane: [stub — implement head.rs]

-- tag index at 100K series (stub lane) --
index lane: [stub — implement index.rs]
```

The B/sample and B/value columns are seeded and exact on every run. Decode
throughput is a timing and moves by up to ~20% between runs; cite one run by
date.

| shape | timestamps | values raw | values xor-bytes | total xor-bytes | decode Msamples/s |
|---|---|---|---|---|---|
| constant | 3.00 | 8.00 | 0.50 | 3.50 | 214 |
| gauge | 3.00 | 8.00 | 6.80 | 9.80 | 132 |
| counter | 3.00 | 8.00 | 6.07 | 9.07 | 124 |
| random | 3.00 | 8.00 | 7.48 | 10.48 | 136 |

What the baseline shows: timestamps compress to 3.00 B on every shape (varint of
jittered deltas), so the shapes differ only in the value column — 0.50 B for a
constant, ~6-7.5 B for everything else. Byte-aligned XOR sees leading zero
*bytes* only. The counter's integer-valued doubles carry ~40 trailing zero bits
it cannot drop; the walk's low mantissa bits are noise. Those are the two gaps a
bit-level window (Gorilla) and a decimal-aware codec (ALP, lane F) attack.

Lane F, decode column: ALP's decimal path decodes 3.5-3.7× faster than the
byte-aligned XOR (590/589/647 vs 164/168/174 Mvalues/s); the last two ALP rows
are `memcpy` of raw-fallback vectors, not decode work.

## Predictions (fill BEFORE implementing the stubs)

| lane | predicted | measured |
|---|---|---|
| gorilla constant — B/sample | | |
| gorilla gauge — B/sample | | |
| gorilla counter — B/sample | | |
| gorilla random — B/sample | | |
| gorilla decode — Msamples/s | | |
| ooo tax 0%→50% — ingest and flush | | |
| index intersect hot∧rare | | |
| index build 100K series | | |

Record actuals in the measured column after implementing.

<details>
<summary>Author's prediction</summary>

| lane | prediction | reasoning |
|---|---|---|
| gorilla constant | ~0.25 B at zero jitter; ~1.5 B on this lane | steady state 1+1 bits; the lane's ±100 ms jitter pushes most timestamps into the 9-bit bucket (12 bits ≈ 1.5 B) and dominates |
| gorilla gauge | ~9.5 B/sample | random walk accumulated in f64: the low mantissa bits are noise, XOR keeps only the shared sign/exponent, and the reuse window never narrows — ~8 B of value plus ~1.5 B of timestamp |
| gorilla counter | ~4.4 B/sample | integer steps: XOR has ~40 trailing zeros, the window is narrow (~24 bits) and mostly reused |
| gorilla random | ~8.8 B/sample | value ≈ 7.4 B (a window stuck at ~57 meaningful bits + 2 flag bits) + ~1.5 B timestamp. NOT >10: the shared exponent is saved |
| gorilla decode | 20-140 Msamples/s | bit-at-a-time reader; slower than the byte-aligned codec's ~130-210 |
| ooo tax 0%→50% | ingest barely moves; flush grows ~k log k | append is O(1) either path; sort of the OOO fraction dominates flush |
| index intersect hot∧rare | <1 µs | shortest-list-first makes the unique instance label do all the work |
| index build 100K series | tens of ms | 400K postings pushes into a HashMap |

</details>

## Reference numbers for the exercise lanes

Read these after filling the worksheet. What the reference solution printed
(measured 2026-10-26, Apple M3 Pro, one run; B/sample are seeded and exact,
rates are timings and move ~20% run to run):

- **Lane 2 — Gorilla** (jittered timestamps, so each figure includes ~1.5 B of
  timestamp): constant **1.54 B/sample** (10.4× vs raw), gauge **9.48** (1.7×),
  counter **4.42** (3.6×), random **8.79** (1.8×). Encode / decode
  Msamples/s: 34 / 140, 5 / 26, 15 / 53, 4 / 22 — the bit-at-a-time
  `BitWriter` is the cost, and the byte-aligned baseline's ~130-210 decode
  beats every Gorilla row but the constant.
- **Lane F, Gorilla column** (value stream only, timestamps regular and 1
  bit/sample subtracted): decimal 2dp **8.03 B/value**, decimal 6dp **8.07**,
  counter **3.01**, constant **0.13**, f64 walk **8.07**, random **7.37** —
  against byte-aligned XOR's 6.79 / 6.82 / 6.07 / 0.50 / 6.80 / 7.48. Gorilla
  *loses* to the byte-aligned codec on the three non-repeating decimal/walk
  shapes and wins on counter, constant and random.
- **Why**: branch counts of the reference encoder on 1M values (`value
  bits/sample` = flags + payload): gauge opens **28 windows**, 99.6% of values
  take the reuse branch, 64.53 bits/value; decimal 2dp **10 windows**, 99.1%
  reuse, 64.23 bits; random **4 windows**, 100% reuse, **59.00 bits** (= 2 flag
  bits + a 57-bit window); counter **3,630 windows**, 97.6% reuse, 2.0% zero
  XOR, 24.07 bits. A window opened by one wide XOR stays wide: the reuse branch
  never narrows it, so random costs 59 bits, not the 77 (13 control + 64
  payload) a "new window every time" accounting predicts.
- **Spec findings the reference run forced**: (1) the stub's original tests
  required gauge ≥3× and random >8 B/sample; neither holds for an honest
  Gorilla on an f64-arithmetic walk (9.48 B total, value 8.07) or uniform doubles
  (7.37 B of value), so the tests now assert what is true (counter beats raw
  3×; gauge and random stay above ~7 and ~6.5 B/sample). (2) The dod buckets'
  upper ends (64, 256, 2048) do not fit two's complement, so `sign_extend`
  cannot read them back; the layout doc now gives the prometheus rule
  `if v > 2^(n-1) { v - 2^n }`.
- **Lane 3 — OOO tax** (ingest Msamples/s, flush): 0% **233**, 1.7 ms; 1% **200**,
  2.2 ms; 10% **197**, 3.4 ms; 50% **182**, 8.8 ms; dropped-too-old 0 at every
  fraction (the 60 s lag fits the 120 s window).
- **Lane 4 — tag index**, 100K series: build **38 ms**, **100,015** postings
  entries (100,000 of them the unique `instance` label — the cardinality bomb,
  counted); `job-3 ∧ env=dev` → 10,000 series in 132 µs;
  `job-3 ∧ env=prod ∧ region=r0` → 0 series in 93 µs; `job-7 ∧ instance=i-70007`
  → 1 series in 0.2 µs.

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

- [ ] reading-alp-chimp.md — 6 questions (Chimp rounding direction; early-exit worst case; FOR vs outliers; 6dp vs 2dp bit widths; fused vs unfused decode; M30 where-born for edge properties)
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

- Crate: `timeseries-experiments`; gen/bits/baseline/alp PROVIDED (14 tests
  pass), gorilla.rs / head.rs / index.rs stubs — 16 tests fail as
  `todo!()` panics.
- tsdb_bench lanes 1 and F always print (numbers above; the Gorilla column of
  lane F fills in with the stub); lanes 2-4 armed behind catch_unwind until the
  stubs are implemented.

## Done when

- [ ] `gorilla.rs`: all 7 tests green — bit-exact roundtrip incl. NaN
      patterns and bucket edges, constant ≤ ~2 bits/sample, counter beats
      raw 3×, the f64-arithmetic gauge (>7 B/sample) and random values
      (>6.5 B/sample) *fail* to compress.
- [ ] `head.rs`: all 4 tests green — window boundaries exact, TooOld
      never stored, flush sorted + LWW, output feeds the encoder.
- [ ] `index.rs`: all 5 tests green — brute-force match, sorted results,
      hot∧rare narrows to one, cardinality bomb counted.
- [ ] tsdb_bench full run: prediction table above filled with actuals;
      the ratio-vs-decode-speed trade quantified against the baseline.
- [ ] Can explain, without notes: why a codec that only touches the timestamp
      column is shape-blind (11.00 B/sample) and what XOR does about the value
      column; why a Gorilla window that opens wide stays wide, and why that
      makes it lose to a byte-aligned XOR on 2-decimal data; why OOO gets a bounded window instead of
      either extreme (reject all / absorb all); why high cardinality is
      an *index* problem, not a data-volume problem.
