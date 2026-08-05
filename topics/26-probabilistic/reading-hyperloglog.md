# HyperLogLog: count distinct in 12 KB

`count(DISTINCT x)` over billions of elements, 0.81% error, 12 KB of
state, and per-shard sketches that merge losslessly in any order — one
probabilistic observation buys all of it. This chapter builds the
estimator step by step from that observation, then walks redis's
production implementation, which adds a sparse encoding and a better
count formula on top.

Every code anchor below is redis at commit `a176d1225`, the revision this
repo pins (`src/hyperloglog.c`), quoted with the line numbers the code
occupies in that version. The α constants and error figures are quoted
from Flajolet et al. 2007 and worked out on the spot; where redis diverges
from the original paper (a 64-bit hash, Ertl's estimator) the guide says
which paper each piece comes from.

## The problem in one sentence

Counting *distinct* elements exactly means remembering every element
you've seen — **8+ GB of hash set for a billion u64s** — because
recognizing a duplicate requires the full history; HLL answers within
0.81% using **12 KB**, and its per-shard sketches merge exactly.

## The concepts, step by step

### Step 1 — why exact counting is expensive: duplicates need memory

> **In:** nothing yet — this step frames why a counter and a hash set are
> the two bad extremes.
> **Out:** the requirement for a *small, duplicate-blind* observable of
> the stream.

Cardinality (the number of *distinct* elements in a stream) can't be
computed with a counter, because a counter can't tell a new element from
a repeat — the only exact answer is a set, and a set's memory grows with
the cardinality itself. 1B distinct u64s ≈ 8 GB of keys before hash-table
overhead; sorting or partitioning helps constants, not the asymptote. So
the question becomes: what *small* observable of a stream changes with
the number of distinct elements but not with repeats?

### Step 2 — the observation: rare hash patterns imply many elements

> **In:** the "small, duplicate-blind observable" requirement from Step 1.
> **Out:** `rank` = leading-zero count + 1, whose running maximum tracks
> log₂(distinct count) — and the reason one lone max is too noisy to use.

Hash every element to uniform random bits; the probability that a given
hash starts with j zero bits is 2^−(j+1), so if the *maximum* run of
leading zeros you ever saw is j, you've plausibly seen ~2^(j+1) distinct
elements. Call `rank` = (leading-zero count + 1). Worked: P(≥3 leading
zeros) = 2^−3 = 1/8, so seeing a rank of 4 (three zeros then a one)
suggests on the order of 2^4 = 16 distinct draws. Two properties make
this the right observable: it's tiny (a max fits in 6 bits, since ranks
top out near 64), and it's **duplicate-blind** — hashing the same element
twice produces the same rank, and `max()` of a repeat changes nothing.
The flaw: a max is extremely noisy — one lucky hash and your estimate is
off by 2–4×.

### Step 3 — registers: average away the noise

> **In:** the single, noisy `rank` maximum from Step 2.
> **Out:** m = 2^P registers and the ~1.04/√m error law — 0.81% at P=14.

Split the stream into m = 2^P substreams by the hash's low P bits, keep
one 6-bit max ("register") per substream, and combine m noisy estimates
into one — averaging cuts the relative error to ~1.04/√m, which at P=14
(m = 16,384 registers) is **0.81%** for 12 KB of state (16,384 × 6 bits).
Worked: √16,384 = 128, so 1.04/128 = 0.008125 = 0.81%; and 16,384 × 6 =
98,304 bits = 12,288 bytes = 12 KB. The 1.04/√m law is Flajolet et al.
2007 (§, "typical relative error ±1.04/√m"). One hashed key contributes
only to one register:

```
  hash(x) = |...... 50 bits pattern ......|.. 14 bits ..|
                     ↓                          ↓
             rank = lzcnt+1 (1..51)       register index j
             regs[j] = max(regs[j], rank)      m = 16384
```

The whole write path is five lines, and the merge is one:

```rust
// ILLUSTRATION — not quoted from redis; the real write path is hllPatLen
// (hyperloglog.c:467) computing rank, then hllDenseSet (:502) packing it.
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

> **In:** the m register maxima from Step 3.
> **Out:** one cardinality number — via Flajolet's `α_m·m²/Σ` estimator
> (with its two range fixes), or redis's Ertl `σ`/`τ` re-derivation.

Turning 16,384 maxima into one number is the delicate part: the naive
arithmetic mean of 2^rank is wrecked by outliers, so HLL uses a
**harmonic mean** (the reciprocal of the average of reciprocals — it
damps large outliers instead of amplifying them), plus corrections at
both extremes.

Three names to keep straight (rule 3 — every constant here is quoted from
its paper, not remembered):

- **Original HLL (Flajolet et al. 2007).** Estimate `E = α_m · m² / Σⱼ
  2^−M[j]`, where `M[j]` is register *j*'s stored max rank and `α_m`
  corrects the harmonic mean's bias. Flajolet §3 *defines* `α₁₆ = 0.673,
  α₃₂ = 0.697, α₆₄ = 0.709`, and `α_m = 0.7213 / (1 + 1.079/m)` for `m ≥
  128`. Worked at m = 16,384: `0.7213 / (1 + 1.079/16384) = 0.72125`.
  Beware — the small-*m* entries are tabulated, not from that formula:
  `0.7213/(1 + 1.079/16) = 0.676`, which is *not* the listed `0.673`, so
  16/32/64 get their own constants. Two range fixes bolt on: **small
  range** — when `E ≤ 5m/2` (= 40,960 at m = 16,384) and `V` registers are
  still zero, switch to linear counting `m·ln(m/V)`; **large range** —
  when `E > 2³²/30 ≈ 1.43×10⁸`, undo 32-bit hash collisions with
  `−2³²·ln(1 − E/2³²)` (Flajolet §4, small/large-range corrections).
- **HLL++ (Heule et al. 2013, "HyperLogLog in Practice").** Replaces the
  32-bit hash with a **64-bit** one (§5.1), which retires the large-range
  correction outright; swaps Flajolet's small-range switch for an
  **empirically-tabulated bias correction** (§5.2); and adds the sparse
  representation of Step 5. These are HLL++'s fixes, *not* Flajolet's and
  *not* Ertl's.
- **Ertl 2017 — what redis ships now.** Re-derives one estimator with no
  piecewise switch: `α_∞ · m² / z`, where `α_∞ = 1/(2 ln 2) = 0.7213475`
  (redis `HLL_ALPHA_INF` = `0.721347520444481703680` at
  `hyperloglog.c:404`, commented "constant for 0.5/ln(2)") and `z` folds
  in two analytic series — `σ` (`hllSigma` :1016) for the
  many-empty-registers low end and `τ` (`hllTau` :1033) for the saturated
  high end.

Redis shipped Google's HLL++ estimator for years, then switched to Ertl's;
the comment above `hllCount` records the change. The Ertl estimator,
transcribed (this is `hllCount` minus the caching):

```rust
// ILLUSTRATION — not quoted from redis; the real estimator is hllCount
// (hyperloglog.c:1058), with sigma/tau at :1016/:1033 and the reghisto
// fold at :1084-:1090.
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

> **In:** the fixed 12 KB dense array from Step 3.
> **Out:** a run-length sparse encoding that starts at ~30 bytes and
> promotes to dense on demand.

Dense = 12 KB always, even for 3 elements — so redis adds a second,
run-length-encoded representation for the mostly-zero early life of a
sketch (the opcode macros at hyperloglog.c:380-392):

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
default) or any rank > 32 arrives (`HLL_SPARSE_VAL_MAX_VALUE = 32` at
:389 — VAL has only 5 value bits).

### Step 6 — merge = max: the killer feature is algebraic

> **In:** the register array from Step 3 (dense, or promoted from sparse).
> **Out:** the merge = per-register max identity, and what that buys a
> distributed distinct-count.

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
| :380-392 opcode macros, `hllSparseSet` :675, `hllSparseToDense` :593 | 5 | the sparse encoding and its promotion (`HLL_SPARSE_VAL_MAX_VALUE 32` at :389) |
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

## Done when

Answer each before unfolding it.

- [ ] You can explain why rare hash patterns imply many distinct elements.

  <details><summary>Answer</summary>

  For a uniform hash, P(a value has ≥ j leading zeros) = 2^−j, so the
  *maximum* rank (leading-zero-run + 1) observed over a stream grows like
  log₂(distinct count). A repeat hashes to the same value and can't push a
  max higher, so the observable tracks cardinality, not traffic. Redis
  computes the rank in `hllPatLen` (hyperloglog.c:467).

  </details>

- [ ] You can say why index bits and pattern bits must not overlap.

  <details><summary>Answer</summary>

  The low P bits pick the register `j`; the remaining bits form the
  pattern whose leading-zero rank is stored. If the two sets overlapped,
  `j` and `rank` would be correlated, violating the assumption that the m
  substreams are independent — and the 1.04/√m error law only holds for m
  *independent* estimators. (Question 1.)

  </details>

- [ ] You can explain what the registers average away and why the harmonic mean.

  <details><summary>Answer</summary>

  A single max is off by 2–4×; splitting into m = 2^P registers and
  combining drops the relative error to 1.04/√m (0.81% at P = 14, since
  1.04/128 = 0.008125). The arithmetic mean of 2^rank is dominated by one
  lucky outlier, so HLL uses a harmonic mean scaled by a bias constant:
  Flajolet's `α₁₆ = 0.673 … α_m = 0.7213/(1+1.079/m)`, or in redis Ertl's
  `α_∞ = 1/(2 ln 2) = 0.72135` (`HLL_ALPHA_INF` at :404) with `σ`/`τ`
  (`hllSigma` :1016, `hllTau` :1033).

  </details>

- [ ] You can explain the sparse encoding and why a PFCOUNT key starts at 30 bytes.

  <details><summary>Answer</summary>

  Dense is 12 KB regardless of load. Sparse run-length-encodes the
  mostly-zero early sketch: ZERO/XZERO pack up to 16,384 zero registers in
  1–2 bytes, VAL packs a rank 1..32 repeated 1..4 times (opcode macros
  hyperloglog.c:380-392). An empty HLL is `XZERO(16384)` ≈ 2 bytes +
  header; ~100 elements ≈ 30 bytes. It promotes to dense
  (`hllSparseToDense` :593) past `hll-sparse-max-bytes` (3 KB) or when a
  rank > 32 arrives (`HLL_SPARSE_VAL_MAX_VALUE = 32` at :389).

  </details>

- [ ] You can state the killer feature — merge is max, therefore algebraic — and say what that buys a distributed count.

  <details><summary>Answer</summary>

  A register is a max, and max is associative, commutative, and
  idempotent, so `merge(A,B)` equals the HLL of `A ∪ B` exactly (register
  equality, not approximate counts) — HLLs form a semilattice. Per-shard,
  per-hour, per-node sketches therefore merge losslessly in any order,
  with repeats and overlaps free, so a distinct-count needs no
  coordination. `hllMergeDense` (:1279) is a per-register max, vectorized
  (AVX2 :1116, Aarch64/NEON :1218). Cost asymmetry: PFADD touches 1
  register, PFMERGE touches all 16,384.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the ZERO/XZERO/VAL comparison against roaring's containers.

  <details><summary>Answer</summary>

  Self-check: the five questions cover index/pattern independence, the
  low-range degeneracy to linear counting `m·ln(m/V)`, the rank ≤ 32
  sparse ceiling, the ZERO/XZERO/VAL-vs-roaring-container density metric,
  and the per-label HLL write-path sketch. All five belong in notes.md
  before this box is checked.

  </details>

## References

**Papers**
- Flajolet, Fusy, Gandouet, Meunier — "HyperLogLog: the analysis of a
  near-optimal cardinality estimation algorithm" (AofA 2007) — §3 defines
  the bias constants (`α₁₆ = 0.673, α₃₂ = 0.697, α₆₄ = 0.709, α_m =
  0.7213/(1+1.079/m)` for `m ≥ 128`) and the `1.04/√m` standard error; §4
  the small/large-range corrections. This is the *original* HLL, not the
  version redis runs today
- Heule, Nunkesser, Hall — "HyperLogLog in Practice" (Google, EDBT 2013)
  — §5.1 the 64-bit hash, §5.2 the empirical bias table, and the sparse
  representation; these are HLL++'s additions on top of Flajolet
- Ertl — "New cardinality estimation algorithms for HyperLogLog
  sketches" ([arXiv:1702.01284](https://arxiv.org/abs/1702.01284), 2017)
  — §2-3; the `σ`/`τ` estimator (`α_∞ = 1/(2 ln 2)`) redis uses now

**Code**
- [redis](https://github.com/redis/redis) `src/hyperloglog.c` — the
  200-line header comment is a full spec of the encodings; read it
  before the functions
