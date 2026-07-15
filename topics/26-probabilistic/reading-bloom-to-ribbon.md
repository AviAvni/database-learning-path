# Bloom → blocked → ribbon: fifty years of filter fixes

A filter answers "definitely absent / maybe present" in ~10 bits per
key, which is why every LSM read path starts with one. Bloom's 1970
design has exactly two sins — space and cache misses — and this chapter
follows the fixes for each into the two filters RocksDB actually ships.
Before touching `bloom_impl.h`, it builds the ideas one at a time: what a
one-sided answer buys, the bloom math you must own, the two sins, and the
two very different fixes — then hands you the file anchors to watch each
one in production code.

## The problem in one sentence

Answering "is key X in this set?" exactly for 10M u64 keys costs a
HashSet — **224 MB** at 24 ns/lookup on the motivation bench — while a
structure allowed to be wrong 1% of the time, in one direction only, does
it in **12 MB** at the same speed; the fifty-year question is how close to
the information-theoretic minimum that 12 MB can get without paying extra
cache misses.

## The concepts, step by step

### Step 1 — the filter contract: one-sided error

A filter is a compact set-membership structure that may answer "maybe
present" for a key that is absent, but must never answer "absent" for a
key that is present. The rate of the first mistake is the **false positive
rate** (FPR — how often the filter says "maybe" for a key that is
definitely absent); the second mistake — a **false negative** — is
forbidden by contract. This one-sidedness is exactly what a lookup path
needs: "definitely absent" lets you *skip* the expensive probe (an SST
read, a disk seek) with certainty, and a false positive costs only one
wasted probe. The going rate: **~10 bits per key buys ~1% FPR** — 5% of
HashSet's memory for the same answer 99% of the time, and the other 1%
merely slower, never wrong.

### Step 2 — the bloom filter: k shared bits per key

Bloom's 1970 design is an m-bit array plus k hash functions: to insert a
key, set the k bits its hashes pick; to query, check them — all k set means
"maybe", any zero means "definitely absent" (a present key's bits were all
set at insert time and bits are never cleared, so no false negatives).
The bits are *shared* between keys, which is where false positives come
from — and the math is worth deriving once, not memorizing.

Derive (don't memorize) `FPR ≈ (1 − e^(−kn/m))^k`:
- One insert with one probe leaves a given bit 0 with prob (1 − 1/m).
- After kn probes: (1 − 1/m)^kn ≈ e^(−kn/m) — fraction of bits still 0.
- A miss query needs all k of its probe bits set: (1 − e^(−kn/m))^k.
- Minimize over k: optimal k = (m/n)·ln2 ≈ 0.69·bits_per_key. At 10 bpk → k≈7.

Rules of thumb that fall out: 10 bits/key ≈ 1% FPR, 16 ≈ 0.04%, and each
added bit/key cuts FPR roughly in half. The cost baked into the design:
shared bits mean you can never delete (clearing a bit may lie about other
keys), and every query touches k scattered bits.

### Step 3 — the two sins: 1.44× space and k cache misses

Measured against the theoretical floor, bloom wastes space: storing a set
with FPR f needs at least log2(1/f) bits per key (the information-theoretic
lower bound), and bloom needs 1.44·log2(1/f) — **44% overhead**, forever,
by construction. And it wastes time: the k probe bits land in k random
words of a large array, so a query costs up to **k cache misses** (~7 at
10 bpk) — on a machine where one miss is ~80–100 ns, the filter meant to
*save* a probe costs seven. Fifty years of fixes attack exactly those two
sins:

```
                sin #1: k cache misses          sin #2: 1.44x space
  bloom '70  ───────────┬─────────────────────────────┬──────────
                        ▼                             ▼
  blocked bloom: all k probes in one line     ribbon: linear algebra over
  (pay ~1.5-2x FPR for it)                    GF(2), ~1.10x space, static
```

Each fix pays a different currency — FPR for the cache fix, updatability
for the space fix. Steps 4–6 walk them in turn.

### Step 4 — blocked bloom: all k probes in one cache line

A blocked bloom filter first hashes the key to one cache-line-sized
**block** (512 bits in RocksDB's `FastLocalBloomImpl`), then runs a
miniature bloom filter entirely inside that block — so a query costs
exactly **one** memory access instead of k. The entire query path,
de-SIMD'd (this is `HashMayMatchPrepared`):

```rust
const PROBES: u32 = 6;

fn may_contain(bits: &[u64], num_blocks: u32, h1: u32, mut h2: u32) -> bool {
    let block = fastrange32(h1, num_blocks) as usize * 8;  // 8 words = 512 bits
    for _ in 0..PROBES {
        let bit = (h2 >> 23) & 511;               // top 9 bits pick 1 of 512
        if bits[block + (bit / 64) as usize] & (1u64 << (bit % 64)) == 0 {
            return false;                          // early exit, ONE line touched
        }
        h2 = h2.wrapping_mul(0x9e3779b9);          // golden-ratio remix per probe
    }
    true                                           // maybe
}
```

The price is **Poisson crowding**: keys per block follow a Poisson
distribution (the statistics of throwing n balls into n/512-bit bins), so
some blocks get twice the average load — and a block that got 2× the keys
has much worse FPR than the formula in Step 2 predicts. RocksDB is honest
about it: `CacheLocalFpRate` (bloom_impl.h:42) computes the real blocked
FPR as the *expectation over the Poisson distribution of keys-per-block* —
the whole blocked-bloom trade in 10 lines, and worse than the naive
`StandardFpRate` at the same bits/key. Measured: **~1.5–2× the standard
FPR** at the same bpk, in exchange for k× fewer misses. That ratio is
exactly what our stub's `fpr < 4× theory` test bounds.

### Step 5 — filters as linear algebra: solve for bits, don't set them

The conceptual jump behind the space fix: a bloom filter *sets* bits; a
xor/ribbon filter *solves for* bits. Give every key an r-bit
**fingerprint** (a short hash of the key), and find an array S of r-bit
slots such that each key's equation holds over GF(2) (arithmetic on bits
where addition is XOR):

```
  row(key) · S = fingerprint(key)     ← S is the filter, r fingerprint bits
```

`row(key)` is a hash-derived coefficient vector saying which slots of S to
XOR together. Query = recompute `row·S`, compare against the key's
fingerprint. For inserted keys the equation holds by construction (no false
negatives); a false positive is a non-key whose equation *happens* to hold —
probability exactly 2^−r. So space ≈ r·(1+overhead) bits/key, where
overhead is the fraction of unusable slots the solver needs — **~10% for
ribbon vs bloom's 44%**. The catch: you must solve a linear system over
all keys at once, which is why this family is *static* — build once, never
insert again.

### Step 6 — the ribbon band: locality makes the solve O(n), and builds can fail

The "ribbon" trick makes the linear solve cheap enough for production:
`StandardHasher` (ribbon_impl.h:165) gives each key a coefficient vector
that is nonzero only in a `kCoeffBits`-wide (:114, = 64 or 128) *band*
starting at a hashed position. A system where every row's nonzeros sit in
a narrow diagonal band admits **banded Gaussian elimination** — O(n) with
tiny constants — and `StandardBanding` (:471, `num_starts_ = num_slots -
kCoeffBits + 1` at :504) does it *incrementally*, back-substituting as
keys stream in (`BandingAddRange` :577). Streaming build is ribbon's edge
over xor filters, which need all keys up front.

Two costs to hold onto. First, construction can *fail* (the random system
comes out singular), and RocksDB retries with a different hash seed
(`StandardRehasherAdapter` :416) — unlike blocked bloom, whose monotone
"set bits" build can never fail. Second, both build and query burn more
CPU than bloom's bit probes. RocksDB's deployment follows directly:
**ribbon for the cold bottom LSM levels** (most keys live there — space
dominates) and **blocked bloom for the hot top levels** (queried
constantly — speed dominates), via `RibbonFilterPolicy`'s
`bloom_before_level`.

## Where each step lives in the code

Peter Dillinger's blog-style comments *inside* the headers are the best
docs — read code and comments together.

`util/bloom_impl.h` — Steps 2–4, RocksDB's two generations:

| anchor | what it is |
|---|---|
| `LegacyBloomImpl` (:364-476) | old format: one cache line per key (`AddHash` :432 picks `num_lines`), but probes derived by weak shift-rotate — measurable FPR bias |
| `FastLocalBloomImpl` (:144) | current "format_version=5" bloom: 512-bit (64-byte) blocks, probes from `h *= 0x9e3779b9` golden-ratio remix (Step 4) |
| `AddHashPrepared` (:206) | the probe loop: each probe uses bits (h >> 27) & 511 of a *re-multiplied* h — 9 bits per probe, all inside one line |
| `HashMayMatchPrepared` (:231) | query = same loop, early-exit on first zero bit — Step 4's code sample |
| `CacheLocalFpRate` (:42) | the honesty function: blocked-bloom FPR as the *expectation over the Poisson distribution of keys-per-block* (Step 4's tax, quantified) |

`util/ribbon_impl.h` — Steps 5–6:

| anchor | what it is |
|---|---|
| `StandardHasher` (:165) | coefficient vectors nonzero only in a `kCoeffBits`-wide band (:114) |
| `StandardBanding` (:471) | incremental banded elimination; `num_starts_` at :504 |
| `BandingAddRange` (:577) | streaming back-substitution as keys arrive |
| `StandardRehasherAdapter` (:416) | the build-failure retry with a fresh seed |

## Tie back to the stub

Our `bloom::BlockedBloom` is `FastLocalBloomImpl` minus SIMD:
`hash2` gives (h1, h2); `fastrange32(h1, blocks)` picks the block;
6 probes each take 9 bits from a rotating h2. After implementing, compare
your measured FPR-vs-theory ratio against what `CacheLocalFpRate` predicts
for your keys-per-block Poisson mean.

## Questions to answer in notes.md

1. At optimal k, exactly half the bits are set. Why is that intuitive?
   (Hint: a bit-array with maximal entropy per bit.)
2. `FastLocalBloomImpl` uses `h1` to pick the block (via fastrange, not
   modulo) and `h2` to derive all probe bits. Our stub does the same. Why
   must the block choice NOT reuse bits that pick probes?
3. Why 512-bit blocks and not 64-bit words? (Two effects fight: smaller
   blocks = fewer distinct probe positions = FPR tax explodes; the answer
   is the cache line is the natural "free" granule.)
4. Ribbon construction can *fail* (singular system) and RocksDB retries
   with a different hash seed (`StandardRehasherAdapter` :416). Cuckoo
   insertion can also fail (MAX_KICKS). Blocked bloom never fails. What
   does this monotone-vs-solve distinction cost each design at build time?
5. **(cross-check with topic 4)** RocksDB picks ribbon for the *bottom*
   LSM levels and blocked bloom for the hot top levels
   (`level_compaction_dynamic_level_bytes` + `RibbonFilterPolicy`'s
   `bloom_before_level`). Why does that split follow directly from
   "ribbon: ~30% less space but several× slower to build and query"?

## References

**Papers**
- Bloom — "Space/Time Trade-offs in Hash Coding with Allowable Errors"
  (CACM 1970) — 5 pages, read whole
- Dillinger & Walzer — "Ribbon filter: practically smaller than Bloom
  and Xor" ([arXiv:2103.02515](https://arxiv.org/abs/2103.02515), 2021)

**Code**
- [rocksdb](https://github.com/facebook/rocksdb) `util/bloom_impl.h` +
  `util/ribbon_impl.h` — Peter Dillinger's blog-style comments *inside*
  the headers are the best docs; read code and comments together
