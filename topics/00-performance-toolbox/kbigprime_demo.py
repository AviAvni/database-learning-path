#!/usr/bin/env python3
"""Why db_bench multiplies by 0x5bd1e995 — and what the choice actually buys.

Companion to reading-rocksdb-db-bench.md, Step 5. It reimplements
`Benchmark::GetRandomKey` (tools/db_bench_tool.cc:7103-7122 at
facebook/rocksdb@7c80a5a) in Python, then swaps the constant on line 7117 for
other values to see what breaks.

The claim under test is the comment on line 7116, "Map to a different number to
avoid locality". Six things fall out, and every number the script prints is
computed by it, not asserted:

  1. 0x5bd1e995 is not prime. The variable is named kBigPrime; the value is
     MurmurHash2's mixing constant m (util/murmurhash.cc:97,153 in the same
     tree), and it factors.
  2. What the multiply buys is a *block-cache footprint*, and the ceiling on
     the effect is exactly the number of KV-pairs per SST data block.
  3. The property that makes a multiplier usable is gcd(M, FLAGS_num) == 1.
     Primality is only a cheap way of making that hold for every --num.
  4. Coprime is necessary, not sufficient, and the magnitude of M is not what
     matters — M mod FLAGS_num is. 1000000007 is a bigger prime than
     0x5bd1e995 and scatters an order of magnitude worse at --num=1000000,
     because it is congruent to 7.
  5. Among residues that do cover the key space, they are still not equal, and
     0x5bd1e995's is a poor one at --num=1000000: its stride walk drops to a
     16-key smallest gap, under the 35 keys per block, so 79% of neighbouring
     hot keys end up sharing a block. It de-localizes *worse than a uniform
     random scatter would*, while 2654435761 (the prime nearest 2^32/phi)
     beats chance.
  6. This constant has --num values where it collapses outright, because it
     factors. --num=13000000 loses 12/13 of the key space, and line 7118's
     64-bit overflow does not rescue it below ~12 billion keys.

Stdlib only, seeded, no arguments, ~15 s:  python3 kbigprime_demo.py
"""

import bisect
import math
import random
from collections import Counter

# --- db_bench defaults, from the pinned clone (facebook/rocksdb@7c80a5a) ------
# tools/db_bench_tool.cc:275  DEFINE_int64(num, 1000000, ...)
# tools/db_bench_tool.cc:388  DEFINE_int32(key_size, 16, ...)
# tools/db_bench_tool.cc:337  DEFINE_int32(value_size, 100, ...)
# include/rocksdb/table.h:400 uint64_t block_size = 4 * 1024;  (db_bench_tool.cc:727 defers to it)
NUM = 1_000_000
KEY_SIZE = 16
VALUE_SIZE = 100
BLOCK_SIZE = 4 * 1024

# An SST data block is filled to ~block_size with key+value pairs, so this many
# adjacent keys share one block and are fetched, cached and evicted together.
# It is a model, not a measured RocksDB figure: real blocks carry restart
# points and prefix-compress the keys, so the true count is somewhat higher.
ENTRIES_PER_BLOCK = BLOCK_SIZE // (KEY_SIZE + VALUE_SIZE)

KBIG_PRIME = 0x5BD1E995          # db_bench_tool.cc:7117
KBIG_INT = 1 << 62               # db_bench_tool.cc:7109
MASK64 = (1 << 64) - 1
EXP_RANGE = 10.0                 # a --read_random_exp_range the flag help invites
SAMPLES = 2_000_000
SEED = 20260830


def rand_num(rand_int: int, num: int, exp_range: float) -> int:
    """Lines 7109-7115: the exponential skew, before any multiply."""
    order = -(rand_int % KBIG_INT) / KBIG_INT * exp_range
    return int(math.exp(order) * num)


def get_random_key(rand_int: int, num: int, exp_range: float, mult: int) -> int:
    """Lines 7103-7121, with the constant on 7117 made a parameter.

    `mult=1` is the un-mapped control: the skewed draw used directly.
    The `& MASK64` is line 7118's "Overflow is like %(2^64)".
    """
    if exp_range == 0:
        return rand_int % num
    return ((rand_num(rand_int, num, exp_range) * mult) & MASK64) % num


def draw(num, exp_range, mult, samples=SAMPLES, seed=SEED):
    rng = random.Random(seed)
    hits = Counter()
    for _ in range(samples):
        hits[get_random_key(rng.getrandbits(64), num, exp_range, mult)] += 1
    return hits


def factor(n):
    fs, d = [], 2
    while d * d <= n:
        while n % d == 0:
            fs.append(d)
            n //= d
        d += 1 if d == 2 else 2
    if n > 1:
        fs.append(n)
    return fs


def working_set(hits, samples):
    """The block-cache working set for half the traffic.

    Take the hottest keys until they account for 50% of the requests, then
    count the distinct SST data blocks those keys live in. Keys first, blocks
    second: picking *blocks* by traffic instead would reward a scattered
    layout for the cold keys that happen to share a block with a hot one,
    which is not a property of the layout under test.
    """
    covered, keys = 0, 0
    blocks = set()
    for key, n in hits.most_common():
        covered += n
        keys += 1
        blocks.add(key // ENTRIES_PER_BLOCK)
        if covered * 2 >= samples:
            break
    return keys, len(blocks)


def rule(title):
    print(f"\n{'=' * 78}\n{title}\n{'=' * 78}")


# --- 1. the name --------------------------------------------------------------
rule("1. Is kBigPrime prime?")
fs = factor(KBIG_PRIME)
print(f"  0x5bd1e995 = {KBIG_PRIME}")
print(f"  factors    = {' x '.join(map(str, fs))}")
print(f"  prime?       {len(fs) == 1}")
print("""
  It is not. 0x5bd1e995 is the mixing constant `m` of 32-bit MurmurHash2 —
  the same tree carries it at util/murmurhash.cc:97 and :153 — borrowed here
  for its bit pattern, and the name on line 7117 is aspirational. Sections 4
  and 5 show what primality would have bought and where the difference bites.""")

# --- 2. what the skew alone does ---------------------------------------------
rule(f"2. The skew alone (--num={NUM:,}  --read_random_exp_range={EXP_RANGE})")
plain = draw(NUM, EXP_RANGE, 1)
mapped = draw(NUM, EXP_RANGE, KBIG_PRIME)
print(f"  Model: {KEY_SIZE}B key + {VALUE_SIZE}B value in a {BLOCK_SIZE}B block"
      f"  ->  {ENTRIES_PER_BLOCK} keys per SST data block")
print(f"  Key space holds {math.ceil(NUM / ENTRIES_PER_BLOCK):,} data blocks"
      f" (~{NUM * (KEY_SIZE + VALUE_SIZE) / 2**20:.0f} MiB of user data),"
      f" drawn {SAMPLES:,} times.\n")
for label, hits in (("mult=1     — the counterfactual: skew, no scatter", plain),
                    ("mult=kBigPrime — line 7119 as written", mapped)):
    top = [k for k, _ in hits.most_common(1000)]
    ws_keys, ws_blocks = working_set(hits, SAMPLES)
    print(f"  {label}")
    print(f"    hottest key id                        {top[0]:,}")
    print(f"    span of the 1000 hottest keys         "
          f"{(max(top) - min(top)) / NUM * 100:6.2f}% of the key space")
    print(f"    blocks holding those 1000 keys        "
          f"{len({k // ENTRIES_PER_BLOCK for k in top}):,}")
    print(f"    keys carrying 50% of the requests     {ws_keys:,}")
    print(f"    blocks those keys live in             {ws_blocks:,}"
          f"   ({ws_blocks * BLOCK_SIZE / 2**20:.1f} MiB of block cache)\n")
p_keys, p_blocks = working_set(plain, SAMPLES)
m_keys, m_blocks = working_set(mapped, SAMPLES)
print(f"""  Same hotness curve, same {p_keys:,} hot keys, and the multiply turns a
  {p_blocks:,}-block working set into a {m_blocks:,}-block one — {m_blocks / p_blocks:.1f}x the block cache to
  serve identical traffic. The ceiling on that ratio is {ENTRIES_PER_BLOCK}, the number of keys
  per block: unmapped, {ENTRIES_PER_BLOCK} adjacent hot keys ride in on one block read; mapped,
  each one costs its own. That is FAST'20 SS7.1's "extremely large number of
  block reads", in one number — and the paper's complaint about db_bench is
  precisely that line 7119 does it on purpose.""")

# --- 3. what makes a multiplier work -----------------------------------------
rule(f"3. Other multipliers, same --num={NUM:,}")
CANDIDATES = [
    (1, "control: no mapping"),
    (3, "coprime, but small"),
    (37, "coprime, > keys-per-block"),
    (65_536, "2^16 - shares 2^6 with 10^6"),
    (1_000_000, "== FLAGS_num"),
    (1_000_000_007, "a bigger prime than kBigPrime"),
    (2_654_435_761, "prime nearest 2^32/phi"),
    (KBIG_PRIME, "kBigPrime (0x5bd1e995)"),
]
print(f"  {'multiplier':>14} {'gcd':>7} {'M mod num':>10} {'distinct':>9} "
      f"{'span%':>7} {'blk/1000':>9} {'ws blocks':>10}  note")
ws, span = {}, {}
for m, note in CANDIDATES:
    hits = draw(NUM, EXP_RANGE, m)
    top = [k for k, _ in hits.most_common(1000)]
    _, ws[m] = working_set(hits, SAMPLES)
    span[m] = (max(top) - min(top)) / NUM * 100
    print(f"  {m:>14,} {math.gcd(m, NUM):>7,} {m % NUM:>10,} {len(hits):>9,} "
          f"{span[m]:>6.2f}% "
          f"{len({k // ENTRIES_PER_BLOCK for k in top}):>9,} {ws[m]:>10,}  {note}")
    del hits
print(f"""
  Read 'gcd' first. i -> (i*M) mod num is a permutation of the key space
  exactly when gcd(M, num) == 1; otherwise it is gcd-to-1 onto the multiples
  of the gcd, and 'distinct' collapses. 65536 shares 2^6 with 10^6 and loses
  {(1 - 1 / math.gcd(65_536, NUM)) * 100:.0f}% of the key space at this --num; M == FLAGS_num sends every read
  to key 0.

  Now read 'M mod num', which is the column that actually explains 'span%'.
  The multiply is a stride walk: the hot ids 0,1,2,... land on
  0, s, 2s, ... (mod num) with s = M mod num, so the magnitude of M is
  irrelevant and only its residue matters. That is why 1,000,000,007 — a
  larger, genuinely prime multiplier — behaves like the multiplier {1_000_000_007 % NUM}: congruent
  to {1_000_000_007 % NUM} mod {NUM:,}, its hot keys sit {1_000_000_007 % NUM} apart, {ENTRIES_PER_BLOCK // (1_000_000_007 % NUM)} to a block, and its
  working set lands at {ws[1_000_000_007]:,} blocks against kBigPrime's {ws[KBIG_PRIME]:,}. 'Big' on line
  7117 has to mean big *after reduction mod FLAGS_num*, which for a fixed
  constant is luck about the --num the user happened to pass.

  M=3 and M=37 make the same point from below, and they separate the two
  scales in play. M=3 keeps the hot ids inside a sliver of the key space and
  inside shared blocks. M=37 is still a sliver — {span[37]:.1f}% of the key space — but
  37 > {ENTRIES_PER_BLOCK} keys per block, so each hot key already lands in its own block:
  locality dies at stride keys-per-block, long before the key space is
  covered.

  Which leaves the question Section 3b answers: among the residues that do
  cover the key space, are they equally good? Read 'ws blocks' as "how many
  block reads the hot set costs" — it is capped at the {p_keys:,} hot keys, one
  block each, and the control sits at {p_blocks:,}.""")

# --- 3b. not all good-looking residues are equal ------------------------------
rule("3b. The gap spectrum: where 0x5bd1e995 undoes its own scatter")
HOT = working_set(plain, SAMPLES)[0]   # the hot prefix, from Section 2
print(f"  Take the hot ids 0..{HOT - 1:,} (Section 2's 50%-of-traffic set), map each with")
print(f"  M, sort the images, and look at the gaps between neighbours.\n")
def convergents(s_, num):
    """Continued-fraction convergents p/q of s/num, with the error |q*s - p*num|.

    q is a number of stride-walk points and the error is the smallest gap that
    appears once you have that many: that is what a convergent *is* — the best
    rational approximation with denominator <= q.
    """
    a, b, out = s_, num, []
    h0, h1, k0, k1 = 0, 1, 1, 0
    while b:
        q, r = divmod(a, b)
        h0, h1 = h1, q * h1 + h0
        k0, k1 = k1, q * k1 + k0
        out.append((h1, k1, abs(k1 * s_ - h1 * num)))
        a, b = b, r
    return out


def first_shared_block(m, kmax=200_000):
    """Measured: the hot-set size at which two hot keys first share a block.

    Walk the stride one point at a time, keeping the running smallest gap
    (which only ever shrinks), and stop when it falls under ENTRIES_PER_BLOCK.
    Reported alongside the convergent denominator it should land just after.
    """
    pts, cur, mg, stride = [0], 0, NUM, m % NUM
    for i in range(1, kmax):
        cur = (cur + stride) % NUM
        j = bisect.bisect(pts, cur)
        mg = min(mg,
                 cur - pts[j - 1] if j > 0 else NUM,
                 pts[j] - cur if j < len(pts) else NUM)
        if mg < ENTRIES_PER_BLOCK:
            return i + 1, mg
        pts.insert(j, cur)
    return None, mg


def gap_spectrum(m):
    pts = sorted((i * m) % NUM for i in range(HOT))
    gaps = Counter(b - a for a, b in zip(pts, pts[1:]))
    lo, lo_n = min(gaps.items())
    return (len(gaps), lo, lo_n / (HOT - 1),
            len({p // ENTRIES_PER_BLOCK for p in pts}))


print(f"  {'multiplier':>14} {'M mod num':>10} {'gaps':>5} "
      f"{'smallest gap':>27} {'blocks':>8}")
for m in (1_000_000_007, KBIG_PRIME, 2_654_435_761):
    n_gaps, lo, lo_share, blocks = gap_spectrum(m)
    print(f"  {m:>14,} {m % NUM:>10,} {n_gaps:>5} "
          f"{f'{lo:,} keys, {lo_share * 100:.1f}% of pairs':>27} {blocks:>8,}")
_rand = (NUM // ENTRIES_PER_BLOCK + 1) * (1 - (1 - ENTRIES_PER_BLOCK / NUM) ** HOT)
print(f"  {'uniform random':>14} {'-':>10} {'-':>5} {'-':>27} {_rand:>8,.0f}")
_, KB_GAP, KB_SHARE, KB_BLOCKS = gap_spectrum(KBIG_PRIME)
_, KN_GAP, _, KN_BLOCKS = gap_spectrum(2_654_435_761)
KB_Q, KB_ERR = first_shared_block(KBIG_PRIME)
KN_Q, KN_ERR = first_shared_block(2_654_435_761)
KB_CQ = next(q for _, q, e in convergents(KBIG_PRIME % NUM, NUM) if e < ENTRIES_PER_BLOCK)
KN_CQ = next(q for _, q, e in convergents(2_654_435_761 % NUM, NUM) if e < ENTRIES_PER_BLOCK)


def partial_quotients(m, n=10):
    a, b, out = m % NUM, NUM, []
    while b and len(out) < n:
        q, r = divmod(a, b)
        out.append(q)
        a, b = b, r
    return ",".join(map(str, out[1:]))


KB_CF, KN_CF = partial_quotients(KBIG_PRIME), partial_quotients(2_654_435_761)
_rows = ["    {:>12}  {:>10}  {:>12}".format("multiplier", "q (points)", "smallest gap")]
for m in (KBIG_PRIME, 2_654_435_761):
    for _, q, err in convergents(m % NUM, NUM)[1:]:
        if q > 20_000:
            break
        _rows.append("    {:>12,}  {:>10,}  {:>12,}".format(m, q, err))
    _rows.append("")
_conv_table = "\n".join(_rows)
print(f"""
  Three gap lengths, every time: that is the three-distance theorem, which
  says a stride walk on a circle always partitions it into at most three
  distinct gaps, however many points you take. What differs is how small the
  smallest one gets, and how soon — and that is decided by the continued
  fraction of s/num, whose convergents p/q are exactly "after q points, the
  smallest gap is |q*s - p*num|":

{_conv_table}
  Read down the error column for the first value below {ENTRIES_PER_BLOCK}, the keys per block:
  past that many hot keys, the multiply stops separating neighbours and starts
  pairing them inside one block read. Walking the stride point by point and
  watching the running smallest gap puts the crossover two points later than
  that denominator, which is where it belongs — the pair realising the gap
  needs both its endpoints drawn:

    {KBIG_PRIME:<14,} first shared block at {KB_Q:,} hot keys (gap {KB_ERR}); convergent q = {KB_CQ:,}
    {2_654_435_761:<14,} first shared block at {KN_Q:,} hot keys (gap {KN_ERR}); convergent q = {KN_CQ:,}

  At this workload's {HOT:,} hot keys, 0x5bd1e995 is well past its crossover and
  Knuth's constant has not reached its own: {KB_SHARE * 100:.0f}% of neighbouring hot keys
  share a block under 0x5bd1e995, giving {KB_BLOCKS:,} blocks where a uniform random
  scatter of the same {HOT:,} keys would give ~{_rand:,.0f}. It scatters *worse than
  chance*, and partially undoes the de-localization it was chosen for.

  Knuth's constant is the other side. 2,654,435,761 is the nearest prime below
  2^32/phi = {2**32 / ((1 + 5**0.5) / 2):,.1f} — {2**32 / ((1 + 5**0.5) / 2) - 2_654_435_761:.1f} above it — picked so that s/num
  is as hard as possible to approximate — its partial quotients stay small
  ({KN_CF} against 0x5bd1e995's {KB_CF}) — so
  the error shrinks as slowly as any ratio can and every one of the {HOT:,} hot
  keys still gets its own block. Better than chance, which is what
  low-discrepancy means and why that constant exists.

  The honest general statement: *no* fixed multiplier keeps hot keys out of
  shared blocks forever. Enlarge the hot set past the crossover and the
  smallest gap falls under a block again. The golden ratio only maximises how
  long that takes — here {KN_Q / KB_Q:.1f}x longer than 0x5bd1e995 manages.

  None of this is visible in the name. 'Big prime' predicts neither the 13
  in Section 5 nor the 16-key gap here; both are properties of the residue
  against the --num that happens to be on the command line.""")

# --- 4. why a prime, then ----------------------------------------------------
rule("4. What primality would have bought")
NUMS = [1_000_000, 10_000_000, 100_000_000, 13_000_000, 4_194_304, 1_000_003]
COLS = [(65_536, "2^16"), (1_000_000_007, "prime"), (KBIG_PRIME, "kBigPrime")]
print(f"  {'--num':>14} " + " ".join(f"{lbl:>13}" for _, lbl in COLS))
for n in NUMS:
    print(f"  {n:>14,} " +
          " ".join(f"{math.gcd(m, n):>13,}" for m, _ in COLS))
print("""  (cells are gcd(multiplier, --num); 1 is the good value)

  A genuine prime P is coprime with every --num that is not a multiple of P,
  so one constant stays a permutation for every key count a user can pass.
  That robustness against an unknown --num is the only thing primality buys —
  it says nothing about how well the multiplier scatters, which Section 3
  showed is a question about M mod num. A good constant needs both, and they
  are independent. 0x5bd1e995 has the second and only approximates the first.""")

# --- 5. where this constant actually collapses -------------------------------
rule("5. The --num values where 0x5bd1e995 fails")
BAD = 13_000_000  # 13 is its smallest factor
bad_hits = draw(BAD, EXP_RANGE, KBIG_PRIME, samples=200_000)
good_hits = draw(BAD, EXP_RANGE, 1_000_000_007, samples=200_000)
g = math.gcd(KBIG_PRIME, BAD)
print(f"  --num={BAD:,}  (= 13 x 1,000,000, and 13 divides 0x5bd1e995)")
print(f"    gcd(kBigPrime, num)                = {g}")
print(f"    every key drawn is a multiple of 13? "
      f"{all(k % g == 0 for k in bad_hits)}")
print(f"    reachable fraction of the key space  {1 / g * 100:.2f}%"
      f"   (measured distinct keys: {len(bad_hits):,})")
print(f"    same draw, multiplier 1000000007     {len(good_hits):,} distinct keys")
print(f"""
  readrandom would never touch 12/13 of the database it just filled, and
  db_bench prints no warning. What makes it a live bug rather than a
  hypothetical is line 7118: the 64-bit overflow that would scramble the
  residue needs rand_num >= 2^64/M = {2**64 // KBIG_PRIME:,}, i.e. --num above
  ~{2**64 // KBIG_PRIME / 1e9:.1f} billion keys. Below that the product never wraps, so the
  comment's reassurance covers a case no one reaches.""")
print(f"    largest product at --num={BAD:,}: {(BAD - 1) * KBIG_PRIME:,}")
print(f"    2^64:                          {2**64:,}"
      f"   -> wraps: {(BAD - 1) * KBIG_PRIME > MASK64}")
