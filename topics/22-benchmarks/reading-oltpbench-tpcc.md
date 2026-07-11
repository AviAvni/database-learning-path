# Reading guide — "OLTP-Bench" (VLDB 2013) + BenchBase TPC-C

Difallah/Pavlo/Curino/Cudré-Mauroux; the maintained fork is CMU's
BenchBase: `~/repos/benchbase`. One harness, ~20 benchmarks
(`src/main/java/com/oltpbenchmark/benchmarks/` — tpcc, ycsb, tatp,
smallbank, twitter, seats, wikipedia, chbenchmark …), one config
format, per-phase rate control.

## The harness's three contributions

1. **Rate control as a first-class knob** — closed-loop (max speed),
   open-loop (fixed rate, honest tails), and phases that change the
   mix mid-run (diurnal patterns). Most homegrown harnesses have
   only closed-loop, which hides queueing (see the coordinated-
   omission note in reading-ycsb.md).
2. **Benchmark = workload descriptor, not code fork** — transaction
   weights in XML (`config/postgres/sample_tpcc_config.xml`), so
   "TPC-C but 100% NewOrder" is a config edit.
3. **Everything is measured the same way** — one histogram, one
   sampling story across 20 benchmarks; comparisons are apples to
   apples.

## TPC-C in 60 seconds (via `benchmarks/tpcc/`)

Five transactions, weighted 45/43/4/4/4: NewOrder, Payment,
OrderStatus, Delivery, StockLevel. Contention is BY DESIGN:

```
  warehouse (W of them) ← every NewOrder updates its W_YTD row-ish
      └─ district (10/W)   ← D_NEXT_O_ID: THE hot counter, serializes
           └─ orders           NewOrders within a district
  ~1% NewOrders touch a REMOTE warehouse ⇒ cross-shard txns exist
  ~1% NewOrders ABORT by spec (rollback path must be exercised)
```

| anchor | what |
|---|---|
| `TPCCWorker.java:85-100` | keying + think times: `-log(c)·mean`, capped at 10× — the spec's human simulator |
| `TPCCUtil.java:94-116` | `NURand` non-uniform randoms; note :94's constraint on `C_LAST_LOAD_C` vs `C_LAST_RUN_C` (157/223) — load-time and run-time skew must DIFFER by spec |
| `TPCCConfig.java` | the 45/43/4/4/4 weights |

**Why nobody runs it honestly**: with think times, one warehouse
supports ~12.86 tpmC max — spec-compliant runs need thousands of
warehouses (= huge data) to post big numbers. Everyone strips think
times and runs 4 warehouses ⇒ they're benchmarking the D_NEXT_O_ID
latch, not the engine. "tpmC" without an audit is a vibe.

## TPC-C vs YCSB-A — what each contention is

- YCSB-A zipfian: skewed READS+UPDATES on independent keys — no
  transaction spans keys; MVCC barely matters.
- TPC-C NewOrder: multi-statement transaction, read-modify-write on
  a hot counter + ~10 item updates — THIS is what write-skew,
  2PL queues, and MVCC abort rates (topic 8) are about.

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
