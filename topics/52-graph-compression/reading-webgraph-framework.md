# WebGraph: a neighbour list is a gap list, a copy, and a few intervals

Every adjacency list in a CSR matrix is a sorted list of integers, and
everything that makes graphs compressible comes from one observation about
those integers: when vertices are numbered well, **the lists are not random**.
Neighbours are numerically close to the vertex and to each other, and
neighbouring vertices have nearly the same neighbours. Boldi and Vigna's
2004 paper is the clearest statement of how to turn those two facts into
bits, and its format (BV) is still what the Rust `webgraph` crate writes. This
guide builds the format one idea at a time on the paper's own five-row
example, works the arithmetic, and then asks the question the rest of this
topic is about: both ideas depend on the *numbering*, so what happens when the
numbering is bad?

**"The WebGraph Framework I: Compression Techniques"** — Paolo Boldi and
Sebastiano Vigna, WWW 2004, pp. 595–602
([pdf](https://vigna.di.unimi.it/ftp/papers/WebGraphI.pdf)). Section and table
numbers below are that PDF's. The companion paper (II, the ζ codes) was not
read; where this guide uses a ζ code it is described from the Rust source.

## The problem in one sentence

Given a directed graph whose vertices are numbered `0..N-1` and whose
adjacency lists are sorted, how few bits per edge can the lists be written in
while a single list can still be decoded without decoding the whole graph?

## The concepts, step by step

### Step 1 — the naive form, and the two properties the Web has

> **In:** nothing; the paper's Table 1 (five vertices of a Web graph).
> **Out:** the vocabulary (adjacency list, locality, similarity) and the one
> assumption every later step rests on.

An **adjacency list** `S(x)` is the sorted list of the vertices that vertex
`x` points to. The naive representation writes each list preceded by its
length, with every vertex id as a fixed-width integer; in a CSR matrix that
is the `Ai` array. Table 1 of the paper gives three lists from a 18.5-million-page
snapshot of the `.uk` domain (vertices numbered by sorting URLs
lexicographically):

```
node  outdegree  successors
15    11         13, 15, 16, 17, 18, 19, 23, 24, 203, 315, 1034
16    10         15, 16, 17, 22, 23, 24, 315, 316, 317, 3041
17     0         (none)
18     5         13, 15, 16, 17, 50
```

Two properties are named in §2. **Locality**: most links are navigational
("home", "next", "up"), so the source and target URLs share a long prefix, and
once URLs are sorted the *ids* of source and target are close. **Similarity**:
pages near each other in that order tend to share many successors, "because
many navigational links are the same within the same local cluster of pages".
Look at the table: vertex 15 and vertex 16 share `15, 16, 17, 23, 24, 315`.

Both properties are statements about the **numbering**, not about the graph:
the same graph with ids handed out by arrival order has neither. Hold that
thought; Step 7 comes back to it.

### Step 2 — gaps: write differences, not ids

> **In:** Step 1's lists. **Out:** each list as small non-negative integers
> (the paper's Table 2), and the signed-to-unsigned map ν.

A **gap** is the difference between a successor and the one before it. The
paper stores `S(x) = (s1, …, sk)` as `(s1 − x, s2 − s1 − 1, …, sk − sk−1 − 1)`
(§3, "Using gaps"). The `− 1` is free: a sorted list with no duplicates has
`s2 > s1`, so the gap `s2 − s1 − 1` is at least 0. Only the first entry can
be negative (a link to a smaller id than the source), so it is mapped to a
natural number by

ν(x) = 2x if x ≥ 0, and ν(x) = 2|x| − 1 if x < 0

— a **zig-zag** map (0→0, −1→1, 1→2, −2→3, …) that makes every value a
natural number.

Worked on vertex 15 (`13, 15, 16, 17, 18, 19, 23, 24, 203, 315, 1034`):

| position | successor | formula | value |
|---|---|---|---|
| first | 13 | ν(13 − 15) = ν(−2) | **3** |
| 2 | 15 | 15 − 13 − 1 | 1 |
| 3 | 16 | 16 − 15 − 1 | 0 |
| 4–6 | 17, 18, 19 | each 1 more | 0, 0, 0 |
| 7 | 23 | 23 − 19 − 1 | 3 |
| 8 | 24 | 24 − 23 − 1 | 0 |
| 9 | 203 | 203 − 24 − 1 | 178 |
| 10 | 315 | 315 − 203 − 1 | 111 |
| 11 | 1034 | 1034 − 315 − 1 | 718 |

The paper's Table 2 prints `3, 1, 0, 0, 0, 0, 3, 0, 178, 111, 718` for this
row: it agrees. (Its prose example in §3 for the *residual* version of the
same list writes the first value as "2·|−2|+1 = 5"; the formula printed two
paragraphs earlier gives 3. The Rust crate uses
`dsi_bitstream::codes::ToNat`, documented as `x ≥ 0 → 2x`, `x < 0 → −2x − 1`
(read at `dsi-bitstream-rs` `main`, 2026-10-26), i.e. the formula, not the
example. Table 5 prints 5. A reader checking the tables digit by digit will
hit this.)

Figure 1 plots the gap distribution of the `.uk` snapshot: a power law with
exponent 1.21 (and 1.20 for the transposed graph, Figure 2), with the gap-1
point sitting above the line — consecutive ids are over-represented. That
shape is why a **variable-length** code is right: small values are far more
frequent than large ones, so they should cost fewer bits. This topic's lane 1
prices the same gaps with a byte code (`varint`) and with the Elias-γ length
(`2·⌊log₂ x⌋ + 1` bits for `x ≥ 1`): γ of 3+1 = 4 costs 5 bits, γ of 718+1 =
719 costs 19.

### Step 3 — reference compression: copy a previous list

> **In:** Step 2's gap lists plus the Similarity property. **Out:** a
> representation that stores only how list `x` *differs* from an earlier list
> `S(x − r)` — the paper's Tables 3–4.

If `S(16)` is mostly `S(15)`, say so instead of repeating it. Pick a
**reference** vertex `y = x − r` (the **reference number** `r`, 0 meaning
"none") and write a **copy list**: one bit per entry of `S(y)`, 1 if that
entry is also in `S(x)`. What is left over is the **extra nodes**.

Worked: `x = 16`, `r = 1`, `S(15) = 13, 15, 16, 17, 18, 19, 23, 24, 203, 315,
1034`. Mark which of those appear in `S(16) = 15, 16, 17, 22, 23, 24, 315,
316, 317, 3041`:

```
S(15):   13  15  16  17  18  19  23  24  203  315  1034
in S(16)? 0   1   1   1   0   0   1   1   0    1    0     → copy list 01110011010
extra nodes (in S(16), not in S(15)):  22, 316, 317, 3041
```

That is Table 3's row for node 16 exactly. The choice of `r` is "critical":
the paper fixes a **window** `W` and picks the `r ≤ W` that gives the best
compression; "a large value of W is likely to produce better compression
ratios … the price … is a slower and more memory-consuming compression and
decompression".

**Differential compression** (the paper's new piece over the LINK database)
writes the copy list as alternating run lengths instead of bits: a **copy
block** of 1s, a block of 0s, and so on. The runs here are
`0, 1, 3, 2, 2, 1, 1, 1` (the list starts with a 0 so the first 1-run has
length 0). Two economies: every block but the first is stored minus 1 (a
non-first block is never empty), and the last block is omitted (its length is
implied by `|S(y)|`). The block count is `7`; the written values are
`0, 0, 2, 1, 1, 0, 0` — Table 4's row for node 16, exactly. The paper's
sentence to remember: "Using typical codes, such as γ coding, copying
entirely a list costs one bit: more generally, differential compression allows
to code a link in less than one bit, something which is impossible with plain
reference compression."

Its empirical claim (§2): "Similarity is much more concentrated than it was
previously thought. Either two lists have almost nothing in common, or they
share large segments." — which is why run-length blocks beat a bit per entry.

### Step 4 — intervals: consecutive ids are the transpose of similarity

> **In:** Step 3's extra nodes. **Out:** extra nodes split into **intervals**
> (runs of consecutive ids) and **residuals** (everything else), the paper's
> Table 5.

§2 observes that "consecutivity is common": a page links to
`/a/1, /a/2, /a/3, …`, and sorted URLs make those consecutive ids. It also
notes a duality: if a graph compresses well by similarity at distance one, its
*transpose* has long intervals, and vice versa (a vertex common to two
consecutive lists becomes an interval of length ≥ 2 in the transpose).

A run of at least `L_min` consecutive extra nodes is an interval, written as
its left extreme and its length minus `L_min`; the remaining nodes are
residuals, written as gaps like Step 2. Worked on vertex 15 (no reference,
`L_min = 2`, so all 11 successors are extra nodes):

* intervals: `[15..19]` (length 5) and `[23, 24]` (length 2). Left extremes:
  `15 − 15 = 0` (the first is relative to the vertex itself) and
  `23 − 19 − 2 = 2` (relative to the previous right end, minus 2, because at
  least one id separates two intervals). Lengths minus `L_min`: `5 − 2 = 3`,
  `2 − 2 = 0`.
* residuals `13, 203, 315, 1034`: `ν(13 − 15) = ν(−2) = 3` (the table prints
  5, see Step 2), then `203 − 13 − 1 = 189`, `315 − 203 − 1 = 111`,
  `1034 − 315 − 1 = 718`.

Table 5's row for node 15: left extremes `0, 2`; lengths `3, 0`; residuals
`5, 189, 111, 718` — the same but for the first residual. For node 16: one
interval `[316, 317]`, left extreme `ν(316 − 16) = 600`, length `0`; residuals
`22, 3041` as `ν(22 − 16) = 12` and `3041 − 22 − 1 = 3018`. 

**The full record** (Table 6, §3) for vertex `x`: outdegree `d`; if the window
is positive, reference `r`, and if `r > 0` the block count and blocks; if
intervals are enabled, interval count, left extremes, lengths; then the
residuals. The only datum always present is `d`; if `d = 0` nothing else is
stored. Every integer is written with a chosen **instantaneous code** (one
that can be decoded without a delimiter): the paper leaves the choice open.

### Step 5 — the price of copying: reference chains and the window

> **In:** Step 3's references. **Out:** why random access gets slow, and the
> parameter `R` that bounds it (§4, Table 7).

If `S(16)` copies from `S(15)` and `S(15)` copies from `S(14)`, then reading
`S(16)` means reading `S(14)`, `S(15)`, `S(16)` in that order: a
**reference chain**. Sequential scans do not care (keep the last `W` lists),
but random access has to walk the chain, which "in the worst case … may
require a decompression of all lists up to `x`". Parameter `R`, the **maximum
reference count**, forbids references that would make a chain longer than
`R`.

Table 7 measures the trade on two crawls. On the 18.5 M-page `.uk` snapshot
(300 M links), bits per link with ζ₃ for residuals and `L_min = 3`:

| R | W = 1 | W = 3 | W = 7 |
|---|---|---|---|
| ∞ | 2.75 | 2.38 | 2.22 |
| 3 | 3.87 | 3.25 | 3.00 |
| 1 | 5.05 | 3.91 | 3.46 |

Reading it: bounding chains to `R = 1` costs 56% more bits at `W = 7`
(3.46 / 2.22 = 1.56); widening the window from 1 to 7 at `R = ∞` saves 19%
(2.22 / 2.75 = 0.81). On the WebBase crawl (118 M pages, 1 G links) the best
cell is 3.08 bits per link and its transpose reaches 2.89 (abstract; Table 7).
The abstract's comparison: the LINK database used "5.61 bits per link" on a 61
Mnode / 1 Glink graph.

For scale: a 32-bit CSR target is 32 bits per link. WebGraph at 3 bits is
10× smaller — on the Web graph with URL-sorted ids. Step 7.

### Step 6 — lazy decoding, and why offsets cost almost as much as lists

> **In:** Steps 3–5. **Out:** how a reader walks a chain without expanding
> it (§6), and what the offset array costs (§5, Table 8).

A bit-packed list has no fixed position, so random access needs an **offset
array** (the bit position of each vertex's record). With 64-bit offsets "the
offset array [would be] larger than the graph itself" (§5) at 3 bits per link
and tens of links per vertex. WebGraph stores offsets only for every `J`-th
vertex (the **jump**) and skips forward over `x mod J` records; each block of
`J` records stores its `J` outdegrees up front, so skipping needs no recursive
decoding. `J` is "not a compression parameter: … fixed when reading the graph
into memory".

**Lazy iteration** (§6): rather than decode the reference list into memory, the
iterator for `S(x)` holds the interval and residual state plus an iterator over
`S(x − r)` that it consults only when it needs the next copied element and the
copy blocks say that element is kept. Footnote 4: "lazy iteration is faster
than eager iteration (e.g., than simply expanding all referenced lists)".
Table 8 (a 2.4 GHz Pentium, the 18.5 M-page `.uk` graph, Java) prices it.
Graph size 79.0 MiB at `R = ∞`, 106.6 at `R = 3`, 122.9 at `R = 1`;
sequential access 198, 206 and 233 ns per link. Random access with the full
offset array (`J = 1`) costs **31,237 ns** per link at `R = ∞` against 611 at
`R = 3` and 442 at `R = 1` — the chain length, paid in time. And the full
offset array is 141.3 MiB, *larger than the 79.0 MiB graph* it indexes (the
`J = 4` array is 35.3 MiB). The paper says "a tuned C implementation would
increase by a factor of about two the timings we report", and that sequential
time "do[es] not depend on R, but only on W". Lane 3's reference decode is
the *eager* kind, deliberately: it shows the cost the lazy iterator exists to
avoid.

### Step 7 — what the numbering does to every step above

> **In:** Steps 2–4. **Out:** the claim this topic tests, and the paper's own
> hedge.

Each of the three ideas needs something from the vertex ids:

* gaps are small only if neighbours' ids are close to each other (locality);
* a reference helps only if consecutive-ish vertices have similar lists
  (similarity);
* intervals exist only if a vertex's neighbours are numbered consecutively.

The paper numbers vertices by sorted URL, which delivers all three because the
Web *has* a hierarchy encoded in URLs. §3's footnote 2 even warns that the
techniques "are not viable" if the original order of the links must be
preserved, and the related-work paragraph credits the Connectivity Server for
"suggest[ing] lexicographical ordering as a way to obtain a good compression
using gaps". Nowhere does the paper say what happens on a graph whose ids carry
no structure; the follow-on work does: [Dhulipala et al.](reading-recursive-bisection.md)
Table 2 prints WebGraph's bits per edge on the *natural* order and after
reordering for the same graphs. Lane 1 and lane 3 of this topic measure that
dependence directly: random ids against community ids on one graph, and
reference coding on lists that are independent draws versus lists that are
copies.

## How to read the paper (with the concepts in hand)

Read §3 straight through with Tables 1–6 beside it; every table is a
different projection of the same two lists and the worked rows above are
yours to check. Skip §4 until you have the format; then Table 7 is the whole
experiment. §5–§6 are the engineering that keeps random access usable. The
source of the format in Rust is in
[reading-webgraph-rs.md](reading-webgraph-rs.md).

## Where each step lives in this topic

| step | here |
|---|---|
| gaps, varint, γ | `gapcode.rs` (lane 1): `VarintGraph`, `gamma_bits`, `log_gap` |
| reference + intervals + residuals | `refcode.rs` (lane 3 stub) |
| the numbering | `order.rs`, `gen.rs`, and every row of lane 1 |
| the Rust format | [reading-webgraph-rs.md](reading-webgraph-rs.md) |

## Questions (answer in notes.md)

1. Recompute Table 4's row for node 18 (`13, 15, 16, 17, 50`) against
   reference 3 (`S(15)`): copy list, blocks, extra nodes.
2. In Table 7, why does `R = 3, W = 7` (3.00 bits) beat `R = 1, W = 7` (3.46)
   by 13%, and what would you expect for a graph where lists are independent
   draws?
3. Reference compression cannot code a link in less than one bit; differential
   compression can. Which Step-3 sentence tells you under what condition?
4. Lane 1 shows random ids at 22.8 varint bits per edge. Which of the three
   properties from Step 7 does a random numbering destroy first?

## Done when

Answer each before unfolding it.

- [ ] You can turn an adjacency list into the paper's gap list by hand.

  <details><summary>Answer</summary>

  Subtract the vertex from the first successor and zig-zag it
  (ν(x) = 2x for x ≥ 0, 2|x| − 1 for x < 0), then write each later successor
  as `successor − previous − 1`. Vertex 15 with successors 13, 15, 16, 17, 18,
  19, 23, 24, 203, 315, 1034 gives 3, 1, 0, 0, 0, 0, 3, 0, 178, 111, 718, the
  paper's Table 2 row (§3).

  </details>

- [ ] You can reproduce the copy list, copy blocks and extra nodes for vertex
  16 against vertex 15.

  <details><summary>Answer</summary>

  Mark which of S(15) = 13, 15, 16, 17, 18, 19, 23, 24, 203, 315, 1034 are
  in S(16): copy list 01110011010, extra nodes 22, 316, 317, 3041. The runs
  are 0, 1, 3, 2, 2, 1, 1, 1; dropping the last and writing every non-first
  run minus 1 gives the block count 7 and blocks 0, 0, 2, 1, 1, 0, 0 (Tables
  3–4).

  </details>

- [ ] You can say what a reference chain is and what `R` bounds.

  <details><summary>Answer</summary>

  If x references y and y references z, decoding x needs y and z first: a
  chain. R (maximum reference count) forbids choosing a reference that would
  make the chain longer than R. Sequential reads are unaffected (a window of
  the last W lists suffices); random access is bounded to R + 1 decodes. The
  price is size: at W = 7, R = 1 costs 3.46 bits per link against 2.22 for
  R = ∞ on the .uk snapshot (Table 7).

  </details>

- [ ] You can state which numbering property each of gaps, references and
  intervals needs.

  <details><summary>Answer</summary>

  Gaps need neighbours with close ids (locality); references need nearby
  vertices to share successors (similarity); intervals need a vertex's
  neighbours to be numbered consecutively. The paper gets all three from
  URL-sorted ids and does not discuss graphs without such an order; the
  reordering papers (Dhulipala et al. Table 2) show WebGraph's bits per edge
  roughly halving when a bad natural order is replaced by a computed one.

  </details>

- [ ] You can name the one place the paper's printed example and its printed
  formula disagree.

  <details><summary>Answer</summary>

  ν(x) = 2|x| − 1 for x < 0 gives ν(−2) = 3, but the worked residual example
  and Table 5 print 5 (written as 2·|−2| + 1). The Rust crate's `to_nat`
  follows the formula (x < 0 → −2x − 1).

  </details>

## References

- Boldi & Vigna, *The WebGraph Framework I: Compression Techniques*, WWW 2004,
  pp. 595–602 — Tables 1–8, §2 (Figures 1–2), §3 (format), §4, §5, §6.
- `dsi-bitstream-rs` `src/codes/mod.rs`, `ToNat` — the zig-zag map the Rust
  crate uses (read at `main`, 2026-10-26).
- Randall, Stata, Wickremesinghe & Wiener, *The LINK database* (2001), and
  Adler & Mitzenmacher (2001) — the reference-compression precursors the paper
  cites; not read.
- [reading-webgraph-rs.md](reading-webgraph-rs.md) — the same format in code.
