# TPC-C: contention by design (and the harness that runs it honestly)

TPC-C doesn't measure throughput — it measures how an engine
behaves when the workload deliberately funnels transactions through
hot rows. This chapter builds that idea step by step: what an OLTP
benchmark even measures, where TPC-C's contention is planted, what
the spec *actually* mandates about the transaction mix (less than
everyone says), the two devices that stop you cheating around the
skew, and why almost nobody runs it honestly — then reads the
OLTP-Bench paper for what a fair OLTP harness must do, with the code
anchors in the maintained successor, CMU's BenchBase.

Clause numbers are **TPC-C Standard Specification revision 5.11.0**.
The paper is **Difallah, Pavlo, Curino, Cudré-Mauroux, "OLTP-Bench:
An Extensible Testbed for Benchmarking Relational Databases", PVLDB
Vol. 7 No. 4 (copyright 2013), presented at the 40th VLDB, September
2014** — cite the volume, not a year, because both years are
defensible and neither is unambiguous. Java and XML line numbers
belong to **cmu-db/benchbase@33c0047**; `OLTP-Bench` and `BenchBase`
are named separately below wherever they differ.

## The problem in one sentence

Strip TPC-C's mandated think times and run 4 warehouses instead of
the thousands the spec forces, and your "tpmC" number measures one
contended counter's row lock, not the engine — which is exactly what
most informal "TPC-C" results do, because a spec-compliant warehouse
supports only **12.86 tpmC** and the spec says so itself.

## The concepts, step by step

### Step 1 — what an OLTP benchmark measures: contention, not speed

> **In:** nothing but the acronym.
> **Out:** the definition of contention, and the reason a benchmark without
> it measures a different machine than the one you are buying.

**OLTP** (online transaction processing) is many small concurrent
read-write transactions — the opposite of TPC-H's few big read-only
scans. Its performance is limited by **contention**: multiple
transactions needing the *same rows* at the same time, forcing the
engine to serialize them behind locks or to abort and retry (topic
8's concurrency control).

An OLTP benchmark with no contention measures how fast you can hash
keys — YCSB territory (Step 7). TPC-C's design question is the
opposite: *given* deliberately contended rows, multi-statement
transactions and mandatory aborts, how much throughput survives? That
is a property of the concurrency-control design, not of the per-op
code path — which is why the same engine can win YCSB and lose
TPC-C, and why the two numbers are not comparable in either
direction.

### Step 2 — the anatomy: where the contention is planted

> **In:** Step 1's definition.
> **Out:** the exact table cardinalities from Clause 4.2.2, the SQL statement
> that serializes a district, and the derived fraction of transactions that
> actually cross a warehouse boundary — which is not the number you have
> heard.

TPC-C models order entry over a fixed hierarchy. Clause 4.2.2 sets
the cardinalities **per warehouse**, and BenchBase mirrors them:

```java
// benchbase src/main/java/com/oltpbenchmark/benchmarks/tpcc/TPCCConfig.java — the cardinalities, 32-38
    32    public static final int configWhseCount = 1;
    33    public static final int configItemCount = 100000; // tpc-c std = 100,000
    34    public static final int configDistPerWhse = 10; // tpc-c std = 10
    35    public static final int configCustPerDist = 3000; // tpc-c std = 3,000
    36
    37    /** An invalid item id used to rollback a new order transaction. */
    38    public static final int INVALID_ITEM_ID = -12345;
```

```
  per warehouse:  10 districts × 3,000 customers = 30,000 customers
                  100,000 stock rows
                  10 terminals                         (Clause 4.2.2)
  fixed:          100,000 items, shared by all warehouses
```

Contention is by design, and it has a specific SQL shape. Every
New-Order must claim the next order id from its district's counter,
and BenchBase takes an explicit row lock to do it:

```java
// benchbase .../tpcc/procedures/NewOrder.java — the district counter, 55-62
    55    public final SQLStmt stmtGetDistSQL =
    56        new SQLStmt(
    57            """
    58          SELECT D_NEXT_O_ID, D_TAX
    59            FROM %s
    60           WHERE D_W_ID = ? AND D_ID = ? FOR UPDATE
    61      """
    62                .formatted(TPCCConstants.TABLENAME_DISTRICT));
```

`FOR UPDATE` on line 60 is the whole benchmark in one clause: every
New-Order in a district takes an exclusive lock on that district's
single row, holds it for the rest of the transaction, and increments
`D_NEXT_O_ID` before committing. New-Orders within a district are
*forcibly serialized*. With 10 districts per warehouse, **warehouse
count directly caps New-Order parallelism at 10 × W**.

Two more devices stop you from optimizing the contention away:

```java
// benchbase .../tpcc/procedures/NewOrder.java — remote lines and the mandated rollback, 147-168 (elided)
   147      int numItems = TPCCUtil.randomNumber(5, 15, gen);
   // ... 148-152: per-item arrays, allLocal = 1 ...
   153      for (int i = 0; i < numItems; i++) {
   154        itemIDs[i] = TPCCUtil.getItemID(gen);
   155        if (TPCCUtil.randomNumber(1, 100, gen) > 1) {
   156          supplierWarehouseIDs[i] = terminalWarehouseID;
   157        } else {
   // ... 158-160: pick a different warehouse ...
   161          allLocal = 0;
   162        }
   // ... 163-164: order quantity ...
   165      }
   166      // we need to cause 1% of the new orders to be rolled back.
   167      if (TPCCUtil.randomNumber(1, 100, gen) == 1) {
   168        itemIDs[numItems - 1] = TPCCConfig.INVALID_ITEM_ID;
   169      }
```

Line 147 is Clause 2.4.1.3 ("The number of items in the order
(ol_cnt) is randomly selected within [5 .. 15] (an average of 10)").
Line 155 is Clause 2.4.1.5 item 2 ("A supplying warehouse number is
selected as the home warehouse 99% of the time and as a remote
warehouse 1% of the time … generating a random number x within
[1 .. 100]"). Lines 166-168 are Clause 2.4.1.4 ("A fixed 1% of the
New-Order transactions are chosen at random to simulate user data
entry errors and exercise the performance of rolling back update
transactions"), and Clause 5.2.5.x requires the observed rate to land
in 0.9%–1.1%.

**Now the arithmetic everyone skips.** "1% remote" is a property of
*order lines*, not of transactions. An order has 10 lines on average,
so:

```
  P(a New-Order is entirely local) = 0.99^10 = 0.9044
  P(a New-Order crosses a warehouse) = 1 − 0.9044 = 0.0956 = 9.56%
```

Clause 2.4.1.5's own Comment 1 confirms it: "With an average of 10
items per order, approximately **90%** of all orders can be supplied
in full by stocks from the home warehouse."

And New-Order is not even the main source of cross-warehouse traffic.
Clause 2.5.1.2 puts Payment's customer in a remote warehouse **15%**
of the time (validated to 14–16% by Clause 5.2.5.x), and Payment is
43% of the mix. So, using Step 3's weights:

```
  distributed New-Order:  0.45 × 0.0956 = 0.0430  = 4.30% of all transactions
  distributed Payment:    0.43 × 0.15   = 0.0645  = 6.45% of all transactions
                                                    ─────
  total distributed:                                10.75%
```

**Payment contributes half again as much cross-partition traffic as
New-Order does.** Any "we partition by warehouse, so only 1% of
transactions are distributed" claim is wrong by an order of
magnitude, and wrong about which transaction to look at.

### Step 3 — the transaction mix: what the spec mandates, and what it doesn't

> **In:** Step 2's transaction types.
> **Out:** Clause 5.2.3's table — which has a blank where everyone quotes a
> 45 — the 23-card deck that produces the folk numbers, and the file in
> BenchBase where the weights actually live.

Everyone writes TPC-C's mix as "45/43/4/4/4". The spec does not.
Clause 5.2.3 gives **minimum percentages**, and New-Order's entry is
empty:

```
  transaction    minimum % of mix        (Clause 5.2.3)
  New-Order      n/a  ← footnote 1: "There is no minimum for the New-Order
                          transaction as its measured rate is the reported
                          throughput"
  Payment        43.0
  Order-Status    4.0
  Delivery        4.0
  Stock-Level     4.0
                 ─────
  mandated floor 55.0  ⇒  New-Order is the residual, at most 45%
```

New-Order is a *residual*, not a mandate: you must run at least 43%
Payment and at least 4% of each of the other three, and New-Order is
whatever is left — at most 45%. The familiar figure comes from
Clause 5.2.4.2's alternative selection method, a shuffled deck of
cards:

```
  a deck of 23 cards: 10 New-Order, 10 Payment, 1 Order-Status,
                      1 Delivery, 1 Stock-Level      (Clause 5.2.4.2)

  New-Order   = 10/23 = 43.478%
  Payment     = 10/23 = 43.478%
  each other  =  1/23 =  4.348%
                        ───────
                        99.998% (rounding)
```

So a compliant run's New-Order share is between 43.478% (deck) and
45% (residual ceiling), and Step 5's tpmC derivation uses 45%
because the spec's own worked example does.

The weights are **not** in `TPCCConfig.java` — that file is 39 lines
and holds only Step 2's cardinalities. They live in the workload
descriptor, which is OLTP-Bench's second contribution (Step 6) made
concrete:

```xml
<!-- benchbase config/postgres/sample_tpcc_config.xml — the workload, 14-25 -->
    14      <!-- Scale factor is the number of warehouses in TPCC -->
    15      <scalefactor>1</scalefactor>
    16
    17      <!-- The workload -->
    18      <terminals>1</terminals>
    19      <works>
    20          <work>
    21              <time>60</time>
    22              <rate>10000</rate>
    23              <weights>45,43,4,4,4</weights>
    24          </work>
    25      </works>
```

Line 15's comment is the one to internalize: in BenchBase,
**`scalefactor` is the warehouse count**, and the shipped sample is
**one** warehouse with **one** terminal, a 60-second run and a target
rate of 10,000 tps (line 22 — effectively "as fast as possible").
Step 5 shows what those three numbers do to the metric.

### Step 4 — NURand: skew you cannot preload away

> **In:** Step 2's item and customer lookups.
> **Out:** the exact NURand formula from Clause 2.1.6, why its `A` constants
> are all one less than a power of two, a computed measure of how much skew
> it actually produces, and the constraint on the load-vs-run constants that
> the existing folklore states backwards.

**NURand** is TPC-C's non-uniform random function. Clause 2.1.6:

```
  NURand(A, x, y) = (((random(0, A) | random(x, y)) + C) % (y − x + 1)) + x

  used as:  NURand(1023, 1, 3000)     for C_ID     (customer id)
            NURand(255,  0,  999)     for C_LAST   (customer last name)
            NURand(8191, 1, 100000)   for OL_I_ID  (item id)
```

BenchBase implements it in one line:

```java
// benchbase src/main/java/com/oltpbenchmark/benchmarks/tpcc/TPCCUtil.java — NURand, 119-125
   119    public static int randomNumber(int min, int max, Random r) {
   120      return (int) (r.nextDouble() * (max - min + 1) + min);
   121    }
   122
   123    public static int nonUniformRandom(int A, int C, int min, int max, Random r) {
   124      return (((randomNumber(0, A, r) | randomNumber(min, max, r)) + C) % (max - min + 1)) + min;
   125    }
```

**Why bitwise OR, and how much skew it buys.** Every `A` is one less
than a power of two — 255 = 2⁸−1, 1023 = 2¹⁰−1, 8191 = 2¹³−1 — so
`random(0, A)` is a uniform bit pattern of exactly k bits. ORing it
into the draw forces those k low bits toward 1: each is 1 unless
*both* sources have a 0 there. For item ids, k = 13:

```
  P(a given low bit is 1)     uniform: 1/2  = 0.5
                              NURand:  3/4  = 0.75

  E[number of the 13 low bits set]
                              uniform: 13 × 0.5  =  6.5
                              NURand:  13 × 0.75 =  9.75

  P(all 13 low bits set)      uniform: (1/2)^13 = 0.000122
                              NURand:  (3/4)^13 = 0.023763
  ratio                                          = 195×
```

Item ids whose low 13 bits are mostly ones are drawn up to two orders
of magnitude more often than a uniform generator would. That is the
hot region — spread through the id space by the `+ C` offset and the
modulo, so it is not a contiguous range you can pin in cache by
sorting.

**The constraint the folklore gets backwards.** Clause 2.1.6.1
requires the C used at load time and the C used at run time to
*differ*, but within a window — and BenchBase quotes the clause in
the source:

```java
// benchbase src/main/java/com/oltpbenchmark/benchmarks/tpcc/TPCCUtil.java — the four constants, 92-97
    92    private static final int OL_I_ID_C = 7911; // in range [0, 8191]
    93    private static final int C_ID_C = 259; // in range [0, 1023]
    94    // NOTE: TPC-C 2.1.6.1 specifies that abs(C_LAST_LOAD_C - C_LAST_RUN_C) must
    95    // be within [65, 119]
    96    private static final int C_LAST_LOAD_C = 157; // in range [0, 255]
    97    private static final int C_LAST_RUN_C = 223; // in range [0, 255]
```

The full clause is slightly stricter than the comment: the delta must
be **in [65..119] and must not equal 96 or 112**. So 157 and 223 are
not "values that work while others don't" — they are one arbitrary
admissible pair, and what makes them admissible is the *difference*:

```
  C-Delta = |C_LAST_RUN_C − C_LAST_LOAD_C| = |223 − 157| = 66
  66 ∈ [65, 119] ✓     66 ≠ 96 ✓     66 ≠ 112 ✓

  (0, 66), (100, 200) [delta 100], (255, 190) [delta 65] are equally legal;
  (157, 253) [delta 96] and (157, 45) [delta 112] are not.
```

The exclusions of 96 and 112 exist because those two deltas make the
run-time hot set overlap the load-time hot set more than the spec
tolerates. The point of the rule is that the loader must not know
which rows the run will hammer — otherwise a vendor could physically
cluster exactly the hot customers, and measure a cache instead of a
database. Lines 86-91 of the same file admit BenchBase does not do
this properly: "TODO: … the constants … are supposed to be selected
ONCE and reused. We just hardcode one selection of parameters here,
but we should generate these each time."

The lesson generalizes past TPC-C: **a benchmark's data loader and
its runtime driver must not share the knowledge that lets one flatter
the other.**

### Step 5 — keying and think times, and where 12.86 tpmC comes from

> **In:** Step 3's mix and Step 2's 10 terminals per warehouse.
> **Out:** the per-transaction wait table from Clause 5.2.5.7, the derivation
> of the spec's own 12.86 tpmC per warehouse, and the checked-in file that
> proves nobody runs it.

The spec does not simulate a client library; it simulates a human at
a terminal. Clause 5.2.5.7's table gives, per transaction type, a
**minimum keying time** (typing the input before the transaction
starts) and a **minimum mean think time** (staring at the result
before starting the next one):

```
  transaction   mix %   keying (s)   90th-%ile RT (s)   mean think (s)
  New-Order     n/a       18.0             5.0               12.0
  Payment      43.0        3.0             5.0               12.0
  Order-Status  4.0        2.0             5.0               10.0
  Delivery      4.0        2.0             5.0                5.0
  Stock-Level   4.0        2.0            20.0                5.0
                                              — Clause 5.2.5.7
```

Think time is exponential and capped, per Clause 5.2.5.4: "Tt =
−log(r) × μ", natural log, r uniform on (0,1), and "each distribution
may be truncated at 10 times its mean value". BenchBase implements
exactly that:

```java
// benchbase src/main/java/com/oltpbenchmark/benchmarks/tpcc/TPCCWorker.java — the human simulator, 83-101
    83    @Override
    84    protected long getPreExecutionWaitInMillis(TransactionType type) {
    85      // TPC-C 5.2.5.2: For keying times for each type of transaction.
    86      return type.getPreExecutionWait();
    87    }
    88
    89    @Override
    90    protected long getPostExecutionWaitInMillis(TransactionType type) {
    91      // TPC-C 5.2.5.4: For think times for each type of transaction.
    92      long mean = type.getPostExecutionWait();
    93
    94      float c = this.getBenchmark().rng().nextFloat();
    95      long thinkTime = (long) (-1 * Math.log(c) * mean);
    96      if (thinkTime > 10 * mean) {
    97        thinkTime = 10 * mean;
    98      }
    99
   100      return thinkTime;
   101    }
```

Line 95 is the spec's formula and 96-98 its 10× truncation.

**Deriving 12.86 tpmC per warehouse.** Every input is in the table
above plus Clause 4.2.2's ten terminals. A terminal's average cycle
is keying + think, weighted by the mix (using 45% New-Order, as the
spec's own comment does):

```
  New-Order    0.45 × (18 + 12) = 0.45 × 30 = 13.50 s
  Payment      0.43 × ( 3 + 12) = 0.43 × 15 =  6.45 s
  Order-Status 0.04 × ( 2 + 10) = 0.04 × 12 =  0.48 s
  Delivery     0.04 × ( 2 +  5) = 0.04 ×  7 =  0.28 s
  Stock-Level  0.04 × ( 2 +  5) = 0.04 ×  7 =  0.28 s
                                              ───────
  mean cycle per terminal                      20.99 s   (response time ≈ 0)

  transactions per minute per terminal  = 60 / 20.99      = 2.8585
  New-Orders  per minute per terminal   = 2.8585 × 0.45   = 1.28633
  terminals per warehouse               = 10             (Clause 4.2.2)
  New-Orders per minute per warehouse   = 12.8633        → 12.86 tpmC
```

Clause 4.1.3's Comment states the same figure — "computed to be
**12.86 tpmC** per warehouse" — and adds the floor: a reported
throughput may not fall below **9 tpmC per warehouse**, which is 70%
of the maximum. That floor is what forces scale: a 1,000,000 tpmC
result needs at least 1,000,000 / 12.86 ≈ **77,760 warehouses**, and
at ~100 MB of data per warehouse that is several terabytes before the
first transaction runs. The metric secretly includes "how much
hardware and data can you bring".

**And here is the proof that nobody runs it.** BenchBase's shipped
sample config contains the spec's keying and think times — commented
out:

```xml
<!-- benchbase config/postgres/sample_tpcc_config.xml — the human simulator, disabled, 28-43 -->
    28      <transactiontypes>
    29          <transactiontype>
    30              <name>NewOrder</name>
    31              <!--<preExecutionWait>18000</preExecutionWait>-->
    32              <!--<postExecutionWait>12000</postExecutionWait>-->
    33          </transactiontype>
    34          <transactiontype>
    35              <name>Payment</name>
    36              <!--<preExecutionWait>3000</preExecutionWait>-->
    37              <!--<postExecutionWait>12000</postExecutionWait>-->
    38          </transactiontype>
    39          <transactiontype>
    40              <name>OrderStatus</name>
    41              <!--<preExecutionWait>2000</preExecutionWait>-->
    42              <!--<postExecutionWait>10000</postExecutionWait>-->
    43          </transactiontype>
```

18000 / 12000 / 3000 / 12000 / 2000 / 10000 milliseconds — the exact
Clause 5.2.5.7 values, present, correct, and inside XML comments (the
same pattern continues for Delivery and Stock-Level at 44-53). With
them enabled, the sample's single warehouse and single terminal would
produce about 1.29 New-Orders per minute. With them commented out and
`<rate>10000</rate>`, the same config runs a closed loop as fast as
one thread can, against one district-counter row per district.

That is why informal TPC-C numbers are a measurement of the
`FOR UPDATE` on `NewOrder.java:60` and its surrounding lock queue,
not of the engine. Which is a perfectly good thing to measure — as
long as you say that is what you measured.

### Step 6 — tpmC: New-Order transactions only

> **In:** Steps 3 and 5.
> **Out:** the exact definition of the reported metric, and the two ways
> people compute it wrongly.

Clause 5.4.2 defines the reported throughput as the **total number of
completed New-Order transactions** during the measurement interval,
divided by that interval's elapsed time in minutes. Clause 5.4.3
names the unit tpmC and Clause 5.4.4 truncates it to zero decimals.

Two consequences that catch people:

- **The other four transaction types contribute nothing to the
  number.** They are mandatory — the minimums in Step 3 exist so you
  cannot skip the expensive ones — but Payment, Order-Status,
  Delivery and Stock-Level are *load*, not *score*. A harness that
  reports "total transactions per minute" is reporting roughly
  1/0.45 ≈ 2.22× the tpmC. This is the single most common error in
  informal results.
- **Rolled-back New-Orders still count.** Step 2's mandated 1%
  rollback (Clause 2.4.1.4) produces transactions that end in an
  abort by design, and Clause 5.4.2 counts them as completed. An
  implementation that quietly retried them until they succeeded would
  be inflating its own denominator *and* skipping the rollback path
  the clause exists to exercise.

### Step 7 — rate control: what an honest harness adds

> **In:** Step 5's closed-loop sample config.
> **Out:** the OLTP-Bench paper's three system models, the experiment that
> shows why rate control matters, and the pointer to where this repo measures
> the size of the lie.

A **closed-loop** driver has each thread wait for a response before
sending the next request. It measures maximum throughput honestly and
hides queueing dishonestly: when the system stalls, the load politely
stops, so the tail latencies a real **open-loop** client would suffer
are never recorded. The OLTP-Bench paper's §3.2 names all three
options:

> "OLTP-Bench supports three different system models for Workers to
> invoke transactions: (1) closed-loop, (2) open-loop, and (3)
> semi-open-loop. … In closed-loop testing, OLTP-Bench initializes a
> fixed number of Workers that repeatedly issue transactions with a
> random think time between each request. With the open-loop execution
> setting, the rate at which requests are invoked follows a stochastic
> process. Lastly, under a semi-open policy, the system acts
> essentially as an open-system with the difference that the Worker
> pauses for a random think time before submitting a new transaction."

That is requirement **R2** of the eight the paper lists in §2, and
**R3** is "Fine-Grained Rate Control: the ability to control request
rates with great precision (since even small oscillations of the
throughput can make the interpretation of results difficult)". §6.1
demonstrates it: MySQL running Wikipedia over 100k articles, starting
at 25 transactions per second and increasing by 25 tps every 10
seconds. Delivered throughput tracks the target exactly until roughly
680 seconds in, when the DBMS saturates — and at that point the
95th-percentile latency crosses one second. A closed-loop run would
have shown the throughput plateau and nothing else.

Do not restate the mechanism of coordinated omission here: **topic 34
owns it and measured it**. FINDINGS.md row 34 — a closed-loop
benchmark reports **p99 = 1.0 µs** where an open-loop one reports
**90 ms** on identical work, a **90,000× lie**. That is the number to
cite whenever someone quotes a closed-loop p999, including the one
this topic's own YCSB driver produces
([reading-ycsb.md](reading-ycsb.md) Step 5).

The paper's other durable contributions:

1. **Benchmark = workload descriptor, not code fork.** Step 3's
   `<weights>45,43,4,4,4</weights>` means "TPC-C but 100% New-Order"
   is a config edit. BenchBase ships 19 such sample configs in
   `config/postgres/` — auctionmark, chbenchmark, epinions, hyadapt,
   noop, otmetrics, resourcestresser, seats, sibench, smallbank,
   tatp, templated, tpcc, tpcds, tpch, twitter, voter, wikipedia,
   ycsb. The paper itself describes **15** implemented benchmarks
   (§1); the extras arrived after it.
2. **Phases.** §3.1 specifies workload parameters *per phase*, so
   rate, mix and worker count can change mid-run — diurnal patterns
   and spikes without restarting.
3. **One measurement path for everything.** §6.2 runs TPC-C at
   saturation and reports per-transaction-type breakdowns; the
   finding is that "although the NewOrder and Payment transactions
   represent the majority of the transaction in the TPC-C workload,
   the **Delivery** transaction has the most significant impact on
   the overall system response time" — a result you cannot even see
   without per-class reporting, and one that Step 3's 4% minimum for
   Delivery exists to preserve.

### Step 8 — TPC-C vs YCSB-A: two different contentions

> **In:** Steps 2 and 7.
> **Out:** the reason a concurrency-control claim backed by YCSB is backed
> by nothing.

Both are "write-heavy contended workloads", but they exercise
different machinery:

- **YCSB-A zipfian**: skewed reads and updates on *independent* keys.
  No operation spans two keys, so there is no transaction to
  serialize; the contention is cache-line and lock-striping
  contention, and MVCC barely matters.
- **TPC-C New-Order**: one multi-statement transaction containing a
  `SELECT … FOR UPDATE` on a hot counter, an update of it, and 5-15
  stock updates — with a mandated 1% abort and a ~9.6% chance of
  touching a second warehouse. This is what write-skew, lock queues
  and MVCC abort rates (topic 8) are actually about.

If your isolation-level, abort-rate or lock-queue claim is backed
only by YCSB, it is backed by nothing: YCSB-A cannot express the
anomaly the claim is about.

## How to read the paper (with the concepts in hand)

PVLDB 7(4), 12 pages. §3 is the part that aged well; §6 is where the
methodology arguments are demonstrated rather than asserted.

- **§1-2** Motivation and the ten requirements R1-R10 — read §2's
  requirement list carefully and skim the rest. R2 (open/closed/
  semi-open), R3 (fine-grained rate control) and R4 (mixed and
  evolving workloads) are Step 7; R10 (repeatability and
  verification) is the reason the config file is the benchmark.
- **§3 — read carefully.** §3.1 Workload Manager (per-phase
  parameters; the reported ceiling of "12.5k transactions per second
  per Worker thread" on a main-memory DBMS is the driver-cost figure
  to remember), §3.2 Workload Generation (the three system models),
  §3.3 the SQL-dialect manager, §3.4 distributed clients, §3.5
  statistics collection. This is a checklist for M22's own driver.
- **§4** The benchmark catalog — skim, but note §4.1.2's CH-benCHmark
  (TPC-C plus 22 analytical queries), which is the bridge between
  this guide and [reading-boncz-tpch.md](reading-boncz-tpch.md).
- **§6 — read 6.1 and 6.2.** 6.1 is the rate-control experiment
  (Step 7); 6.2 is multi-class reporting and the Delivery finding.
- Then open BenchBase with Step 5's anchor table:
  `TPCCWorker.java:83-101` for the human simulator,
  `TPCCUtil.java:92-125` for NURand and its constants,
  `procedures/NewOrder.java:55-62` for the `FOR UPDATE`, and
  `config/postgres/sample_tpcc_config.xml` for the weights and the
  commented-out think times.

## Questions (answer in notes.md)

1. `D_NEXT_O_ID`: under MVCC-OCC (topic 8's stub), what abort rate do
   you expect at 4 warehouses × 16 threads, closed loop? Step 2 caps
   concurrent New-Orders at 10 × W = 40 districts — does 16 threads
   even reach the ceiling? What changes with per-district queues
   (topic 9)?
2. Step 4 showed `|223 − 157| = 66` is admissible under Clause
   2.1.6.1's [65..119] window. Pick a *different* legal pair, and
   explain what cheat the constraint blocks — and what
   `TPCCUtil.java:86-91`'s TODO means for BenchBase's compliance.
3. Design "TPC-C for graphs": what is the `D_NEXT_O_ID` analogue in a
   social-network write workload (hint: supernode edge appends, topic
   13)? What plays the role of the 1% remote warehouse, and what
   fraction of transactions would actually cross a partition?
4. Step 2 derived 10.75% of TPC-C transactions as cross-warehouse,
   with Payment contributing more than New-Order. Redo that
   derivation for a partitioning scheme that shards by *district*
   instead of warehouse. Does it get better or worse?
5. OLTP-Bench's phased rates (§3.1): sketch the config that
   reproduces a cache-warmup-then-spike incident (topic 6's eviction
   storm), and say which of R1-R4 each phase exercises.

## Done when

Answer each before unfolding it.

- [ ] You can explain that an OLTP benchmark measures contention, identify TPC-C's hot row, and name the SQL clause that serializes it.

  <details><summary>Answer</summary>

  Contention is multiple transactions needing the same rows at once, forcing
  the engine to serialize behind locks or abort and retry. A benchmark without
  it measures the per-op code path, not the concurrency-control design.

  TPC-C's hot row is the district's `D_NEXT_O_ID` counter — one row per
  district, 10 districts per warehouse (Clause 4.2.2). BenchBase serializes it
  explicitly: `NewOrder.java:60` ends `WHERE D_W_ID = ? AND D_ID = ? FOR
  UPDATE`, an exclusive row lock held for the rest of the transaction. So
  New-Order parallelism is capped at 10 × W, and warehouse count *is* the
  parallelism dial.

  </details>

- [ ] You can state what the spec actually mandates about the transaction mix, and where the 45/43/4/4/4 figures come from.

  <details><summary>Answer</summary>

  Clause 5.2.3 gives **minimums**, and New-Order's cell is "n/a" — footnote 1:
  "There is no minimum for the New-Order transaction as its measured rate is
  the reported throughput". The mandated floors are Payment 43.0%,
  Order-Status 4.0%, Delivery 4.0%, Stock-Level 4.0% — 55% total, leaving
  New-Order as a residual of at most 45%.

  The familiar numbers come from Clause 5.2.4.2's alternative: a shuffled deck
  of **23 cards** — 10 New-Order, 10 Payment, 1 each of the rest — giving
  10/23 = 43.478% for the first two and 1/23 = 4.348% for the others. A
  compliant New-Order share is therefore between 43.478% and 45%.

  In BenchBase the weights are in `config/postgres/sample_tpcc_config.xml:23`,
  not in `TPCCConfig.java`, which is 39 lines of table cardinalities.

  </details>

- [ ] You can explain NURand, quantify the skew it produces, and state the constraint on its load-time and run-time constants correctly.

  <details><summary>Answer</summary>

  Clause 2.1.6: `NURand(A, x, y) = (((random(0,A) | random(x,y)) + C) %
  (y−x+1)) + x`, with A = 255 for C_LAST, 1023 for C_ID and 8191 for OL_I_ID.
  Every A is 2^k − 1, so `random(0,A)` is a uniform k-bit pattern and the OR
  forces the low k bits toward 1: each is 1 with probability 3/4 instead of
  1/2. For items (k=13), all-13-bits-set goes from (1/2)^13 = 0.000122 to
  (3/4)^13 = 0.023763 — **195× more likely**. `+ C` and the modulo scatter that
  hot region so it is not a contiguous range you can pin in cache.

  Clause 2.1.6.1 requires `|C_load − C_run|` to be **in [65..119], excluding 96
  and 112**. BenchBase's 157 and 223 (`TPCCUtil.java:96-97`) satisfy it because
  their difference is 66 — the values are arbitrary, the *delta* is what the
  spec constrains. The rule stops the loader from physically clustering the
  rows the run will hammer. `TPCCUtil.java:86-91` admits the constants should
  be re-drawn per run and are hardcoded instead.

  </details>

- [ ] You can derive 12.86 tpmC per warehouse from the spec's own tables, and say what removing think times changes.

  <details><summary>Answer</summary>

  Inputs: Clause 5.2.5.7's keying and mean-think times, Step 3's 45/43/4/4/4
  mix, and Clause 4.2.2's 10 terminals per warehouse. The mean terminal cycle
  is 0.45×(18+12) + 0.43×(3+12) + 0.04×(2+10) + 0.04×(2+5) + 0.04×(2+5) =
  13.50 + 6.45 + 0.48 + 0.28 + 0.28 = **20.99 s**. So 60/20.99 = 2.8585
  transactions per minute per terminal, × 0.45 = 1.28633 New-Orders, × 10
  terminals = **12.86 tpmC per warehouse** — the figure Clause 4.1.3's Comment
  states. Clause 4.1.3 also sets a floor of 9 tpmC/warehouse (70% of maximum),
  so a 1,000,000 tpmC result needs ≥ 77,760 warehouses.

  Removing think times removes the only thing bounding request rate, so the
  run becomes a closed loop at full speed against 10 × W district rows. You
  are then measuring the `FOR UPDATE` lock queue. `sample_tpcc_config.xml:31-52`
  makes this visible: the spec's 18000/12000/3000/12000/2000/10000/2000/5000/
  2000/5000 ms values are present and commented out.

  </details>

- [ ] You can state exactly what tpmC counts, and name the two common ways of computing it wrongly.

  <details><summary>Answer</summary>

  Clause 5.4.2: the reported throughput is the **number of completed New-Order
  transactions** in the measurement interval divided by its length in minutes.
  Clause 5.4.3 names it tpmC; 5.4.4 truncates to zero decimals.

  Wrong way one: counting all five transaction types. That inflates the figure
  by about 1/0.45 = 2.22×. The other four are mandatory load, not score.
  Wrong way two: excluding the 1% of New-Orders that roll back by design
  (Clause 2.4.1.4). Clause 5.4.2 counts them as completed, and retrying them
  until they commit both inflates the count and skips the rollback path the
  clause exists to exercise.

  </details>

- [ ] You can name the three system models OLTP-Bench supports, and cite this repo's measurement of what choosing wrongly costs.

  <details><summary>Answer</summary>

  §3.2: **closed-loop** (a fixed number of Workers, each issuing the next
  transaction after the previous reply, with a random think time),
  **open-loop** (arrivals follow a stochastic process regardless of replies),
  and **semi-open-loop** (open arrivals, but the Worker pauses for a think
  time before submitting). Requirement R2 is that the user gets to choose; R3
  is fine-grained rate control, demonstrated in §6.1, where a Wikipedia
  workload ramped 25 tps every 10 s tracks its target until the DBMS saturates
  at ~680 s and 95th-percentile latency crosses one second.

  The cost of choosing wrongly is measured in topic 34, not restated here:
  FINDINGS.md row 34 reports **p99 = 1.0 µs closed-loop against 90 ms
  open-loop on identical work — a 90,000× lie**.

  </details>

- [ ] You can contrast TPC-C's contention with YCSB-A's, and say what each cannot measure.

  <details><summary>Answer</summary>

  YCSB-A is skewed reads and updates on independent keys: no operation spans
  two keys, so there is nothing to serialize and no anomaly to prevent. It
  measures the per-op path, lock striping and cache behaviour under skew.

  TPC-C New-Order is a multi-statement transaction: `SELECT … FOR UPDATE` on
  the district counter, an increment, and 5-15 stock updates, with a mandated
  1% abort and (Step 2) a 9.56% chance of touching a second warehouse — plus
  Payment's 15% remote customers, which together make 10.75% of all
  transactions distributed. It measures isolation, abort rates and lock
  queues.

  So: an isolation-level or abort-rate claim backed by YCSB is backed by
  nothing, and a "our per-op path is fast" claim backed by TPC-C is buried
  under lock waiting.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including your design for a graph analogue of the hot counter.

  <details><summary>Answer</summary>

  Self-check — the answers belong in `notes.md`. The one worth arguing about
  is question 3: the honest graph analogue of `D_NEXT_O_ID` is not "a hot
  node" but "a hot *counter on* a node" — an append to a supernode's adjacency
  list plus an update of its degree, which serializes every writer touching
  that node exactly the way a district serializes New-Orders. The remote-
  warehouse analogue is an edge whose endpoints live in different partitions,
  and its rate is set by the partitioner's edge-cut, which for a power-law
  graph is far worse than TPC-C's 1%.

  </details>

## References

**Papers**
- Difallah, Pavlo, Curino, Cudré-Mauroux — "OLTP-Bench: An Extensible
  Testbed for Benchmarking Relational Databases", **PVLDB Vol. 7,
  No. 4** (copyright 2013), presented at the 40th International
  Conference on Very Large Data Bases, September 2014, Hangzhou.
  [PDF](https://www.vldb.org/pvldb/vol7/p277-difallah.pdf). Sections
  used above: §1 (15 implemented benchmarks), §2 (requirements
  R1-R10), §3.1 (per-phase parameters; 12.5k txn/s per Worker thread),
  §3.2 (the three system models), §4.1.2 (CH-benCHmark), §6.1 (rate
  control), §6.2 (multi-class reporting; the Delivery finding).
  **OLTP-Bench** is the artifact the paper describes; **BenchBase**
  (cmu-db) is its maintained successor and the code anchored below.

**Specification**
- TPC BenchmarkTM C Standard Specification, revision 5.11.0
  ([tpc.org](https://www.tpc.org/tpcc/)). Clauses used above:

| Clause | What |
|---|---|
| 2.1.6 | the NURand formula and its A constants (255, 1023, 8191) |
| 2.1.6.1 | C-Delta must be in [65..119] and not 96 or 112 |
| 2.4.1.3 | `ol_cnt` random in [5..15], average 10 |
| 2.4.1.4 | a fixed 1% of New-Orders roll back by design |
| 2.4.1.5 | 1% of order *lines* are remote; Comment 1's "approximately 90% of all orders" are fully local |
| 2.5.1.2 | Payment's customer is remote 15% of the time; 60% selected by last name |
| 4.1.3 | Comment: maximum "computed to be 12.86 tpmC per warehouse"; floor of 9 |
| 4.2.2 | 10 terminals per warehouse; 10 districts, 30,000 customers, 100,000 stock per warehouse; 100,000 items fixed |
| 5.2.3 | mix minimums — New-Order "n/a", Payment 43.0, others 4.0 |
| 5.2.4.2 | the 23-card deck: 10 / 10 / 1 / 1 / 1 |
| 5.2.5.4 | think time `Tt = −log(r) × μ`, truncated at 10 × μ |
| 5.2.5.7 | the keying / response-time / think-time table |
| 5.4.2–5.4.4 | tpmC = completed New-Order transactions per minute, truncated |

**Code**

| File | Lines | What |
|---|---|---|
| benchbase `.../tpcc/TPCCConfig.java` | 32-38 | the per-warehouse cardinalities; `INVALID_ITEM_ID` — **not** the weights |
| benchbase `.../tpcc/TPCCUtil.java` | 86-91 | the TODO admitting the NURand constants are hardcoded |
| benchbase `.../tpcc/TPCCUtil.java` | 92-97 | `OL_I_ID_C`, `C_ID_C`, `C_LAST_LOAD_C` 157, `C_LAST_RUN_C` 223, with Clause 2.1.6.1 quoted at 94-95 |
| benchbase `.../tpcc/TPCCUtil.java` | 99-117 | `getItemID`, `getCustomerID`, load-vs-run last names |
| benchbase `.../tpcc/TPCCUtil.java` | 119-125 | `randomNumber` and `nonUniformRandom` |
| benchbase `.../tpcc/TPCCWorker.java` | 83-101 | keying wait, and think time with the 10× cap |
| benchbase `.../tpcc/procedures/NewOrder.java` | 55-62 | `SELECT D_NEXT_O_ID, D_TAX … FOR UPDATE` — the serialization point |
| benchbase `.../tpcc/procedures/NewOrder.java` | 73-81 | `UPDATE … SET D_NEXT_O_ID = D_NEXT_O_ID + 1` |
| benchbase `.../tpcc/procedures/NewOrder.java` | 147-168 | 5-15 items, the 1% remote branch, the 1% forced rollback |
| benchbase `config/postgres/sample_tpcc_config.xml` | 11 | `TRANSACTION_SERIALIZABLE` |
| benchbase `config/postgres/sample_tpcc_config.xml` | 14-25 | scalefactor = warehouses, 1 terminal, 60 s, rate 10000, weights 45,43,4,4,4 |
| benchbase `config/postgres/sample_tpcc_config.xml` | 28-53 | the spec's keying and think times, commented out |
| benchbase `config/postgres/` | — | 19 sample configs, one per bundled benchmark |

Pinned revision: cmu-db/benchbase@33c0047 (regenerate the pin table
with `python3 tools/pin-table.py`).

**Cross-topic**
- topic 34 — coordinated omission, measured: closed-loop p99 = 1.0 µs
  against open-loop 90 ms, a 90,000× lie (FINDINGS.md row 34). Cite
  it rather than re-deriving the mechanism.
- topic 8 — MVCC and abort rates, which Step 2's hot counter is the
  canonical workload for.
- topic 9 — contended counters and per-district queueing.
- [reading-ycsb.md](reading-ycsb.md) — Step 8's other contention.
- [reading-boncz-tpch.md](reading-boncz-tpch.md) — the analytical
  half, and CH-benCHmark's other end.
