# Geo indexes: 2D queries through the 1D index you already have

Spatial search looks like it demands a new index structure — valkey's
GEO commands prove it doesn't: interleave the coordinate bits into one
integer and a plain sorted index becomes a spatial one. This chapter
builds that trick step by step — the encoding, the search, the curve's
seams — then surveys the families that *do* build real spatial
structures (R-tree, S2, H3), with the valkey source as the running
example.

Every code anchor below is valkey at commit `8891441ab`, the revision
this repo pins (`src/geohash.c`, `src/geohash_helper.c`, `src/geo.c`),
quoted with the line numbers the code occupies in that version. The S2
and H3 figures are quoted from their project docs; the geohash precision
is worked out on the spot from `GEO_STEP_MAX`.

## The problem in one sentence

"Every member within 200 m of this point" over millions of stored
locations is, naively, a full scan with a distance computation per row —
yet valkey answers it with the sorted set it already had, plus **9 range
queries and a distance check on the few candidates** they return.

## The concepts, step by step

### Step 1 — the reframe: make 2D nearness look like key order

> **In:** nothing yet — this step fixes the one question a sorted index
> answers fast.
> **Out:** the plan to turn "near in 2D" into "a few 1D key ranges" via a
> single interleaved key.

A sorted index (zset, B-tree, anything) answers exactly one question
fast: "give me all keys in range [a, b]". Spatial search needs a
different question — "all points near (x, y)" — where nearness lives in
two dimensions at once. The trick is not a new structure but a new
*key*: encode (x, y) into a single integer such that points close in
space usually get numerically close codes. Then "near in space" becomes
"a few key ranges", and the index you already have does the rest. The
payoff is enormous in code terms: zero new index structures, one encode
function, one range computation.

### Step 2 — quantize: coordinates become fixed-width integers

> **In:** the "one integer key" plan from Step 1.
> **Out:** each of lat/lon as a 26-bit cell number, and why 26 (not 27)
> is the ceiling.

Bit tricks need integers, so each coordinate is first mapped from its
continuous range to a fixed-width integer: valkey quantizes latitude and
longitude each to **26 bits** within their range (lat −90..90, lon
−180..180) — cell number = `(value − min) / range × 2^26`
(`GEO_STEP_MAX = 26` at geohash.h:46, commented "26*2 = 52 bits"). Worked
at the equator: `2^26 = 67,108,864` cells per axis; longitude spans 360°
≈ 40,075,017 m, so one cell is `40,075,017 / 2^26 ≈ 0.60 m` wide, and
latitude's 180° ≈ 20,003,931 m gives `20,003,931 / 2^26 ≈ 0.30 m` tall —
a sub-metre cell. Two costs to note: quantization is lossy (everything
inside one cell is indistinguishable until the final exact check), and 26
was not picked casually — the combined 52 bits must survive storage in a
zset score, an IEEE double whose exact-integer ceiling is `2^53`; a 52-bit
code clears it, but 27 bits/axis (54 bits) would not (question 1 below
makes you work out both the precision and what breaks at 27 bits).

### Step 3 — interleave the bits: the Morton / Z-order code

> **In:** the two 26-bit cell numbers from Step 2.
> **Out:** one 52-bit Morton code whose prefixes name square cells — so a
> cell equals a contiguous key range.

A **Morton code** (Z-order code) interleaves the bits of the two
quantized coordinates — y's bit i and x's bit i alternate — producing one
52-bit integer whose *prefixes* mean something: the top 2k bits identify
a square cell at level k, so **two codes sharing a prefix are in the
same cell** — prefix-similar codes = spatially-near points. The
interleave is five magic-mask rounds, quoted verbatim:

```c
// src/geohash.c:52-76 (interleave64, valkey@8891441ab)
    52  static inline uint64_t interleave64(uint32_t xlo, uint32_t ylo) {
    53      static const uint64_t B[] = {0x5555555555555555ULL, 0x3333333333333333ULL, 0x0F0F0F0F0F0F0F0FULL,
    54                                   0x00FF00FF00FF00FFULL, 0x0000FFFF0000FFFFULL};
    55      static const unsigned int S[] = {1, 2, 4, 8, 16};
    56
    57      uint64_t x = xlo;
    58      uint64_t y = ylo;
    59
    60      x = (x | (x << S[4])) & B[4];
    61      y = (y | (y << S[4])) & B[4];
    62
    63      x = (x | (x << S[3])) & B[3];
    64      y = (y | (y << S[3])) & B[3];
    65
    66      x = (x | (x << S[2])) & B[2];
    67      y = (y | (y << S[2])) & B[2];
    68
    69      x = (x | (x << S[1])) & B[1];
    70      y = (y | (y << S[1])) & B[1];
    71
    72      x = (x | (x << S[0])) & B[0];
    73      y = (y | (y << S[0])) & B[0];
    74
    75      return x | (y << 1);
    76  }
```

(The same bit-twiddling as HAKMEM / Bit Twiddling Hacks — `y << 1` puts
latitude in the odd bit positions.) The consequence that makes everything
work: a level-k cell is exactly the set of codes in one contiguous range
`[prefix << shift, (prefix+1) << shift)` — a cell IS a key range.

### Step 4 — the search: candidate cells, range scans, exact verify

> **In:** the Morton-coded zset from Step 3.
> **Out:** the 9-cell candidate scan + haversine verify — one-sided
> over-fetch, then exact filter.

A radius query now decomposes into three moves: pick a cell size roughly
matching the radius, scan that cell plus its 8 neighbors as zset score
ranges, then filter the candidates with the exact **haversine** distance
(the great-circle distance formula on a sphere). The full valkey
pipeline:

```
 GEOADD key lon lat member
   │
   ▼
 lat, lon each quantized to 26 bits within their range
   │
   ▼ interleave64(lat_bits, lon_bits)        geohash.c:52
 52-bit Morton code:  y25 x25 y24 x24 ... y0 x0
   │        (interleave via magic-mask shifts — the same
   │         bit-twiddling as HAKMEM/Bit Twiddling Hacks)
   ▼
 ZADD key <52-bit code as double score> member
        ── the "index" is the zset you already had

 GEOSEARCH radius r:
   step = geohashEstimateStepsByRadius(r, lat)   geohash_helper.c:64
     (pick cell level so one cell ≳ the radius; higher lat ⇒
      cells narrow ⇒ adjust — spherical reality leaks in)
   for cell + 8 neighbors:                        geo.c:375
     score range = [hash << (52-2·step), (hash+1) << ...]
                                                  geo.c:338
     ZRANGEBYSCORE → candidates                   geo.c:367
   exact haversine filter on candidates
```

Why 9 cells? The query point can sit at a cell's edge, so the radius can
spill into any neighbor — the 3×3 block is the cheapest cover that never
misses. It over-fetches (corners of the 3×3 square aren't in the
circle), and the exact filter fixes that. Two ideas worth stealing:

1. **Reuse the index you have.** A sorted structure + a
   space-filling-curve key = a spatial index. FalkorDB could do the
   same over any sorted node-property index.
2. **Candidate-then-verify.** The 9-cell scan over-fetches
   (corners of the square aren't in the circle); the exact filter
   fixes it. One-sided error, then verification — a bloom filter's
   control flow, applied to geometry.

### Step 5 — the curve's seams: Z-order vs Hilbert

> **In:** the Z-order code from Step 3 and the 9-cell scan from Step 4.
> **Out:** why Z-order's jumps force many ranges per box, and what
> Hilbert trades to fix them.

A **space-filling curve** is the 1D visiting order a code imposes on the
2D grid, and Z-order's has seams — adjacent cells can be far apart on the
curve:

```
 Z-order visits cells:        Hilbert visits cells:
   0 ─ 1     4 ─ 5              0 ─ 1     E ─ F
       │   ╱     │              │       │
   2 ─ 3     6 ─ 7              3 ─ 2   D ─ C
        BIG JUMP                 neighbors stay
   (3 → 4 crosses the           1 apart on the
    whole quadrant)              curve, mostly
```

Because of the jumps, one bounding box decomposes into many score ranges
(valkey caps the damage by scanning the fixed 3×3 neighborhood instead of
decomposing precisely). The **Hilbert curve** rotates its pattern per
quadrant so spatial neighbors stay close on the curve — fewer, longer
ranges per query — at the cost of a more expensive encode (per-level
rotations instead of one mask cascade). That trade is the one S2 takes.

### Step 6 — the families that do build real spatial structures

> **In:** the curve-plus-verify approach from Steps 4–5 and its limits.
> **Out:** three real spatial families (R-tree, S2, H3) and the exact
> price each pays.

When candidate-then-verify over a curve isn't enough — exact containment,
arbitrary polygons, spherical correctness — three families take over:

- **R-tree (Guttman '84)**: tree of bounding boxes; children may
  OVERLAP, so a lookup may descend multiple paths — the `penalty`/
  `picksplit` heuristics (minimize area/overlap enlargement) are
  the whole game; R* re-inserts to fix bad early splits. PostGIS =
  R-tree implemented *as a GiST extension* — read
  [reading-postgres-indexam.md](reading-postgres-indexam.md) with this
  in mind: GiST is the AM that lets `picksplit`/`penalty` be plugins.
- **S2 (Google)**: sphere → 6 cube faces (level 0 is exactly **6 cells**)
  → quadtree per face (×4 cells per level, **levels 0–30**) →
  Hilbert-ordered **64-bit** cell IDs, with sub-cm² leaf cells at level
  30. Hierarchy = prefix relation, so containment tests are integer ops;
  coverings of a region are sets of cells at mixed levels
  ([s2geometry.io cell statistics](https://s2geometry.io/resources/s2cell_statistics)).
- **H3 (Uber)**: hexagons (equidistant neighbors — nicer for
  gradients/flows), icosahedron-based across **16 resolutions (0–15)**;
  resolution 0 has **122 base cells (110 hexagons + 12 pentagons)**, and
  there are *exactly* 12 pentagons at every resolution (aperture-7, so a
  cell has ≈7 children). Hexes don't nest cleanly — the hierarchy is
  approximate. Great for sharding/aggregation, weaker for exact
  containment ([h3geo.org resolution table](https://h3geo.org/docs/core-library/restable/)).

The through-line: geohash-in-a-zset spends zero new structures and pays
in over-fetch; the R-tree spends a whole tree and pays in overlap-driven
multi-path descents; S2/H3 spend sphere-aware cell math and pay in
discrete-cell-only answers.

## Where each step lives in the code

| anchor | step | what it does |
|---|---|---|
| `geohash.c:52-76` `interleave64` | 3 | the Morton interleave, five magic-mask rounds |
| `geohash_helper.c:64` `geohashEstimateStepsByRadius` | 4 | pick the cell level covering the radius; latitude-dependent, clamped near the poles |
| `geo.c:338` `scoresOfGeoHashBox` | 4 | cell → zset score range: `hash << shift` to `(hash+1) << shift` |
| `geo.c:367` `membersOfGeoHashBox` | 4 | one box's score range → ZRANGEBYSCORE candidate fetch |
| `geo.c:375` `membersOfAllNeighbors` | 4 | the 3×3 neighborhood scan (calls `membersOfGeoHashBox` per box at :424) + haversine post-filter |

Read them in pipeline order (encode → step estimate → ranges → neighbors)
— it is one straight-line data path, ~400 lines total.

## Questions

1. Why 26 bits per axis (52 total)? Connect to the zset score being
   a double — what goes wrong at 27 bits, and what precision in
   meters does 26 give at the equator?
2. `geohashEstimateStepsByRadius` takes the latitude as an argument
   (geohash_helper.c:64). Why does the same radius need a different
   cell level at 60°N than at the equator, and what breaks near the
   poles (see the clamps)?
3. The 9-cell candidate scan over-fetches by roughly what factor
   (area of 3×3 cells vs the inscribed circle)? When is precise
   Z-range decomposition (many small ranges) worth it instead?
4. An R-tree lookup can descend multiple children; a B-tree never
   does. What property of the keys makes single-path descent
   impossible for boxes, and how does R* `picksplit` reduce (not
   eliminate) it?
5. S2 cell IDs make "is cell A inside cell B" a prefix check on
   integers. Show the bit layout that makes this work, and why H3's
   hexagons can't have the same exact property.
6. **M26 mapping**: sketch `GEO.ADD`/`GEO.SEARCH` for the capstone
   graph — node position as a property, 52-bit Morton key in the
   sorted property index M26 already builds. What's the *only* new
   code (encode + 9-cell range computation + haversine), and what's
   reused verbatim?

## Done when

Answer each before unfolding it.

- [ ] You can explain the reframe: making 2D nearness look like key order.

  <details><summary>Answer</summary>

  A sorted index answers "all keys in [a, b]" fast. Interleaving (x, y)
  into one Morton code makes code *prefixes* name square cells, so "near
  in 2D" becomes a handful of 1D key ranges — the zset you already have
  becomes a spatial index for one encode function plus one range
  computation, zero new structures.

  </details>

- [ ] You can compute a Morton code by hand and say why interleaving works.

  <details><summary>Answer</summary>

  Alternate the bits so y-bit-i and x-bit-i interleave (`interleave64`,
  geohash.c:52-76: five `(v | (v << S)) & B` mask rounds, then `x | (y <<
  1)`). The top 2k bits then identify a level-k square cell, so a shared
  prefix ⇒ same cell ⇒ numerically close codes ⇒ spatially near points,
  and a level-k cell is one contiguous code range.

  </details>

- [ ] You can explain why 26 bits per axis, connected to the zset score's precision.

  <details><summary>Answer</summary>

  26 bits/axis = a 52-bit code (`GEO_STEP_MAX = 26`, geohash.h:46). A zset
  score is an IEEE double whose largest exact integer is 2^53, so 52 bits
  store losslessly while 54 (27/axis) would round. At the equator a cell
  is ~0.60 m wide (40,075,017 m / 2^26) × ~0.30 m tall (20,003,931 m /
  2^26).

  </details>

- [ ] You can describe the candidate-cells, range-scan, exact-verify search and estimate the over-fetch factor.

  <details><summary>Answer</summary>

  Pick a cell level ≈ the radius (`geohashEstimateStepsByRadius`,
  geohash_helper.c:64), scan the cell + 8 neighbors as zset score ranges
  (`scoresOfGeoHashBox` geo.c:338 → `membersOfGeoHashBox` geo.c:367,
  looped over the 9 in `membersOfAllNeighbors` geo.c:375), then
  haversine-filter. The 3×3 block is 9 cells against a query circle of
  area ≈ π r²; with one cell ≳ the radius that is roughly a 3–10×
  over-fetch, which the exact filter removes.

  </details>

- [ ] You can explain the curve's seams and what Hilbert fixes.

  <details><summary>Answer</summary>

  Z-order (Morton) jumps far along the curve when it crosses a quadrant
  boundary (3 → 4 in the diagram), so one bounding box shatters into many
  score ranges; valkey caps the damage with a fixed 3×3 neighborhood scan.
  Hilbert rotates its pattern per quadrant so spatial neighbors stay
  adjacent on the curve — fewer, longer ranges — at the cost of per-level
  rotations instead of one mask cascade. S2 takes that trade.

  </details>

- [ ] You wrote answers to all questions in notes.md, including the `GEO.ADD`/`GEO.SEARCH` sketch for M26.

  <details><summary>Answer</summary>

  Self-check: the six questions cover the 26-bit/double-precision link,
  the latitude-dependent step estimate and pole clamps, the 3×3
  over-fetch factor vs precise Z-range decomposition, R-tree multi-path
  descent, S2 prefix-containment vs H3 hexagons, and the M26
  `GEO.ADD`/`GEO.SEARCH` mapping — where encode + 9-cell range + haversine
  are the only new code and the sorted property index is reused verbatim.

  </details>

## References

**Papers**
- Guttman — "R-Trees: A Dynamic Index Structure for Spatial Searching"
  (SIGMOD 1984)
- Beckmann, Kriegel, Schneider, Seeger — "The R*-tree" (SIGMOD 1990)

**Code & docs**
- [valkey](https://github.com/valkey-io/valkey) `src/geohash.c`,
  `src/geohash_helper.c`, `src/geo.c` (pinned at `8891441ab`;
  `GEO_STEP_MAX = 26` in `src/geohash.h`)
- [s2geometry.io cell statistics](https://s2geometry.io/resources/s2cell_statistics)
  — S2 cell hierarchy: 6 faces at level 0, levels 0–30
- [h3geo.org resolution table](https://h3geo.org/docs/core-library/restable/)
  — H3 hex grid: 16 resolutions, 122 base cells (110 hex + 12 pentagons)
