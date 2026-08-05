# SIMD for databases: two primitives, four operators

Polychroniou, Raghavan & Ross's SIGMOD '15 paper, "Rethinking SIMD
Vectorization for In-Memory Databases", turned "SIMD for databases"
from folklore into a catalog. It vectorises four fundamental operators
— selection scan, hash table probe/build, Bloom filter probe, radix
partition — and shows each is a composition of two primitives:
**selective store** (compress) and **selective load** (expand), plus
their indexed cousins gather and scatter. Before the paper, this
chapter builds those primitives and each operator's shape step by
step. Read it as the specification for this topic's
`experiments/filter.rs` and for M17's engine kernels.

There is no pinned clone for the paper's code, so every claim is
anchored to the paper by section, algorithm or figure number, and
every speedup is reported **with the operator and the machine it was
measured on** — the paper's numbers vary from 1.05× to 10× depending
on both. Local measurements come from this topic's `notes.md` and are
labelled as such.

## The problem in one sentence

Database operators branch on data (does this row pass? did this probe
hit?), and a mispredicted branch costs tens of cycles; the paper
recasts the four core operators as branch-free lane operations, and —
just as usefully — tells you exactly where that stops paying, which is
wherever the kernel's cost is a random memory access rather than an
instruction.

## The concepts, step by step

### Step 1 — lanes, masks, and the operator question

> **In:** an operator whose per-row work diverges — keep this row, drop
> that one, probe another bucket.
> **Out:** the same operator expressed as a **mask** plus instructions
> that consume masks, with no branch anywhere.

SIMD — single instruction, multiple data — applies one instruction to a
vector of W values, its **lanes**. On this Mac a 128-bit NEON register
holds W = 4 `f32` or `u32` lanes; the paper's Xeon Phi holds W = 16
32-bit lanes in 512 bits.

`a[i] + b[i]` is trivial to vectorise. Database operators are not,
because each lane wants to do something *different*. The paper's whole
framing is: express every divergence as a **mask** — a value with one
bit per lane, produced by a vector comparison — and then find
instructions that *consume* masks instead of branching on them. Four
such instructions carry the entire catalog.

### Step 2 — the two primitives (§3), and how to fake them

> **In:** a vector of values and a mask.
> **Out:** a memory operation that touches only the masked lanes —
> either natively, or emulated with a permutation lookup table.

§3 defines four operations, each with its own figure:

```
 selective STORE (Fig. 1)        selective LOAD (Fig. 2)
 lanes:  a b c d e f g h         memory: p q r s ...
 mask:   1 0 1 1 0 0 1 0         mask:   1 0 1 1 ...
 memory: a c d g  -------->      lanes:  p . q r ...  <--------
 (filter output, partition out)  (refill lanes after some finish)

 gather  (Fig. 3): lanes = mem[idx[0..W]]   (hash probe, dict decode)
 scatter (Fig. 4): mem[idx[0..W]] = lanes   (partition, hash build)
```

§3's note on scatter semantics matters later: "If multiple vector lanes
point to the same location, we assume that the rightmost value will be
written." That single sentence is why Step 6 exists.

Table 1 tells you which machine has which. Xeon Phi 7120P: gather yes,
scatter yes. Haswell E3-1275v3: gather yes, scatter **no**. Sandy
Bridge E5-4620: neither. Your NEON machine has none of the four as
instructions — so the paper's emulation is not a historical footnote
for you, it is the implementation:

> "Selective loads and stores are also not supported on the latest
> mainstream CPUs, but can be emulated using vector permutations. The
> lane selection mask is extracted as a bitmask and is used as an array
> index to load a permutation mask from a pre-generated table. The data
> vector is then permuted in a way that splits the active lanes of the
> mask to the one side of the register and the inactive lanes to the
> other side." (§3)

For a selective store you then store the whole vector unaligned and
advance the pointer by the popcount; for a selective load you load a
new vector and blend. §3 credits the technique to the vectorized Bloom
filter work [27], "without defining the operations".

Size the table, because that is what decides whether you can use it:

```
 permutation LUT = 2^W entries, each W lane-indices

 W = 4  (NEON, u32 lanes) : 2^4  =    16 x 16 B  =   256 B  <- this topic
 W = 8  (Haswell, 32-bit) : 2^8  =   256 x  8 B  =     2 KB
 W = 16 (Phi / AVX-512)   : 2^16 = 65536 x 16 B  =     1 MB  <- L2-sized
```

The W = 16 row is why simdjson splits its 16-byte compress into two
8-byte halves (`internal/simdprune_tables.h:11`, a 256 × 8 B = 2 KB
table) rather than building the 1 MB one. This topic's own kernel is
the top row: `notes.md`'s implementation log calls for a compact_neon
"(LUT built, all 16 masks pass)" — 16 masks because W = 4.

### Step 3 — selection scan (§4): three shapes, one selectivity sweep

> **In:** a key column, a range predicate, and a selectivity — the
> fraction of rows that pass.
> **Out:** the surviving rows (or their indexes), and a curve that
> shows which of three implementations wins at which selectivity.

§4 gives three algorithms. Algorithm 1 is scalar with a branch;
Algorithm 2 is the scalar branchless trick — store unconditionally,
advance the cursor by the predicate; Algorithm 3 is the vector version.
This topic ships the first two verbatim:

```rust
// topics/17-simd/experiments/src/filter.rs:5-25 — §4's Algorithms 1 and 2
     5  pub fn compact_branchy(vals: &[f32], t: f32, out: &mut Vec<f32>) {
     6      out.clear();
     7      for &v in vals {
     8          if v < t {
     9              out.push(v);
    10          }
    11      }
    12  }
// ... 14-15: doc comment ...
    16  pub fn compact_branchless(vals: &[f32], t: f32, out: &mut Vec<f32>) {
    17      out.clear();
    18      out.resize(vals.len(), 0.0);
    19      let mut k = 0usize;
    20      for &v in vals {
    21          out[k] = v;
    22          k += (v < t) as usize;
    23      }
    24      out.truncate(k);
    25  }
```

Line 8 is a data-dependent branch; line 22 is the same decision as
arithmetic. That is the entire difference, and the sweep is dramatic.
From `notes.md` (provided rungs, release, Apple Silicon, 2026-07-10;
GB/s of *input*, N = 4M f32 = 16,777,216 B, 20 reps):

| selectivity | branchy GB/s | branchless GB/s |
|---|---|---|
| 1 % | 10.95 | 12.70 |
| 25 % | 2.13 | 13.32 |
| 50 % | **1.19** | 12.73 |
| 75 % | 2.11 | 12.38 |
| 99 % | 6.65 | 11.98 |

Now derive the mispredict cost from that, instead of quoting a folklore
number. The paper itself never states one — §1 says only that a
mispredicted branch costs "several cycles" — so the honest figure is
the one your own machine just produced:

```
 branchy   at 50%: 16,777,216 B / 1.19e9 B/s  = 14.098 ms
 branchless at 50%: 16,777,216 B / 12.73e9 B/s =  1.318 ms
 elements: 4,194,304

 per element:  branchy 3.361 ns,  branchless 0.314 ns
 gap                          =   3.047 ns/element
 clock, derived not asserted (reading-simsimd.md Step 2):
   naive dot 10.89 GB/s / 8 B per element-pair = 1.361 G pair/s
   one scalar FMA chain, FMLA latency 3 cy  =>  >= 4.08 GHz
 3.047 ns x 4.08 GHz          =  12.43 cycles per element
 at 50% selectivity a coin-flip branch misses ~half the time:
   12.43 / 0.5                =  ~25 cycles per mispredict
```

Treat 25 as an **upper bound**: the two kernels do not do identical
work apart from the branch (branchless always stores, so it moves more
bytes), and the clock is a lower bound derived from another lane. But
it is a measured bound from this machine, which is worth more than an
unattributed "~15 cycles".
(`FINDINGS.md`'s row 17 records 0.95 GB/s at 50 % from a different run
of the same bench; use whichever you are citing, do not average them.)

Two things the README's version of this story leaves out. First, §4's
Algorithm 3 does not buffer the qualifying *values* — it selectively
stores their **indexes** into a cache-resident buffer and dereferences
keys and payloads only when the buffer is flushed, "which are used to
dereference the actual key and payload values during buffer flushing"
(§10.1). At low selectivity that skips the payload column almost
entirely, which is the real source of the low-selectivity win. Second,
the winner depends on the machine as much as the selectivity: §10.1
reports that on Xeon Phi "scalar code is almost an order of magnitude
slower than vector code, whereas on Haswell, vector code is about twice
faster", and that "on Haswell, all vector versions are almost identical
by saturating the bandwidth, while the branchless scalar code catches
up on 10% selectivity."

Your own sweep matches Haswell's shape, not Phi's — and it adds a
finding the paper does not have. `notes.md`: "branchy never actually
wins here even at 1%/99% — the paper's crossover needs even more
extreme selectivities (<1%) on this core." Branchless is flat within
±5 % across the whole sweep, which is the point: its control flow does
not depend on the data.

### Step 4 — hash probe (§5.1): W independent probes, refilled as they finish

> **In:** a probe column and a linear-probing hash table.
> **Out:** W probes in flight at once, each lane on its own bucket,
> with finished lanes refilled from the input mid-loop.

The obvious way to vectorise a probe is **horizontally**: compare one
key against several table slots at once (which is what a bucketised
table does, and what hashbrown does with control bytes). §5.1's way is
**vertical**: run W *independent* probes, one per lane.

The complication is that probes finish at different times — one hits
immediately, another collides and walks on — and Algorithm 5 solves it
with exactly Step 2's primitives:

```rust
// ILLUSTRATION — the shape of §5.1's Algorithm 5, not quoted code.
// The primitives are §3's Figures 1-4; this topic's scalar analogue is
// topics/17-simd/experiments/src/filter.rs:16, whose "advance by the
// predicate" is the same idea one lane wide.
loop {
    keys   = selective_load(keys, input, done);   // refill finished lanes
    let slot  = gather(table, hash(keys) + offset); // ~1 cache access PER LANE
    let hit   = slot.key.simd_eq(keys);
    let empty = slot.key.simd_eq(EMPTY);
    selective_store(out, hit, slot.payload);      // compress matches out
    done   = hit | empty;
    offset = done.select(ZERO, offset + 1);       // collided lanes probe on
}
```

Every lane carries its own bucket offset, so the vector never waits for
its slowest lane; a lane that finishes is refilled on the next
iteration rather than idling. §5.1 states the price plainly: "By
reusing vector lanes dynamically, we are reading the probing input
'out-of-order'. Thus, the probing algorithm is no longer stable, i.e.,
the order of the output does not match the order of the input."

This is hashbrown's group probing turned 90°. hashbrown is SIMD
*within* one probe — 8 control bytes of one bucket compared at once on
aarch64 (`reading-hashbrown-simd.md`, Step 2) — while §5.1 is SIMD
*across* probes, W separate lookups in flight. They compose badly:
vertical probing wants one table slot per lane, which is exactly what a
control-byte group is not.

### Step 5 — the gather cost model (§3): parallel instructions, serial memory

> **In:** a gather instruction over W arbitrary addresses.
> **Out:** one instruction, but W cache accesses — and therefore a rule
> about which parts of an operator vectorisation can help.

This is the paper's most durable paragraph, and it is worth memorising
verbatim:

> "Gathers and scatters are not really executed in parallel because the
> (L1) cache allows one or two distinct accesses per cycle. Executing W
> cache accesses per cycle is an impractical hardware design. Thus,
> random memory accesses have to be excluded from the O(f(n)/W)
> vectorization rule." (§3)

Cost it:

```
 W = 16 lanes, L1 serves 1-2 distinct accesses/cycle
   -> a fully-scattered 16-lane gather takes >= 8-16 cycles
   -> i.e. no better than 16 scalar loads that all hit L1
 the gather WINS only on the instruction stream around it:
   address arithmetic, comparisons, masking, the compress-store
 and it wins nothing at all if the lines miss L1 (topic 13)
```

The corollary shows up in the measured results. §10.2 on linear probing
and double hashing: the vertical vector code is "up to 6X faster than
everything else on Xeon Phi, and gain a smaller speedup for cache
resident hash tables on Haswell". The 6× is a 61-core in-order machine
with 512-bit vectors at 1.238 GHz, whose scalar code is unusually weak;
the Haswell number is small enough that the paper does not give it a
figure. Vectorisation never fixed the memory system — it removed the
instructions *around* the memory system.

### Step 6 — partition (§7.3): scatter needs conflict serialisation

> **In:** W tuples in a vector, each destined for
> `out[offset[digit(k)]++]`.
> **Out:** correct offsets even when two lanes share a digit — using
> only gather and scatter, because the machine has no conflict-detect
> instruction.

Radix partitioning scatters each lane to its partition's current
offset. The hazard is new: if two lanes in the same vector have the
same digit, both read the same counter, both compute the same address,
and §3's "the rightmost value will be written" rule silently drops a
row.

Get the fix right, because it is commonly mis-stated. AVX-512's
`vpconflictd` is mentioned in §5.1 only as **future** hardware: "Future
SIMD instruction sets include special instructions that can support
this functionality (`vpconflictd` in AVX 3), thus saving the need for
the extra scatter and gather to detect conflicts. Nevertheless, these
instructions are not supported on mainstream CPUs or the Xeon Phi as of
yet." What the paper actually implements is Algorithm 13 (§7.3),
hand-rolled from the two primitives:

```
 §7.3, Algorithm 13 — conflict serialization(h, A):
   1. reverse the lane order (permute by {W-1, ..., 0})
   2. repeat:
        scatter the unique per-lane values l into A[h]
        gather them back into l_back
        a lane is CONFLICTING where l != l_back
        increment that lane's offset c
      until no lane conflicts
   3. un-reverse c

 cost: up to W iterations, but "the total number of accesses to
 distinct memory locations is always W" — sum over iterations of
 the distinct accesses = W.
```

Two details in §7.3 explain the reversal. Because the rightmost lane is
the one that survives a conflicting scatter, tuples of the same
partition inside a vector would be written in reverse order; and per
group of k conflicting lanes the rightmost lane would increment the
offset by 1 instead of k. Reversing first fixes both, which keeps the
partitioning **stable** — and §7.3 notes that "Stable partitioning is
essential for algorithms such as LSB radixsort."

Your machine has no scatter either, so if you ever need this you are in
the same position as the paper's Sandy Bridge row: the practical answer
is topic 13's software write-combining buffers, which make the
per-tuple scatter disappear into a per-partition sequential store.

### Step 7 — what to steal for the experiments

> **In:** the paper's methodology.
> **Out:** three habits that make this topic's bench comparable to it.

- **The sweep axes.** The paper plots throughput against selectivity
  (§10.1, Fig. 5). `simd_bench`'s filter lane does the same; plot
  branchy, branchless and compress on one chart and the crossover is
  visible rather than argued.
- **Rigged input.** Selectivity is controlled exactly by construction,
  not sampled — this topic's bench picks the threshold as a quantile so
  1 %, 25 %, 50 %, 75 %, 99 % are exact.
- **Report the right unit.** Cycles or ns per tuple for probe kernels,
  GB/s for scan kernels. A memory-bound kernel reported in GB/s hides
  every instruction-level win behind the bandwidth ceiling — which is
  precisely what §10.1 saw on Haswell, where "all vector versions are
  almost identical by saturating the bandwidth".

## How to read the paper (with the concepts in hand)

- **§3 — read carefully, twice.** Figures 1-4 are the whole vocabulary,
  and the gather-cost paragraph (Step 5) is the sentence the rest of the
  paper's honesty rests on. Read the permutation-LUT emulation
  paragraph as *your* implementation, not as history.
- **§4 with Algorithms 1-3** — Step 3. Note that Algorithm 3 buffers
  indexes, not values, and work out why before reading §10.1.
- **§5.1, Algorithm 5** — Step 4, including the stability sentence.
  §5.2 (double hashing) and §5.3 (cuckoo) are variations on it; skim
  unless you are implementing them.
- **§6 (Bloom filters)** — the paper evaluates the design of [27]
  rather than inventing one. The load-bearing line is that "aborting a
  tuple as soon as one bit-test fails is essential"; that is why
  §10.3's speedups are the largest of any operator.
- **§7.3, Algorithm 13** — Step 6. Read the two paragraphs after the
  algorithm, which explain the reversal.
- **§10 — read Table 1 first.** Every speedup below it is
  per-operator and per-machine, and the two machines differ by more
  than SIMD width: 61 in-order cores at 1.238 GHz versus 4
  out-of-order cores at 3.5 GHz.

The speedups, kept together so they cannot be quoted loose:

| operator | Xeon Phi (512-bit) | Haswell (256-bit) | where |
|---|---|---|---|
| selection scan | ~10× ("almost an order of magnitude") | ~2× | §10.1, Fig. 5 |
| linear probing / double hashing probe | up to 6× | "smaller … for cache resident hash tables" | §10.2, Fig. 6 |
| cuckoo probe | 5× | 1.7× | §10.2, Fig. 7 |
| Bloom filter probe | 3.6–7.8× | 1.3–3.1× | §10.3, Fig. 10 |
| radix histogram (count replication) | 2.55× | bandwidth-saturated | §10.4, Fig. 11 |
| LSB radixsort | 2.2× | saturated | §10.5.1 |
| hash join, no-partition / min-partition / max-partition | 1.05× / 1.25× / 3.3× | — | §10.5.1, Fig. 15 |

The abstract's headline — "up to an order of magnitude faster than the
state-of-the-art scalar and vector approaches" — is the top-left cell
of that table, not a general result.

## Questions for notes.md

1. Recompute Step 3's mispredict bound from the 25 % and 75 % rows of
   `notes.md` instead of the 50 % row. A random branch taken with
   probability p mispredicts with probability 2p(1-p) under a simple
   predictor; does the implied cycles-per-miss stay near 19, and what
   does it mean if it does not?
2. Step 2's LUT table has three rows. Your NEON kernel needs the
   256 B one. Work out what changes if you compact `f64` (W = 2) or
   `u8` (W = 16) lanes instead, and say at which W you would switch to
   simdjson's split-the-mask-in-half trick.
3. §4's Algorithm 3 buffers indexes rather than values. At 1 %
   selectivity with a 4-byte key and a 32-byte payload, compute the
   bytes touched per input row for both designs, and check the answer
   against §10.1's claim that "avoiding payload column accesses
   dominates low selectivities".
4. Vertical probing is "no longer stable" (§5.1). Which downstream
   operators in a Cypher pipeline care about tuple order, and which
   can be told not to (topic 11's selection vectors)?
5. §10.3's Bloom filter speedup is the largest in the table. The
   mechanism is that a Bloom probe aborts as soon as one bit-test
   fails (§6). Explain why that makes vectorisation *easier* here and
   harder for the hash probe of Step 4.
6. For M17: rank the four operators by expected engine-level win in
   our Cypher pipeline, using the table above *and* Step 5's rule
   about random memory access. Where does Amdahl bite first?

## Done when

Answer each before unfolding it.

- [ ] You can name the two primitives and their indexed cousins, and say how to emulate them on a machine that has none.

  <details><summary>Answer</summary>

  Selective store (compress, §3 Fig. 1) and selective load (expand,
  Fig. 2); gather (Fig. 3) and scatter (Fig. 4) are the indexed
  versions. Emulation (§3): extract the lane mask as a bitmask, index a
  pre-generated permutation table with it, permute the vector so the
  active lanes are contiguous, then store unaligned (for a store) or
  load-and-blend (for a load). The table is `2^W` entries of W lane
  indices — 256 B at W = 4, 2 KB at W = 8, 1 MB at W = 16.

  </details>

- [ ] You can state what a mispredicted branch costs on *your* machine, and show the arithmetic.

  <details><summary>Answer</summary>

  The paper gives no figure — §1 says only "several cycles" — so derive
  it. From `notes.md`, at 50 % selectivity over N = 4M f32
  (16,777,216 B): branchy 1.19 GB/s = 14.098 ms, branchless
  12.73 GB/s = 1.318 ms. Per element that is 3.361 ns vs 0.314 ns, a
  gap of 3.047 ns ≈ 12.43 cycles at the ≥ 4.08 GHz clock derived in
  `reading-simsimd.md` Step 2; a coin-flip branch misses
  about half the time, so **≲ 25 cycles per mispredict**. It is an
  upper bound because branchless also stores every element.

  </details>

- [ ] You can explain the selection-scan result *with* its machine, and say how your own sweep differs from the paper's.

  <details><summary>Answer</summary>

  §10.1: on Xeon Phi scalar is "almost an order of magnitude slower
  than vector code"; on Haswell "vector code is about twice faster",
  and all the vector variants tie by saturating bandwidth while the
  branchless scalar catches up at 10 % selectivity. Algorithm 3's real
  trick is buffering qualifier *indexes* so payload columns are skipped
  at low selectivity.

  On this Mac (`notes.md`) branchy collapses from 10.95 GB/s at 1 % to
  1.19 GB/s at 50 % while branchless stays flat within ±5 % — and
  branchy **never wins**, not even at 1 % or 99 %, because the
  crossover needs selectivity below 1 % on this core.

  </details>

- [ ] You can describe vertical hash probing, and say what it costs you.

  <details><summary>Answer</summary>

  §5.1's Algorithm 5 runs W independent probes, one per lane, each with
  its own bucket offset: selectively load new keys into finished lanes,
  gather one table slot per lane, compare for hit and for EMPTY,
  selectively store the matched payloads, advance the offset only where
  not done. It never stalls on its slowest lane.

  The price, stated in §5.1: lanes are reused dynamically, so the probe
  input is read out of order and "the probing algorithm is no longer
  stable". Contrast hashbrown, which is SIMD *within* a single probe
  (8 control bytes per group on aarch64).

  </details>

- [ ] You can state the gather cost model and use it to predict where vectorisation will not help.

  <details><summary>Answer</summary>

  §3: gathers and scatters are not really parallel, because the L1
  cache serves only one or two distinct accesses per cycle; "random
  memory accesses have to be excluded from the O(f(n)/W)
  vectorization rule". So a W-lane gather costs about W cache accesses
  and the vector win comes only from the instructions around it. This
  predicts exactly §10.2's result: up to 6× on Xeon Phi's weak in-order
  cores, "a smaller speedup for cache resident hash tables on Haswell",
  and nothing at all once the table exceeds cache.

  </details>

- [ ] You can explain why scatter needs conflict serialisation, and say what the paper actually implemented.

  <details><summary>Answer</summary>

  Two lanes with the same partition digit read the same offset counter
  and scatter to the same address; §3's semantics say the rightmost
  wins, so a row is lost and the counter is short by k-1.

  The paper does **not** use `vpconflictd` — §5.1 names it as future
  AVX-3 hardware unavailable on its machines. §7.3's Algorithm 13 does
  it with the primitives: reverse the lanes, scatter unique per-lane
  values, gather them back, treat mismatches as conflicts, bump those
  lanes' offsets, repeat. Total distinct memory accesses is always W.
  The reversal keeps the partition stable, which LSB radixsort needs.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including your ranking of the four operators.

  <details><summary>Answer</summary>

  Self-check. Question 6 has a defensible shape rather than one right
  answer: selection scan first (pure instruction work, this topic
  measures a 10× branchy/branchless gap), Bloom filter second (§10.3's
  largest speedups, and no gather if the filter is cache-resident),
  hash probe third (Step 5's rule caps it once the table leaves cache),
  partition last on a machine with no scatter — where topic 13's
  write-combining buffers are the real answer.

  </details>

## References

**Papers**
- Orestis Polychroniou, Arun Raghavan, Kenneth A. Ross — "Rethinking
  SIMD Vectorization for In-Memory Databases", *SIGMOD 2015*.
  <https://www.cs.columbia.edu/~orestis/sigmod15.pdf> — §3 for the four
  primitives, their emulation and the gather cost model; §4 for
  selection scans (Algorithms 1-3); §5.1 for vertical hash probing
  (Algorithm 5) and its stability cost; §6 for Bloom filters; §7.3 for
  conflict serialisation (Algorithm 13); Table 1 for the three machines
  and §10.1-§10.5 for the per-operator speedups.

**Code**
- This topic's `experiments/src/filter.rs` — Algorithms 1 and 2 of §4,
  verbatim, plus the NEON rungs you are asked to write.
- `reading-polars-compute.md` — the AVX-512 `vpcompressd`/`vpcompressb`
  the paper forecast, now shipping, and the scalar path your machine
  actually runs instead.
