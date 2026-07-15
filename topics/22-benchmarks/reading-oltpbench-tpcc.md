# TPC-C: contention by design (and the harness that runs it honestly)

TPC-C doesn't measure throughput — it measures how an engine
behaves when the workload deliberately funnels transactions through
hot rows. This chapter builds that idea step by step — what an OLTP
benchmark even measures, where TPC-C's contention is planted, the
two spec devices that stop you from cheating around it, and why
almost nobody runs it honestly — then reads the OLTP-Bench paper
(VLDB 2013) for what a fair OLTP harness must do (rate control
above all), with the code anchors in the maintained fork, CMU's
BenchBase: one harness, ~20 benchmarks, one config format,
per-phase rate control.

## The problem in one sentence

Strip TPC-C's mandated think times and run 4 warehouses instead of
the thousands the spec forces, and your "tpmC" number measures one
contended counter's latch, not the engine — which is exactly what
most informal "TPC-C" results do (spec-compliant, one warehouse
supports only **~12.86 tpmC**).

## The concepts, step by step

### Step 1 — what an OLTP benchmark measures: contention, not speed

OLTP (online transaction processing — many small concurrent
read-write transactions, opposite of TPC-H's big read-only scans)
performance is limited by **contention**: multiple transactions
needing the *same rows* at the same time, forcing the engine to
serialize them via locks or abort-and-retry (topic 8's concurrency
control). An OLTP benchmark with no contention just measures how
fast you can hash keys — YCSB territory. TPC-C's design question
is: *given* deliberately contended rows, multi-statement
transactions, and mandatory aborts, how much throughput survives?
That is a property of the concurrency-control design, not the
per-op code path.

### Step 2 — TPC-C's anatomy: the hot counter is the benchmark

TPC-C models order entry: a hierarchy of warehouses, districts, and
customers, with five transaction types weighted 45/43/4/4/4 —
NewOrder, Payment, OrderStatus, Delivery, StockLevel
(`TPCCConfig.java` holds the weights). Contention is BY DESIGN:

```
  warehouse (W of them) ← every NewOrder updates its W_YTD row-ish
      └─ district (10/W)   ← D_NEXT_O_ID: THE hot counter, serializes
           └─ orders           NewOrders within a district
  ~1% NewOrders touch a REMOTE warehouse ⇒ cross-shard txns exist
  ~1% NewOrders ABORT by spec (rollback path must be exercised)
```

Every NewOrder in a district must read-increment-write that
district's `D_NEXT_O_ID` counter — so NewOrders within a district
are *forcibly serialized*, and with 10 districts per warehouse,
warehouse count directly caps parallelism. The 1% remote-warehouse
orders exist so that partitioning by warehouse can't make
cross-partition transactions disappear; the 1% mandated aborts
force the rollback path to be real code, not dead code.

### Step 3 — NURand: skew you can't preload away

**NURand** is TPC-C's non-uniform random function: it ORs two
uniform random numbers, which biases bits toward 1 and concentrates
selections (customer names, item ids) in a hot region — skew, like
YCSB's Zipfian, but with a spec-mandated twist: the constant `C`
that positions the hot region **must differ between load time and
run time** (the delta is constrained — e.g. 157 or 223 work, others
don't). Otherwise a vendor could pre-sort or pre-cache exactly the
rows the run will hammer.

```rust
// NURand: TPC-C's non-uniform random — OR of two uniforms biases bits
// toward 1, concentrating hits in a hot region you can't cheat away
fn nurand(a: u64, x: u64, y: u64, c: u64, rng: &mut Rng) -> u64 {
    // c MUST differ between load time and run time (TPCCUtil:94) —
    // otherwise the loader could pre-sort the hot region into cache
    (((rng.range(0, a) | rng.range(x, y)) + c) % (y - x + 1)) + x
}
```

In code: `TPCCUtil.java:94-116` — note :94's constraint on
`C_LAST_LOAD_C` vs `C_LAST_RUN_C` (157/223). The lesson generalizes:
a benchmark's data loader and its runtime driver must not share the
knowledge that lets one flatter the other.

### Step 4 — think times: the human simulator nobody runs

The spec simulates a human terminal operator: after each
transaction, the emulated user "keys in" the next one and "thinks"
— a capped exponential wait (`TPCCWorker.java:85-100`):

```rust
// keying + think time: the simulated human nobody runs — capped
// exponential wait between transactions (TPCCWorker:85-100)
fn think_time(mean: f64, rng: &mut Rng) -> f64 {
    (-rng.f64().ln() * mean).min(10.0 * mean)   // spec caps at 10× mean
}
```

Consequence: with think times, one warehouse supports **~12.86 tpmC
max** — so a spec-compliant run posting millions of tpmC needs
hundreds of thousands of warehouses, i.e. terabytes of data, and
the metric secretly becomes "how much hardware can you scale to".
Everyone strips think times and runs 4 warehouses instead ⇒ they're
benchmarking the D_NEXT_O_ID latch (Step 2), not the engine.
"tpmC" without an audit is a vibe.

| anchor | what |
|---|---|
| `TPCCWorker.java:85-100` | keying + think times: `-log(c)·mean`, capped at 10× — the spec's human simulator |
| `TPCCUtil.java:94-116` | `NURand` non-uniform randoms; note :94's constraint on `C_LAST_LOAD_C` vs `C_LAST_RUN_C` (157/223) — load-time and run-time skew must DIFFER by spec |
| `TPCCConfig.java` | the 45/43/4/4/4 weights |

### Step 5 — rate control: what an honest harness must add

A **closed-loop** driver (each thread waits for a response before
sending the next request) measures maximum throughput but hides
queueing — when the system stalls, the load politely stops, and the
tail latencies a real **open-loop** client (requests arrive on a
fixed schedule regardless of responses) would suffer are never
recorded — the coordinated-omission problem from reading-ycsb.md.
The OLTP-Bench paper's three contributions are exactly the fixes:

1. **Rate control as a first-class knob** — closed-loop (max speed),
   open-loop (fixed rate, honest tails), and phases that change the
   rate/mix mid-run (diurnal patterns). Most homegrown harnesses
   have only closed-loop.
2. **Benchmark = workload descriptor, not code fork** — transaction
   weights in XML (`config/postgres/sample_tpcc_config.xml`), so
   "TPC-C but 100% NewOrder" is a config edit, not a patched driver.
3. **Everything is measured the same way** — one histogram, one
   sampling story across ~20 benchmarks; comparisons are apples to
   apples.

The cost of skipping this: any tail-latency claim from a
closed-loop run understates real p999, sometimes by orders of
magnitude — the higher (open-loop) number is the honest one.

### Step 6 — TPC-C vs YCSB-A: two different contentions

Both are "write-heavy contended workloads", but they exercise
different machinery:

- YCSB-A zipfian: skewed READS+UPDATES on independent keys — no
  transaction spans keys; MVCC barely matters.
- TPC-C NewOrder: multi-statement transaction, read-modify-write on
  a hot counter + ~10 item updates — THIS is what write-skew,
  2PL queues, and MVCC abort rates (topic 8) are about.

If your concurrency-control claim (isolation levels, abort rates,
lock queues) is backed only by YCSB, it's backed by nothing —
YCSB-A can't even express the anomaly the claim is about.

## How to read the paper (with the concepts in hand)

VLDB 2013, ~12 pages; §3 is the part that aged well:

- **§1–2** Motivation + benchmark taxonomy — skim; the "everyone
  hand-rolls a broken harness" complaint is still true.
- **§3 — read carefully.** Harness architecture: the worker/driver
  split, rate control and phases (Step 5), the workload-descriptor
  design. This is a checklist for M22's own driver.
- **§4–5** The benchmark catalog and demo experiments — skim; note
  which of the ~20 benchmarks map to which contention pattern
  (TPC-C = Step 2's designed hot rows, YCSB = Step 6's independent
  keys, TATP/SmallBank in between).
- Then open BenchBase (the maintained fork) with Step 4's anchor
  table: `TPCCWorker.java` for think times, `TPCCUtil.java` for
  NURand, `TPCCConfig.java` for the weights, and the XML config to
  see contribution 2 in the flesh.

## Questions (answer in notes.md)

1. D_NEXT_O_ID: under MVCC-OCC (topic 8's stub), what abort rate do
   you expect at 4 warehouses × 16 threads, closed loop? What
   changes with per-district queues (topic 9)?
2. Why must load-time and run-time C_LAST constants differ
   (TPCCUtil:94)? What cheat does the constraint block?
3. Design "TPC-C for graphs": what's the hot-counter analogue in a
   social-network write workload (hint: supernode edge appends,
   topic 13)?
4. Open vs closed loop on workload E (our scan-heavy mix): which
   reports the higher p999, and why is that the honest one?
5. OLTP-Bench's phased rates: sketch the config that reproduces a
   cache-warmup-then-spike incident (topic 6's eviction storm).

## References

**Papers**
- Difallah, Pavlo, Curino, Cudré-Mauroux — "OLTP-Bench: An
  Extensible Testbed for Benchmarking Relational Databases" (VLDB
  2013) — §3 (harness architecture, rate control) is the part that
  aged well

**Code**
- [benchbase](https://github.com/cmu-db/benchbase) — the maintained
  fork; `src/main/java/com/oltpbenchmark/benchmarks/tpcc/`
  (`TPCCWorker.java`, `TPCCUtil.java`, `TPCCConfig.java`) and
  `config/postgres/sample_tpcc_config.xml`
