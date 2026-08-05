# ART: sorted like a tree, probed like a hash table

A radix tree tuned until it beats a chained hash table on lookups *while
staying sorted*. Where rax spends its design budget on memory, ART spends it
on lookup latency: four inner-node layouts, each picking the cheapest search
its density allows. It is also where this topic's SwissTable and radix-tree
threads literally meet, in Node16's SSE probe. This chapter builds the paper's
ideas one at a time — the sparse-node waste, the four layouts, the two
collapsing tricks, the key encoding that makes it universal, and the space
proof — then routes you through the sections.

The paper is **Viktor Leis, Alfons Kemper, Thomas Neumann, "The Adaptive Radix
Tree: ARTful Indexing for Main-Memory Databases", ICDE 2013**, 12 pages,
[PDF](https://db.in.tum.de/~leis/papers/ART.pdf). Every number below is
followed by the section, figure or table it came from; if a claim here has no
such tag, treat it as this guide's own arithmetic and check it. Budget ~2 h.

The paper's own system is **HyPer** (§V-D). ART is not a museum piece: DuckDB
ships it as one of its two built-in index types, "mainly used to ensure primary
key constraints and to speed up point and very highly selective (i.e., < 0.1%)
queries"
([DuckDB docs, *Indexes*](https://duckdb.org/docs/current/sql/indexes.html)).

## The problem in one sentence

A radix tree that branches on a full byte needs a 256-entry pointer array per
inner node — 2064 bytes with a 16-byte header and 8-byte pointers (Table I) —
while a real node often holds a handful of children, so a naive main-memory
radix index spends almost all of its space on null pointers; shrink the span
instead and the tree gets taller, which is the trade §III-B calls "excessive"
in one direction and slow in the other.

## The concepts, step by step

### Step 1 — the tension: span buys height and costs memory

> **In:** the radix-tree idea from the rax chapter — spell the key, one branch
> per chunk, no comparisons.
> **Out:** the span parameter *s*, the height formula it controls, the
> exponential space cost it carries, and the exact key count above which a
> radix tree is shorter than a perfect binary search tree.

§III-A defines the knob. An inner node is "an array of 2ˢ pointers"; during
traversal an *s*-bit chunk of the key indexes that array, "and thereby
determines the next child node without any additional comparisons". The
parameter *s* is the **span**, and it fixes the height:

> "A radix tree storing k bit keys has ⌈k/s⌉ levels of inner nodes. With 32 bit
> keys, for example, a radix tree using s = 1 has 32 levels, while a span of 8
> results in only 4 levels." — §III-A

So span is pure height leverage — and pure space cost, because the node is
2ˢ pointers wide whether or not the children exist. §III-B: "Space usage can be
excessive when most child pointers are null", illustrated by Figure 3, which
plots height against space for 1M uniformly distributed 32-bit integers and
shows the space axis running from 32 MB to 32 GB as *s* goes from 1 to 32.
Real systems pick a middle value: the Generalized Prefix Tree uses s = 4, the
Linux kernel radix tree s = 6 (§III-B).

§III-A also gives the comparison against comparison-based trees, and it is
worth doing the arithmetic rather than reading past it. A perfect BST has
height log₂ n; a radix tree has height k/s; they are equal when
n = 2^(k/s), and the paper states radix trees are shorter "for n > 2^(k/s)":

```
 32-bit keys, s = 8:  height 32/8 = 4      crossover n = 2^4  = 16 keys
 64-bit keys, s = 8:  height 64/8 = 8      crossover n = 2^8  = 256 keys
```

Two hundred and fifty-six. Above a few hundred 64-bit keys, a byte-wise radix
tree is already shorter than any balanced binary tree can ever be, and it stops
growing entirely. That is the whole motivation, in one division.

ART's move is to keep s = 8 — "This choice also has the advantage of
simplifying the implementation, because bytes are directly addressable which
avoids bit shifting and masking operations" (§III-C) — and make the node's
*physical* size adapt to how many children it actually has.

### Step 2 — the four node types: one logical node, four layouts

> **In:** a logical inner node that maps up to 256 key bytes to children, and
> a child count that varies wildly across a real tree.
> **Out:** four concrete layouts with their capacity ranges and byte sizes
> from Table I, and the growth/shrink rule between them.

§III-C names four data structures "according to their maximum capacity", and
Table I gives their sizes under the paper's stated assumptions — a **16-byte
header** storing node type, child count and compressed path, and **8-byte
pointers**:

```
 Table I (§III-G) — SUMMARY OF THE NODE TYPES (16 BYTE HEADER, 64 BIT POINTERS)

 Type      Children   Space (bytes)
 Node4       2-4      16 +   4 +  4·8 =   52
 Node16      5-16     16 +  16 + 16·8 =  160
 Node48     17-48     16 + 256 + 48·8 =  656
 Node256    49-256    16 +      256·8 = 2064
```

Read the arithmetic column, not just the totals — each one tells you the
layout. Node4 and Node16 are "one key part and one pointer part" (§III-C):
`n` key bytes plus `n` pointers, keys sorted and at corresponding positions.
Node48's `256` is not keys, it is a **256-entry index array**: "a 256-element
array is used, which can be indexed with key bytes directly … this array stores
indexes into a second array which contains up to 48 pointers. This indirection
saves space in comparison to 256 pointers of 8 bytes, because the indexes only
require 6 bits (we use 1 byte for simplicity)" (§III-C). Node256's `256·8` is
the plain pointer array with no keys at all.

Note Node4's minimum is **2**, not 1. That is not arbitrary: Step 4's path
compression guarantees "each inner node has at least two children" (§III-E),
and Step 6's proof leans on it.

Nodes change type in place as they fill or empty: "When the capacity of a node
is exhausted due to insertion, it is replaced by a larger node type.
Correspondingly, when a node becomes underfull due to key removal, it is
replaced by a smaller node type" (§III-B). Figure 9 shows this as `if
isFull(node) grow(node)` on lines 31-32 of the insert pseudocode.

**Work the saving.** A node with 4 children stored as a Node256 wastes 252 of
its 256 pointers:

```
 null pointers:  252 / 256              = 98.44 % of the array
 wasted bytes:   252·8 / 2064 = 2016/2064 = 97.67 % of the node
 Node256 / Node4:      2064 / 52          = 39.7× larger
```

Thirty-nine point seven times. That factor, applied to the many sparse nodes a
real key distribution produces, is what §III-B means by "excessive".

### Step 3 — one search strategy per layout

> **In:** the four layouts from Step 2 and a key byte to find.
> **Out:** four different `findChild` implementations — loop, SIMD, double
> indirection, direct index — and the reason the choice is per-node rather
> than global.

Figure 8 is the whole point of the paper compressed into 21 numbered lines. The
paper's own pseudocode:

```
 Fig. 8 (§III-F) — findChild(node, byte), abridged; line numbers are the paper's

  1  if node.type==Node4                       // simple loop
  2    for (i=0; i<node.count; i=i+1)
  3      if node.key[i]==byte
  4        return node.child[i]
  ...
  6  if node.type==Node16                      // SSE comparison
  7    key=_mm_set1_epi8(byte)
  8    cmp=_mm_cmpeq_epi8(key, node.key)
  9    mask=(1<<node.count)-1
 10    bitfield=_mm_movemask_epi8(cmp)&mask
 11    if bitfield
 12      return node.child[ctz(bitfield)]
 ...
 15  if node.type==Node48                      // two array lookups
 16    if node.childIndex[byte]!=EMPTY
 17      return node.child[node.childIndex[byte]]
 ...
 20  if node.type==Node256                     // one array lookup
 21    return node.child[byte]
```

Line 21 is the ideal: a node with enough children that the key byte *is* the
index, no search at all. Lines 15-17 are the fallback that keeps the 256-entry
lookup while paying only 1 byte per slot instead of 8. Lines 6-12 are the SSE
group probe: broadcast the search byte (7), compare against all 16 stored key
bytes at once (8), mask off the unused entries because "the node may have less
than 16 valid entries" (9-10), and turn the bitfield into an index with count
trailing zeros (12). Lines 1-4 are a plain loop, because "a Node4 has only 2-4
entries" (§III-F).

Lines 6-12 are the same instruction sequence this topic's `reading-hashbrown.md`
reads in `src/control/group/sse2.rs`: `_mm_set1_epi8`, `_mm_cmpeq_epi8`,
`_mm_movemask_epi8`, then trailing zeros. Two structures, two decades of
literature apart, converge on one 16-byte compare — but they consume the result
differently, and that difference is worth holding: ART's key bytes are
**unique**, so at most one bit is set and `ctz` gives the answer directly;
hashbrown's control bytes are 7-bit *hashes*, so several bits may be set and
each is a candidate to verify. ART gets an index; hashbrown gets a candidate
list. Same instruction, different contract.

The paper also notes the fallback for portability: "Alternatively, binary
search can be used if SIMD instructions are not available" (§III-F). Compare
`rax.c:481-483`, which argues a *linear* scan beats binary search "even when
`h->size` is large" — rax and ART reach opposite conclusions about the same
inner loop because they are optimising different corners.

### Step 4 — lazy expansion and path compression: kill the boring levels

> **In:** a tree whose height is still the key length in bytes, because every
> byte gets a level whether or not it distinguishes anything.
> **Out:** two independent height reductions from §III-E, the pessimistic /
> optimistic choice for storing skipped bytes, and ART's actual hybrid — which
> the previous version of this chapter had backwards.

§III-E, titled *Collapsing Inner Nodes*, introduces two techniques:

- **Lazy expansion** — "inner nodes are only created if they are required to
  distinguish at least two leaf nodes". Figure 6 shows it saving two inner
  nodes by truncating the path to the leaf "FOO". The catch is stated in the
  same paragraph: "because paths to leaves may be truncated, this optimization
  requires that the key is stored at the leaf or can be retrieved from the
  database". Figure 7's search pseudocode handles it at line 4,
  `leafMatches(node, key, depth)`.
- **Path compression** — "removes all inner nodes that have only a single
  child", exactly rax's `iscompr`. The removed bytes still have to be dealt
  with, and §III-E gives two approaches:
  - **Pessimistic**: store a variable-length partial key vector at each inner
    node holding the bytes of the removed one-way nodes, and compare it against
    the search key before descending.
  - **Optimistic**: store only the *count* of removed nodes, skip that many
    bytes without comparing, and compare the full key once at the leaf to
    catch a "wrong turn".

Now the sentence to get right, because it is easy to invert:

> "We therefore use a hybrid approach by storing a vector at each node like in
> the pessimistic approach, but with a constant size (8 bytes) for all nodes.
> Only when this size is exceeded, the lookup algorithm dynamically switches to
> the optimistic strategy." — §III-E

ART is **pessimistic by default**, with a fixed 8-byte prefix in the header,
and falls back to **optimistic** only when a compressed path is longer than 8
bytes. The direction matters: pessimistic means "compare as you go and never
be wrong"; optimistic means "skip and verify at the leaf". ART pays 8 bytes of
every header to stay in the safe mode for the common case. The paper's stated
reason for the cap is the same one rax answers differently: the optimistic
approach "requires one additional check, while the pessimistic method uses more
space, and has variable sized nodes leading to increased memory fragmentation".
Fixed-size nodes are non-negotiable in ART; rax took exactly the other side of
that trade with its arbitrary-length runs.

Both approaches share one guarantee that Step 6 needs: "Both approaches ensure
that each inner node has at least two children."

How much does this buy? §V-D measured it on TPC-C indexes (Figure 17): "the
height of index 3 would be 40 without any optimizations. Path compression and
lazy expansion reduce the average height to 8.1." Index 3 is a
`int,int,varchar(16),varchar(16),TID` compound key (Table IV) — 40 bytes of key
collapsed to about 8 levels.

### Step 5 — binary-comparable keys: the encoding that makes it universal

> **In:** a structure that iterates in byte-lexicographic order, and data types
> whose byte representation does not sort the way the type does.
> **Out:** the formal definition of a binary-comparable key and the per-type
> transformations, one of which you have already read in redis.

Section IV is a whole section for a reason: without it, ART's sortedness is
useless for anything but ASCII. The paper's definition:

> A transformation t : D → {0,1,…,255}^k produces binary-comparable keys if,
> for all x, y ∈ D:  x < y ⇔ memcmp_k(t(x), t(y)) < 0, and likewise for > and
> =. — §IV-A

And the transformations (§IV-B):

| Type | Transformation |
|------|----------------|
| Unsigned integers | already ordered; **byte-swap on little-endian machines** so bytes run most- to least-significant |
| Signed integers | flip the sign bit — `x XOR 2^(b−1)` — then store as unsigned |
| IEEE 754 floats | classify into 10 non-overlapping classes (±normalised, ±denormalised, NaN, ±∞, 0), compute a rank, store as unsigned; "3 if statements, 1 integer multiplication, and 2 additions" |
| Character strings | UCA sort keys (e.g. ICU's `ucol_getSortKey`); terminate with a byte that appears nowhere else, "because keys must not be prefixes of other keys" |
| Null | give it a rank — e.g. widen only the smallest values: null → `0,0,0,0,0`, previously-smallest 0 → `0,0,0,0,1`, everything else keeps 4 bytes |
| Compound keys | transform each attribute separately and concatenate |

You have already read the first row in production. redis's
`encodeTimeoutKey` (`src/timeout.c:78-83`, in the rax chapter) calls
`htonu64` on a millisecond timestamp before using it as a rax key, then appends
the client pointer as a tiebreaker — a big-endian unsigned integer followed by
a concatenated second attribute. That is §IV-B rows 1 and 6, written years
earlier without the vocabulary.

Section IV also makes a claim worth carrying past this topic: binary-comparable
keys are what let you "replace comparison-based sorting algorithms like
quicksort or mergesort with the radix sort algorithm which can be
asymptotically superior". The same encoding buys ordered radix indexes *and*
radix sorting.

### Step 6 — the space proof: why 52, and why exactly 52

> **In:** Table I's four node sizes and their minimum child counts.
> **Out:** the budget argument from §III-G worked on the actual numbers,
> showing both that 52 bytes per key holds and that it is tight.

§III-G proves a worst-case bound of **52 bytes per key**, "even for arbitrarily
long keys". The mechanism is an amortisation argument: "Think of each leaf as
providing x bytes and inner nodes as consuming space provided by their
children." Formally, the budget of a node is x for a leaf, and otherwise the
sum of its children's budgets minus its own size. If every node's budget stays
non-negative, the tree costs less than x bytes per key.

The paper says the induction goes through for x = 52 and leaves the four cases
to the reader. Do them — the arithmetic is four lines and it shows *which* node
type is binding:

```
 budget(node) = (min children) · x − size(node),   with x = 52 and Table I sizes

 Node4  :  2 · 52 −   52 =  104 −   52 =  52     ← exactly 52: the binding case
 Node16 :  5 · 52 −  160 =  260 −  160 = 100
 Node48 : 17 · 52 −  656 =  884 −  656 = 228
 Node256: 49 · 52 − 2064 = 2548 − 2064 = 484

 all ≥ 52  ⇒  the induction closes, and the bound is 52 bytes per key.
```

Node4 comes out at exactly 52, so the bound is **tight** for this node set —
try x = 51 and Node4 gives 2·51 − 52 = 50 < 51 and the induction fails
immediately. The worst case is a tree of minimally-filled Node4s, which is
precisely the shape a sparse key distribution produces, and it is bounded
because path compression forbids one-child nodes (Step 4).

Footnote 1 says the bound "can be reduced to 34 bytes per key" with six node
types, where "the Node4 type is replaced by the new node types Node2 and
Node5". That number is derivable from the same argument: a Node2 costs
16 + 2 + 2·8 = **34** bytes, and the binding constraint 2x − 34 ≥ x gives
x ≥ 34. Splitting the smallest node type is exactly how you move the bound,
because the smallest type is what binds.

Table II puts the bound in company:

```
 Table II (§III-G) — worst-case bytes per key, 64-bit pointers

              k = 32     k → ∞
   ART           43        52
   GPT          256         ∞
   LRT         2048         ∞
   KISS       >4096        NA
```

GPT and LRT are unbounded "because [they] do not use path compression, the
number of inner nodes is proportional to the length of the keys" (§III-G).
A bound that survives k → ∞ is the thing adaptive nodes plus path compression
buy, and it is what lets a database *promise* an index memory budget.

The measured side is much better than the bound. The contributions list in §I
claims "often as low as 8.1 bytes per key", and Table IV shows where that comes
from — four of the seven major TPC-C indexes land at exactly 8.1 or 8.3 bytes
per key, all of them dense integers; the worst, index 3's long strings, is
32.6, "well below the worst case of 52 bytes" (§V-D).

**Derive the 8.1.** Take 1,000,000 dense 32-bit integer keys — the paper's own
best case, "integers ranging from 1 to n" (§III-G) — with s = 8, so four levels:

```
 level 4 (last key byte)  : fully dense ⇒ Node256; ⌈10⁶/256⌉      = 3907 nodes
 level 3                  : ⌈3907/256⌉                            =   16 nodes (Node256)
 level 2                  : 16 children                           =    1 node  (Node16)
 level 1                  : one-way ⇒ removed by path compression =    0 nodes

 bytes = 3907·2064 + 16·2064 + 1·160 = 8,097,072 + 160 = 8,097,232
 per key = 8,097,232 / 1,000,000 = 8.097 bytes
```

**8.1 bytes per key** — the paper's number, reproduced from Table I and a
division. Dense keys fill Node256s completely, so the amortised cost is
2064/256 = 8.06 bytes of pointer per key plus a rounding error. That is why
§V-D says "the best case of 8.1 bytes … does occur quite frequently because
surrogate integer keys are often dense".

### Step 7 — what §V actually measured, and its caveats

> **In:** the claim "comparable to hash tables" from the abstract.
> **Out:** the hardware, the contestants, the two caveats that shape the micro
> benchmarks, and the specific figures — including the one that this repo has
> independently measured.

The setup (§V): an Intel Core i7 3930K — 6 cores, 12 threads, 3.2 GHz (3.8 GHz
turbo), 12 MB shared L3, 32 GB quad-channel DDR3-1600 — on Linux 3.2, GCC 4.6.
Contestants: a cache-sensitive B⁺-tree (CSB), k-ary search, FAST, the
Generalized Prefix Tree (GPT), a red-black tree, and a **chained hash table
using MurmurHash64A**.

Two caveats decide how far the micro benchmarks generalise, and the paper
states both plainly:

1. **32-bit integer keys only**, "because some of the implementations only
   support 32 bit integer keys".
2. **Path compression was removed** for the micro benchmarks: "For such very
   short keys, path compression usually increases space consumption instead of
   reducing it. Therefore, we removed this feature for the micro benchmarks.
   Path compression is enabled in the more realistic second part."

So Figures 10-15 measure an ART without one of its two headline optimisations,
on the key type most favourable to radix trees. Read them accordingly. The
paper also reports separately for **dense** keys (1..n, randomly permuted) and
**sparse** keys (each bit equally likely 0 or 1) — and the gap between those
two bars is the real story.

Table III is the most useful table in the paper, because it is counters rather
than throughput:

```
 Table III (§V-A) — performance counters per lookup
                    65K keys                    16M keys
                ART(dense/sparse) FAST   HT   ART(dense/sparse) FAST   HT
 Cycles              40 / 105      94    44      188 / 352      461   191
 Instructions        85 / 127      75    26       88 /  99      110    26
 Misp. branches     0.0 / 0.85    0.0  0.26      0.0 / 0.84     0.0  0.25
 L3 hits           0.65 / 1.9     4.7   2.2      2.6 / 3.0      2.5   2.1
 L3 misses          0.0 / 0.0     0.0   0.0      1.2 / 2.6      2.4   2.4
```

Three readings. At 16M keys ART-dense takes **188 cycles** against the hash
table's **191** and FAST's **461** — "comparable to hash tables" is a fair
summary of that column. The dense/sparse split is entirely a cache-miss story:
1.2 versus 2.6 L3 misses, and the paper says so — "With dense keys, ART causes
only half as many cache misses because its compact nodes can be cached
effectively." And ART-sparse carries **0.84 mispredicted branches per lookup**
that ART-dense does not, "which occur during node type dispatch" (§V-A) — the
price of having four node types is a branch the CPU cannot predict when the
types are mixed. That is the RUM bill for adaptivity, paid in the pipeline.

The other figures worth knowing:

- **Figure 13, cache pressure.** "With 1/64th of the cache (192KB), ART reaches
  only about one third of the performance of the entire cache (12MB)", while
  the hash table "is mostly unaffected, as it does not use caches effectively
  anyway". Tree structures live on cached upper levels; a shared cache is a
  hidden dependency.
- **§V-C, adaptivity's insert cost.** "The impact of adaptive nodes on the
  insertion performance (in comparison with only using Node256) is 20% for
  trees with 16M dense keys" — the growth/shrink machinery costs a fifth of
  insert throughput and the paper calls it "usually a worthwhile trade off".
  Bulk loading recovers 2.5× on sparse keys and 17% on dense; sorted dense
  insertion reaches "50 million sorted, dense keys … per second".
- **§V-D, TPC-C end to end.** "ART is almost twice as fast as the hash table /
  red-black tree combination and almost four times as fast as the red-black
  tree alone", and — the sentence to underline — the hash table "introduced
  unacceptable rehashing latencies which are clearly visible as spikes in the
  graph" (Figure 16).

That last one is not a claim you have to take on faith: **this repo measured
it**. Topic 2's `rehash_spike` lane inserts 10 M keys into `hashbrown` one at a
time and reports `p50 = 42 ns` against `max = 58.4 ms` — a 1.4-million-fold
spread, with four of ten deciles carrying a multi-millisecond spike at the
power-of-two boundaries. ART's advantage in Figure 16 is not that it is faster
on average; it is that it has no rehash, so it has no tail. Leis et al.
observed the shape in 2013; the lane in this topic reproduces it in 2025 on a
different hash table.

## How to read the paper (with the concepts in hand)

The section numbering matters — several of these are commonly misquoted.

| Section | Contents | Step |
|---------|----------|------|
| §I | motivation, contributions (the 52 and 8.1 figures appear here first) | — |
| §II | related work — GPT, LRT, KISS-Tree, Judy, Graefe on normalised keys | 1 |
| §III-A | Preliminaries — span, height, the n > 2^(k/s) crossover, Figure 2 | 1 |
| §III-B | Adaptive Nodes — the space/height tradeoff, Figure 3, grow/shrink | 1, 2 |
| §III-C | **Structure of Inner Nodes** — Node4/16/48/256, Figure 5 | 2 |
| §III-D | Structure of Leaf Nodes — single-value, multi-value, combined slots | 2 |
| §III-E | **Collapsing Inner Nodes** — lazy expansion, path compression, hybrid | 4 |
| §III-F | Algorithms — Figures 7 (search), 8 (findChild), 9 (insert); bulk load | 3 |
| §III-G | **Space Consumption** — Tables I and II, the 52-byte proof | 2, 6 |
| §IV | **Constructing Binary-Comparable Keys** — definition and per-type rules | 5 |
| §V-A | Search performance — Figures 10-12, Table III | 7 |
| §V-B | Caching effects — Figures 12, 13 | 7 |
| §V-C | Updates — Figures 14, 15 | 7 |
| §V-D | End-to-end TPC-C in HyPer — Figures 16, 17, Table IV | 6, 7 |

A route through it:

1. **§III-A**, two pages. Do the crossover division yourself for 64-bit keys
   before looking at Figure 2.
2. **§III-C with Figure 5 open.** Write the four layouts from the Table I
   arithmetic (`16 + 256 + 48·8`) rather than from the prose; the arithmetic
   tells you what is stored.
3. **§III-F, Figure 8 only.** Twenty-one lines. Map each branch onto its node
   type and say what it costs in memory touches.
4. **§III-E.** Read the pessimistic/optimistic paragraph twice and write down
   which one ART uses by default. (It is pessimistic, with an 8-byte cap.)
5. **§III-G.** Work the four budget lines from Step 6 on paper. Then work
   x = 51 and watch Node4 fail.
6. **§IV.** Work the encodings until you could encode a `(u64, u16)` pair cold,
   including the null case.
7. **§V.** Read §V's first two paragraphs for the caveats *before* any figure,
   then Table III. Skim the throughput bars.
8. **Aha:** the paper's four node types are not four optimisations, they are
   one — pick the cheapest search the density allows — and the price is
   Table III's `0.84 mispredicted branches per lookup` on sparse keys, which
   is the node-type dispatch. Every adaptive structure pays for its adaptivity
   somewhere; ART pays in a branch. Once you see the cost line for the headline
   feature, you are reading the paper the way its authors did.

**Contrast case.** Read §III-C's Node4-through-Node256 progression directly
against `rax.c:150-155` (`raxNodeCurrentLength`) from the previous chapter. rax
has *one* node layout whose size is computed per node — a 4-child rax node is
4 + 4 + 0 + 32 = 40 bytes against ART's fixed Node4 at 52, and a 16-child rax
node is 4 + 16 + 4 + 128 = 152 against ART's Node16 at 160. rax is smaller at
every fanout, and pays for it with a linear scan and variable-size nodes that
fragment; ART is slightly larger and fixed-size, and gets a SIMD probe, a 16-byte
header with room for a compressed path, and a provable bound. Neither is
"better". They are two points on the same curve, and the paper and the C file
each argue their own corner in a comment.

## Questions to answer in notes.md

1. Node16's probe (Fig. 8, lines 6-12) is instruction-for-instruction the
   SwissTable group probe from `reading-hashbrown.md`. State the *structural*
   difference in what each does with the resulting bitfield, and say which one
   can have more than one bit set and why.
2. §III-A gives the crossover n > 2^(k/s). Compute it for 64-bit keys at
   s = 8, s = 4 and s = 1, and say what that implies about the Linux kernel's
   choice of s = 6 for a tree indexed by page offsets.
3. Work the Step 6 budget table yourself, then redo it assuming a 32-byte
   header instead of 16. What is the new bound, and which node type is binding?
4. §V removed path compression for the micro benchmarks and used 32-bit
   integer keys. Name one figure whose conclusion you would expect to change
   with 40-byte string keys and path compression on, and say in which
   direction.
5. Figure 16's hash-table rehash spikes are the same phenomenon this repo
   measured in the `rehash_spike` lane (`p50 = 42 ns`, `max = 58.4 ms`). Which
   ART property removes the tail, and what does it cost — name the counter in
   Table III that pays for it.
6. For the capstone: would ART beat a hash-based attribute store for
   `(entity id, attr id) → value`? Write the §IV-B encoding for that compound
   key explicitly, then state the RUM trade in the terms of Table I.

## Takeaway

ART is one idea applied four times: pick the cheapest search the local density
allows, and let the node layout follow. That converts the radix tree's
fundamental problem — a big span costs 2ˢ pointers whether or not you use them
— from a global parameter into a per-node decision, which is why Figure 3 shows
ART below *and* to the left of every fixed-span tree. Path compression and lazy
expansion then cap the height at the number of *distinguishing* bytes, and the
combination yields a proof, not just a measurement: 52 bytes per key for any
key set, any key length, worked in four lines of arithmetic in Step 6. The
costs are equally concrete — 0.84 mispredicted branches per sparse lookup for
node-type dispatch, and 20% of insert throughput for the grow/shrink machinery.
Carry the pattern rather than the structure: when a data structure has one
parameter that trades space against time, the interesting move is usually to
make it local.

## Done when

Answer each before unfolding it.

- [ ] Name the four node types with their capacity ranges and byte sizes, and
      say what search each performs.

<details>
<summary>Answer</summary>

From Table I (§III-G) and Figure 8 (§III-F):

| Type | Children | Bytes | Search |
|------|----------|-------|--------|
| Node4 | 2-4 | 16 + 4 + 4·8 = 52 | linear loop over the sorted key array |
| Node16 | 5-16 | 16 + 16 + 16·8 = 160 | one SSE compare of all 16 keys, masked, then `ctz` |
| Node48 | 17-48 | 16 + 256 + 48·8 = 656 | index the 256-byte `childIndex`, then the pointer array |
| Node256 | 49-256 | 16 + 256·8 = 2064 | the key byte *is* the index — no search |

The 16 in every row is the constant-size header holding node type, child count
and the compressed path (§III-C).

</details>

- [ ] ART caps its stored path-compression prefix at 8 bytes. What happens
      beyond 8 bytes — does it become pessimistic or optimistic, and what does
      that mean operationally?

<details>
<summary>Answer</summary>

It becomes **optimistic**. §III-E: ART stores a constant-size 8-byte partial
key vector "like in the pessimistic approach", and "only when this size is
exceeded, the lookup algorithm dynamically switches to the optimistic
strategy". Pessimistic means the skipped bytes are stored and compared during
descent, so a mismatch is caught immediately (Figure 7, lines 7-8). Optimistic
means only the *count* of skipped bytes is kept, the lookup skips them without
comparing, and the full key is compared once at the leaf to catch a wrong
turn. The default is the safe one; the fallback is the cheap one. Getting this
backwards inverts both the cost model and the failure mode.

</details>

- [ ] Show that the 52-byte bound is tight, and name the node type that binds
      it.

<details>
<summary>Answer</summary>

With x = 52 and Table I's sizes, the budget of each node type at its minimum
child count is 2·52 − 52 = **52** (Node4), 5·52 − 160 = 100 (Node16),
17·52 − 656 = 228 (Node48), 49·52 − 2064 = 484 (Node256). All are ≥ 52, so the
induction closes. Node4 hits it exactly, so it is **binding**: at x = 51 the
Node4 line gives 2·51 − 52 = 50 < 51 and the argument fails. That is also why
footnote 1's six-type variant reaches 34 — splitting Node4 into a Node2
(16 + 2 + 2·8 = 34 bytes) makes 2x − 34 ≥ x the new binding constraint, i.e.
x ≥ 34.

</details>

- [ ] Derive the paper's best-case 8.1 bytes per key from Table I, for 10⁶
      dense 32-bit integer keys.

<details>
<summary>Answer</summary>

Dense keys fill nodes completely, so every inner node is a Node256 except the
top. With s = 8 and 4-byte keys there are four levels: the last byte needs
⌈10⁶/256⌉ = 3907 Node256s, the level above ⌈3907/256⌉ = 16 Node256s, the level
above that one node with 16 children (a Node16, 160 bytes), and the top level
is a one-way node that path compression removes. Total
3907·2064 + 16·2064 + 160 = 8,097,232 bytes, i.e. **8.097 bytes per key**.
The intuition is the amortised cost of a full Node256: 2064/256 = 8.06 bytes of
pointer per child. Table IV's 8.1 for TPC-C indexes 1, 4 and 5 is this number.

</details>

- [ ] Adaptivity is not free. Name the two costs the paper measures, with their
      figures and sections.

<details>
<summary>Answer</summary>

(1) **Branch mispredictions from node-type dispatch**: Table III (§V-A) shows
0.84-0.85 mispredicted branches per lookup for sparse keys at both 65K and 16M
keys, against 0.0 for dense keys where every node is a Node256 and the dispatch
is predictable. (2) **Insert throughput**: §V-C, "The impact of adaptive nodes
on the insertion performance (in comparison with only using Node256) is 20% for
trees with 16M dense keys." Both are the price of having four layouts instead
of one, and the paper judges them worth paying — "Since the space savings from
adaptive nodes can be large, this is usually a worthwhile trade off."

</details>

- [ ] Why must a binary-comparable string key be terminated with a byte that
      appears nowhere else?

<details>
<summary>Answer</summary>

§IV-B(d): "it is important that each string is terminated with a value which
does not appear anywhere else in any string (e.g., the 0 byte). The reason is
that keys must not be prefixes of other keys." If "foo" were a prefix of
"foobar", a radix tree would have to represent "foo" at an *inner* node rather
than a leaf, which breaks lazy expansion (a truncated path can no longer be
resolved by comparing the leaf's key) and breaks the definition in §IV-A, since
`memcmp_k` compares fixed-length vectors. The terminator restores the property
that every key ends at a leaf. Compare rax, which allows a key at any node —
`iskey` is a bit on every node (`rax.h:79`) — and pays for it with a value
pointer slot in the node layout.

</details>

## References

**Paper**

- Viktor Leis, Alfons Kemper, Thomas Neumann — "The Adaptive Radix Tree: ARTful
  Indexing for Main-Memory Databases", ICDE 2013.
  [PDF](https://db.in.tum.de/~leis/papers/ART.pdf), 12 pages.

| Where | What |
|-------|------|
| §III-A, Fig. 2 | span, ⌈k/s⌉ height, the n > 2^(k/s) crossover |
| §III-B, Fig. 3 | why a fixed span is either tall or huge; grow/shrink |
| §III-C, Fig. 5 | the four inner-node layouts |
| §III-E, Fig. 6 | lazy expansion, path compression, pessimistic/optimistic hybrid |
| §III-F, Figs. 7-9 | search, `findChild` (the SSE probe), insert, bulk loading |
| §III-G, Tables I-II | node sizes; the 52-byte proof; comparison to GPT/LRT/KISS |
| §IV | binary-comparable keys — definition and per-type transformations |
| §V-A, Table III | per-lookup cycles, instructions, mispredictions, L3 traffic |
| §V-C | 20% insert cost of adaptivity; bulk loading; 50 M sorted inserts/s |
| §V-D, Table IV, Figs. 16-17 | TPC-C in HyPer; 8.1-32.6 bytes/key; height collapse |

**Related reading verified for this chapter**

- [DuckDB, *Indexes*](https://duckdb.org/docs/current/sql/indexes.html) — ART
  is one of DuckDB's two built-in index types, used for primary-key
  constraints and highly selective (< 0.1%) point queries.

**Measured in this repo**

- `topics/02-in-memory-structures/README.md` and `notes.md`, the `rehash_spike`
  lane: `p50 = 42 ns`, `p99.9 = 1292 ns`, `max = 58.4 ms` inserting 10 M keys
  into `hashbrown`. This is §V-D's "unacceptable rehashing latencies", measured
  independently on modern hardware and a different hash table.
- `topics/00-performance-toolbox/notes.md`, `lookup_shootout` at n = 10⁶:
  `hashmap 8.8 ns`, `btreemap 26.6 ns`, `vec_binary_search 25.8 ns`. The
  ordered-versus-unordered gap ART set out to close. Note this repo has no ART
  lane — every ART number in this chapter comes from the paper, on 2013
  hardware. Building one is the honest way to hold the claims to account.
- `topics/00-performance-toolbox/notes.md`: 21% of a `HashMap` lookup is
  SipHash. ART's cost model has no hash at all, which is part of why Table III
  shows it matching a MurmurHash64A table on cycles.

**Companion chapters**

- [`reading-redis-rax.md`](reading-redis-rax.md) — the same structure with the
  opposite RUM priority; the contrast case above uses its size arithmetic.
- [`reading-hashbrown.md`](reading-hashbrown.md) — Figure 8's lines 6-12, in
  Rust, in a hash table.
