# Reading guide — Bloom → Blocked Bloom → Ribbon (RocksDB code)

**Sources:**
- Bloom (1970), "Space/Time Trade-offs in Hash Coding with Allowable Errors" — 5 pages, read whole
- RocksDB `util/bloom_impl.h` + `util/ribbon_impl.h` (clone at `~/repos/rocksdb`)
- Peter Dillinger's blog-style comments *inside* the headers — the best docs are the code

## Why this sequence

Bloom's 1970 filter is information-theoretically ~44% wasteful (1.44·log2(1/fpr)
bits/key vs the log2(1/fpr) lower bound) and cache-hostile (k probes = k misses).
Fifty years of fixes attack exactly those two sins:

```
                sin #1: k cache misses          sin #2: 1.44x space
  bloom '70  ───────────┬─────────────────────────────┬──────────
                        ▼                             ▼
  blocked bloom: all k probes in one line     ribbon: linear algebra over
  (pay ~1.5-2x FPR for it)                    GF(2), ~1.10x space, static
```

## 1. The math you must own before reading code

Derive (don't memorize) `FPR ≈ (1 − e^(−kn/m))^k`:
- One insert with one probe leaves a given bit 0 with prob (1 − 1/m).
- After kn probes: (1 − 1/m)^kn ≈ e^(−kn/m) — fraction of bits still 0.
- A miss query needs all k of its probe bits set: (1 − e^(−kn/m))^k.
- Minimize over k: optimal k = (m/n)·ln2 ≈ 0.69·bits_per_key. At 10 bpk → k≈7.

**Q1.** At optimal k, exactly half the bits are set. Why is that intuitive?
(Hint: a bit-array with maximal entropy per bit.)

## 2. `bloom_impl.h` — RocksDB's two generations

| anchor | what it is |
|---|---|
| `LegacyBloomImpl` (:364-476) | old format: one cache line per key (`AddHash` :432 picks `num_lines`), but probes derived by weak shift-rotate — measurable FPR bias |
| `FastLocalBloomImpl` (:144) | current "format_version=5" bloom: 512-bit (64-byte) blocks, probes from `h *= 0x9e3779b9` golden-ratio remix |
| `AddHashPrepared` (:206) | the probe loop: each probe uses bits (h >> 27) & 511 of a *re-multiplied* h — 9 bits per probe, all inside one line |
| `HashMayMatchPrepared` (:231) | query = same loop, early-exit on first zero bit |
| `CacheLocalFpRate` (:42) | the honesty function: computes blocked-bloom FPR as the *expectation over the Poisson distribution of keys-per-block* |

Read `CacheLocalFpRate` carefully — it's the whole blocked-bloom trade in
10 lines. A block that got 2× the average keys has much worse FPR, and the
weighted sum is worse than the naive `StandardFpRate` at the same bpk.
That's the number our stub's `fpr < 4× theory` test bounds.

**Q2.** `FastLocalBloomImpl` uses `h1` to pick the block (via
fastrange, not modulo) and `h2` to derive all probe bits. Our stub does the
same. Why must the block choice NOT reuse bits that pick probes?

**Q3.** Why 512-bit blocks and not 64-bit words? (Two effects fight:
smaller blocks = fewer distinct probe positions = FPR tax explodes; the
answer is the cache line is the natural "free" granule.)

## 3. `ribbon_impl.h` — filters as linear algebra

The conceptual jump: a bloom filter *sets* bits; a ribbon filter *solves for*
bits. Each key contributes one equation over GF(2):

```
  row(key) · S = fingerprint(key)     ← S is the filter, r fingerprint bits
```

Query recomputes row·S and compares. False positive = a non-key whose
equation happens to hold: 2^−r exactly, so space ≈ r·(1+overhead) bits/key —
overhead is the fraction of unusable slots, ~10% for standard ribbon vs 44%
for bloom.

The "ribbon" trick makes solving cheap: `StandardHasher` (:165) gives each
key a coefficient vector that is nonzero only in a `kCoeffBits`-wide (:114,
= 64 or 128) *band* starting at a hashed position. Banded Gaussian
elimination is then O(n) with tiny constants — `StandardBanding` (:471,
`num_starts_ = num_slots - kCoeffBits + 1` at :504) does incremental
back-substitution as keys stream in (`BandingAddRange` :577).

**Q4.** Ribbon construction can *fail* (singular system) and RocksDB
retries with a different hash seed (`StandardRehasherAdapter` :416). Cuckoo
insertion can also fail (MAX_KICKS). Blocked bloom never fails. What does
this monotone-vs-solve distinction cost each design at build time?

**Q5 (cross-check with topic 4).** RocksDB picks ribbon for the *bottom*
LSM levels and blocked bloom for the hot top levels
(`level_compaction_dynamic_level_bytes` + `RibbonFilterPolicy`'s
`bloom_before_level`). Why does that split follow directly from
"ribbon: ~30% less space but several× slower to build and query"?

## 4. Tie back to the stub

Our `bloom::BlockedBloom` is `FastLocalBloomImpl` minus SIMD:
`hash2` gives (h1, h2); `fastrange32(h1, blocks)` picks the block;
6 probes each take 9 bits from a rotating h2. After implementing, compare
your measured FPR-vs-theory ratio against what `CacheLocalFpRate` predicts
for your keys-per-block Poisson mean.
