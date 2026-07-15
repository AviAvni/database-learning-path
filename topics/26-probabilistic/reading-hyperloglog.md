# HyperLogLog: count distinct in 12 KB

`count(DISTINCT x)` over billions of elements, 0.81% error, 12 KB of
state, and per-shard sketches that merge losslessly in any order — one
probabilistic observation buys all of it. This chapter builds the
estimator step by step from that observation, then walks redis's
production implementation, which adds a sparse encoding and a better
count formula on top.

## The problem in one sentence

Counting *distinct* elements exactly means remembering every element
you've seen — **8+ GB of hash set for a billion u64s** — because
recognizing a duplicate requires the full history; HLL answers within
0.81% using **12 KB**, and its per-shard sketches merge exactly.

## The concepts, step by step

### Step 1 — why exact counting is expensive: duplicates need memory

Cardinality (the number of *distinct* elements in a stream) can't be
computed with a counter, because a counter can't tell a new element from
a repeat — the only exact answer is a set, and a set's memory grows with
the cardinality itself. 1B distinct u64s ≈ 8 GB of keys before hash-table
overhead; sorting or partitioning helps constants, not the asymptote. So
the question becomes: what *small* observable of a stream changes with
the number of distinct elements but not with repeats?

### Step 2 — the observation: rare hash patterns imply many elements

Hash every element to uniform random bits; the probability that a given
hash starts with j zero bits is 2^−(j+1), so if the *maximum* run of
leading zeros you ever saw is j, you've plausibly seen ~2^(j+1) distinct
elements. Call `rank` = (leading-zero count + 1). Two properties make
this the right observable: it's tiny (a max fits in 6 bits, since ranks
top out near 64), and it's **duplicate-blind** — hashing the same element
twice produces the same rank, and `max()` of a repeat changes nothing.
The flaw: a max is extremely noisy — one lucky hash and your estimate is
off by 2–4×.

### Step 3 — registers: average away the noise

Split the stream into m = 2^P substreams by the hash's low P bits, keep
one 6-bit max ("register") per substream, and combine m noisy estimates
into one — averaging cuts the relative error to ~1.04/√m, which at P=14
(m = 16,384 registers) is **0.81%** for 12 KB of state (16,384 × 6 bits).
One hashed key contributes only to one register:

```
  hash(x) = |...... 50 bits pattern ......|.. 14 bits ..|
                     ↓                          ↓
             rank = lzcnt+1 (1..51)       register index j
             regs[j] = max(regs[j], rank)      m = 16384
```

The whole write path is five lines, and the merge is one:

```rust
const P: u32 = 14;
const M: usize = 1 << P;                        // 16384 registers, 1 byte each here

fn add(regs: &mut [u8; M], x: &[u8]) {
    let h = hash64(x);
    let j = (h & (M as u64 - 1)) as usize;      // low P bits: which register
    let pat = h >> P;                            // remaining 50 bits: the pattern
    let rank = (pat.trailing_zeros() + 1).min(64 - P + 1) as u8;
    regs[j] = regs[j].max(rank);                 // max is idempotent: dups free
}

fn merge(a: &mut [u8; M], b: &[u8; M]) {
    for j in 0..M { a[j] = a[j].max(b[j]); }     // == the HLL of the union, exactly
}
```

Note the index bits and pattern bits are disjoint — question 1 below asks
why that's load-bearing. Cost: adds are O(1) and touch one register;
you've committed 12 KB per counted thing even when it holds 3 elements
(Step 5 fixes that).

### Step 4 — the estimator: harmonic means and Ertl's formula

Turning 16,384 maxima into one number is the delicate part: the naive
arithmetic mean of 2^rank is wrecked by outliers, so HLL uses a
**harmonic mean** (the reciprocal of the average of reciprocals — it
damps large outliers instead of amplifying them), plus corrections at
both extremes. Historically this was patched piecewise: Google's
"HLL in Practice" added an empirical bias table and a switch to linear
counting for small n; Ertl then *re-derived* the estimator so one formula
— two analytic series, `sigma` for the many-empty-registers low end and
`tau` for the saturation high end — is unbiased across the whole range.
Redis shipped Google's version for years, then switched (see the comment
above `hllCount`). The estimator, transcribed (this is `hllCount` minus
the caching):

```rust
fn count(regs: &[u8; M]) -> f64 {
    let mut histo = [0u32; 64];
    for &r in regs { histo[r as usize] += 1; }   // count() reads the HISTOGRAM
    let m = M as f64;
    let q = 64 - P;                              // max rank = q + 1
    let mut z = m * tau((m - histo[q as usize + 1] as f64) / m);
    for k in (1..=q).rev() { z = 0.5 * (z + histo[k as usize] as f64); }
    z += m * sigma(histo[0] as f64 / m);         // zero registers → low-range fix
    ALPHA_INF * m * m / z                        // alpha_inf = 1/(2 ln 2)
}
```

Notice `count()` consumes the *histogram* of register values
(`reghisto[rank]`), never the registers directly — 64 counters summarize
16,384 registers, which is also why redis can cache the count.

### Step 5 — the sparse encoding: why PFCOUNT keys start at 30 bytes

Dense = 12 KB always, even for 3 elements — so redis adds a second,
run-length-encoded representation for the mostly-zero early life of a
sketch (the opcode table at hyperloglog.c:380-383):

```
  ZERO:  00xxxxxx            → 1..64 zero registers in ONE byte
  XZERO: 01xxxxxx yyyyyyyy   → 1..16384 zero registers in two bytes
  VAL:   1vvvvvxx            → a value 1..32, repeated 1..4 times
```

An empty HLL = `XZERO(16384)` = 2 bytes + header; an HLL tracking 100
elements costs ~30 bytes, not 12 KB. The price is write complexity:
`hllSparseSet` (:675) is a 150-line opcode splice — an *insert into a
compressed stream* — and the encoding promotes to dense
(`hllSparseToDense` :593) when it exceeds `hll-sparse-max-bytes` (3 KB
default) or any rank > 32 arrives (VAL has only 5 value bits).

### Step 6 — merge = max: the killer feature is algebraic

Because a register is a max and max is associative, commutative, and
idempotent, `merge(A,B).regs == union(A∪B).regs` *exactly* (our test
demands register equality, not approximate counts) — HLLs form a
**semilattice** (a merge operation with exactly those three properties),
so sketches commute with any partitioning. Per-shard, per-hour, per-node
sketches merge losslessly in any order, with repeats and overlaps free.
This is why topic 9's `count(DISTINCT)` can be pushed below a shuffle,
and why M26's approximate distinct-count needs no coordination. The cost
asymmetry to remember: PFADD touches 1 register; PFMERGE touches all
16,384 (redis vectorizes it — AVX2 at :1116, NEON at :1218).

## Where each step lives in the code

`hyperloglog.c` — the 200-line header comment is a full spec of the
encodings; read it before the functions.

| anchor | step | what it does |
|---|---|---|
| :196-198 (header comment area) | 3 | P=14, 6-bit registers, the dense layout |
| `hllPatLen` :467 | 2–3 | hash, split index/pattern, count zero run — mirrors our `add` recipe exactly (note: redis sets bit 63 as a sentinel so the loop terminates; we cap rank at 64−P+1 instead) |
| `hllDenseSet` :502 | 3 | the 6-bit pack/unpack shift dance (:354 comment walks it) — we spend a byte per register to skip this |
| `hllDenseRegHisto` :528 | 4 | builds `reghisto[rank]` — count() consumes the *histogram*, not the registers |
| `hllSigma` :1016, `hllTau` :1033 | 4 | Ertl's two series (linear-counting-like correction at the low end, saturation correction at the high end) |
| `hllCount` :1058 | 4 | the estimator: `m·tau(...)`, fold histogram with repeated halving, `+ m·sigma(reghisto[0]/m)`, then `alpha_inf·m²/z` |
| :380-383 opcode table, `hllSparseSet` :675, `hllSparseToDense` :593 | 5 | the sparse encoding and its promotion |
| `hllMergeDense` :1279 (AVX2 :1116, NEON :1218) | 6 | merge = per-register max, vectorized |

## Tie back to the stub

`hll::Hll` = dense redis at byte granularity: `add` is hllPatLen +
register max, `count` is hllCount's tau/sigma transcribed, `merge` is
hllMergeDense scalar. The `< 3%` error test at n ∈ {1K, 100K, 5M} spans
the ranges the old estimator needed three different formulas for.

## Questions to answer in notes.md

1. Why must the index bits and the pattern bits not overlap? (What
   correlation would `rank` and `j` share, and what does it do to the m
   independent-substreams assumption?)
2. `reghisto[0]` counts *never-touched* registers. sigma() blows up to
   +inf as that fraction → 1. Show that for n ≪ m the estimator
   degenerates to linear counting `m·ln(m/V)` where V = zero registers —
   i.e., the low-range "switch" is now built into the formula.
3. Why can sparse only represent ranks ≤ 32, and why is that almost never
   the trigger for promotion in practice? (What cardinality does a rank
   of 33 imply for that substream?)
4. **(cross-topic)** ZERO/XZERO/VAL vs roaring's array/bitmap/run
   containers ([reading-roaring-internals.md](reading-roaring-internals.md)):
   both are "adaptive encodings that promote when density crosses a
   threshold." Name the density metric each one switches on.
5. PFADD on a dense HLL touches 1 register; PFMERGE touches all 16384.
   Redis stores HLLs as strings and PFADD is O(1) amortized. Sketch how
   you'd maintain a per-label HLL inside a graph engine's write path
   (topic 26 M-log) without making every node-insert O(m).

## References

**Papers**
- Heule, Nunkesser, Hall — "HyperLogLog in Practice" (Google, EDBT 2013)
  — §3-5 are the practical fixes; the original Flajolet '07 analysis is
  optional
- Ertl — "New cardinality estimation algorithms for HyperLogLog
  sketches" ([arXiv:1702.01284](https://arxiv.org/abs/1702.01284), 2017)
  — §2-3; the estimator redis uses now

**Code**
- [redis](https://github.com/redis/redis) `src/hyperloglog.c` — the
  200-line header comment is a full spec of the encodings; read it
  before the functions
