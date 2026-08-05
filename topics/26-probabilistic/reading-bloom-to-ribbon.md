# Bloom → blocked → ribbon: fifty years of filter fixes

A filter answers "definitely absent / maybe present" in ~10 bits per
key, which is why every LSM read path starts with one. Bloom's 1970
design has exactly two sins — space and cache misses — and this chapter
follows the fixes for each into the two filters RocksDB actually ships.
Before touching `bloom_impl.h`, it builds the ideas one at a time: what a
one-sided answer buys, the bloom math you must own, the two sins, and the
two very different fixes — then hands you the file anchors to watch each
one in production code.

Every code anchor below is RocksDB at commit `7c80a5a`, the revision this
repo pins (`util/bloom_impl.h` is 489 lines; `util/ribbon_impl.h` is 1137
lines), quoted with the line numbers the code occupies in that version.
The bloom-math figures are worked out on the spot from the formulas the
code uses.

## The problem in one sentence

Answering "is key X in this set?" exactly for 10M u64 keys costs a
HashSet — **224 MB** at **28 ns/lookup** ([FINDINGS.md](../../FINDINGS.md)
row 26) — while a structure allowed to be wrong 1% of the time, in one
direction only, does it in **12 MB** (10M keys × 10 bits) at roughly the
same speed; the fifty-year question is how close to the
information-theoretic minimum that 12 MB can get without paying extra cache
misses.

## The concepts, step by step

### Step 1 — the filter contract: one-sided error

> **In:** nothing yet — this step fixes the contract every later step
> leans on.
> **Out:** the one-sided guarantee, and the going exchange rate (~10 bits
> per key ≈ 1% FPR) that Steps 2–3 derive and Steps 4–6 defend.

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

> **In:** the one-sided contract from Step 1.
> **Out:** the false-positive formula and the optimal probe count `k`,
> both of which Step 3 measures against the theoretical floor.

Bloom's 1970 design is an m-bit array plus k hash functions: to insert a
key, set the k bits its hashes pick; to query, check them — all k set means
"maybe", any zero means "definitely absent" (a present key's bits were all
set at insert time and bits are never cleared, so no false negatives).
The bits are *shared* between keys, which is where false positives come
from — and the math is worth deriving once, not memorizing.

Derive (don't memorize) the false-positive rate. Name the symbols: **m**
is the number of bits in the array, **n** the number of keys inserted,
**k** the number of hash probes per key, so **b = m/n** is the *bits per
key* budget:

- One insert with one probe leaves a given bit 0 with probability
  (1 − 1/m).
- After all kn probes: (1 − 1/m)^kn ≈ e^(−kn/m) — the fraction of bits
  still 0.
- A miss query reports "maybe" only if all k of its probe bits are set:
  **FPR ≈ (1 − e^(−k/b))^k** (using kn/m = k/b). This is exactly RocksDB's
  `BloomMath::StandardFpRate` (`util/bloom_impl.h:35`,
  `pow(1.0 - exp(-num_probes / bits_per_key), num_probes)`).
- Minimize over k: **optimal k = (m/n)·ln2 = b·ln2**.

Worked once at **b = 10 bits/key**: optimal k = 10 × 0.6931 = **6.93 ≈ 7**
probes. Plug back in: k/b = 7/10 = 0.7, e^(−0.7) = 0.4966, so
FPR = (1 − 0.4966)^7 = 0.5034^7 = **0.0082 = 0.82%**. At b = 16 and its
optimal k = 11, the same formula gives (1 − e^(−11/16))^11 = **0.046%**.
Those are the two rules of thumb — **10 bits/key ≈ 0.8% FPR, 16 ≈ 0.05%**,
each extra bit/key cutting FPR by roughly half. The cost baked into the
design: shared bits mean you can never delete (clearing a bit may lie
about other keys), and every query touches k scattered bits.

### Step 3 — the two sins: 1.44× space and k cache misses

> **In:** the working bloom filter and its FPR formula from Step 2.
> **Out:** the two named defects — a 44% space tax and up to k cache
> misses — that Step 4 attacks one of and Steps 5–6 the other.

Measured against the theoretical floor, bloom wastes space. Storing a set
so that a non-member reports "maybe" with probability f needs at least
**log₂(1/f)** bits per key — the **information-theoretic lower bound**, the
fewest bits any approximate-membership structure can use for that FPR.
Bloom instead spends **(1/ln2)·log₂(1/f) = 1.4427·log₂(1/f)** bits — **44%
overhead**, forever, by construction. Worked at the Step 2 operating point
(b = 10 bits/key, f = 0.82%): the floor is log₂(1/0.0082) = log₂(122) =
**6.93 bits/key**, and 10 / 6.93 = **1.44** — the 44% is exactly the factor
1/ln2. The Ribbon paper opens on this same number: Bloom uses "at least
44% more space than the information-theoretic" minimum
([arXiv:2103.02515](https://arxiv.org/abs/2103.02515), §1).

And bloom wastes time: the k probe bits land in k random words of a large
array, so a query costs up to **k cache misses** (7 at 10 bpk). Taking a
DRAM miss at an assumed ~80–100 ns, that is 7 × ~90 ns ≈ **630 ns** of
stall — a filter meant to *save* one SST probe costing the equivalent of
several. Fifty years of fixes attack exactly those two sins:

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

> **In:** sin #2 from Step 3 — the k scattered cache misses.
> **Out:** a query that touches exactly one cache line, and the FPR tax
> it pays for that (a currency Step 5's fix does *not* spend).

A blocked bloom filter first hashes the key to one cache-line-sized
**block** (512 bits in RocksDB's `FastLocalBloomImpl`, `util/bloom_impl.h:144`),
then runs a miniature bloom filter entirely inside that block — so a query
costs exactly **one** memory access instead of k. The real probe loop is
nine lines; this is the *insert* side, `AddHashPrepared`, and the query
side at :231 is the same loop with an early-exit `return false`:

```c
// util/bloom_impl.h — FastLocalBloomImpl::AddHashPrepared, 206-214
   206    static inline void AddHashPrepared(uint32_t h2, int num_probes,
   207                                       char* data_at_cache_line) {
   208      uint32_t h = h2;
   209      for (int i = 0; i < num_probes; ++i, h *= uint32_t{0x9e3779b9}) {
   210        // 9-bit address within 512 bit cache line
   211        int bitpos = h >> (32 - 9);
   212        data_at_cache_line[bitpos >> 3] |= (uint8_t{1} << (bitpos & 7));
   213      }
   214    }
```

Line 211 is the one to watch: `h >> (32 - 9)` keeps the **top 9 bits** of
the 32-bit hash — a value 0..511, one bit in the 512-bit line — and line
209 re-multiplies `h` by the golden-ratio constant `0x9e3779b9` between
probes so each probe reads different bits. The block itself was already
chosen by `h1` in `AddHash` (:200-204, `FastRange32(h1, len_bytes >> 6)`).
The same six-probe loop, de-SIMD'd into one function so the control flow
is visible at a glance:

```rust
// ILLUSTRATION — not quoted from RocksDB; the query path is the AVX2 loop
// in util/bloom_impl.h:231 (HashMayMatchPrepared), block choice at :225-228.
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

The price is **Poisson crowding**: keys land in blocks independently, so
the count per block follows a Poisson distribution (the statistics of
throwing n balls into n/512 bins), and a block that happens to hold twice
the average load has a much worse local FPR than Step 2's formula predicts.
RocksDB prices it in `CacheLocalFpRate` (`util/bloom_impl.h:42`), and the
model is cruder than "sum the Poisson distribution": it takes the **average
of two `StandardFpRate` values**, one at one standard deviation *above* the
mean block occupancy (the crowded case, :53-54) and one *below* (the
uncrowded case, :55-56), where the mean is `keys_per_cache_line =
cache_line_bits / bits_per_key` (:48) and the spread is `sqrt` of that
(:52).

Worked at b = 10, the stub's 6 probes, a 512-bit line: mean =
512/10 = 51.2 keys, stddev = √51.2 = 7.16, so the crowded arm evaluates
`StandardFpRate(512/58.36, 6)` = `StandardFpRate(8.77, 6)` = **1.48%** and
the uncrowded arm `StandardFpRate(512/44.04, 6)` = `StandardFpRate(11.62, 6)`
= **0.43%**; their average is **0.95%**, versus the un-blocked **0.84%** —
a **1.13× tax** for the k× fewer misses. The tax is small *because the
block is a whole cache line*; it grows fast as the block shrinks — the same
formula gives 1.32× at 256-bit blocks, 1.64× at 128-bit, and 2.27× at
64-bit. The "1.5–2×" figure that folklore attaches to blocked bloom is a
small-block number; at a 512-bit line it is closer to 1.1–1.2×. (This
ratio is unmeasured in our repo — the stub is not yet implemented; notes.md
*predicts* 1.2–1.8×, and the `fpr < 4× theory` test only bounds it below
2.5% at 10 bpk.)

### Step 5 — filters as linear algebra: solve for bits, don't set them

> **In:** sin #1 from Step 3 — the 44% space overhead. (This step and
> Step 4 fork off Step 3: Step 4 spent FPR to fix the misses; this one
> spends updatability to fix the space.)
> **Out:** the "solve for bits" reframe, the exact 2^−r false-positive
> rate, and the space law r·(1+overhead) that Step 6 makes cheap to build.

The conceptual jump behind the space fix: a bloom filter *sets* bits; a
xor/ribbon filter *solves for* bits. Give every key an r-bit
**fingerprint** (a short hash of the key), and find an array S of r-bit
slots such that each key's equation holds over **GF(2)** (arithmetic on
single bits, where addition is XOR and multiplication is AND):

```
  row(key) · S = fingerprint(key)     ← S is the filter, r fingerprint bits
```

`row(key)` is a hash-derived coefficient vector saying which slots of S to
XOR together. Query = recompute `row·S`, compare against the key's
fingerprint. For inserted keys the equation holds by construction (no false
negatives); a false positive is a non-key whose equation *happens* to hold,
with probability exactly **2^−r**.

Worked at r = 12: FP = 2^−12 = 1/4096 = **0.024%**. Space is
**r·(1+overhead)** bits/key, where *overhead* is the fraction of extra
slots the solver needs beyond one per key. Line the three up at that same
0.024% target, whose information floor is log₂(1/f) = r = 12 bits/key:
bloom spends 1.44 × 12 = **17.3 bits/key**, an xor filter (Step 5's family,
overhead 23%) spends 1.23 × 12 = **14.8**, and a ribbon filter (overhead
~10%) spends ~1.10 × 12 = **13.2** — about **24% less than bloom** for the
identical FPR. The catch: you must solve a linear system over all keys at
once, which is why this family is *static* — build once, never insert
again.

### Step 6 — the ribbon band: locality makes the solve O(n), and builds can fail

> **In:** the "solve for bits" system from Step 5.
> **Out:** the banding trick that makes the solve O(n) and incremental,
> the failure mode it introduces, and the LSM-level deployment that trades
> the two filters off against each other.

The name is the trick: **Ribbon** stands for "Rapid Incremental Boolean
Banding ON the fly" ([arXiv:2103.02515](https://arxiv.org/abs/2103.02515),
§1). `StandardHasher` (`util/ribbon_impl.h:165`) gives each key a
coefficient vector that is nonzero only in a `kCoeffBits`-wide *band*
starting at a hashed position — and `kCoeffBits` is just `sizeof(CoeffRow)
* 8`, i.e. **64 or 128** (:114-115). A system where every row's nonzeros
sit in a narrow diagonal band admits **banded Gaussian elimination** —
O(n) with tiny constants instead of the O(n³) of a dense solve.
`StandardBanding` (:471) runs it *incrementally*: as each key arrives its
banded row is reduced against the rows already placed and dropped into an
empty pivot slot (the on-the-fly insertion the paper describes at §4). The
number of band start positions is `num_starts_ = num_slots - kCoeffBits + 1`
(:504) — for, say, 1000 slots and 64-bit bands, 937 places a band can
begin. The actual back-substitution lives one file over, in
`BandingAddRange` (`util/ribbon_alg.h:611`), which `AddRange`
(`util/ribbon_impl.h:570-577`) calls. Streaming build is ribbon's edge over
xor filters, which need all keys up front.

Two costs to hold onto. First, construction can *fail* — the random banded
system can come out singular — and RocksDB retries with a different hash
seed (`StandardRehasherAdapter` :416), unlike blocked bloom, whose monotone
"set bits" build can never fail. Second, both build and query burn more CPU
than bloom's bit probes. RocksDB's own deployment note quantifies the
trade: across large LSM deployments, blocked Bloom filters use "roughly 10%
of memory and roughly 1% of CPU" (paper §1, footnote 5) — so ribbon buys
back that ~10% memory at a CPU cost. The policy follows directly:
**ribbon for the cold bottom LSM levels** (most keys live there — space
dominates) and **blocked bloom for the hot top levels** (queried constantly
— speed dominates), via `RibbonFilterPolicy`'s `bloom_before_level` knob.

## Where each step lives in the code

Peter Dillinger's blog-style comments *inside* the headers are the best
docs — read code and comments together.

`util/bloom_impl.h` — Steps 2–4, RocksDB's two generations:

| anchor | what it is |
|---|---|
| `LegacyLocalityBloomImpl` (:404) | old "one cache line per key" format: `AddHash` (:432) picks a line via `GetLine` (:406), but probes are derived by a weak shift-rotate (`delta = (h>>17)\|(h<<15)` at :473) — measurable FPR bias (the comment at :107 clocks it at 1.138% vs `FastLocalBloomImpl`'s 0.957% at the same setting) |
| `FastLocalBloomImpl` (:144) | current "format_version=5" bloom: 512-bit (64-byte) blocks, probes from `h *= 0x9e3779b9` golden-ratio remix (Step 4) |
| `AddHashPrepared` (:206) | the probe loop: each probe takes `h >> (32 - 9)` — the top 9 bits of a *re-multiplied* h, one bit inside the line (Step 4's quoted block) |
| `HashMayMatchPrepared` (:231) | query = same loop, early-exit on the first zero bit (the AVX2 path; Step 4's illustration is its scalar shape) |
| `StandardFpRate` (:32) | the un-blocked bloom FPR, `pow(1 - exp(-k/b), k)` — Step 2's formula, verbatim |
| `CacheLocalFpRate` (:42) | the blocked-bloom tax: the **average of two `StandardFpRate` values**, at one std-dev above (:53-54) and below (:55-56) the mean keys-per-line — Step 4's ±1σ model, *not* a Poisson sum |

`util/ribbon_impl.h` (+ `util/ribbon_alg.h`) — Steps 5–6:

| anchor | what it is |
|---|---|
| `StandardHasher` (:165) | coefficient vectors nonzero only in a `kCoeffBits`-wide band; `kCoeffBits = sizeof(CoeffRow)*8` = 64 or 128 (:114-115) |
| `StandardBanding` (:471) | incremental banded elimination; `num_starts_ = num_slots - kCoeffBits + 1` at :504 |
| `AddRange` (:570-577) → `BandingAddRange` (`ribbon_alg.h:611`) | on-the-fly back-substitution as keys arrive |
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

## Done when

Answer each before unfolding it.

- [ ] You can state the filter contract: one-sided error, and which side.

  <details><summary>Answer</summary>

  A filter may answer "maybe present" for a key that is absent (a false
  positive) but must never answer "absent" for a key that is present (a
  false negative is forbidden by construction, because a present key's bits
  were all set at insert and bits are never cleared). The one-sidedness is
  what a lookup path needs: "definitely absent" is trustworthy, so you skip
  the expensive SST read with certainty; a false positive costs only one
  wasted probe. At the going rate of ~10 bits per key the FPR is ~0.8%
  (Step 2's `StandardFpRate(10, 7)`), so 99.2% of misses are caught for
  5% of a HashSet's memory.

  </details>

- [ ] You can explain why exactly half the bits are set at optimal k, and why that is intuitive.

  <details><summary>Answer</summary>

  The fraction of bits still 0 after all inserts is e^(−k/b) (Step 2). At
  the optimal probe count k = b·ln2, that exponent is −(b·ln2)/b = −ln2, so
  e^(−ln2) = **exactly 1/2** — half the bits are 0, half are 1. Worked at
  b = 10, k = 7: e^(−0.7) = 0.497, essentially half.

  It is intuitive as an entropy argument: a single bit carries the most
  information when it is 0 with probability 1/2 (maximal entropy, 1 bit).
  Push more keys in and too many bits are 1, so every query's probes match
  and the FPR climbs; use fewer probes and you waste array capacity. The
  minimum FPR sits exactly where each bit is a fair coin.

  </details>

- [ ] You can name the two sins — 1.44x space and k cache misses — and say which one blocked bloom fixes.

  <details><summary>Answer</summary>

  Sin #1 is space: bloom spends 1.4427·log₂(1/f) bits per key against an
  information floor of log₂(1/f) — a 44% overhead, the factor 1/ln2 (Step 3;
  Ribbon paper §1). Sin #2 is time: the k probe bits are in k random words,
  so a query costs up to k cache misses (7 at 10 bpk).

  Blocked bloom fixes **sin #2 only** — `FastLocalBloomImpl`
  (`bloom_impl.h:144`) puts all probes in one 512-bit cache line, so a
  query touches one line instead of k. It pays for that fix in FPR, not
  space: `CacheLocalFpRate` (:42) prices the crowding tax at ~1.13× the
  un-blocked rate for a 512-bit line. Sin #1 (space) is what the ribbon/xor
  family attacks instead.

  </details>

- [ ] You can explain filters as a linear solve, and why the ribbon band makes it O(n).

  <details><summary>Answer</summary>

  A xor/ribbon filter *solves for* an array S of r-bit slots such that
  `row(key)·S = fingerprint(key)` over GF(2) for every key, where `row(key)`
  is a hash-derived coefficient vector. Query recomputes `row·S` and
  compares; a non-key collides with probability exactly 2^−r. A dense
  Gaussian solve over n such equations is O(n³).

  Ribbon makes `row(key)` nonzero only inside a `kCoeffBits`-wide (64 or
  128) diagonal band starting at a hashed position (`StandardHasher`,
  `ribbon_impl.h:165`). Because every row's nonzeros are confined to that
  band, banded Gaussian elimination reduces each new row against only the
  O(kCoeffBits) rows it overlaps — O(n) total, done incrementally by
  `StandardBanding` (:471), back-substituting in `BandingAddRange`
  (`ribbon_alg.h:611`).

  </details>

- [ ] You can say what happens when ribbon construction fails and what RocksDB does about it.

  <details><summary>Answer</summary>

  The random banded system can come out singular — no assignment of S
  satisfies all the equations. Construction then *fails*, and RocksDB
  retries the whole build with a different hash seed via
  `StandardRehasherAdapter` (`ribbon_impl.h:416`). This is the price of
  "solve" over "set": blocked bloom's build is monotone (only ever sets
  bits) and can never fail, whereas ribbon and cuckoo can, so both need a
  retry/fallback path. It is a build-time cost, not a query-time one — once
  a seed succeeds, queries are deterministic.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including why RocksDB picks ribbon for the bottom level — and you have this topic's measured miss costs to compare against.

  <details><summary>Answer</summary>

  RocksDB's split — ribbon on the bottom LSM levels, blocked bloom on the
  hot top levels (`RibbonFilterPolicy`'s `bloom_before_level`) — follows
  from ribbon being ~10% smaller than bloom but several× slower to build and
  query. The bottom level holds the overwhelming majority of keys, so its
  filters dominate memory, yet it is probed rarely; there, the ~10% space
  win is worth the CPU. The top levels are tiny but queried constantly, so
  blocked bloom's one-cache-line speed wins. The paper's own measurement
  frames the stakes: blocked Bloom filters are "roughly 10% of memory and
  roughly 1% of CPU" in large deployments (§1, footnote 5).

  Put the win beside this topic's baseline: a point miss costs **246 ns**
  (binary search) or **299 ns** (BTreeMap), while a 224 MB HashSet does it
  in **28 ns** ([FINDINGS.md](../../FINDINGS.md) row 26). A blocked bloom
  aims for HashSet-class miss-skipping at ~12 MB (10M × 10 bits) — the
  space that ribbon then shaves another ~24% off on the cold levels.

  </details>

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
