# db_bench: the shared vocabulary of storage benchmarking

`fillseq`, `readrandom`, `readwhilewriting` — these workload names started in
LevelDB, were extended by RocksDB, and now appear in every LSM paper since.
This chapter is a skim route through the 12,021-line tool that defines them —
but first it builds the concepts step by step: why a shared workload
vocabulary exists at all, how every workload reduces to picking an integer,
what each name actually stresses, and what to distrust in the numbers the
tool prints. The goal is the *vocabulary* and the measurement shape, not the
harness code. Name your own benchmarks in this language and your numbers
become comparable to two decades of published results.

Every anchor below is RocksDB at commit **`ea3136053`** (`main`, carrying the
11.13 release), the revision this repo pins (`resources/codebases.md`, pin
table), quoted with the line numbers the code occupies in that revision.
`tools/db_bench_tool.cc` is **12,021 lines** there; it churns fast, so on any
other commit re-grep before trusting a number. `tools/pinned-source.py show
rocksdb tools/db_bench_tool.cc -r 6953:6965` opens exactly what is quoted here.

## The problem in one sentence

"Our engine does 500K writes/s" is uninterpretable — sequential or random
keys? new inserts or overwrites? uniform or skewed? durable or buffered?
measured during compaction or before it? — and this repo has already measured
one of those axes on its own: the durability choice alone moves the ceiling
from **856,898/s** (buffered `write()`) to **44,109/s** (`fsync`) to
**337/s** (`F_FULLFSYNC`), a 2,542× spread
([FINDINGS.md](../../FINDINGS.md) row 5). Without a shared workload
vocabulary, no two papers' numbers name the same experiment.

## The concepts, step by step

### Step 1 — why storage engines need a standard workload vocabulary

> **In:** nothing yet — this step fixes the vocabulary and the units every
> later step uses.
> **Out:** the axes a benchmark name has to pin down, and the measured size of
> two of them. Step 2 then shows that all of the *key-order* axis reduces to
> one function.

A storage engine's performance is not one number but a surface: it depends on
the operation mix (reads vs writes vs scans), the key order (sequential vs
random), whether keys are new or overwrite old ones, whether each write is
made durable, and what background work is running.

Four terms, defined before anything leans on them:

- An **LSM engine** (log-structured merge-tree) buffers writes in memory, then
  writes them out as immutable sorted files that are later merged in the
  background.
- **Compaction** is that deferred merging: reading several sorted files and
  writing one merged file, discarding shadowed versions. It is real IO and CPU
  that competes with foreground traffic, and it happens *after* the write that
  caused it was already reported as fast.
- **Write amplification** is bytes actually written to storage ÷ bytes the
  application asked to write. It is the price of compaction. Topic 4's notes
  give the arithmetic: leveled compaction with size ratio `T` over `L` levels
  rewrites each byte about `T/2` times per level, so `T/2 × L` — at `T=10`,
  `L=4` that is **~20×** ([topic 4 notes](../04-lsm-deep-dive/notes.md)).
- **Space amplification** is bytes on disk ÷ logical bytes. Topic 1 measured
  it end to end on the same 108 MB of records: **0.45× for fjall** (an LSM,
  which compresses its sorted runs) against **63.28× for redb** (a
  copy-on-write B-tree under random-order inserts) — a 140× spread
  ([FINDINGS.md](../../FINDINGS.md) row 1).

So the same engine can absorb a sequential load near disk bandwidth and
collapse under random overwrites, purely because of compaction debt — and the
same *workload* can produce wildly different verdicts on two engine families.

db_bench's fix: give each meaningful point on that surface a *name*, so "we
ran `fillrandom` then `readwhilewriting`" pins down the experiment as
precisely as a chess opening's name pins down twelve moves. The menu of names
is one flag, `DEFINE_string(benchmarks, …)` at **139-194**, with its own help
text at **196-316** — that help text is the best documentation the tool has.

Why it matters: LevelDB shipped these names in 2011, RocksDB extended them,
and every LSM paper since reports in them — the vocabulary *is* the
comparability.

### Step 2 — every workload is "pick an integer, then lay it out as a key"

> **In:** the axes from Step 1, in particular key order.
> **Out:** the single function that decides key order for every `fill*`
> workload, and the duplicate rate it produces — the input to Step 3's four
> names.

Under every workload name sits the same skeleton: choose an integer, then lay
that integer out as a fixed-width key. All the drama — sequential vs random,
insert vs overwrite — lives entirely in *how the next integer is chosen*.
There are exactly three ways, and they are an enum:

```cpp
// tools/db_bench_tool.cc — the whole key-order axis, 6718
  6718    enum WriteMode { RANDOM, SEQUENTIAL, UNIQUE_RANDOM };
```

`KeyGenerator` (**6934-6980**) turns that enum into integers, and its `Next()`
is thirteen lines that contain everything Step 3 will name:

```cpp
// tools/db_bench_tool.cc — KeyGenerator::Next, 6953-6965
  6953      uint64_t Next() {
  6954        switch (mode_) {
  6955          case SEQUENTIAL:
  6956            return next_++;
  6957          case RANDOM:
  6958            return rand_->Next() % num_;
  6959          case UNIQUE_RANDOM:
  6960            assert(next_ < num_);
  6961            return values_[next_++];
  6962        }
  6963        assert(false);
  6964        return std::numeric_limits<uint64_t>::max();
  6965      }
```

The line that carries the argument is **6958**: `% num_` is a draw *with
replacement*, so the same key can come up twice. `values_` on 6961 is a
pre-shuffled permutation of `0..num_`, built once in the constructor:

```cpp
// tools/db_bench_tool.cc — inside the KeyGenerator constructor, 6939-6950
  6939        if (mode_ == UNIQUE_RANDOM) {
  // ... 6940-6943: a comment on the memory cost of materialising the vector ...
  6944          values_.resize(num_);
  6945          for (uint64_t i = 0; i < num_; ++i) {
  6946            values_[i] = i;
  6947          }
  6948          RandomShuffle(values_.begin(), values_.end(),
  6949                        static_cast<uint32_t>(*seed_base));
  6950        }
```

Line **6944** is the cost of the mode: `UNIQUE_RANDOM` materialises an 8-byte
slot per key before the first write, so `--num=1000000000` wants 8 GB of RAM
just for the permutation. Line 6949 is why the order reproduces: the shuffle
is seeded from `seed_base`.

**The integer then becomes a key** through `GenerateKeyFromInt`
(**4462-4502**) — and here the tidy story ("zero-padded fixed-width key") is
wrong. The code's own comment at 4457-4461 says what it does:

```cpp
// tools/db_bench_tool.cc — GenerateKeyFromInt's comment and its tail, 4457-4461 and 4490-4502
  4457    //   - If keys_per_prefix_ is 0, the key is simply a binary representation of
  4458    //     random number followed by trailing '0's
  4459    //     ----------------------------
  4460    //     |        key 00000         |
  4461    //     ----------------------------
  // ... 4462-4489: the signature, the --use_existing_keys shortcut, and the
  // ...            optional prefix block written when keys_per_prefix_ > 0 ...
  4490      int bytes_to_fill = std::min(key_size_ - static_cast<int>(pos - start), 8);
  4491      if (port::kLittleEndian) {
  4492        for (int i = 0; i < bytes_to_fill; ++i) {
  4493          pos[i] = (v >> ((bytes_to_fill - i - 1) << 3)) & 0xFF;
  4494        }
  4495      } else {
  4496        memcpy(pos, static_cast<void*>(&v), bytes_to_fill);
  4497      }
  4498      pos += bytes_to_fill;
  4499      if (key_size_ > pos - start) {
  4500        memset(pos, '0', key_size_ - (pos - start));
  4501      }
  4502    }
```

Line **4493** is the one to look at. The shift `(bytes_to_fill - i - 1) << 3`
emits the *most significant* byte first: the integer is written **big-endian
binary**, not decimal digits. The padding on 4500 is ASCII `'0'` (0x30) filling
the remaining `--key_size` bytes (default 16, line 440). Big-endian is the
load-bearing choice: RocksDB's default comparator orders keys byte by byte, so
big-endian layout makes byte order agree with numeric order, and that is
*why* `SEQUENTIAL` integers produce keys that arrive in sorted order.

**How many duplicates does `RANDOM` produce?** The guide's claim is "~37% of a
full pass", and it is worth deriving rather than asserting, because it is the
whole difference between `fillrandom` and `filluniquerandom`. Line 6958 draws
uniformly from `0..n-1` (up to negligible modulo bias, `n ≪ 2^64`), and
`DoWrite` runs `num_ops = num_` draws per thread (**7006**), each thread with
its own generator over the full key space (**7023-7027**). So, with
`--threads=1`:

```
symbols
  n   = FLAGS_num = the key space size AND the number of draws (7006, 7023)
  k   = a particular key in 0..n-1

  P(one draw misses k)      = 1 - 1/n            each draw is uniform (6958)
  P(all n draws miss k)     = (1 - 1/n)^n        draws are independent
  E[keys never written]     = n * (1 - 1/n)^n
  E[distinct keys written]  = n * (1 - (1 - 1/n)^n)
  E[duplicate writes]       = n - E[distinct] = n * (1 - 1/n)^n

worked on the guide's own --num=10000000, and on four smaller n to show the limit

  n = 10          (1-1/n)^n = 0.348678   ->  3.49 of 10 keys never written
  n = 100         (1-1/n)^n = 0.366032   ->  36.6 of 100
  n = 1000        (1-1/n)^n = 0.367695   ->  368 of 1000
  n = 1000000     (1-1/n)^n = 0.3678793  ->  367,879 of 1,000,000
  n = 10000000    (1-1/n)^n = 0.36787942 ->  3,678,794 of 10,000,000

  at n = 10,000,000:
    expected keys never written   = 3,678,794   (36.788%)
    expected distinct keys        = 6,321,206   (63.212%)
    expected duplicate writes     = 10,000,000 - 6,321,206 = 3,678,794  (36.788%)

  the limit: (1 - 1/n)^n -> 1/e = 0.36787944...   (already matched to 7 digits at n = 1e7)
```

Two counts fall out, and they are equal by conservation — every write is
either a key's first appearance or a repeat, so `n − distinct` repeats and
`n × (1−1/n)^n` never-written keys are the same number. So a 10 M-key
`fillrandom` writes **10 M records into 6.32 M distinct keys**, leaving 3.68 M
keys never touched and 3.68 M writes that are overwrites. Each of those
overwrites creates a dead version that compaction must later rewrite and drop
— write amplification (Step 1) with nothing to show for it.

`UNIQUE_RANDOM` exists precisely to remove that term: 6961 hands out each
integer exactly once, so arrival order is random but the duplicate count is
**zero**. That is the isolation experiment — random *placement* without random
*garbage*.

Why it matters: once you see this skeleton, the entire workload menu in
Steps 3-4 is this one function plus an operation type, and "random writes"
splits into two genuinely different experiments.

### Step 3 — the fill family: four ways to write

> **In:** the three key orders from Step 2, and the duplicate arithmetic.
> **Out:** four named write workloads, each pinning one more variable — the
> write-side half of the menu Step 6 composes into a methodology.

The `fill*` names are write workloads. `Benchmark::Run`'s dispatch chain turns
each name into a method pointer and a couple of flag mutations, and the
mutations are where the meaning lives:

```cpp
// tools/db_bench_tool.cc — the fill family's arms of the dispatch chain, 4700-4726
  4700        } else if (name == "fillseq") {
  4701          fresh_db = true;
  4702          method = &Benchmark::WriteSeq;
  // ... 4703-4706: fillbatch, the same but with entries_per_batch_ = 1000 ...
  4707        } else if (name == "fillrandom") {
  4708          fresh_db = true;
  4709          method = &Benchmark::WriteRandom;
  // ... 4710-4719: filluniquerandom, which forces num_threads to 1 ...
  4720        } else if (name == "overwrite") {
  4721          method = &Benchmark::WriteRandom;
  4722        } else if (name == "fillsync") {
  4723          fresh_db = true;
  4724          num_ /= 1000;
  4725          write_options_.sync = true;
  4726          method = &Benchmark::WriteRandom;
```

The two lines that carry the argument are **4720-4721**: `overwrite` and
`fillrandom` call the *same method*, `WriteRandom`. The entire difference is
that `fillrandom` sets `fresh_db = true` on 4708 and `overwrite` does not —
Step 6 shows what `fresh_db` does. So:

- **`fillseq`** — `SEQUENTIAL` mode (4702 → 6729 → `DoWrite(thread,
  SEQUENTIAL)`). The LSM fast path: keys arrive in the comparator's own order
  (Step 2's big-endian layout), so successive memtable flushes produce files
  with disjoint key ranges and there is nothing for compaction to merge.
  Papers use it to *build* the database before the real test — it is a setup
  step wearing a benchmark's name.
- **`fillrandom`** — random-order inserts into a fresh DB (4707-4709). Files
  overlap, compaction runs continuously, and Step 2's 3.68 M duplicate writes
  per 10 M ops add garbage on top. This is the honest write-throughput number.
- **`overwrite`** — the same random writes with **no** fresh DB (4720-4721),
  so it runs against whatever the previous entry in the comma list left
  behind. Against a DB that already holds the full key space, nearly every
  write shadows a live version, which is maximum compaction pressure — a
  different beast from `fillrandom` even though both are "random writes", and
  the one that matches a steady-state production database.
- **`fillsync`** — random writes with `write_options_.sync = true` (4725) over
  `num_ / 1000` ops (4724). The `/1000` is the tell: the author already knew
  this workload is far slower and shortened it so the run would finish.

**How much slower is `fillsync`?** Not a number to guess, and the tidy
folklore answer ("three to four orders of magnitude") is wrong. Two sources
settle it. First, what `sync = true` actually promises — RocksDB's own header
is unusually precise:

```cpp
// include/rocksdb/options.h — the WriteOptions::sync contract, 2706-2719
  2706    // If true, the write will be flushed from the operating system
  2707    // buffer cache (by calling WritableFile::Sync()) before the write
  2708    // is considered complete.  If this flag is true, writes will be
  2709    // slower.
  // ... 2710-2715: what is and is not lost when the process or machine dies ...
  2716    // In other words, a DB write with sync==false has similar
  2717    // crash semantics as the "write()" system call.  A DB write
  2718    // with sync==true has similar crash semantics to a "write()"
  2719    // system call followed by "fdatasync()".
```

Line **2719** is the one that decides the number: the promise is
`fdatasync`-grade, *not* a drive cache flush. Second, topic 5 measured that
exact ladder ([FINDINGS.md](../../FINDINGS.md) row 5, Apple M3 Pro / APFS):

```
per-call p50 and the implied single-threaded commit ceiling (topic 5, notes.md)

  write() only    1.17 µs   ->  856,898 commits/s   the fillseq/fillrandom rung
  fsync          22.67 µs   ->   44,109 commits/s   856,898 / 44,109 =    19.4x slower
  F_FULLFSYNC     2.97 ms   ->      337 commits/s   856,898 /    337 = 2,542x slower

  fillsync's rung, per options.h:2719                = the middle one
  so the expected gap is  ~19x  (1.3 orders of magnitude), not 3-4 orders
```

The "3-4 orders" figure belongs to the *bottom* rung — a `Sync()` that really
flushes the drive's volatile cache, which on macOS is `F_FULLFSYNC` and costs
2,542× (3.4 orders). Both rungs get called "fsync" in conversation and they
differ by 131×, so a `fillsync` number is uninterpretable until you know which
one your platform's `WritableFile::Sync()` reached. That is Step 1's point
applied to db_bench itself.

Why it matters: a paper quoting "write throughput" without saying which of
these four it ran has told you almost nothing — and the durability axis alone,
measured in this repo, spans 19× to 2,542× depending on a distinction the
benchmark name does not make.

### Step 4 — the read family: point, scan, and interference

> **In:** a database in whatever state the write workloads of Step 3 left it.
> **Out:** the read-side half of the menu, plus the one name whose reported
> number comes from a subset of its threads — which Step 7 revisits.

The read-side names split along two axes — access shape, and whether writes
run concurrently:

- **`readrandom` / `readseq` / `readreverse`** (dispatch at 4748, 4732, 4746)
  — point lookups versus iterator scans, forward and backward. A **point
  lookup** must consult the memtable and then every level that could hold the
  key, so it may touch several files; a **scan** positions once and then
  streams.
- **`seekrandom`** (4845) — the cost of positioning an iterator. `Seek` has to
  touch every level to build the merging iterator's min-heap before it can
  yield the first key (`table/merging_iterator.cc:23-39` states the invariant
  every `Seek*()` must restore), so its profile is unlike a point `Get`.
- **`multireadrandom`** (4763) — `MultiGet` batching: `entries_per_batch_`
  keys per call, amortising per-call overhead across the batch.
- **`readwhilewriting`** (4860-4862) — the "does compaction wreck my read
  tail?" test, and the closest thing in the menu to production. The
  `*whilemerging` / `*whilescanning` variants (4863-4868) isolate other
  interference sources.

`readwhilewriting` is worth one more level of detail, because its thread
arithmetic is not what the flag says:

```cpp
// tools/db_bench_tool.cc — the dispatch arm, 4860-4862, and the method, 9770-9776
  4860        } else if (name == "readwhilewriting") {
  4861          num_threads++;  // Add extra thread for writing
  4862          method = &Benchmark::ReadWhileWriting;
  // ... 4863-9769: the rest of the dispatch chain, and the other benchmarks ...
  9770    void ReadWhileWriting(ThreadState* thread) {
  9771      if (thread->tid > 0) {
  9772        ReadRandom(thread);
  9773      } else {
  9774        BGWriter(thread, kWrite);
  9775      }
  9776    }
```

Line **4861** adds one thread beyond `--threads`, and **9771** makes thread 0
the writer and threads 1..N readers. So `--threads=8 readwhilewriting` runs
nine threads: eight readers and one writer. And the writer removes itself from
the reported figure:

```cpp
// tools/db_bench_tool.cc — inside BGWriter, 9805-9806
  9805      // Don't merge stats from this thread with the readers.
  9806      thread->stats.SetExcludeFromMerge();
```

Line **9806** sets the flag that `Stats::Merge` honours at 2984-2986 (Step 7),
so the ops/s `readwhilewriting` prints is a *readers-only* number measured
while a writer ran. That is the right choice — mixing a writer's ops into a
read throughput figure would be meaningless — but it means the line tells you
nothing about what the writer achieved, and the writer's rate is a free
variable unless you also set `--benchmark_write_rate_limit` (1859-1862).

Why it matters: read-only numbers (`readrandom` on a freshly-compacted DB) are
the engine's best case; `readwhilewriting` is where LSM read/write
interference — the thing users actually hit — shows up, and it is reported
from only part of the run.

### Step 5 — distribution knobs: uniform lies, skew is reality

> **In:** the read workloads of Step 4, which so far draw keys uniformly.
> **Out:** the two skew models db_bench offers, and the property that separates
> them — a property Step 7's caveats depend on.

By default the random modes draw keys **uniformly** — every key equally
likely, which is exactly what line 6958's `% num_` gives. Production traffic is
**skewed**: a few hot keys absorb most of the requests. The classic model is a
**Zipfian distribution**, where the popularity of the k-th hottest key falls
off as `1/k^s` for some exponent `s`, so a small fraction of keys takes a large
fraction of the traffic. (The capstone's `workload` crate uses `s = 0.99`, the
YCSB default, for the same reason.)

The difference is not cosmetic. Uniform access over a key space larger than
memory defeats every cache — no key is hot enough to stay resident — while
skewed access lets the block cache serve most reads. The two can disagree on
read throughput by an order of magnitude on identical hardware.

db_bench offers two skew knobs, and they differ in a way that matters more
than the shape of the curve.

**`--read_random_exp_range`** (**524-529**) bends `readrandom`'s draw
exponentially:

```cpp
// tools/db_bench_tool.cc — GetRandomKey, 7952-7969
  7952    int64_t GetRandomKey(Random64* rand) {
  7953      uint64_t rand_int = rand->Next();
  7954      int64_t key_rand;
  7955      if (read_random_exp_range_ == 0) {
  7956        key_rand = rand_int % FLAGS_num;
  7957      } else {
  // ... 7958-7964: order = -(uniform in [0,1)) * read_random_exp_range_, then
  // ...            rand_num = exp(order) * FLAGS_num, which concentrates draws
  // ...            near 0 — larger flag value, sharper skew ...
  7965        // Map to a different number to avoid locality.
  7966        const uint64_t kBigPrime = 0x5bd1e995;
  7967        // Overflow is like %(2^64). Will have little impact of results.
  7968        key_rand = static_cast<int64_t>((rand_num * kBigPrime) % FLAGS_num);
  7969      }
```

The line to focus on is **7968**, and the comment above it on 7965 states the
intent: the hot key *IDs* are deliberately scattered across the key space by
multiplying them by a constant and taking the result modulo `FLAGS_num`. So
this flag gives you hotness **without** key-space locality — the hot keys are
hot, but they live in different SST blocks. Hold that; it is the exact defect
the FAST'20 paper measures.

Take the variable's *name* on 7966 as a claim to check rather than a fact.
`kbigprime_demo.py` in this directory reimplements 7952-7970 and swaps the
constant, and three things come out of it. 0x5bd1e995 is not prime — it is
13 × 199 × 457 × 1303, the mixing constant `m` of 32-bit MurmurHash2, which
the same tree carries at `util/murmurhash.cc:97` and `:153`. The multiply is
worth about 14× the block-cache working set at `--num=1000000`
(277 blocks → 3,855, for identical traffic), which is FAST'20 §7.1's
complaint as a number. And what makes a multiplier work is not its size but
`M mod FLAGS_num`: the larger, genuinely prime 1,000,000,007 is congruent to
7 there and barely scatters at all. Run the script before Step 7's caveats —
the number a benchmark reports can turn on a constant nobody re-derived.

**`mixgraph`** (dispatch at **4835**) is the industrial-strength answer. It
models Facebook's *measured* production workloads, from Cao et al.,
"Characterizing, Modeling, and Benchmarking RocksDB Key-Value Workloads at
Facebook" (FAST '20). §7.1 of that paper is the indictment: YCSB reproduces
the overall hotness distribution, but "the hot KV-pairs are actually randomly
distributed in the whole key-space", which makes a large number of data blocks
hot and triggers "an extremely large number of block reads" — and the paper
adds explicitly, "db_bench has a similar situation". §7.2 is the fix: partition
the key space into key-ranges sized at the average number of KV-pairs per SST
file, and model the *hotness of the ranges*, so hot keys sit near each other.

The paper's fitted models (§7.4, on UDB's Assoc workload) map onto db_bench's
flags almost one to one — with one discrepancy the paper wins:

| db_bench flag (lines) | form in the flag's help text | FAST '20 §7.4's fit |
|---|---|---|
| `keyrange_dist_a..d` (**1865-1876**) | `f(x)=a*exp(b*x)+c*exp(d*x)` — two-term **exponential** | "The average KV-pair access count of key-ranges can be better fit in a two-term **power** model" |
| `key_dist_a`, `key_dist_b` (**1880-1883**) | `f(x)=a*x^b` — simple power | "the distribution of KV-pair access counts follows a power-law that can be fit to the simple power model" ✓ |
| `value_theta/k/sigma` (**1884-1894**) | Generalized Pareto | "Generalized Pareto Distribution best fits the value sizes" ✓ |
| `iter_theta/k/sigma` (**1895-1905**) | Generalized Pareto | "…and Iterator scan length" ✓ |
| `sine_a..d` (**1848-1854**) | `f(x) = A sin(bx + c) + d` | "the QPS variation has a strong diurnal pattern… better fit to the Sine model with a 24-hour period" ✓ |

The Pareto value sizes are the paper's, confirmed at §7.4 (and §7.2 for
ZippyDB). The key-range model is **not** the paper's two-term power model:
db_bench implements a two-term *exponential*, and the code says so twice — the
help text on 1866-1867 and the call to `gen_exp.InitiateExpDistribution` at
**9375-9377**, guarded by "is any `keyrange_dist_*` non-zero" on 9372-9373.
Both are two-parameter-pair mixtures fitted to the same empirical curve, so
they are close in practice, but if you cite mixgraph as "the paper's model",
this is the term that is yours and not theirs. The `value_k = 0.2615` and
`value_sigma = 25.45` defaults, by contrast, are flagged in the source as
"reasonable defaults based on the mixgraph paper" (1887-1894).

The property that separates the two knobs: `read_random_exp_range` destroys
locality on purpose (7965-7968); `mixgraph` preserves it by construction
(9397-9399 routes the draw through the key-range distribution first). Same
hotness curve, opposite block-cache behaviour.

Why it matters: a benchmark's key distribution silently decides whether the
cache hierarchy participates in the result — and *where* the hot keys sit
decides it a second time, independently of how hot they are.

### Step 6 — the comma list is the methodology

> **In:** the named workloads of Steps 3-5, as strings.
> **Out:** the state each one runs against — the missing half of every
> published db_bench figure, and the thing Step 7 tells you to ask for.

`--benchmarks` is split on commas and run in order, against one database that
is opened once:

```cpp
// tools/db_bench_tool.cc — the top of Benchmark::Run, 4584-4595
  4584    void Run(ToolHooks& hooks) {
  4585      if (!SanityCheck()) {
  4586        ErrorExit();
  4587      }
  4588      Open(&open_options_, hooks);
  4589      PrintHeader(open_options_);
  4590      std::stringstream benchmark_stream(FLAGS_benchmarks);
  4591      std::string name;
  4592      std::unique_ptr<ExpiredTimeFilter> filter;
  4593      while (std::getline(benchmark_stream, name, ',')) {
  4594        // Sanitize parameters
  4595        num_ = FLAGS_num;
```

Line **4588** opens the DB *before* the loop on 4593, so by default every
entry inherits the previous entry's database. But — and this is the detail
that changes how you read a comma list — an entry that set `fresh_db = true`
in Step 3's dispatch destroys it first:

```cpp
// tools/db_bench_tool.cc — after the dispatch chain, 5005-5027
  5005        if (fresh_db) {
  5006          DbStateMutationGuard mutation(this);
  5007          if (FLAGS_use_existing_db) {
  5008            fprintf(stdout, "%-12s : skipped (--use_existing_db is true)\n",
  5009                    name.c_str());
  5010            method = nullptr;
  5011          } else {
  5012            if (db_.db != nullptr) {
  5013              db_.DeleteDBs();
  5014              DestroyDB(FLAGS_db, open_options_);
  5015            }
  // ... 5016-5025: the same destroy loop for the --num_multi_db case ...
  5026          Open(&open_options_, hooks);  // use open_options for the last accessed
  5027        }
```

Line **5014** is the one to look at: `DestroyDB`. Every `fill*` name except
`overwrite` sets `fresh_db` (4701, 4708, 4712, 4723, 4728), so it *wipes*
whatever came before it. So:

```
db_bench --benchmarks=fillseq,readrandom --num=10000000 --value_size=100 --histogram
              │        │
              │        └── measured against the DB fillseq just built
              └── fresh_db = true (4701) → DestroyDB (5014) → build 10M keys in order

fillseq,fillrandom,readrandom     fillrandom wipes fillseq's work (4708) — the
                                  first name contributed nothing at all
fillrandom,overwrite,readrandom   overwrite does NOT wipe (4720-4721), so it
                                  shadows fillrandom's 6.32M live keys and
                                  readrandom sees a DB thick with dead versions
```

`fillseq,readrandom` measures reads on a clean, fully-sorted DB;
`fillrandom,readrandom` measures reads on a fragmented one — same second
benchmark, very different numbers. And `--use_existing_db` (5007-5010) turns
the destroy into a *skip*, so the same command line means something different
again. That ordering **is** the methodology, and it is the first thing to
check when reproducing a published result.

Why it matters: two papers can both say "readrandom, 10M keys" and still be
measuring different databases — and a comma list you have not traced through
5005 may contain a name that did nothing.

### Step 7 — what to distrust in the reported numbers

> **In:** everything above — the workload, the state it ran against, and the
> threads that ran it.
> **Out:** a list of claims a db_bench figure can and cannot support, and the
> line of code behind each one.

Knowing how the numbers are produced tells you which claims they can support:

```mermaid
flowchart TD
    F["--benchmarks=fillseq,readrandom --histogram<br/>split on commas at 4593, run IN ORDER against one DB opened at 4588"]
    F --> RUN["Benchmark::Run 4584<br/>dispatch chain 4700-5003: name → method pointer<br/>fresh_db → DestroyDB at 5014"]
    RUN --> RB["RunBenchmark 5298<br/>spawn N threads 5332-5365"]
    RB --> T1["thread 1<br/>closed loop, e.g. ReadRandom 8026-8169<br/>Stats + HistogramImpl 2803-2839"]
    RB --> T2["thread 2 ..."]
    RB --> TN["thread N"]
    T1 --> M["merge_stats.Merge per thread 5391-5395<br/>Stats::Merge 2983-2995 adds histograms at 2991<br/>never averages percentiles — Tene's rule"]
    T2 --> M
    TN --> M
    M --> OUT["Stats::Report 3223-3269<br/>always: micros/op mean + ops/sec + MB/s<br/>percentiles only under FLAGS_histogram at 3254"]
```

- **Default output is throughput, plus a mean.** `Stats::Report` prints
  `micros/op`, `ops/sec`, elapsed seconds and MB/s on 3249-3253 — and the
  `micros/op` on 3252 is `seconds_ * 1e6 / done_`, an arithmetic mean.
  Percentiles appear only inside `if (FLAGS_histogram)` on **3254-3260**. A
  quoted p99 without that flag did not come from here.
- **The per-op clock is an inter-arrival time, not a service time.**
  `FinishedOps` (**3078-3219**) computes `micros = op_finish -
  last_op_finish_` (3086) and then sets `last_op_finish_ = op_finish` (3110).
  The interval it records is "since the previous op finished", so it includes
  everything the loop did between the two ops — key generation, the
  `GetRandomKey` call, the loop bookkeeping — not just the engine call. (With
  `--report_interval_percentiles` the reset moves to the end of the function,
  3216-3217, so the cost of printing an interval report is kept out of the
  next sample; the interval is still inter-arrival.)
- **It's a closed loop.** Each thread issues the next op only after the
  previous one completed — `ReadRandom`'s `while (!duration.Done(1))` at
  **8026** runs to `FinishedOps` at **8168**, and the only thing in between
  that waits for a clock is the optional `--benchmark_read_rate_limit`
  request on 8162-8166, which throttles a closed loop rather than opening it:
  the next op still starts only after the previous one returned. This is the same structure as redis-benchmark
  ([reading-redis-benchmark.md](reading-redis-benchmark.md)), so it suffers
  **coordinated omission**: the measurement error where the generator, by
  waiting for the server, stops issuing requests during a stall and therefore
  under-samples exactly the worst moments. A compaction stall yields a handful
  of bad samples instead of the thousands a paced workload would record. Topic
  34 measured the size of this on identical work: **closed-loop p99 = 1.0 µs
  against open-loop p99 = 90 ms**, a 90,000× understatement
  ([FINDINGS.md](../../FINDINGS.md) row 34).
- **The rate limiter does not fix it — it is explicitly excluded from the
  measurement.** `--benchmark_write_rate_limit` (1859-1862) does exist, and
  `RunBenchmark` builds the limiters at 5305-5313. But look at what `DoWrite`
  does immediately after waiting on it:

```cpp
// tools/db_bench_tool.cc — inside DoWrite, after the rate limiter's Request, 7373-7381
  7373        if (thread->shared->write_rate_limiter.get() != nullptr) {
  7374          thread->shared->write_rate_limiter->Request(
  7375              batch_bytes, Env::IO_HIGH, nullptr /* stats */,
  7376              RateLimiter::OpType::kWrite);
  7377          // Set time at which last op finished to Now() to hide latency and
  7378          // sleep from rate limiter. Also, do the check once per batch, not
  7379          // once per write.
  7380          thread->stats.ResetLastOpTime();
  7381        }
```

  Line **7380** calls `ResetLastOpTime` (3073-3076), whose whole body is
  `last_op_finish_ = clock_->NowMicros()`. The comment on 7377-7378 says the
  intent out loud: *hide* the pacing wait from the latency. So even the paced
  mode measures service time by construction — there is no intended-arrival
  timestamp anywhere in `Stats` to subtract from. Coordinated omission here is
  not an oversight, it is a documented design choice.
- **"Latency" here is service time by construction** for a second reason:
  db_bench measures the *embedded* engine — no network, no queueing, no client
  library. Legitimate for engine work; misleading if quoted as user-facing
  latency.
- **The histogram is coarse.** `HistogramImpl`'s buckets grow by **1.5×**
  (`monitoring/histogram.cc:23-42`, the `bucket_val = 1.5 * bucket_val` on
  line 28, rounded to two significant digits on 31-38), and `Percentile`
  interpolates *linearly* inside the chosen bucket
  (`monitoring/histogram.cc:130-160`, the interpolation on 137-147, clamped to
  the observed min and max on 148-155). A p99 that lands mid-bucket is
  therefore a linear guess across a range 50% wide. The printed set is fixed at
  P50/P75/P99/P99.9/P99.99 (`monitoring/histogram.cc:197-199`) — there is no
  p99.999, which is where Step 4's rare compaction stalls would have shown up.
- One thing it gets *right*: per-thread histograms are **merged**, never
  averaged. `RunBenchmark` folds each thread's `Stats` in at 5391-5395, and
  `Stats::Merge` (2983-2995) adds the *bucket counts* together at 2991. You
  cannot average percentiles (Tene's rule), and db_bench does not try. The
  same function honours `exclude_from_merge_` at 2984-2986, which is how
  Step 4's background writer drops out.

Why it matters: db_bench numbers are honest answers to narrow questions;
distrust begins when they are quoted as answers to broad ones.

## Where each step lives in the code — the skim route (30–60 min)

`tools/db_bench_tool.cc` is a **12,021-line** flag-driven monolith at `ea3136053`
— **do not read it linearly**; hit these anchors:

| Lines | What | Step |
|-------|------|------|
| 139-194 | `DEFINE_string(benchmarks, …)` — the full workload menu; the help text at 196-316 is the best documentation the tool has | 1, 3, 4 |
| 318-531 | The knobs that define a workload: `num` (318), `threads` (371), `value_size` (380), `key_size` (440), `read_random_exp_range` (524-529), `histogram` (531) | 1, 5, 7 |
| 1848-1854 | `sine_a..d` — the diurnal QPS model, `f(x) = A sin(bx+c)+d` | 5 |
| 1859-1862 | `benchmark_write_rate_limit` — the paced-write flag whose wait is hidden at 7380 | 7 |
| 1865-1876 | `keyrange_dist_a..d` — mixgraph's two-term exponential key-range model | 5 |
| 1880-1905 | `key_dist_a/b` (power), `value_*` and `iter_*` (Generalized Pareto) — the rest of the mixgraph fit | 5 |
| 2803-2839 | `class Stats` — per-thread state; `hist_` (2828) is an `OperationHistograms`, a map of `HistogramImpl` per op type (2763-2765) | 7 |
| 2983-2995 | `Stats::Merge` — histogram buckets added at 2991, `exclude_from_merge_` honoured at 2984-2986 | 4, 7 |
| 3073-3076 | `Stats::ResetLastOpTime` — one line, and the whole coordinated-omission story | 7 |
| 3078-3219 | `Stats::FinishedOps` — `micros = op_finish - last_op_finish_` (3086), recorded only under `FLAGS_histogram` (3084, 3093) | 7 |
| 3223-3269 | `Stats::Report` — throughput always (3249-3253), percentiles only at 3254-3260 | 7 |
| 4457-4502 | `GenerateKeyFromInt` — big-endian binary at 4493, `'0'` padding at 4500, not decimal zero-padding | 2 |
| 4584-4595 | `Benchmark::Run` — `Open` once at 4588, comma split at 4593 | 6 |
| 4700-5003 | The dispatch chain: `name == "fillseq"` → method pointer. Fill family 4700-4731, read family 4732-4766, `mixgraph` 4835, `seekrandom` 4845, `readwhilewriting` 4860-4862, unknown-name error 5000-5002 | 3, 4, 5, 6 |
| 5005-5027 | `if (fresh_db)` → `DestroyDB` at 5014 — why some comma-list entries erase the ones before them | 6 |
| 5298-5424 | `RunBenchmark` — rate limiters 5305-5313, thread spawn 5332-5365, per-thread merge 5391-5395 | 7 |
| 6718 | `enum WriteMode { RANDOM, SEQUENTIAL, UNIQUE_RANDOM }` | 2 |
| 6934-6980 | `class KeyGenerator` — the shuffle at 6944-6949, `Next()` at 6953-6965 | 2 |
| 7004-7027 | `DoWrite` — one `KeyGenerator` per thread over the whole key space | 2, 3 |
| 7373-7381 | the write rate limiter, and `ResetLastOpTime` hiding its wait | 7 |
| 7952-7969 | `GetRandomKey` — exponential skew, then `kBigPrime` to destroy locality (7965-7968) | 5 |
| 8026-8169 | `ReadRandom`'s loop — a closed loop, `FinishedOps` at 8168 | 7 |
| 9372-9377 | mixgraph's `InitiateExpDistribution` — the two-term exponential, in code | 5 |
| 9770-9776 | `ReadWhileWriting` — tid 0 writes, the rest read | 4 |
| 9805-9806 | `SetExcludeFromMerge` — the writer drops out of the reported number | 4, 7 |

Suggested route: the menu (139-194) and its help text → `Benchmark::Run`
(4584) → the dispatch chain (4700-5003) for the three or four names you care
about → `fresh_db` (5005-5027) → `KeyGenerator::Next` (6953) →
`GenerateKeyFromInt` (4462) → `RunBenchmark` (5298) → `FinishedOps` (3078) and
`Report` (3223). As you trace it, look for an intended-arrival timestamp
anywhere in `Stats`'s members (2816-2837); its absence is the last bullet of
Step 7.

Three anchors live outside `db_bench_tool.cc` and are worth the detour:
`include/rocksdb/options.h:2706-2719` (what `sync = true` actually promises —
Step 3), `monitoring/histogram.cc:23-42` and `130-160` (how coarse every
printed percentile is — Step 7), and `table/merging_iterator.cc:23-39` (why
`seekrandom` costs what it does — Step 4).

## Questions to answer in notes.md

1. `fillrandom` and `overwrite` dispatch to the *same* method, `WriteRandom`
   (4709, 4721). Find the one line that differs, then say what
   `fillrandom,overwrite,readrandom` measures that `fillrandom,readrandom`
   does not.
2. `GenerateKeyFromInt` writes the integer big-endian at 4493. Rewrite line
   4493 as little-endian in your head: which of Step 3's four fill workloads
   changes character, and what happens to compaction under `fillseq`?
3. Step 2's arithmetic says a 10 M-op `fillrandom` touches 6.32 M distinct
   keys. Run the same derivation for `--num=1000 --writes=10000` (10 draws per
   key): what fraction of the key space is still never written, and why does
   the answer stop being 1/e?
4. `--benchmark_write_rate_limit` paces writes, and 7380 then hides the pacing
   wait from the histogram. If you wanted db_bench to report open-loop latency
   instead, which field would you add to `class Stats` (2803-2839), and which
   of 3086 and 7380 would have to change?
5. `readwhilewriting` with `--threads=8` runs nine threads and reports eight
   of them (4861, 9771, 9806). Design the smallest experiment using only the
   flags in 318-531 and 1859-1862 that tells you whether a p99 regression came
   from the writer's rate or from compaction.

## Takeaway

db_bench's value is the workload taxonomy, not the harness. When topic 4 (LSM)
and M4 (backend shootout) arrive, name capstone benches in this vocabulary
(`fillseq`, `readrandom`, `readwhilewriting`) so numbers are comparable against
published RocksDB results — and record the comma list, not just the name, since
Step 6 shows the list is the methodology.

## Done when

Answer each before unfolding it.

- [ ] You can explain why a shared workload vocabulary matters more than any single number db_bench prints.

  <details><summary>Answer</summary>

  Because a single number names a point on a surface without naming the point.
  The axes are operation mix, key order, insert-vs-overwrite, durability, and
  what background work was running, and this repo has measured two of them
  independently: the durability axis alone spans 856,898/s (buffered `write()`)
  to 44,109/s (`fsync`) to 337/s (`F_FULLFSYNC`), a 2,542× range
  ([FINDINGS.md](../../FINDINGS.md) row 5); the same 108 MB of records lands at
  0.45× space amplification on an LSM and 63.28× on a copy-on-write B-tree
  (row 1). A number without the axes is compatible with almost any engine
  quality.

  A name fixes the axes. `fillrandom` means `WriteRandom` into a destroyed and
  recreated DB (4707-4709 plus the `DestroyDB` at 5014); `overwrite` means the
  same method against inherited state (4720-4721); `fillsync` means
  `write_options_.sync = true` over `num_/1000` ops (4722-4726). Three names,
  one method pointer, three different experiments — and anyone who has read
  those twenty lines can reproduce yours.

  </details>

- [ ] You can name the four members of the fill family and say which one is the adversarial case for a B-tree — then check that against topic 1's measured 63.28× space amplification on redb.

  <details><summary>Answer</summary>

  `fillseq` (4700-4702, `SEQUENTIAL`), `fillrandom` (4707-4709, `RANDOM` into a
  fresh DB), `overwrite` (4720-4721, `RANDOM` into inherited state), `fillsync`
  (4722-4726, `RANDOM` with `sync = true` over `num_/1000` ops). A fifth,
  `filluniquerandom` (4710-4719), is the isolation experiment: random order,
  zero duplicates, single-threaded by force.

  `fillrandom` is the adversarial case for a B-tree, and topic 1 measured
  exactly it: 1.08 M records of 100 B in random key order, batched 1000 at a
  time, gave redb (a copy-on-write B-tree) **63.28× space amplification** —
  6,833.9 MB on disk for 108.0 MB of logical data — against fjall's 0.45×
  ([FINDINGS.md](../../FINDINGS.md) row 1,
  [topic 1 notes](../01-storage-engine-landscape/notes.md)). The mechanism in
  those notes is the same one Step 2 derives: random-order inserts touch a new
  leaf almost every time, and each batch commit copies every page on the path
  to the root without being able to free the old ones yet. Sequential order
  (`fillseq`) removes it, which is why using `fillseq` as your headline write
  number flatters both engine families and settles nothing.

  </details>

- [ ] You can explain why a uniform key distribution flatters almost every engine, and what changes under skew — including the difference between db_bench's two skew knobs.

  <details><summary>Answer</summary>

  Uniform is the default because line 6958 is `rand_->Next() % num_`, and it is
  the *hardest* case for caching: with the key space larger than memory, no key
  is requested often enough to stay resident, so the block cache hit rate
  collapses toward the ratio of cache size to data size. Real traffic is
  skewed — a Zipfian tail where popularity falls as `1/k^s` — so the same
  engine on the same hardware serves most reads from cache and posts an
  order-of-magnitude better number. Reporting uniform is not conservative; it
  measures a workload nobody runs.

  The two knobs differ in *where* the hot keys sit, not how hot they are.
  `--read_random_exp_range` (524-529, implemented at 7952-7969) skews the draw
  exponentially and then multiplies by `kBigPrime` at 7968, under a comment
  that says "Map to a different number to avoid locality" — hotness with the
  key-space locality deliberately removed. `mixgraph` (4835) does the opposite:
  it models the hotness of *key-ranges* first (9372-9377, 9397-9399), so hot
  keys share SST blocks. FAST '20 §7.1 is exactly this measurement — YCSB
  reproduces the hotness curve but scatters the hot keys, which triggers "an
  extremely large number of block reads", and the paper notes "db_bench has a
  similar situation".

  </details>

- [ ] You can read a db_bench comma list and reconstruct the methodology it encodes — including which entries erased the ones before them.

  <details><summary>Answer</summary>

  The list is split on commas at 4593 and run in order against a database
  opened once at 4588 — but every entry that set `fresh_db` in the dispatch
  chain hits `DestroyDB` at 5014 before it runs. `fillseq` (4701), `fillrandom`
  (4708), `filluniquerandom` (4712), `fillsync` (4723) and `fill100K` (4728)
  all do; `overwrite` (4720-4721) pointedly does not.

  So `fillseq,readrandom` reads a clean, fully-sorted, compaction-debt-free DB;
  `fillrandom,readrandom` reads a fragmented one holding ~6.32 M distinct keys
  out of 10 M writes (Step 2's arithmetic); `fillrandom,overwrite,readrandom`
  reads a DB thick with dead versions, because `overwrite` inherited
  `fillrandom`'s keys and shadowed them. And `fillseq,fillrandom,readrandom`
  contains a lie of omission: `fillrandom` destroyed everything `fillseq`
  built, so the first name contributed nothing but its own throughput line. One
  more flag changes it again — `--use_existing_db` (5007-5010) converts the
  destroy into a *skip*, so the fill entry prints "skipped" and the read runs
  against whatever was on disk.

  </details>

- [ ] You can name three things in a reported db_bench figure you would refuse to take at face value, and what you would ask for instead.

  <details><summary>Answer</summary>

  First, **a percentile without `--histogram`**. `Stats::Report` prints ops/s,
  MB/s and a mean `micros/op` (3249-3253); the percentile block is inside
  `if (FLAGS_histogram)` at 3254-3260, and `FinishedOps` does not even record
  the sample otherwise (3084, 3093). Ask for the full command line. Then ask which
  percentiles: the set is fixed at P50/P75/P99/P99.9/P99.99
  (`monitoring/histogram.cc:197-199`), the buckets grow by 1.5×
  (`monitoring/histogram.cc:23-42`), and `Percentile` interpolates linearly
  inside them (130-160), so a mid-bucket p99 is a guess across a 50%-wide range.

  Second, **any tail number at all**, because the loop is closed:
  `ReadRandom` runs 8026→8168 issuing each op only after the last returned,
  and even the paced write mode hides the wait — `DoWrite` calls
  `ResetLastOpTime` right after the rate limiter (7380), under a comment that
  says the goal is to "hide latency and sleep from rate limiter" (7377-7378). Topic 34 measured what that costs
  on identical work: p99 of 1.0 µs closed-loop against 90 ms open-loop, a
  90,000× understatement ([FINDINGS.md](../../FINDINGS.md) row 34). Ask for an
  open-loop rerun, or treat the figure as service time only.

  Third, **the benchmark name without its comma list and its flags**, per
  Step 6: `readrandom` alone does not say whether `DestroyDB` ran at 5014
  before it, whether the keys were uniform or bent by
  `--read_random_exp_range` (524), or — for `readwhilewriting` — that the
  printed number came from `--threads` readers while a writer that
  `SetExcludeFromMerge`'d itself (9806) ran at an unstated rate. Ask for the
  whole invocation, not the name.

  </details>

- [ ] You can say what `fillsync` actually measures, and why "three to four orders of magnitude below fillseq" is the wrong number for it.

  <details><summary>Answer</summary>

  `fillsync` is `WriteRandom` with `write_options_.sync = true` (4725) over
  `num_ / 1000` ops (4723-4726) into a freshly destroyed DB — so it varies two
  things against `fillseq` at once, key order *and* durability, which already
  makes the comparison a poor isolation experiment. `fillrandom` is the right
  control.

  The size of the durability term is pinned by RocksDB's own header:
  `include/rocksdb/options.h:2716-2719` says a write with `sync == true` has
  "similar crash semantics to a `write()` system call followed by
  `fdatasync()`" — the `fdatasync` rung, not a drive cache flush. Topic 5
  measured that ladder on this hardware: `write()` p50 1.17 µs → 856,898
  commits/s, `fsync` p50 22.67 µs → 44,109/s, `F_FULLFSYNC` p50 2.97 ms →
  337/s ([FINDINGS.md](../../FINDINGS.md) row 5). 856,898 / 44,109 = **19.4×**,
  about 1.3 orders of magnitude. The 3–4 orders belongs to the bottom rung:
  856,898 / 337 = 2,542×, or 3.4 orders — and that rung costs 131× more than
  the one `sync = true` promises. So quote 19× unless you know the platform's
  `WritableFile::Sync()` reached the cache flush, and say which you mean.

  </details>

## References

**Papers**
- Cao, Dong, Vemuri, Du — "Characterizing, Modeling, and Benchmarking RocksDB
  Key-Value Workloads at Facebook", FAST '20
  ([PDF](https://www.usenix.org/system/files/fast20-cao_zhichao.pdf)) — the
  measured production distributions behind `mixgraph`. Read **§7.1** (why YCSB
  and db_bench mislead: hot keys scattered across the key space cause "an
  extremely large number of block reads"; "db_bench has a similar situation"),
  **§7.2** (key-range based modeling, and why the key-range size is the average
  number of KV-pairs per SST file), and **§7.4** (the fitted models: two-term
  *power* for key-range access counts, simple power within a range, Generalized
  Pareto for value sizes and iterator scan lengths, Sine for QPS). Note the
  mismatch flagged in Step 5: db_bench implements a two-term *exponential*
  where §7.4 reports a two-term *power* fit.

**Code**
- [rocksdb](https://github.com/facebook/rocksdb) `tools/db_bench_tool.cc`
  (**12,021 lines** at `ea3136053`, the commit in `resources/codebases.md`'s pin
  table) — **do not read this linearly**; it is a flag-driven monolith. Follow
  the skim route above (30–60 min).

| File | Lines | What |
|------|-------|------|
| `tools/db_bench_tool.cc` | 139-194 | the workload menu, with its help text at 196-316 |
| `tools/db_bench_tool.cc` | 318-531 | the knobs: `num`, `threads`, `value_size`, `key_size`, `read_random_exp_range`, `histogram` |
| `tools/db_bench_tool.cc` | 1865-1905 | mixgraph's fitted-distribution flags |
| `tools/db_bench_tool.cc` | 2983-2995 | `Stats::Merge` — histograms added, never averaged |
| `tools/db_bench_tool.cc` | 3073-3076 | `ResetLastOpTime` — the pacing wait, hidden |
| `tools/db_bench_tool.cc` | 3078-3219 | `FinishedOps` — the only per-op clock |
| `tools/db_bench_tool.cc` | 3223-3269 | `Stats::Report` — throughput always, percentiles under a flag |
| `tools/db_bench_tool.cc` | 4462-4502 | `GenerateKeyFromInt` — big-endian binary, `'0'`-padded |
| `tools/db_bench_tool.cc` | 4700-5003 | the name → method dispatch chain |
| `tools/db_bench_tool.cc` | 5005-5027 | `fresh_db` → `DestroyDB` |
| `tools/db_bench_tool.cc` | 5298-5424 | `RunBenchmark` — spawn, join, merge |
| `tools/db_bench_tool.cc` | 6718 | `enum WriteMode` |
| `tools/db_bench_tool.cc` | 6934-6980 | `KeyGenerator` — the three key orders |
| `tools/db_bench_tool.cc` | 7373-7381 | the write rate limiter and its hidden wait |
| `tools/db_bench_tool.cc` | 7952-7969 | `GetRandomKey` — exponential skew, locality destroyed |
| `tools/db_bench_tool.cc` | 9770-9776 | `ReadWhileWriting` — tid 0 writes |
| `monitoring/histogram.cc` | 23-42 | 1.5× bucket growth — the resolution of every db_bench percentile |
| `monitoring/histogram.cc` | 130-160 | `Percentile` — linear interpolation inside a 50%-wide bucket |
| `monitoring/histogram.cc` | 197-199 | the fixed printed set: P50, P75, P99, P99.9, P99.99 |
| `include/rocksdb/options.h` | 2706-2719 | `WriteOptions::sync` — "similar crash semantics to a `write()` followed by `fdatasync()`", which is the rung `fillsync` lands on |
| `table/merging_iterator.cc` | 23-39 | the min-heap invariant every `Seek*()` restores — why `seekrandom` is not a point `Get` |

**Connections**
- [reading-redis-benchmark.md](reading-redis-benchmark.md) — the same
  closed-loop defect in a network load generator, with the open-loop fix
  sketched.
- [topic 34](../34-debugging/README.md) — coordinated omission measured:
  p99 1.0 µs closed-loop against 90 ms open-loop.
- [topic 5](../05-durability-wal/README.md) — the fsync ladder `fillsync`
  lands on.
- [topic 1](../01-storage-engine-landscape/README.md) — `fillrandom`'s space
  amplification, measured on two engine families.
