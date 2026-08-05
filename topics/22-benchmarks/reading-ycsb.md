# YCSB: six mixes, five distributions, one Zipfian generator

YCSB is the most-cited and most-misquoted benchmark in key-value
storage. This chapter takes it apart in the order that matters: what
it was designed to measure (and the two tiers it never covers), what
the six workloads *actually* are in the paper's own Table 2, how the
Zipfian generator produces skew cheaply enough to keep up with a
million ops per second, why the popular keys have to be scattered by
a hash, and where the shipped harness — and this topic's own driver —
throw away the timestamp that would make the tail latencies honest.

Paper: **Cooper, Silberstein, Tam, Ramakrishnan, Sears,
"Benchmarking Cloud Serving Systems with YCSB", SoCC 2010**, 8 pages.
Code line numbers are **pingcap/go-ycsb@f030f99**, the Go port; the
original is brianfrankcooper/YCSB in Java, and the two agree on the
algorithm and the property names. Repo line numbers are this topic's
`experiments/src/`.

The title says *five* distributions because that is how the folklore
counts them. The paper names **four** (§4.1) and go-ycsb accepts
**six** for `requestdistribution` — Step 3 reconciles this. Chapter
titles are load-bearing links in SUMMARY.md, so the heading stays;
the body is where the count gets fixed.

## The problem in one sentence

A key-value benchmark has to produce realistic skew — a few keys
getting most of the traffic — millions of times per second without
the generator itself becoming the bottleneck, and without leaking the
hot set's *identity* into the physical layout it is supposed to be
testing.

## The concepts, step by step

### Step 1 — what YCSB measures, and the two tiers nobody runs

> **In:** nothing but the acronym.
> **Out:** the paper's tier structure, and the reason "YCSB numbers" in the
> wild almost always mean tier 1 only.

YCSB — Yahoo! Cloud Serving Benchmark — was written to compare
"cloud serving" stores (Cassandra, HBase, PNUTS, sharded MySQL in the
paper's §6) on **online serving** work: single-record reads, updates,
inserts and short scans, no joins, no multi-record transactions. The
paper structures it as tiers:

```
  Tier 1  Performance   §3.1  latency vs throughput as offered load rises
  Tier 2  Scaling       §3.2  scaleup (grow servers with data) and
                              elastic speedup (add servers mid-run)
  Tier 3  Availability  §7.1  performance while a server is killed
  Tier 4  Replication   §7.2  consistency/performance of replicas
```

Tiers 3 and 4 are §7's *proposed future* tiers — the paper describes
them but does not report results for them, and neither the Java YCSB
nor go-ycsb ships them. So a "YCSB result" you read anywhere is
almost certainly tier 1, one workload, one client count, and it says
nothing about failure behaviour.

Also worth knowing before you cite the paper's own numbers: §6 states
its own precision limit — "The 95th and 99th percentile latencies are
not reported" — and its runs were 30 minutes each after 10-20 hour
loads (§4.2). The paper is about *shapes of curves*, not about tails.

### Step 2 — the six mixes, exactly as Table 2 defines them

> **In:** Step 1's operation types.
> **Out:** the paper's Table 2 verbatim, the reason F is not in it, and the
> mapping onto this repo's `WORKLOADS` array.

The classic error is swapping D and E, or claiming F is one of the
core five. Table 2 (§4.2) defines **A through E only**:

```
  workload      operations                record selection   application example
  A update-heavy  Read 50% / Update 50%   Zipfian            session store recording recent
                                                             actions in a user session
  B read-heavy    Read 95% / Update  5%   Zipfian            photo tagging; add a tag is an
                                                             update, most operations read tags
  C read-only     Read 100%               Zipfian            user profile cache, profiles
                                                             constructed elsewhere (e.g. Hadoop)
  D read-latest   Read 95% / Insert  5%   Latest             user status updates; people want
                                                             to read the latest statuses
  E short-ranges  Scan 95% / Insert  5%   Zipfian/Uniform*   threaded conversations, each scan
                                                             for the posts in a given thread
                                              — Table 2, YCSB SoCC 2010
  * Table 2's footnote: "Workload E uses the Zipfian distribution to choose
    the first key in the range, and the Uniform distribution to choose the
    number of records to scan."
```

Two things to fix in your memory:

- **D is "read latest" and uses the Latest distribution; E is "short
  ranges" and is the only one with scans.** D inserts and reads the
  *newest* records; E inserts and *scans* from a Zipfian-chosen start.
  They are the two insert workloads, which is why they get confused —
  but only E scans, and only D is skewed toward recency.
- **F is not in Table 2.** It appears once, in §6.5: "a
  'read-modify-write' workload … similar to workload A (50/50) except
  that the updates are 'read-modify-write' rather than blind writes.
  The results (not shown) showed the same trends as workload A." F is
  a later addition to the tool, and the paper explicitly did not plot
  it. If your result for F differs wildly from A, that is a finding
  about your read-modify-write path, not a reproduction of anything
  in the paper.

This repo encodes all six, with F's provenance in mind:

```rust
// topics/22-benchmarks/experiments/src/ycsb.rs — the six mixes, 61-68
    61  pub const WORKLOADS: [Mix; 6] = [
    62      Mix { name: "A update-heavy", read: 0.5, update: 0.5, insert: 0.0, scan: 0.0, rmw: 0.0 },
    63      Mix { name: "B read-mostly", read: 0.95, update: 0.05, insert: 0.0, scan: 0.0, rmw: 0.0 },
    64      Mix { name: "C read-only", read: 1.0, update: 0.0, insert: 0.0, scan: 0.0, rmw: 0.0 },
    65      Mix { name: "D read-latest", read: 0.95, update: 0.0, insert: 0.05, scan: 0.0, rmw: 0.0 },
    66      Mix { name: "E short-ranges", read: 0.0, update: 0.0, insert: 0.05, scan: 0.95, rmw: 0.0 },
    67      Mix { name: "F read-mod-write", read: 0.5, update: 0.0, insert: 0.0, scan: 0.0, rmw: 0.5 },
    68  ];
```

The proportions match Table 2 for A-E and §6.5 for F. What the array
*cannot* express is the record-selection column — that is the
`KeyGen` passed in, which is why D's "latest" is approximated by
whatever generator you hand it (`ycsb.rs:59-60`) and E's two
distributions collapse to one.

### Step 3 — the distributions: four in the paper, six in the tool

> **In:** Step 2's "record selection" column.
> **Out:** an exact count of what exists where, and the one that is not a
> record-selection distribution at all.

§4.1 says "YCSB has several built-in distributions" and lists
**four**:

```
  Uniform      every record equally likely
  Zipfian      a few records extremely popular, most unpopular
  Latest       like Zipfian, but the head is the most recently inserted
  Multinomial  explicit per-item probabilities
                                        — §4.1, YCSB SoCC 2010
```

**Multinomial is not a record-selection distribution.** §4.1's own
example is choosing the *operation*: "we might assign a probability
of 0.95 to the Read operation, a probability of 0.05 to the Update
operation, and a probability of 0 to Scan and Insert." That is Step
2's mix, not Step 2's key choice. So the paper offers **three**
record-selection distributions, not five.

The tool has grown since. go-ycsb's `requestdistribution` property
accepts exactly six values:

```go
// go-ycsb pkg/workload/core.go — the requestdistribution switch, 655-678 (elided)
   655  	switch requestDistrib {
   656  	case "uniform":
   657  		c.keyChooser = generator.NewUniform(keyrangeLowerBound, keyrangeUpperBound)
   658  	case "sequential":
   659  		c.keyChooser = generator.NewSequential(keyrangeLowerBound, keyrangeUpperBound)
   660  	case "zipfian":
   // ... 661-664: expand the keyrange for expected inserts — see Step 6 ...
   665  		c.keyChooser = generator.NewScrambledZipfian(keyrangeLowerBound, keyrangeUpperBound, generator.ZipfianConstant)
   666  	case "latest":
   667  		c.keyChooser = generator.NewSkewedLatest(c.transactionInsertKeySequence)
   668  	case "hotspot":
   // ... 669-670: read hotset/hotop fractions ...
   671  		c.keyChooser = generator.NewHotspot(keyrangeLowerBound, keyrangeUpperBound, hotsetFraction, hotopnFraction)
   672  	case "exponential":
   // ... 673-674: read percentile/frac ...
   675  		c.keyChooser = generator.NewExponential(percentile, float64(c.recordCount)*frac)
   676  	default:
   677  		util.Fatalf("unknown request distribution %s", requestDistrib)
   678  	}
```

Note line 665: **asking for `zipfian` gets you `NewScrambledZipfian`,
not `NewZipfian`.** The plain Zipfian generator is never a key chooser
— it is always wrapped. Step 5 is why.

So the honest count: three record-selection distributions in the
paper, six selectable in go-ycsb (uniform, sequential, zipfian,
latest, hotspot, exponential), and the chapter title's "five" is
folklore that split the difference. Use the numbers, not the title.

### Step 4 — the Zipfian generator: constant time, precomputed zeta

> **In:** Step 3's `zipfian` case.
> **Out:** the three precomputed constants, the two fast paths, and a
> computed probability for the hottest key that you can check against the
> code's own hardcoded constant.

A Zipfian draw over n items assigns rank i (1-based) probability
proportional to `1/i^θ`, normalized by the generalized harmonic
number:

```
  ζ(n, θ) = Σ  1/i^θ           for i = 1 .. n
  P(rank i) = (1/i^θ) / ζ(n, θ)
```

Computing that sum per draw is O(n) — hopeless. YCSB uses the
inversion method from **Gray, Sundaresan, Englert, Baclawski, Weinberger,
"Quickly Generating Billion-Record Synthetic Databases", SIGMOD
1994** (§5.3 cites it), which precomputes three constants at
construction and then does one `Float64()` and one `math.Pow` per
draw:

```go
// go-ycsb pkg/generator/zipfian.go — precomputation, 97-118 (elided)
    97  func NewZipfian(min int64, max int64, zipfianConstant float64, zetan float64) *Zipfian {
    98  	items := max - min + 1
   // ... 99-107: z.items, z.base, z.theta ...
   108  	z.zeta2Theta = z.zeta(0, 2, theta, 0)
   109
   110  	z.alpha = 1.0 / (1.0 - theta)
   111  	z.zetan = zetan
   112  	z.countForZeta = items
   113  	z.eta = (1 - math.Pow(2.0/float64(items), 1-theta)) / (1 - z.zeta2Theta/z.zetan)
   // ... 114-117: seed, prime the generator ...
   118  }
```

```go
// go-ycsb pkg/generator/zipfian.go — the O(n) sum, done once, 125-133
   125  func zetaStatic(st int64, n int64, theta float64, initialSum float64) float64 {
   126  	sum := initialSum
   127
   128  	for i := st; i < n; i++ {
   129  		sum += 1 / math.Pow(float64(i+1), theta)
   130  	}
   131
   132  	return sum
   133  }
```

- `zetan` = ζ(n, θ), the normalizer — the only O(n) work, done once.
- `zeta2Theta` = ζ(2, θ) = 1 + 2^−θ, the mass of the top two ranks.
- `alpha` = 1/(1−θ), the inversion exponent (line 110).
- `eta` (line 113) is the interpolation constant that maps a uniform
  u onto the rank scale; it is the piece that lets the draw be one
  `Pow` instead of a search.

The draw itself:

```go
// go-ycsb pkg/generator/zipfian.go — one draw, 151-164
   151  	u := r.Float64()
   152  	uz := u * z.zetan
   153
   154  	if uz < 1.0 {
   155  		return z.base
   156  	}
   157
   158  	if uz < 1.0+math.Pow(0.5, z.theta) {
   159  		return z.base + 1
   160  	}
   161
   162  	ret := z.base + int64(float64(itemCount)*math.Pow(z.eta*u-z.eta+1, z.alpha))
   163  	z.SetLastValue(ret)
   164  	return ret
```

Lines 154-160 are the two special cases of the inversion, and they
are also the arithmetic worth doing. Since `uz = u × ζn` with u
uniform on [0,1):

```
  P(fast path 1, rank 0)  = P(u·ζn < 1)          = 1 / ζn
  P(fast path 2, rank 1)  = P(1 ≤ u·ζn < 1+2^−θ) = 2^−θ / ζn
  2^−0.99 = 0.503478
```

Evaluate at θ = 0.99 for a 1,000,000-key store — sum
`Σ 1/i^0.99` for i = 1..10⁶ and you get **ζ = 15.3918497460**:

```
  P(hottest key)       = 1 / 15.39185          = 0.064969  =  6.50%
  P(second key)        = 0.503478 / 15.39185   = 0.032711  =  3.27%
  P(either fast path)  =                          0.097680 =  9.77%

  top     100 keys (ζ(100)/ζ(10⁶) = 5.29457/15.39185)  = 34.40% of all draws
  top   1,000 keys (ζ(1000)/ζ(10⁶) = 7.72895/15.39185) = 50.21% of all draws
```

**Half of all traffic lands on 0.1% of the keys, and one draw in ten
never reaches the `math.Pow` on line 162.** That is what "Zipfian
θ=0.99" buys, and it is why cache-hit-rate results are so sensitive
to it.

You can check the code's arithmetic without running it. Step 5's
scrambled generator hardcodes `zetan = 26.46902820178302` for
n = 10¹⁰, θ = 0.99. Summing 10 billion terms directly is impractical,
but Euler–Maclaurin on the tail reproduces it:

```
  ζ(10¹⁰, 0.99) ≈ Σ_{i≤10⁵} i^−0.99
                + (10¹⁰·⁰¹ − 10⁵·⁰¹)/0.01     [∫ x^−0.99 dx]
                − ½·10⁵^−0.99 + ½·10¹⁰^−0.99
                + 0.99·(10⁵^−1.99 − 10¹⁰^−1.99)/12
                = 26.46902820175…
  hardcoded       26.46902820178302   → agrees to 10 significant figures
```

At that keyspace, `P(hottest) = 1/26.469 = 3.78%` and the two fast
paths absorb 5.68% of draws — skew *falls* as the keyspace grows,
which is exactly what a fixed θ means.

**A defect worth noticing while you read**: `SetLastValue(ret)` is
called only on line 163, on the general path. Both fast paths return
at 155 and 159 without updating it. Any consumer that reads
`LastValue()` — the `SkewedLatest` generator does — sees a stale
value ~9.8% of the time at n = 10⁶.

### Step 5 — scrambling: the hot set must not be lexicographically first

> **In:** Step 4's generator, which makes rank 0 the hottest key.
> **Out:** the layout leak that creates, the paper's two failed fixes and its
> accepted one, and the exact constants go-ycsb uses.

Gray's algorithm returns *ranks*: item 0 is hottest, item 1 next, and
so on. §5.3 states the problem plainly:

> "The first problem is that the popular items are clustered together
> in the keyspace. In particular, the most popular item is item 0; the
> second most popular item is item 1, and so on. For the Zipfian
> distribution, the popular items should be scattered across the
> keyspace. In real web applications, the most popular user or blog
> topic is not necessarily the lexicographically first item."

This is a *methodology* bug, not an aesthetic one. If the hot keys
are `user0, user1, user2, …`, then in any range-partitioned or
clustered store they land in the same leaf pages, the same shard and
the same cache lines. You would be measuring one hot page, and a
B-tree would look artificially wonderful against a hash index. The
benchmark must not leak the hot set's identity into the physical
layout it is testing — the same principle as TPC-C's NURand
load-vs-run constants ([reading-oltpbench-tpcc.md](reading-oltpbench-tpcc.md)
Step 4).

§5.3 records two attempts and the fix:

1. **Hash with `String.hashCode()`** — "tended to leave the popular
   items clustered". Rejected.
2. **Hash with anything, 1:1** — "after hashing, collisions meant
   that only about **80 percent** of the keyspace would be generated
   in the sequence. This was true even as we tried a variety of hash
   functions (FNV, Jenkins, etc.)." Perfect hashing was considered
   and rejected for setup cost — "multiple minutes for hundreds of
   millions of records".
3. **Accepted**: "construct a Zipfian generator for a much larger
   keyspace than we actually needed; apply the FNV hash to each
   generated value; and then take mod N … The result was that
   **99.97%** of the keyspace is generated, and the generated keys
   continued to have a Zipfian distribution."

"Much larger" is a specific number in go-ycsb:

```go
// go-ycsb pkg/generator/scrambled_zipfian.go — the oversized inner keyspace, 50-67 (elided)
    50  func NewScrambledZipfian(min int64, max int64, zipfianConstant float64) *ScrambledZipfian {
    51  	const (
    52  		zetan               = float64(26.46902820178302)
    53  		usedZipfianConstant = float64(0.99)
    54  		itemCount           = int64(10000000000)
    55  	)
   // ... 56-60: s.min, s.max, s.itemCount = max - min + 1 ...
    61  	if zipfianConstant == usedZipfianConstant {
    62  		s.gen = NewZipfian(0, itemCount, zipfianConstant, zetan)
    63  	} else {
    64  		s.gen = NewZipfianWithRange(0, itemCount, zipfianConstant)
    65  	}
    66  	return s
    67  }
```

```go
// go-ycsb pkg/generator/scrambled_zipfian.go — scatter, 70-76
    70  func (s *ScrambledZipfian) Next(r *rand.Rand) int64 {
    71  	n := s.gen.Next(r)
    72
    73  	n = s.min + util.Hash64(n)%s.itemCount
    74  	s.SetLastValue(n)
    75  	return n
    76  }
```

The inner generator always draws over **10 billion** items regardless
of your `recordcount`; line 73 folds that down with FNV into the real
range. Line 52's hardcoded zetan is why: ζ over 10¹⁰ terms is the one
sum you cannot afford at startup, so it is precomputed for the
default θ = 0.99, and line 64's fallback pays the O(n) loop only if
you change θ (which the doc comment at `zipfian.go:47-64` warns takes
"over a minute for 100 million objects").

The hash is FNV-1a over the big-endian 8 bytes:

```go
// go-ycsb pkg/util/hash.go — FNV-1a 64, 21-32
    21  // Hash64 returns a fnv Hash of the integer.
    22  func Hash64(n int64) int64 {
    23  	var b [8]byte
    24  	binary.BigEndian.PutUint64(b[0:8], uint64(n))
    25  	hash := fnv.New64a()
    26  	hash.Write(b[0:8])
    27  	result := int64(hash.Sum64())
    28  	if result < 0 {
    29  		return -result
    30  	}
    31  	return result
    32  }
```

**Why 10¹⁰ and not, say, 2×N?** The 99.97% figure depends on the
ratio. If the inner keyspace M equals the outer N, FNV mod N behaves
like a random function and the expected coverage is

```
  N · (1 − (1 − 1/N)^M)  →  N · (1 − 1/e) = 63.2%   when M = N
```

— a third of your keys never drawn at all. With M = 10¹⁰ ≫ N, every
residue class is hit many times over and the shortfall collapses to
the paper's 0.03%. This is the calculation to redo before you shrink
the inner keyspace in `zipf.rs`, whose stub currently pairs a
1,000,000-item inner generator with a 1,000,000-key store.

### Step 6 — the growing keyspace: N + T×I + ε

> **In:** Step 5's fixed inner keyspace and Step 2's insert workloads.
> **Out:** why a Zipfian generator cannot simply be resized mid-run, and the
> two different fixes for Zipfian and Latest.

Workloads D and E insert. If the generator's keyspace is fixed at
load size N, it can never draw the records the run inserts. §5.3:

> "For Zipfian, we expanded the initial keyspace to the expected size
> after inserts. If a data set had N records, and the workload had T
> total operations, with an expected fraction I of inserts, then we
> constructed the Zipfian generator to draw from a space of size
> **N + T × I + ε**. We added an additional factor ε since the actual
> number of inserts depends on the random choice of operations during
> the workload according to a multinomial distribution. While running
> the workload, if the generator produced an item which had not been
> inserted yet, we [skipped it and redrew]."

go-ycsb implements ε as a factor of two on the insert term:

```go
// go-ycsb pkg/workload/core.go — N + T×I×2, 660-665
   660  	case "zipfian":
   661  		insertProportion := p.GetFloat64(prop.InsertProportion, prop.InsertProportionDefault)
   662  		opCount := p.GetInt64(prop.OperationCount, 0)
   663  		expectedNewKeys := int64(float64(opCount) * insertProportion * 2.0)
   664  		keyrangeUpperBound = insertStart + insertCount + expectedNewKeys
   665  		c.keyChooser = generator.NewScrambledZipfian(keyrangeLowerBound, keyrangeUpperBound, generator.ZipfianConstant)
```

For workload E at the shipped defaults (`recordcount=1000`,
`operationcount=1000`, `insertproportion=0.05`):

```
  expectedNewKeys = 1000 × 0.05 × 2.0 = 100
  keyrange        = [0, 0 + 1000 + 100 − 1] = 1,099 keys for a 1,000-record load
```

Latest gets the *opposite* treatment — its head must move to the new
keys, so §5.3 recomputes the distribution on every insert, "to do
this cheaply we modified the Gray algorithm of [23] to compute its
constants incrementally". go-ycsb does the same for Zipfian when the
item count grows:

```go
// go-ycsb pkg/generator/zipfian.go — incremental zeta on growth, 136-149 (elided)
   136  	if itemCount != z.countForZeta {
   137  		z.lock.Lock()
   138  		if itemCount > z.countForZeta {
   139  			//we have added more items. can compute zetan incrementally, which is cheaper
   140  			z.zetan = z.zeta(z.countForZeta, itemCount, z.theta, z.zetan)
   141  			z.eta = (1 - math.Pow(2.0/float64(z.items), 1-z.theta)) / (1 - z.zeta2Theta/z.zetan)
   142  		} else if itemCount < z.countForZeta && z.allowItemCountDecrease {
   143  			//note : for large itemsets, this is very slow. so don't do it!
   144  			fmt.Printf("recomputing Zipfian distribution, should be avoided,item count %v, count for zeta %v\n", itemCount, z.countForZeta)
   // ... 145-146: full O(n) recompute ...
   147  		}
   148  		z.lock.Unlock()
   149  	}
```

Growth is a partial sum resumed from `countForZeta` (line 140) —
O(Δ). *Shrinking* is O(n) and prints a warning to stderr (144), which
is a rare and admirable piece of honesty: a benchmark generator
telling you it is about to distort your measurement.

### Step 7 — where the tail latency goes missing

> **In:** Step 6's driver loop.
> **Out:** the two lines that create coordinated omission in go-ycsb, the one
> line that creates it in this repo, and the pointer to where it is measured.

go-ycsb *does* have a rate knob — the paper's Fig. 2 lists "Target
throughput" among the client's command-line properties — and it
computes each operation's **intended** start time correctly:

```go
// go-ycsb pkg/client/client.go — the intended schedule, 97-111
    97  func (w *worker) throttle(ctx context.Context, startTime time.Time) {
    98  	if w.targetOpsPerMs <= 0 {
    99  		return
   100  	}
   101
   102  	d := time.Duration(w.opsDone * w.targetOpsTickNs)
   103  	d = startTime.Add(d).Sub(time.Now())
   104  	if d < 0 {
   105  		return
   106  	}
   107  	select {
   108  	case <-ctx.Done():
   109  	case <-time.After(d):
   110  	}
   111  }
```

Line 102 is an *absolute* schedule — operation k was supposed to
begin at `startTime + k × tick`, not "tick nanoseconds after the last
one finished". That is the right way to build an open-loop driver,
and it means the intended time genuinely exists in the process.

Then line 104 throws it away. If the worker is already late
(`d < 0`), it returns and immediately issues the next operation — and
the measurement clock starts at *issue* time, not intended time:

```go
// go-ycsb pkg/client/dbwrapper.go — where the clock starts, 53-57
    53  func (db DbWrapper) Read(ctx context.Context, table string, key string, fields []string) (_ map[string][]byte, err error) {
    54  	start := time.Now()
    55  	defer func() {
    56  		measure(start, "READ", err)
    57  	}()
```

`start := time.Now()` on line 54 is taken after the stall has already
happened. Every microsecond spent waiting to *get to* the operation
is invisible; only service time is recorded. The intended time is
computed at `client.go:102` and never passed to `measure`. The
one-line fix is to plumb it through — which tells you this is a
design decision, not an oversight.

The throttle also runs *after* the operation, not before:

```go
// go-ycsb pkg/client/client.go — throttle placement, 144-147
   144  		if measurement.IsWarmUpFinished() {
   145  			w.opsDone += int64(opsCount)
   146  			w.throttle(ctx, startTime)
   147  		}
```

So with no target set (`targetOpsPerMs <= 0`, line 98), the loop is a
pure closed loop: the next request is only issued once the previous
reply arrives. When the store stalls, the load politely stops.

**This repo's driver has the same omission, deliberately and
declaredly.** `ycsb.rs:1-7` says so — "closed-loop, single thread …
no threads, no target rate" — and the clock placement makes it
concrete:

```rust
// topics/22-benchmarks/experiments/src/ycsb.rs — the clock starts after key selection, 95-117 (elided)
    95      for _ in 0..ops {
    96          let k = keygen.next(next_id as usize) as u64;
    97          let r: f64 = rng.gen();
    98          let t = Instant::now();
    99          if r < mix.read {
   100              std::hint::black_box(store.read(k));
   // ... 101-116: the update / insert / scan / read-modify-write arms ...
   117          hist.record(t.elapsed().as_nanos() as u64);
```

Line 98 starts the timer *after* the key generator and RNG have run,
so `zipf.rs`'s own cost is excluded — good, that is what you want
when comparing mixes — but there is no intended time anywhere, so
every percentile this driver prints is a **service-time** percentile.

Do not re-derive what that costs: **topic 34 measured it**.
FINDINGS.md row 34 records a closed-loop p99 of **1.0 µs** against an
open-loop **90 ms** on identical work — a **90,000×** lie. Cite that
number; the mechanism is topic 34's to explain.

The practical rule: a YCSB percentile is comparable to another YCSB
percentile on the same driver, and to nothing else. This topic's
headline (FINDINGS.md row 22) is stated as a *ratio* for exactly that
reason — "YCSB-E's p999 is 12.9 µs against read-only's 4.0 µs".

### Step 8 — read the property files, not the property names

> **In:** Steps 2 and 3.
> **Out:** three concrete discrepancies in the shipped workload files, and
> the habit they should install.

The shipped `workloads/workload*` files are the most-copied
configuration in the field, and at `f030f99` they contradict their own
documentation:

```
# go-ycsb workloads/workloada — the comment and the property disagree, 18-36 (elided)
    18  # Workload A: Update heavy workload
   # ... 19-22: application example, ratio, record size ...
    23  #   Request distribution: zipfian
    24
    25  recordcount=1000
    26  operationcount=1000
   # ... 27-30: workload=core, readallfields ...
    31  readproportion=0.5
    32  updateproportion=0.5
    33  scanproportion=0
    34  insertproportion=0
    35
    36  requestdistribution=uniform
```

Line 23 says zipfian. Line 36 says **uniform**. The same
contradiction is in `workloadb` (comment :22, property :35),
`workloadc` (:22, :35), `workloade` (:22, :40) and `workloadf`
(:22, :36). Only `workloadd` is consistent, because Latest is what
it sets (`requestdistribution=latest`, :40). **Running the shipped
files unmodified gives you a uniform key distribution for five of the
six workloads** — no skew, no hot set, and a cache-hit rate that has
nothing to do with the workload you think you ran.

Workload E has a second problem:

```
# go-ycsb workloads/workloade — "short ranges" of length one, 35-44
    35  readproportion=0
    36  updateproportion=0
    37  scanproportion=0.95
    38  insertproportion=0.05
    39
    40  requestdistribution=uniform
    41
    42  maxscanlength=1
    43
    44  scanlengthdistribution=uniform
```

`maxscanlength=1` (line 42) makes every "scan" return a single
record. Table 2's whole point for E is *short ranges* — sequential
access that a hash index cannot serve and a B-tree can. With
`maxscanlength=1`, E degenerates into workload C with 5% inserts, and
the structural difference the workload exists to expose disappears.

And `recordcount=1000` / `operationcount=1000` (lines 29-30) is a
smoke-test size: 1,000 records of ~1 KB is a megabyte, which fits in
L2. Any storage result from the unmodified files is a measurement of
your CPU cache.

This repo's driver is, on this narrow point, *more* faithful than the
shipped files: `ycsb.rs:110` scans 100 records
(`store.scan(k, 100)`), which is a short range in Table 2's sense.

**The habit:** before quoting any benchmark result, open the config
and read the properties, not the comments. The Boncz guide's version
of this is checking which query variant was run; TPC-C's is checking
whether think times were enabled ([reading-oltpbench-tpcc.md](reading-oltpbench-tpcc.md)
Step 5). Same failure, three benchmarks.

## Where each step lives in the sources

The paper is 8 pages; read §4 and §5, skim the rest.

- **§1-2** — the motivation and the tradeoffs (read/write, latency/
  durability, synchronous/asynchronous replication). Skim.
- **§3** — the tier model (Step 1). Read §3.1 and §3.2; §7's tiers 3
  and 4 are proposals, so read them last and do not cite them as
  implemented.
- **§4 — read carefully.** §4.1 the four distributions (Step 3),
  §4.2 and **Table 2** the five core workloads (Step 2). Table 2 is
  the single most-misquoted table in the paper; copy it out by hand.
- **§5 — read carefully.** §5.1 architecture, §5.2 the extension
  points, and **§5.3** the generator engineering: Gray's algorithm,
  the clustering problem, the 80%-coverage failure, the oversized-
  keyspace fix and its 99.97%, and the `N + T×I + ε` growing keyspace
  (Steps 4-6). §5.3 is the densest page in the paper.
- **§6** — the results. Read 6.4 (workload E) and 6.5 (where F is
  mentioned, and only mentioned). Remember §6's own caveat that 95th
  and 99th percentiles are not reported.
- Then open go-ycsb in this order: `pkg/generator/zipfian.go`
  (97-133 precompute, 135-165 draw), `scrambled_zipfian.go` (50-76),
  `pkg/util/hash.go` (21-32), `pkg/workload/core.go` (655-678),
  `pkg/client/client.go` (97-111, 144-147), `pkg/client/dbwrapper.go`
  (30-39, 53-60), and finally `workloads/workload{a..f}` with Step 8
  in mind.

## Questions (answer in notes.md)

1. Step 4 computed P(hottest key) = 6.50% at n = 10⁶, θ = 0.99, and
   3.78% at go-ycsb's inner n = 10¹⁰. Our `zipf.rs` stub builds its
   inner generator at 1,000,000 items. Which of those two skews will
   `Scrambled` actually produce, and does it matter for the
   measurement — or only for the comparison to published numbers?
2. Step 5's coverage formula gives 63.2% when the inner keyspace
   equals the outer. Work out what inner size you need for 99%
   coverage of a 1,000,000-key store, and decide whether `zipf.rs`
   should adopt it or document the divergence.
3. `hash.go:24` uses **big-endian** bytes; `zipf.rs`'s stub doc says
   little-endian. Does endianness change the *distribution* of
   `hash % N`, or only which specific keys are hot? Justify, then say
   which property the benchmark actually needs.
4. Step 4 found `SetLastValue` is skipped on both fast paths
   (`zipfian.go:155`, `:159`). `SkewedLatest` consumes `LastValue()`.
   Estimate how often that stale read happens at n = 10⁶ and at
   n = 10¹⁰, and say whether it biases workload D toward or away from
   recency.
5. Step 8: reproduce the *intended* workload A by fixing the property
   files. Predict, before running, how the Mops/s and p999 move when
   `requestdistribution` goes uniform → zipfian at 1M keys, and what
   fraction of reads should hit the top 1,000 keys (Step 4 gives you
   the number).
6. Design a workload G that would expose something A-F cannot. What
   is the operation mix, what is the record-selection distribution,
   and what structural property of the store does it separate?

## Done when

Answer each before unfolding it.

- [ ] You can write out Table 2 from memory — all five core workloads with their operation mix and record-selection distribution — and say where F comes from.

  <details><summary>Answer</summary>

  A update-heavy: Read 50% / Update 50%, Zipfian, session store.
  B read-heavy: Read 95% / Update 5%, Zipfian, photo tagging.
  C read-only: Read 100%, Zipfian, user profile cache.
  D read-latest: Read 95% / **Insert** 5%, **Latest**, user status updates.
  E short-ranges: **Scan** 95% / Insert 5%, **Zipfian/Uniform** — Zipfian
  picks the first key, Uniform picks the scan length (Table 2's footnote) —
  threaded conversations.

  F is **not** in Table 2. §6.5 mentions it once: a read-modify-write variant
  of A, "The results (not shown) showed the same trends as workload A."

  </details>

- [ ] You can say how many record-selection distributions the paper actually defines, and which of its four is not one.

  <details><summary>Answer</summary>

  §4.1 lists four: Uniform, Zipfian, Latest, Multinomial. **Multinomial is not
  a record-selection distribution** — §4.1's own example uses it to choose the
  *operation* (0.95 Read / 0.05 Update / 0 Scan / 0 Insert), which is the mix,
  not the key. So the paper defines **three** record-selection distributions.

  go-ycsb accepts six for `requestdistribution` (`core.go:655-678`): uniform,
  sequential, zipfian, latest, hotspot, exponential — and `zipfian` constructs
  a **Scrambled**Zipfian (line 665), never a bare one. The chapter title's
  "five" is folklore.

  </details>

- [ ] You can name the three constants the Zipfian generator precomputes, compute the probability of the hottest key at n = 10⁶ and θ = 0.99, and say what fraction of draws never reach `math.Pow`.

  <details><summary>Answer</summary>

  `zetan` = ζ(n,θ) (the O(n) normalizer, `zipfian.go:111` — passed in, or
  summed by `zetaStatic` at 125-133), `zeta2Theta` = ζ(2,θ) = 1 + 2^−θ (:108),
  `alpha` = 1/(1−θ) (:110), and the derived `eta` (:113).

  ζ(10⁶, 0.99) = 15.3918497460, so P(rank 0) = 1/15.39185 = **6.50%** and
  P(rank 1) = 2^−0.99/15.39185 = 0.503478/15.39185 = **3.27%**. The two fast
  paths at `zipfian.go:154-156` and `:158-160` therefore absorb **9.77%** of
  draws before line 162's `math.Pow`. Top-1,000 of 1,000,000 keys take
  ζ(1000)/ζ(10⁶) = 7.72895/15.39185 = **50.2%** of all traffic.

  Bonus check: the hardcoded `zetan = 26.46902820178302` in
  `scrambled_zipfian.go:52` is ζ(10¹⁰, 0.99), reproducible to 10 significant
  figures by Euler–Maclaurin.

  </details>

- [ ] You can explain why the Zipfian output must be hashed, what the paper tried first, and why the inner keyspace is 10 billion.

  <details><summary>Answer</summary>

  Gray's algorithm returns ranks, so the hottest keys are 0, 1, 2, … — §5.3:
  "the popular items should be scattered across the keyspace. In real web
  applications, the most popular user or blog topic is not necessarily the
  lexicographically first item." Unscattered, the hot set lands in the same
  leaf pages and shard, so you measure one hot page and flatter range-
  partitioned stores.

  Attempt 1: `String.hashCode()` — "tended to leave the popular items
  clustered". Attempt 2: any 1:1 hash — collisions meant "only about **80
  percent** of the keyspace would be generated", true for FNV, Jenkins and
  others; perfect hashing was rejected at "multiple minutes for hundreds of
  millions of records". Accepted fix: draw from a much larger keyspace, FNV
  the value, take mod N — **99.97%** coverage with the Zipfian shape intact.

  go-ycsb makes "much larger" = **10,000,000,000**
  (`scrambled_zipfian.go:54`), folded down by FNV-1a over big-endian bytes
  (`hash.go:22-32`) at `scrambled_zipfian.go:73`. The ratio is what matters:
  at inner = outer the expected coverage is only N(1 − 1/e) = **63.2%**.

  </details>

- [ ] You can state the growing-keyspace rule and compute the keyrange for workload E at the shipped defaults.

  <details><summary>Answer</summary>

  §5.3: for Zipfian, expand the keyspace to **N + T × I + ε**, where N is the
  loaded record count, T the total operations and I the expected insert
  fraction; ε covers the variance from choosing operations multinomially.
  Items not yet inserted are skipped and redrawn. Latest instead recomputes
  its constants incrementally on every insert.

  go-ycsb sets ε by doubling the insert term (`core.go:663`:
  `opCount × insertProportion × 2.0`). At workloade's shipped
  `recordcount=1000`, `operationcount=1000`, `insertproportion=0.05`:
  expectedNewKeys = 1000 × 0.05 × 2.0 = **100**, giving a keyrange of
  [0, 1099] — 1,100 keys for a 1,000-record load.

  Growth is handled incrementally (`zipfian.go:138-141`, resuming the partial
  sum from `countForZeta`); shrinking costs O(n) and prints a warning
  (`:142-147`).

  </details>

- [ ] You can point at the exact lines where go-ycsb and this repo's driver discard the intended start time, and cite what that costs.

  <details><summary>Answer</summary>

  `client.go:102-103` computes the correct absolute schedule —
  `startTime + opsDone × targetOpsTickNs` — and `:104-106` discards it with
  `if d < 0 { return }` when the worker is late. The measurement clock then
  starts at issue time: `dbwrapper.go:54`, `start := time.Now()`, inside
  `Read` after the stall. `measure` (`:30-39`) never sees an intended time.
  The throttle also runs *after* the operation (`client.go:144-147`), so with
  no target it is a pure closed loop.

  This repo's `ycsb.rs:98` does the same: `let t = Instant::now();` after
  key generation, with no intended time anywhere — declaredly, per the module
  header at `ycsb.rs:1-7`. So every percentile it prints is service time.

  The cost is measured in **topic 34**, not here: FINDINGS.md row 34 reports
  closed-loop p99 = 1.0 µs against open-loop 90 ms on identical work — a
  **90,000×** lie. That is why this topic's headline is a ratio: FINDINGS.md
  row 22, "YCSB-E's p999 is 12.9 µs against read-only's 4.0 µs".

  </details>

- [ ] You can name three things wrong with the shipped `workloads/` files, and say what each one silently changes.

  <details><summary>Answer</summary>

  1. **The comment contradicts the property.** `workloada:23` documents
     "Request distribution: zipfian"; `workloada:36` sets
     `requestdistribution=uniform`. Same in b (:22/:35), c (:22/:35),
     e (:22/:40) and f (:22/:36); only d is consistent. Five of six shipped
     workloads run with **no skew at all** — no hot set, and a cache-hit rate
     unrelated to the workload's premise.
  2. **`workloade:42` sets `maxscanlength=1`.** Every "scan" returns one
     record, so E collapses into C plus 5% inserts and stops testing ordered
     access — the one structural property it exists to test.
  3. **`recordcount=1000` / `operationcount=1000`** in every file. At ~1 KB
     per record that is one megabyte — an L2-resident smoke test, not a
     storage benchmark.

  This repo's `ycsb.rs:110` scans 100 records, which is closer to Table 2's
  intent than the shipped file is.

  </details>

- [ ] You have predictions in notes.md for the uniform → Zipfian move on every workload, written before you implement `zipf.rs`.

  <details><summary>Answer</summary>

  Self-check — the predictions belong in `notes.md`, written before the run.
  Anchor them to the measured baseline rather than to intuition. The canonical
  headline is FINDINGS.md row 22 (measured 2026-07-28): **YCSB-E's p999 is
  12.9 µs against read-only's 4.0 µs**, a ratio of 3.2×. The `notes.md`
  baseline records an earlier run (M3 Pro, 2026-07-10) with uniform keys at
  A 2.88, B 4.15, C 3.72, D 4.40, E 1.11, F 2.85 Mops/s and p999 of 2,041 ns
  (E) against 958 ns (C) — a ratio of 2.1×. The absolute numbers are
  machine- and run-dependent; **the ratio is the invariant**, so predict
  ratios.

  A model for E-vs-C: a read is one descent of a `BTreeMap` holding 10⁶ keys,
  and a 100-record scan is that descent plus a leaf walk. With Rust's node
  capacity of 11 keys (`B = 6` in your toolchain's
  `library/alloc/src/collections/btree/node.rs` — check it), the descent is
  ⌈log₁₁ 10⁶⌉ ≈ 6 node visits and the walk is ⌈100/11⌉ ≈ 9 more, so
  (6 + 9)/6 ≈ **2.5×** the work. That brackets both measured ratios, which is
  the point: a prediction you can defend beats a number you remembered.

  Going uniform → Zipfian at 1M keys, expect *throughput to rise* on the
  read-mostly mixes, because Step 4's top 1,000 keys take 50.2% of the
  traffic and fit trivially in cache — and expect the p999 to move much less,
  because the tail is set by the cold 49.8%.

  </details>

## References

**Papers**
- Cooper, Silberstein, Tam, Ramakrishnan, Sears — "Benchmarking Cloud
  Serving Systems with YCSB", **SoCC 2010**
  ([PDF](https://www.cs.duke.edu/courses/fall13/cps296.4/838-CloudPapers/ycsb.pdf)).
  Sections used above: §3.1-3.2 (tiers 1-2), §4.1 (the four
  distributions), §4.2 and **Table 2** (workloads A-E), §5.3 (Gray's
  algorithm, the clustering problem, 80% coverage, the oversized-
  keyspace fix and 99.97%, `N + T × I + ε`), §6.4-6.5 (workload E;
  F's only appearance), §7.1-7.2 (the unimplemented tiers 3-4).
- Gray, Sundaresan, Englert, Baclawski, Weinberger — "Quickly
  Generating Billion-Record Synthetic Databases", **SIGMOD 1994**.
  Reference [23] of the YCSB paper; the source of the constant-time
  Zipfian inversion in Step 4.

**Code**

| File | Lines | What |
|---|---|---|
| go-ycsb `pkg/generator/zipfian.go` | 42-45 | `ZipfianConstant = 0.99` (the value is on 44) |
| go-ycsb `pkg/generator/zipfian.go` | 47-64 | doc comment; cites Gray et al.; "over a minute for 100 million objects" |
| go-ycsb `pkg/generator/zipfian.go` | 97-118 | precompute `zeta2Theta` (108), `alpha` (110), `zetan` (111), `eta` (113) |
| go-ycsb `pkg/generator/zipfian.go` | 125-133 | `zetaStatic` — the one O(n) sum |
| go-ycsb `pkg/generator/zipfian.go` | 136-149 | incremental zeta on growth; the warning on shrink |
| go-ycsb `pkg/generator/zipfian.go` | 154-160 | the two fast paths — and where `SetLastValue` is skipped |
| go-ycsb `pkg/generator/zipfian.go` | 162-163 | the general inversion, and the only `SetLastValue` |
| go-ycsb `pkg/generator/scrambled_zipfian.go` | 50-67 | hardcoded `zetan`, θ = 0.99, inner `itemCount = 10¹⁰` |
| go-ycsb `pkg/generator/scrambled_zipfian.go` | 70-76 | `min + Hash64(n) % itemCount` — the scatter |
| go-ycsb `pkg/util/hash.go` | 21-32 | FNV-1a 64 over big-endian bytes |
| go-ycsb `pkg/workload/core.go` | 655-678 | the six `requestdistribution` values; `zipfian` → Scrambled |
| go-ycsb `pkg/workload/core.go` | 660-665 | `N + T × I × 2` keyspace expansion |
| go-ycsb `pkg/client/client.go` | 97-111 | the intended schedule (102-103) and its discard (104-106) |
| go-ycsb `pkg/client/client.go` | 144-147 | throttle called *after* the operation |
| go-ycsb `pkg/client/dbwrapper.go` | 30-39 | `measure` — takes a start time, never an intended time |
| go-ycsb `pkg/client/dbwrapper.go` | 53-60 | `start := time.Now()` at issue time (54) |
| go-ycsb `workloads/workloada` | 23, 36 | comment says zipfian, property says uniform |
| go-ycsb `workloads/workloade` | 22, 40, 42 | same contradiction, plus `maxscanlength=1` |
| this repo `experiments/src/ycsb.rs` | 1-7 | the declared simplifications |
| this repo `experiments/src/ycsb.rs` | 61-68 | the six mixes |
| this repo `experiments/src/ycsb.rs` | 95-117 | the driver loop; the clock at 98; the 100-record scan at 110 |
| this repo `experiments/src/zipf.rs` | 1-12 | the stub's spec and its go-ycsb anchors |

Pinned revision: pingcap/go-ycsb@f030f99 (regenerate the pin table
with `python3 tools/pin-table.py`).

**Measurements**
- FINDINGS.md row 22 — the canonical headline: "YCSB-E's p999 is
  **12.9 µs** against read-only's **4.0 µs**". Reproduce with
  `./verify.sh 22`.
- `notes.md` — an earlier baseline run (M3 Pro): uniform-key
  throughputs A 2.88, B 4.15, C 3.72, D 4.40, E 1.11, F 2.85 Mops/s.
  Use ratios, not absolutes, when comparing across runs or machines.

**Cross-topic**
- topic 34 — coordinated omission, measured: closed-loop p99 = 1.0 µs
  against open-loop 90 ms, a 90,000× lie (FINDINGS.md row 34). Step 7
  cites it rather than re-deriving it.
- [reading-oltpbench-tpcc.md](reading-oltpbench-tpcc.md) — the other
  contention shape, and the same "read the config, not the comments"
  failure in TPC-C's think times.
- [reading-boncz-tpch.md](reading-boncz-tpch.md) — the analytical
  counterpart: what a benchmark's *queries* encode, rather than what
  its *keys* do.
