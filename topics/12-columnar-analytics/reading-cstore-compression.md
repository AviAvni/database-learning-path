# C-Store: operate on compressed data

Two papers out of the same lab, read here as a pair:

- Mike Stonebraker, Daniel Abadi, Adam Batkin, Xuedong Chen, Mitch Cherniack, Miguel
  Ferreira, Edmond Lau, Amerson Lin, Sam Madden, Elizabeth O'Neil, Pat O'Neil, Alex Rasin,
  Nga Tran, Stan Zdonik. *C-Store: A Column-oriented DBMS*. VLDB 2005 — the architecture.
- Daniel Abadi, Samuel Madden, Miguel Ferreira. *Integrating Compression and Execution in
  Column-Oriented Database Systems*. SIGMOD 2006 — the thesis this topic is named for: the
  executor should **operate on** compressed data, not merely store it.

Twenty years on, the value is seeing which of the original bets survived and in what
disguise — and the 2006 paper is unusually good at giving you the *numbers* to score them
with. Before you open either paper, this chapter builds the ideas step by step, then hands
you a reading route through both.

Vocabulary you will need, all defined at the step that introduces them: **row store**,
**column store**, **projection**, **segment**, **storage key**, **join index**,
**run-length encoding**, **bit-vector encoding**, **delta encoding**, **dictionary
encoding**, **null suppression**, **position list**, **bitstring**, **late
materialization**, **eager** vs **lazy decompression**.

---

## The problem in one sentence

A 2005 **row store** — a system that stores all attributes of a tuple consecutively — must
read every column of every row to answer an analytic query that mentions three columns out
of a hundred; C-Store's answer was to store each column separately, sorted, and compressed,
which immediately raised the harder question the 2006 follow-up answers: **once the data is
compressed, must you decompress it to compute on it?**

---

## The concepts, step by step

### Step 1 — The column store, and the four encodings C-Store shipped

> **In:** a table, and a query that mentions 3 of its 100 columns.
> **Out:** the column-store layout, and the 2×2 rule C-Store used to pick an encoding for
> each column.

A **column store** is "one in which each attribute is stored in a separate column, such
that successive values of that attribute are stored consecutively on disk" (Abadi §1) — as
opposed to a row store, where "values of different attributes from the same tuple are
stored consecutively". The first-order win is obvious: read 3 columns, not 100.

The second-order win is the one this whole topic runs on, and Abadi §1 states it plainly:
"Compression ratios are also generally higher in column-stores because consecutive entries
in a column are often quite similar to each other, whereas adjacent attributes in a tuple
are not." A column is *self-similar* — one type, one domain, often sorted — which is
exactly the shape every lightweight encoding feeds on. A row interleaves types and kills
every trick. And §1 adds the structural reason a row store cannot copy this: "In a
row-oriented database, such schemes do not work as well because an attribute is stored as a
part of an entire tuple, so combining the same attribute from different tuples together
into one value would require some way to 'mix' tuples."

C-Store §3.1 then does something worth copying: it makes the encoding choice a **2×2 table**
on two properties of the column — is it sorted by *its own* values (self-order) or by some
other column's (foreign-order), and does it have few or many distinct values?

| | few distinct values | many distinct values |
| --- | --- | --- |
| **self-order** | **Type 1** — RLE triples `(v, f, n)`: value, first position, run count | **Type 3** — **delta encoding**: store each value as its difference from the previous one, block-oriented, with the first value of each block stored whole |
| **foreign-order** | **Type 2** — **bit-vector encoding**: one bitmap per distinct value, marking the positions where it occurs; each bitmap is itself run-length encoded because it is sparse | **Type 4** — leave it uncompressed ("we are still investigating possible compression techniques for this situation") |

Two terms defined by that table. **Run-length encoding (RLE)** replaces a run of identical
values with a single record describing the run — C-Store's is a *triple*, `(4, 12, 7)`
meaning "the value 4 occupies positions 12 through 18". **Bit-vector encoding** turns
`1132231` into three bitmaps: `1100001` for value 1, `0001100` for 2, `0010010` for 3
(Abadi §4.4).

Note what is *not* in C-Store's 2005 table: dictionary encoding, and any heavyweight codec.
Those arrive in the 2006 paper. Type 4 — "leave it alone" — is the honest admission of a
gap, and Steps 5–7 are the paper that fills it.

**Why it matters:** every system in this topic descends from this one storage decision, and
every one of them still asks the same two questions C-Store asked — how sorted, how many
distinct values. DuckDB's analyze pass, BtrBlocks' sampler and ClickHouse's `LowCardinality`
hint are three different mechanisms for filling in the same 2×2.

### Step 2 — Projections: there is no base table

> **In:** a logical table `EMP(name, age, salary, dept)`.
> **Out:** what C-Store actually stores on disk, and the machinery needed to get a row back.

This is the step where most summaries of C-Store go wrong, so read the paper's own sentence
first (C-Store §2): "Whereas most row stores implement physical tables directly and then add
various indexes to speed access, **C-Store implements only projections**." And two sentences
later: "we use the term projection slightly differently than is common practice, as **we do
not store the base table(s) from which the projection is derived**."

A **projection** is "anchored on a given logical table, T, and contains one or more
attributes from this table", plus optionally attributes from other tables reachable by a
chain of n:1 foreign-key relationships — so a projection may be pre-joined. It has the same
number of rows as its anchor table, duplicates retained. Every column of the projection is
stored column-wise and **sorted on a common sort key**, written after a vertical bar:

```
EMP1(name, age | age)
EMP2(dept, age, DEPT.floor | DEPT.floor)
EMP3(name, salary | salary)
DEPT1(dname, floor | floor)
```

Each projection is horizontally cut into **segments**, value-partitioned on the sort key.
Within a segment, every value carries a **storage key** — its ordinal position, 1, 2, 3, …
— which in the read store is "not physically stored, but inferred from a tuple's physical
position in the column".

Now the consequence. If there is no base table, reconstructing a row means stitching
several differently-sorted projections together, and C-Store does that with **join
indexes**: "a collection of `(sid, storage_key)` pairs", one per tuple, mapping each row of
projection T1 to the corresponding row of T2. "An alternative view of a join index is that
it takes T1, sorted in some order O, and logically resorts it into the order O' of T2."

That is the cost that killed the design in its full form. A join index is a permutation with
one entry per row per projection pair — and §7 notes that the tuple mover's merge-out
assigns *new* storage keys in the rebuilt read store, "thereby requiring join index
maintenance". Every projection multiplies storage, every insert lands in every projection,
and every rebuild rewrites the permutations that tie them together.

**Why it matters:** sort order is the enabler for everything downstream — a column sorted or
clustered on the filter key gets long RLE runs and tight min/max ranges. C-Store's mistake
was not the idea but the price: paying for *k* sort orders with *k* full copies plus *k²*
permutations. ClickHouse's `ORDER BY` makes one sort order mandatory and free; its
"projections" feature — literally named after this — buys extra ones lazily, on the merge
machinery it was already paying for (see
[reading-clickhouse-paper.md](reading-clickhouse-paper.md), §3.2).

### Step 3 — WS and RS: an LSM tree, named otherwise

> **In:** a stream of inserts arriving at a store optimized for reads.
> **Out:** the two-structure answer, and the honest caveat attached to its benchmark.

C-Store splits the store in two: **WS**, the writeable store, "efficiently updatable
transactionally", with storage keys explicitly materialized; and **RS**, the read store,
compressed and sorted, storage keys inferred from position. A background **tuple mover**
migrates WS into RS.

The paper does not leave the resemblance implicit — §1: "we use a variant of the
**LSM-tree** concept [ONEI96], which supports a **merge out** process that moves tuples from
WS to RS in bulk by an efficient method of merging ordered WS data objects with large RS
blocks, resulting in a new copy of RS that is installed when the operation completes."

Map it onto topic 4's vocabulary:

| C-Store | LSM |
| --- | --- |
| WS — small, updatable, column-organized | memtable |
| RS — large, compressed, sorted, immutable | sorted runs on disk |
| tuple mover / merge-out process (MOP) | compaction |
| high/low water mark epoch numbers | the visibility snapshot a compaction may drop below |

§7 spells out the old-master/new-master discipline: MOP reads blocks from the RS segment,
drops rows deleted at or before the low water mark, merges in the WS values, writes a new
segment RS′, then "the system cuts over from RS to RS′. The disk space used by the old RS
can now be freed." Immutable inputs, a new output, an atomic swap — ClickHouse's merge, in
2005 clothing. The paper's own justification: "This old-master/new-master approach will be
more efficient than an update-in-place strategy, since essentially all data objects will
move."

And the caveat that any honest reading has to carry. §9: "At the present time, we have a
storage engine and the executor for RS running. We have an **early implementation of the WS
and tuple mover; however they are not at the point where we can run experiments on them**."
§1 says the same: "we have not fully integrated the WS and tuple mover, **whose overhead
may be significant**." So every C-Store number in Step 7's tables is a *read-only* number
from RS alone. The write side of the design was designed, not measured.

**Why it matters:** every read-optimized layout in this book — columnar or graph, including
FalkorDB's `Delta_Matrix` in topic 13 — converges on this same two-structure answer, because
a layout can be write-friendly or read-optimal and not both. What differs between systems is
only who pays for the mover.

### Step 4 — Bitstrings and Mask: late materialization before it had a name

> **In:** a predicate on one column and a `SELECT` list naming three others.
> **Out:** the currency C-Store's operators actually exchange, and the optimizer decision it
> creates.

A **position** is a value's ordinal offset within a column (Abadi §5.1 defines it exactly
that way). A **position list** is a set of them — "rows 17, 204, 9,881 survived" — and a
**bitstring** is the same set as one bit per row.

C-Store's operator set (§8.1) is built around them. Of its ten node types, three matter here:

- **`Select`** "is equivalent to the selection operator of the relational algebra (σ), but
  rather than producing a restriction of its input, instead **produces a bitstring
  representation of the result**."
- **`Mask`** "accepts a bitstring B and projection Cs, and restricts Cs by emitting only
  those values whose corresponding bits in B are 1."
- **`Permute`** "permutes a projection according to the ordering defined by a join index."

Plus `BAnd` / `BOr` / `BNot` for combining bitstrings without touching data. Joins are the
same idea: Abadi §3 — "Joins produce positions rather than values", and §5.2 shows the
output of a join being a pair of position columns which are then "sent to other columns from
the input relations… to extract the values at these positions".

**Late materialization** is the modern name for this discipline: run the filters and joins
on the cheap columns, carry positions through the plan, and fetch the wide payload columns
only for the survivors. Neither paper uses the phrase — C-Store calls it `Mask` placement,
Abadi §6.5 calls it **position filtering**, and Abadi §2 credits the general idea of holding
data compressed in memory to Graefe and Shapiro under the name **lazy decompression**. The
name "late materialization" arrives in the 2007 follow-up (Abadi, Myers, DeWitt, Madden,
*Materialization Strategies in a Column-Oriented DBMS*, ICDE 2007); it is worth knowing that
the idea predates its label by two years.

What makes this a *concept* and not a trick is that C-Store §8.2 turns it into an explicit
optimizer decision: "the optimizer must decide **where in the plan to mask a projection**
according to a bitstring. For example, in some cases it is desirable to push the `Mask` early
in the plan… while in other cases it is best to delay masking until a point where it is
possible to feed a bitstring to the next operator in the plan (e.g., `COUNT`) that can
produce results solely by processing the bitstring."

One implementation detail worth keeping: "C-Store iterators return **64K blocks** from a
single column. This approach preserves the benefit of using iterators… while changing the
granularity of data flow to better match the column-based model" (§8.1). Vectorized
execution, arrived at from the compression side rather than the CPU side.

**Why it matters:** positions are what make Step 5's compressed operators possible at all.
An operator that never assembles a row never has to decode one.

### Step 5 — The 2006 experiment: eager decompression versus direct operation

> **In:** one aggregation query, six encodings, and two executor policies.
> **Out:** the measured gap between decompress-then-process and process-compressed.

The experiment (Abadi §6) is a single-column aggregation:

```sql
SELECT SUM(C) FROM TABLE GROUP BY C
```

over **100 million 32-bit integers**, with the six encodings of §4 — null suppression, LZ,
RLE, bit-vector, dictionary, none — and two policies:

```
 eager decompression:   [decode everything off disk] -> [scan rows]   work per ROW
 direct operation:      [scan runs / codes / bitmaps directly]        work per RUN / CODE
```

The data is generated so that two parameters can be dialled independently: the **number of
distinct values** (2 to 40 in the first set) and the **average sorted run length** (50, 100,
500, 1000). The rationale is worth noting because it is the same clustering argument as Step
2: "if column C is tertiarily sorted and the first column in the projection has 500 unique
values and the second column in the projection has 1000 unique values then C will have
average sorted runs of size 100000000/(500*1000)=200."

The measured result, §6.2, on the data with 1000-record sorted runs — average improvement
from *not* eagerly decompressing:

| Encoding | speed-up from direct operation |
| --- | --- |
| bit-vector | **10.3×** |
| dictionary, group-by-self (multi-value) | **3.94×** |
| RLE | **3.3×** |
| dictionary, value-at-a-time (single-value) | **1.1×** |
| LZ, null suppression | 1.0× — "LZ and NS cannot operate on encoded data" |

And the reason, stated as a complexity claim rather than a speed-up (§6.2): the CPU cost of
the aggregation is proportional to *n*, where *n* is

- `num_tuples` for the uncompressed and row-oriented schemes,
- `num_tuples / avg_run_len` for RLE,
- `num_tuples / dict_entry_size` for dictionary multi-value,
- `num_distinct_values` for bit-vector encoding.

That last line is why bit-vector wins by 10.3×: at 40 distinct values, a `GROUP BY` over
100 million rows becomes 40 popcounts. It is a different complexity class, not a constant
factor.

The paper then runs the same queries **with CPU contention** (§6.2, Figure 6(c)) and finds
that the schemes with executor shortcuts barely degrade, while LZ, null suppression and
value-at-a-time dictionary degrade most — and confirms with performance counters that
"competition for cache lines accounted for less than 2% of the increase in query time", so
it is genuinely CPU cycles. The conclusion drawn is the durable one:

> "while normal compression simply trades 'expensive' I/O time for 'cheap' CPU, operating
> directly on compressed data reduces **both** I/O and CPU cycles. This suggests that even
> on a machine with a much faster I/O or a much slower CPU, compressing data and operating
> directly on it will be beneficial."

The whole thesis fits in one loop — a filtered `SUM` over RLE that never materializes a row:

```rust
// ILLUSTRATION — pseudocode, not quoted from any repo. The paper's own version is
// the Count aggregator in Figure 2 of Abadi et al., SIGMOD 2006. For a real
// implementation of the same idea see duckdb/duckdb@6c0c1a68
// src/storage/compression/rle.cpp:113 (RLEFinalAnalyze) and rle.cpp:99.
struct Run { value: u64, len: u32 }

// decompress-then-process is O(rows); this is O(runs).
fn sum_where_gt(runs: &[Run], threshold: u64) -> u64 {
    let mut sum = 0;
    for r in runs {
        if r.value > threshold {               // predicate: ONCE per run
            sum += r.value * r.len as u64;     // aggregate: multiply, don't decode
        }
    }
    sum
}
```

Note one difference from the paper: C-Store's RLE record is a *triple* `(value, start_pos,
run_length)`, because positions have to be addressable for Step 4's `Mask` to work. DuckDB's
is a pair — `rle.cpp:113-116` sizes a compressed segment as
`(sizeof(rle_count_t) + sizeof(T)) * seen_count` — because DuckDB reconstructs positions by
running prefix sums instead of storing them. Step 6 prices both.

**Why it matters:** compression stops being a storage tax paid back at scan time and becomes
the executor's fast path. That inversion is the reason this topic exists.

### Step 6 — Do the sizes yourself

> **In:** one concrete column — 1,000,000 `INT64` values, 200 distinct, average run 8.
> **Out:** the byte count under each encoding, with the multiplication shown, and the two
> break-evens worth memorising.

Use the same column the rest of this topic uses, so the numbers compose across guides.
1,000,000 values, 200 distinct, average run length 8 ⇒ `1,000,000 / 8` = **125,000 runs**.
Baseline: `1,000,000 × 8 B` = **8,000,000 B**.

**Dictionary encoding** replaces each value with an index into a table of the distinct
values. The code width is `ceil(log2(distinct))` = `ceil(log2(200))` = `ceil(7.64)` = **8
bits**, because 2⁷ = 128 < 200 ≤ 256 = 2⁸. So:

```
codes      1,000,000 × 8 bits = 1,000,000 B
dictionary       200 × 8 B    =     1,600 B
                               -----------
                                1,001,600 B     8,000,000 / 1,001,600 = 7.99x
```

**Run-length encoding**, C-Store's three-field triple `(value: 8 B, start_pos: 4 B,
run_len: 4 B)` = 16 B per run:

```
125,000 runs × 16 B = 2,000,000 B                8,000,000 / 2,000,000 = 4.00x
```

DuckDB's two-field pair `(value: 8 B, count: 4 B)` = 12 B per run:

```
125,000 runs × 12 B = 1,500,000 B                8,000,000 / 1,500,000 = 5.33x
```

Storing the start position costs **33% of the compressed size** — the price of Step 4's
random access into a run, and exactly the trade DuckDB declines.

**Bit-vector encoding**, one bitmap per distinct value:

```
200 values × 1,000,000 bits = 200,000,000 bits = 25,000,000 B    ratio 0.32x
```

Three times *larger* than the raw column. The break-even is worth deriving because Abadi §6.1
states it as an observed fact and it is really arithmetic: bit-vector costs `c` bits per row
for cardinality `c`, against `w` bits per row raw, so it only shrinks when `c < w`. The paper
on 32-bit data: "as soon as the column cardinality is more than 32, type-2 compression is no
longer more compressed than the original 32-bit data." ✓ For our 64-bit column the
break-even is 64, and at 200 distinct values we are `200 / 64` = 3.1× over it. (Which is why
Abadi §4.4 notes their bitmaps are left *un*-further-compressed: "one needs the bit-maps to
be fairly sparse (on the order of 1 bit in 1000) in order for query performance to not be
hindered".)

**The second break-even — position list versus bitstring** (Step 4's currency, and question
4). Over 1,000,000 rows a bitstring costs `1,000,000 / 8` = **125,000 B**, flat, whatever the
selectivity. A position list at 4 B per surviving row costs `4 × s`. Setting them equal:

```
4 x s = 125,000   =>   s = 31,250 rows = 3.125% selectivity
```

At 1% (10,000 survivors) the list is `4 × 10,000` = 40,000 B — **3.1× smaller** than the
bitstring. At 10% (100,000 survivors) it is 400,000 B — **3.2× larger**. Below ~3%, ship
positions; above, ship bits. That single crossover explains why C-Store carries both
representations and why Abadi §5.2's `isPosContig()` property exists at all.

**One more, from the paper's own §4.2.** C-Store's dictionary packs several codes into a
machine word and keeps entries **byte-aligned**, choosing 1, 2, 3 or 4 bytes per entry "by
requiring the dictionary to fit in the L2 cache". Their worked example: 32 values ⇒ 5-bit
codes ⇒ 1 code fits in 1 byte, 3 in 2 bytes, 4 in 3 bytes, 6 in 4 bytes; picking 3-per-2-bytes
makes the dictionary `32³` = **32,768 entries** = **524,288 B**, "which is half of the L2
cache on our machine (1MB)". Run the same rule on our column: 8-bit codes pack exactly 1 per
byte with no waste, and a 2-codes-per-entry dictionary would need `200²` = 40,000 entries
(640,000 B — borderline), while 3 codes per entry needs `200³` = 8,000,000 entries, hopeless.
The cache, not the bit width, is what caps the trick.

**Why it matters:** every ratio in Step 7's tables is one of these five multiplications with
different inputs. Doing them once means you can predict the paper's results before reading
them — and catch the ones that do not follow.

### Step 7 — Lightweight versus heavyweight, proven, and the decision tree

> **In:** the same aggregation, run across the cardinality × run-length grid.
> **Out:** the 2×2 result table, the condemnation of heavyweight codecs on the scan path,
> and the heuristic the paper distilled.

Abadi §6.3's summary table — aggregation query times in seconds, best in **bold**. "High"
and "low" cardinality are 10,000 and 37 distinct values; "runs" means average run length 14.

| Data | RLE | LZ | Dictionary | Bit-vector | No compression |
| --- | --- | --- | --- | --- | --- |
| no runs, low cardinality | 17.67 | 9.30 | **7.49** | 12.02 | 10.86 |
| runs, low cardinality | **2.43** | 3.93 | 3.29 | 9.83 | 7.59 |
| no runs, high cardinality | 32.48 | 15.05 | **11.25** | N/A | 13.31 |
| runs, high cardinality | **2.56** | 4.48 | 4.56 | N/A | 9.52 |

Read the columns, not the rows. RLE swings from **worst** (32.48 s) to **best** (2.56 s) on
the same cardinality purely because runs appeared — "for RLE and LZ, run-length is a better
indicator of performance than cardinality". Dictionary is the safe default when there is no
locality. Bit-vector is unusable above ~40 distinct values. And LZ is never best in any of
the four cells.

That is the condemnation of heavyweight codecs on the scan path, and §6.2 gives the reason:
"since LZ and NS cannot operate on encoded data, their performance for these experiments was
identical" to the eager-decompression case. There is no "sum a gzip block" shortcut; you must
inflate first. The conclusion (§7) states the trade as a recommendation: "**Sacrificing the
compression ratio of heavy-weight schemes for the efficiency light-weight schemes in
operating on compressed data is a good trade-off to make.**"

The join experiment (§6.5) is the most dramatic number in the paper. A foreign-key join with
predicates on both sides, times in seconds:

| Encoding of the fact-table join column | 50 distinct keys | 50,000 distinct keys |
| --- | --- | --- |
| RLE | **0.06** | **0.07** |
| Bit-vector | 0.97 | N/A |
| Dictionary | 3.15 | 3.86 |
| No compression | 4.08 | 4.3 |

`4.08 / 0.06` = **68×** at 50 keys, `4.3 / 0.07` = **61×** at 50,000. The mechanism is Step
4's: an RLE run joins once and emits a whole position range, so the join does work per *run*
where the uncompressed plan does work per *row*.

§6.5 also carries the finding that most summaries drop. The same query with the two columns'
roles swapped (Figure 9(b)) makes bit-vector encoding go from fastest to catastrophically
slow, "because the query requires the values of the bit-vector column **in position order**
which forces decompression". The lesson the authors draw: "the proper choice of encoding type
for a column depends not just on data characteristics, but also on **the expected query
workload**… It also indicates that redundantly storing the same column in the same sort order
using different compression schemes might be a good idea."

Figure 10 distils all of it into a decision tree, whose two non-obvious predicates are worth
memorising: "**exhibits good locality**" means the column is a sort column, correlated with
one, or otherwise repetitive; "**likely to be used in a position contiguous manner**" means
it must be read in parallel with another column — true for a `SELECT`-list column read
through a sorted position list, false for a column that only appears in the `WHERE` clause.

**Why it matters:** this 2006 finding is Parquet's two compression layers (semantic encoding,
then optional block codec) and DuckDB's zstd-as-last-resort, decided twenty years in advance.
It is also the reason `FINDINGS.md` row 12's benchmarks measure *scan* rates rather than
storage ratios: the ratio is not the figure of merit if the executor has to inflate.

### Step 8 — Three booleans instead of n² operators

> **In:** an executor with *n* encodings and a set of binary operators.
> **Out:** the abstraction that stopped the combinatorics, and its modern descendant.

Abadi §5.2 states the engineering problem before the solution: "Every time a new compression
scheme is added to the system, all operators that operate directly on this type of data have
to be supplemented to handle the new scheme. Without careful engineering, there would end up
being **n versions of each operator** – one for each type of compression scheme that can be
input to the operator. Operators that take two inputs (like joins) would need **n²
versions**."

Figure 1's nested-loop join pseudocode makes it vivid, ending with the line "etc. etc. for
every possible combination of encoding types".

The fix is a **compressed block API** (Table 1) with three groups of methods:

| Properties | Iterator access | Block information |
| --- | --- | --- |
| `isOneValue()` | `getNext()` | `getSize()` |
| `isValueSorted()` | `asArray()` | `getStartValue()` |
| `isPosContig()` | | `getEndPosition()` |

Only **three** properties, and — this is the point — none of them names an encoding. An
operator never asks "is this RLE?"; it asks "does this block hold one value at many
positions?" The properties table (§5.2):

| Encoding | sorted? | one value? | position contiguous? |
| --- | --- | --- | --- |
| RLE | yes | yes | yes |
| bit-string | yes | yes | **no** |
| null suppression | data-dependent | no | yes |
| Lempel-Ziv | data-dependent | no | yes |
| dictionary | data-dependent | no | yes |
| uncompressed | data-dependent | no | data-dependent |

RLE and bit-vector differ in exactly one bit of that table — `isPosContig()` — and that single
bit is what selects between Figure 1's second and third optimization. The paper says so:
those optimizations work "in general, not just for RLE" and "in general, not just for
bit-vector compression".

The payoff is the `Count` aggregator of Figure 2, which is four lines and knows nothing about
encodings: if `isOneValue()`, add `getSize()` to that value's counter; otherwise fall back to
`asArray()` and iterate. "Note that despite RLE and bit-vector encoding being very different
compression techniques, the pseudocode in Figure 2 need not distinguish between them, pushing
the complexity of calculating the block size into the compressed block code."

**Why it matters:** this is the difference between a benchmark paper and an architecture. The
API is what let the idea survive into production — DuckDB's vector types (`FLAT`, `CONSTANT`,
`DICTIONARY`, `FSST`; topic 11) are the same three questions asked with different names,
`CONSTANT` being `isOneValue()` and the selection vector being the negation of
`isPosContig()`.

### Step 9 — Which GB/s? Twenty years of hardware, one surviving conclusion

> **In:** the 2006 machine's disk rate and this topic's measured scan floor.
> **Out:** the factor between them, and why the paper's conclusion still holds anyway.

Abadi §6 gives the hardware: "a 3.0 GHz Pentium IV, running RedHat Linux, with 2 Gbytes of
memory, 1MB L2 cache, and 750 Gbytes of disk. **The disk can read cold data at 50-60
MB/sec.**"

That number explains the shape of every graph in the paper. The uncompressed column is
`100,000,000 × 4 B` = 400 MB; at 55 MB/s that is `400 / 55` = **7.3 seconds of pure I/O**,
and the measured no-compression times sit at 7.59–13.31 s. In 2006, a scan was an I/O
problem with a CPU epilogue.

Now put this topic's own measurement beside it. `FINDINGS.md` row 12 records a scan floor of
**24–57 GB/s on a machine with roughly 150 GB/s of memory bandwidth**. Against the paper's
disk:

```
57 GB/s / 55 MB/s = 57,000 MB/s / 55 MB/s = ~1,036x
```

Three orders of magnitude. Every premise of the 2006 experiment has moved — and the
conclusion did not, because the paper's own §6.2 anticipated exactly this: "even on a machine
with a much faster I/O or a much slower CPU, compressing data and operating directly on it
will be beneficial." The reason is the complexity argument, not the bandwidth one: shrinking
`num_tuples` to `num_tuples / avg_run_len` is an algorithmic win that survives any change in
the constant.

Which brings the discipline this topic keeps insisting on: **say which bytes you counted.**
Our Step 6 column compresses 7.99× under dictionary encoding, so 8 MB of logical `INT64`
values live in 1,001,600 B on disk. Report the same scan two ways:

```
physical: 1,001,600 B read           logical: 8,000,000 B processed
at 24 GB/s logical  =>  3.0 GB/s of compressed bytes actually moved
at 3.0 GB/s physical =>  24 GB/s of "effective" throughput
```

Both sentences describe one query. Neither is wrong; a report that omits which one it means
is. And `FINDINGS.md` row 12 preserves this topic's cautionary tale for the case where the
arithmetic is not just ambiguous but impossible: a hoisted timing loop once printed
**19,047,619 GB/s**, which is `19,047,619 / 150` ≈ **127,000×** the machine's peak memory
bandwidth. A throughput above the hardware's ceiling is never a result.

**Why it matters:** it is the reason to re-derive the 2006 findings on your own machine
rather than quote them. The ranking survived; the absolute numbers did not, and this topic's
benchmarks exist to replace them.

---

## How to read the papers (with the concepts in hand)

Budget about three hours for the pair. **Read the 2006 paper first** if you only read one —
it is the one with the thesis and the numbers; C-Store 2005 is the architecture it extends.

**C-Store (VLDB 2005)** — read for the bets, and score them against twenty years of history:

| C-Store bet | §  | survived as |
| --- | --- | --- |
| columns, not rows, for reads | 2, 3 | everything in this topic |
| **only** projections — no base table, several sort orders, join indexes to reassemble | 2 | mostly died: the storage and the permutation maintenance. Echoes in ClickHouse's mandatory `ORDER BY` and its lazily-populated "projections" feature, which is literally named after this |
| four encodings picked by (self/foreign order) × (few/many distinct values) | 3.1 | DuckDB's analyze-and-score, BtrBlocks' sampler — the mechanism changed, the two questions did not |
| WS / RS split with a tuple mover, "a variant of the LSM-tree concept" | 1, 4, 7 | delta + main (SAP HANA), parts + merges (ClickHouse) — and never benchmarked in this paper |
| `Select` → bitstring, `Mask` placement as an optimizer decision | 8.1, 8.2 | DuckDB selection vectors, Parquet late decode, every late-materialization plan since |
| K-safety through redundant overlapping projections instead of RAID | 1, 6.3 | died; replication won |
| snapshot isolation to avoid 2PC and locking for queries | 1, 6.1 | survived everywhere, including ClickHouse §3.7's versioned parts |

Read §2 (data model) and §3.1 (encodings) carefully — Steps 1–2. Read §7 (tuple mover) —
Step 3. Read §8.1's operator list — Step 4; it is one page and it is the whole idea. Skim
§6.1–6.3 (snapshot isolation, locking, recovery) and §5 (grid allocation) unless you have a
specific interest. Read §9's first paragraph before its tables, so the read-only caveat is in
place before the 164× lands.

**SIGMOD 2006** — read the experiment design, then internalise the findings:

1. **§4** — the six schemes, and *how* each is implemented. §4.2's byte-alignment argument is
   the surprising one: "column stores are so I/O efficient that even a small amount of
   compression is enough to make queries on that column become CPU-limited", so they
   deliberately waste bits to save shifts.
2. **§5.1–5.2** — Step 8. Table 1 and the properties table are the two things to copy into
   `notes.md`.
3. **§6.1 vs §6.2** — the same experiment twice, eager then direct. The gap between the two
   figures *is* the paper.
4. **§6.3's summary table and §6.5's join table** — Step 7. These are the numbers to cite.
5. **§7 and Figure 10** — the decision tree, and the three closing observations. The third
   one — "cost models that only take into account I/O costs will likely perform poorly in the
   context of column-oriented systems since CPU cost is often the dominant factor" — is the
   sentence the next twenty years of the field spent proving.

Compare the graphs of speed-up versus average run length against your own `scan_bench`
numbers from this topic's `experiments/` crate. They will not match; the ranking should.

---

## Questions for notes.md

1. **`SUM` over RLE runs is O(runs).** Which *other* aggregates stay run-shortcuttable and
   which break? Work through `MIN`/`MAX` (what does `isValueSorted()` buy — Abadi §5.2's
   Figure 3 says "finding the maximum or minimum value in a sorted block is a single
   operation"), `COUNT` (Figure 2's aggregator), `AVG`, then `COUNT DISTINCT` and `MEDIAN`.
   For each, say whether the shortcut needs `isOneValue()`, `isValueSorted()`, both, or
   whether no block property saves you.
2. **Projections died of write amplification and join-index maintenance.** ClickHouse revives
   them with the merge machinery paying the cost — what changed? Name the two specific
   mechanisms in the ClickHouse paper's §3.2 (lazy population from newly inserted parts only;
   the optimizer choosing per part on estimated I/O cost) and say which of C-Store's two
   costs each one addresses. Does either remove the need for join indexes, or did ClickHouse
   sidestep that by never splitting the base table?
3. **WS/RS + tuple mover is an LSM with different names** — the paper says so itself in §1.
   Map the four components onto topic 4's vocabulary, then find the one place the analogy
   fails: what does C-Store's low-water-mark epoch do that an LSM compaction's sequence
   numbers do not?
4. **Position lists versus bitstrings** for intermediate results: derive the crossover for
   your own row count and pointer width, as Step 6 does for 1 M rows and 4-byte positions
   (3.125%). Then connect it to your topic 11 select-vs-compact question — is it the same
   crossover?
5. **M12.** `WHERE n.country = 'IL'` on a dictionary-encoded property column of 1 M nodes.
   Write the process-compressed plan — code lookup, integer compare, positions out — and
   count the decodes at 1% selectivity, against the decompress-then-process plan. Then say
   which of Abadi §5.2's three properties your plan relied on.

---

## Takeaway

The 2006 thesis in one sentence: **expose the properties of compressed blocks to operators,
execute per run and per code, and decode the losers never.**

The 2005 architecture in one more: store only sorted, compressed projections, reassemble rows
from join indexes, absorb writes in a small updatable store, and move them to the read store
in bulk.

Score them separately, because they aged differently. The compression thesis is now
universal — Parquet, DuckDB, ClickHouse and BtrBlocks all implement it, and BtrBlocks in 2023
is still filling in C-Store's Type 4 cell. The architecture half is more mixed: the WS/RS
split won under other names, projections lost on cost, and join indexes disappeared entirely
because everyone else kept the base table.

The transferable habit is the properties API. Two decades of encodings — FSST, FastPFOR,
Roaring, Pseudodecimal — have been added to column stores since, and none of them required a
new join operator, because the 2006 paper made operators depend on three booleans instead of
on an encoding list.

---

## Done when

Answer each before unfolding it.

- [ ] State the SIGMOD '06 thesis in one sentence, and say what the alternative was called.

<details><summary>Answer</summary>

*Expose the properties of compressed blocks to operators, execute per run and per code, and
decode the losers never.*

The alternative is **eager decompression** — the classical design in which "data would be
compressed on disk and then eagerly decompressed upon being read into memory… everything read
into memory had to be decompressed whether or not it was actually used" (Abadi §2). The
intermediate position, credited there to Graefe and Shapiro and to MonetDB/X100, is **lazy
decompression**: keep it compressed in memory, decode only what an operator actually needs.
The 2006 paper's contribution is the step past that — for RLE, bit-vector and dictionary
data, many operators need never decode at all.

§6.2 measures the gap on 1000-record sorted runs: **10.3×** for bit-vector, **3.94×** for
group-by-self dictionary, **3.3×** for RLE, **1.1×** for value-at-a-time dictionary, and
nothing at all for LZ and null suppression, which "cannot operate on encoded data".

</details>

- [ ] C-Store stores "only projections". Say what that costs, and name the structure that
      pays the cost.

<details><summary>Answer</summary>

§2: "C-Store implements only projections… we do not store the base table(s) from which the
projection is derived." A projection is anchored on one table, may pre-join columns from
others through n:1 foreign keys, has the same row count as its anchor, and is sorted on a
declared key.

The cost is reassembly. With no base table, answering a query that needs columns from two
differently-sorted projections means resorting one into the other's order, and the structure
that does it is the **join index**: "a collection of `(sid, storage_key)` pairs", one entry
per row, per projection pair. §2 describes it as taking "T1, sorted in some order O, and
logically resort[ing] it into the order O′ of T2".

That is a permutation per pair of projections, and it is not static: §7's merge-out process
assigns new storage keys in the rebuilt read store, "thereby requiring join index
maintenance". So `k` sort orders cost `k` copies of the data *plus* the permutations tying
them together *plus* rewriting those permutations on every merge. That is what did not
survive — ClickHouse keeps a base table and buys extra sort orders as optional, lazily
populated projections instead.

</details>

- [ ] Bit-vector encoding beat every other scheme by 10.3× in one experiment and was
      catastrophically slow in another. Explain both.

<details><summary>Answer</summary>

It wins when the query wants **a set of positions**, and loses when the query wants **values
in position order**.

The win (§6.2): for a `GROUP BY` on a column with `c` distinct values, the aggregation cost is
proportional to `num_distinct_values`, not `num_tuples` — a `COUNT` per group is the size of
one bitmap. At 40 distinct values over 100 million rows that is a different complexity class,
hence 10.3×. §6.5 extends it to predicates: bit-vector encoding "is already storing the result
of the predicate as it already contains a position list for each unique value in the column",
so an equality predicate is a projection, not a scan.

The loss (§6.5, Figure 9(b)): reverse the roles so the bit-vector column is the one being
*position filtered* for its values, and "the query requires the values of the bit-vector
column in position order which forces decompression, which has already been shown to be
slow". Reading the *i*-th value means consulting every bitmap.

The sizing constraint is separate and just as fatal: `c` bits per row versus `w` bits raw
means it only shrinks below cardinality `w` — "as soon as the column cardinality is more than
32, type-2 compression is no longer more compressed than the original 32-bit data" (§6.1).

The authors' own conclusion is the one to keep: "the proper choice of encoding type for a
column depends not just on data characteristics, but also on the expected query workload."

</details>

- [ ] The properties API has exactly three predicates. Name them, and say why none of them
      mentions an encoding.

<details><summary>Answer</summary>

`isOneValue()` — the block holds a single value at many positions. `isValueSorted()` — the
block's values are sorted (trivially true when there is one value). `isPosContig()` — the
block covers a consecutive range of the column.

They avoid naming encodings because naming them is what causes the combinatorial explosion
§5.2 opens with: "there would end up being n versions of each operator… Operators that take
two inputs (like joins) would need n² versions."

The reason three booleans suffice is that the optimizations were never really *about* the
encodings. RLE and bit-vector both "encoded multiple positions for the same value" — RLE
consecutively, bit-vector not — so both admit the same shortcut, differing in exactly one
predicate, `isPosContig()`. Figure 3's optimizations are stated accordingly: they work "in
general, not just for RLE" and "in general, not just for bit-vector compression". Figure 2's
`Count` aggregator branches on `isOneValue()` alone and handles both.

The practical test of the abstraction is that FSST, FastPFOR, Roaring bitmaps and
Pseudodecimal have all been added to column stores since 2006 without anyone writing a new
join operator.

</details>

- [ ] The 2006 machine read cold data at 50–60 MB/s. Compute the factor against this topic's
      measured scan floor, and say why the paper's conclusion survives it.

<details><summary>Answer</summary>

`FINDINGS.md` row 12 records a scan floor of **24–57 GB/s** on a machine with roughly
**150 GB/s** of memory bandwidth. Against the paper's disk: `57 GB/s / 55 MB/s` = `57,000 /
55` ≈ **1,036×**. Three orders of magnitude, and the bottleneck moved from disk to memory
bandwidth on the way.

The conclusion survives because it was never a bandwidth argument. §6.2 states the mechanism
as a complexity claim — aggregation cost is proportional to `num_tuples` uncompressed, but to
`num_tuples / avg_run_len` for RLE, `num_tuples / dict_entry_size` for multi-value dictionary
and `num_distinct_values` for bit-vector. Dividing the work by the run length is an
algorithmic win, immune to changes in the constant. The authors said so explicitly: "even on
a machine with a much faster I/O or a much slower CPU, compressing data and operating
directly on it will be beneficial."

What did *not* survive is every absolute number, which is why this topic re-measures rather
than quotes. And the corollary discipline: always say whether a GB/s figure counts compressed
bytes moved or logical bytes processed — at our Step 6 column's 7.99× ratio the same scan is
honestly describable as 3.0 GB/s or 24 GB/s. When it is describable as 19,047,619 GB/s —
about 127,000× a 150 GB/s bus — it is a hoisted loop, not a discovery.

</details>

---

## References

**Papers**

- Mike Stonebraker et al. *C-Store: A Column-oriented DBMS*. VLDB 2005.
  <https://web.stanford.edu/class/cs245/readings/c-store.pdf>
  Read §2 (data model), §3.1 (encodings), §7 (tuple mover), §8.1–8.2 (operators and
  optimizer), §9 (results — with its read-only caveat). Skim §5, §6.
- Daniel Abadi, Samuel Madden, Miguel Ferreira. *Integrating Compression and Execution in
  Column-Oriented Database Systems*. SIGMOD 2006.
  <https://www.cs.umd.edu/~abadi/papers/abadi-sigmod06.pdf>
  Read §4 (the six schemes), §5 (the compressed block API), §6.1–6.5 (the experiments),
  §7 + Figure 10 (the decision tree).
- Daniel Abadi, Daniel Myers, David DeWitt, Samuel Madden. *Materialization Strategies in a
  Column-Oriented DBMS*. ICDE 2007 — where "late materialization" gets its name. Optional;
  Step 4 has what you need.

**Code**

- [duckdb/duckdb](https://github.com/duckdb/duckdb) @ `6c0c1a68` —
  `src/storage/compression/rle.cpp:113` sizes an RLE segment as
  `(sizeof(rle_count_t) + sizeof(T)) * seen_count`, the two-field pair to C-Store §3.1's
  three-field triple. Covered properly in
  [reading-duckdb-compression.md](reading-duckdb-compression.md).

**In this topic**

- [reading-clickhouse-paper.md](reading-clickhouse-paper.md) — projections, twenty years
  later and lazily populated (§3.2), for question 2
- [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md) — what finally filled C-Store's
  Type 4 cell, "many distinct values, foreign order"
- [reading-duckdb-compression.md](reading-duckdb-compression.md) — the properties API as
  shipped, and who picks the encoding
- `FINDINGS.md` row 12 — the measured scan floor (24–57 GB/s on a ~150 GB/s machine) and the
  19,047,619 GB/s hoisted-loop bug
