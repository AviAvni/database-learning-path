# Reading guide — HyperLogLog (paper + redis hyperloglog.c)

**Sources:**
- Heule, Nunkesser, Hall — "HyperLogLog in Practice" (Google, EDBT 2013) —
  read §3-5 (the practical fixes); the original Flajolet '07 analysis is
  optional
- Ertl — "New cardinality estimation algorithms for HyperLogLog sketches"
  (arXiv 2017) §2-3 — this is the estimator redis actually uses now
- Redis `src/hyperloglog.c` (clone at [`~/repos/redis`](https://github.com/redis/redis)) — the 200-line header
  comment is a full spec of the encodings; read it first

## 1. The idea in three sentences

Hash every element; the probability that a hash starts with j zero bits is
2^−(j+1), so the *maximum* leading-zero run seen is a (very noisy) log2 of
the cardinality. Split the stream into m=2^P substreams by the low P bits
and keep one 6-bit max ("register") per substream; averaging m noisy
estimates cuts the error to ~1.04/√m — 0.81% at P=14. Duplicates are free:
max() is idempotent, which is also why union = register-wise max, exactly.

```
  hash(x) = |...... 50 bits pattern ......|.. 14 bits ..|
                     ↓                          ↓
             rank = lzcnt+1 (1..51)       register index j
             regs[j] = max(regs[j], rank)      m = 16384
```

**Q1.** Why must the index bits and the pattern bits not overlap? (What
correlation would `rank` and `j` share, and what does it do to the m
independent-substreams assumption?)

## 2. hyperloglog.c anchors — the dense path (what our stub implements)

| anchor | what it does |
|---|---|
| :196-198 (header comment area) | P=14, 6-bit registers, the dense layout |
| `hllPatLen` :467 | hash, split index/pattern, count zero run — mirrors our `add` recipe exactly (note: redis sets bit 63 as a sentinel so the loop terminates; we cap rank at 64−P+1 instead) |
| `hllDenseSet` :502 | the 6-bit pack/unpack shift dance (:354 comment walks it) — we spend a byte per register to skip this |
| `hllDenseRegHisto` :528 | builds `reghisto[rank]` — count() consumes the *histogram*, not the registers |
| `hllSigma` :1016, `hllTau` :1033 | Ertl's two series (linear-counting-like correction at the low end, saturation correction at the high end) |
| `hllCount` :1058 | the estimator: `m·tau(...)`, fold histogram with repeated halving, `+ m·sigma(reghisto[0]/m)`, then `alpha_inf·m²/z` |
| `hllMergeDense` :1279 (AVX2 :1116, NEON :1218) | merge = per-register max, vectorized |

Ertl's estimator replaced the old empirical-bias-table + linear-counting
switchover from HLL-in-Practice §5. That's worth pausing on: Google's fix
was *piecewise empirical patching*; Ertl re-derived the estimator so one
formula is unbiased across the whole range. Redis shipped Google's version
for years, then switched (see the comment above hllCount).

**Q2.** `reghisto[0]` counts *never-touched* registers. sigma() blows up to
+inf as that fraction → 1. Show that for n ≪ m the estimator degenerates to
linear counting `m·ln(m/V)` where V = zero registers — i.e., the low-range
"switch" is now built into the formula.

## 3. The sparse encoding — why PFCOUNT keys start at 30 bytes

Dense = 12 KB always, even for 3 elements. The sparse encoding
(:380-383 opcode table) run-length-encodes the mostly-zero register array:

```
  ZERO:  00xxxxxx            → 1..64 zero registers in ONE byte
  XZERO: 01xxxxxx yyyyyyyy   → 1..16384 zero registers in two bytes
  VAL:   1vvvvvxx            → a value 1..32, repeated 1..4 times
```

An empty HLL = `XZERO(16384)` = 2 bytes + header. `hllSparseSet` (:675) is
a 150-line opcode splice — an *insert into a compressed stream* — and
promotes to dense (`hllSparseToDense` :593) when the encoding exceeds
`hll-sparse-max-bytes` (3 KB default) or any rank > 32 arrives (VAL only
has 5 value bits).

**Q3.** Why can sparse only represent ranks ≤ 32, and why is that almost
never the trigger for promotion in practice? (What cardinality does a rank
of 33 imply for that substream?)

**Q4 (cross-topic).** ZERO/XZERO/VAL vs roaring's array/bitmap/run
containers ([reading-roaring-internals.md](reading-roaring-internals.md)):
both are "adaptive encodings that promote when density crosses a
threshold." Name the density metric each one switches on.

## 4. Sharding — the killer feature

`merge(A,B).regs == union(A∪B).regs` *exactly* (our test demands register
equality, not approximate counts). So HLLs commute with any partitioning:
per-shard, per-hour, per-node sketches merge losslessly in any order — a
semilattice (max is associative, commutative, idempotent). This is why
topic 9's `count(DISTINCT)` can be pushed below a shuffle, and why M26's
approximate distinct-count needs no coordination.

**Q5.** PFADD on a dense HLL touches 1 register; PFMERGE touches all 16384.
Redis stores HLLs as strings and PFADD is O(1) amortized. Sketch how you'd
maintain a per-label HLL inside a graph engine's write path (topic 26 M-log)
without making every node-insert O(m).

## 5. Tie back to the stub

`hll::Hll` = dense redis at byte granularity: `add` is hllPatLen +
register max, `count` is hllCount's tau/sigma transcribed, `merge` is
hllMergeDense scalar. The `< 3%` error test at n ∈ {1K, 100K, 5M} spans
the ranges the old estimator needed three different formulas for.
