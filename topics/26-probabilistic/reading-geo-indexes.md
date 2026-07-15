# Geo indexes: 2D queries through the 1D index you already have

Spatial search looks like it demands a new index structure — valkey's
GEO commands prove it doesn't: interleave the coordinate bits into one
integer and a plain sorted index becomes a spatial one. This chapter
builds that trick step by step — the encoding, the search, the curve's
seams — then surveys the families that *do* build real spatial
structures (R-tree, S2, H3), with the valkey source as the running
example.

## The problem in one sentence

"Every member within 200 m of this point" over millions of stored
locations is, naively, a full scan with a distance computation per row —
yet valkey answers it with the sorted set it already had, plus **9 range
queries and a distance check on the few candidates** they return.

## The concepts, step by step

### Step 1 — the reframe: make 2D nearness look like key order

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

Bit tricks need integers, so each coordinate is first mapped from its
continuous range to a fixed-width integer: valkey quantizes latitude and
longitude each to **26 bits** within their range (lat −90..90, lon
−180..180) — cell number = `(value − min) / range × 2^26`. Two costs to
note: quantization is lossy (everything inside one cell is
indistinguishable until the final exact check), and 26 was not picked
casually — the combined 52 bits must survive storage in a zset score,
which is an IEEE double with a 52-bit mantissa (question 1 below makes
you work out both the precision and what breaks at 27 bits).

### Step 3 — interleave the bits: the Morton / Z-order code

A **Morton code** (Z-order code) interleaves the bits of the two
quantized coordinates — y's bit i and x's bit i alternate — producing one
52-bit integer whose *prefixes* mean something: the top 2k bits identify
a square cell at level k, so **two codes sharing a prefix are in the
same cell** — prefix-similar codes = spatially-near points. The
interleave is five magic-mask rounds (geohash.c:52 does exactly this):

```rust
fn interleave64(xlo: u32, ylo: u32) -> u64 {
    let spread = |mut v: u64| {                 // 26 bits → every other bit
        v = (v | (v << 16)) & 0x0000FFFF0000FFFF;
        v = (v | (v << 8))  & 0x00FF00FF00FF00FF;
        v = (v | (v << 4))  & 0x0F0F0F0F0F0F0F0F;
        v = (v | (v << 2))  & 0x3333333333333333;
        v = (v | (v << 1))  & 0x5555555555555555;
        v
    };
    spread(xlo as u64) | (spread(ylo as u64) << 1)   // y25 x25 ... y0 x0
}
```

(The same bit-twiddling as HAKMEM / Bit Twiddling Hacks.) The consequence
that makes everything work: a level-k cell is exactly the set of codes in
one contiguous range `[prefix << shift, (prefix+1) << shift)` — a cell IS
a key range.

### Step 4 — the search: candidate cells, range scans, exact verify

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

When candidate-then-verify over a curve isn't enough — exact containment,
arbitrary polygons, spherical correctness — three families take over:

- **R-tree (Guttman '84)**: tree of bounding boxes; children may
  OVERLAP, so a lookup may descend multiple paths — the `penalty`/
  `picksplit` heuristics (minimize area/overlap enlargement) are
  the whole game; R* re-inserts to fix bad early splits. PostGIS =
  R-tree implemented *as a GiST extension* — read
  [reading-postgres-indexam.md](reading-postgres-indexam.md) with this
  in mind: GiST is the AM that lets `picksplit`/`penalty` be plugins.
- **S2 (Google)**: sphere → 6 cube faces → quadtree per face →
  Hilbert-ordered 64-bit cell IDs. Hierarchy = prefix relation, so
  containment tests are integer ops; coverings of a region are
  sets of cells at mixed levels.
- **H3 (Uber)**: hexagons (equidistant neighbors — nicer for
  gradients/flows), icosahedron-based, but hexes don't nest
  cleanly — the hierarchy is approximate. Great for
  sharding/aggregation, weaker for exact containment.

The through-line: geohash-in-a-zset spends zero new structures and pays
in over-fetch; the R-tree spends a whole tree and pays in overlap-driven
multi-path descents; S2/H3 spend sphere-aware cell math and pay in
discrete-cell-only answers.

## Where each step lives in the code

| anchor | step | what it does |
|---|---|---|
| `geohash.c:52` `interleave64` | 3 | the Morton interleave, five magic-mask rounds |
| `geohash_helper.c:64` `geohashEstimateStepsByRadius` | 4 | pick the cell level covering the radius; latitude-dependent, clamped near the poles |
| `geo.c:338` `scoresOfGeoHashBox` | 4 | cell → zset score range: `hash << shift` to `(hash+1) << shift` |
| `geo.c:367` | 4 | the ZRANGEBYSCORE candidate fetch |
| `geo.c:375` `membersOfAllNeighbors` | 4 | the 3×3 neighborhood scan + haversine post-filter |

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

## References

**Papers**
- Guttman — "R-Trees: A Dynamic Index Structure for Spatial Searching"
  (SIGMOD 1984)
- Beckmann, Kriegel, Schneider, Seeger — "The R*-tree" (SIGMOD 1990)

**Code & docs**
- [valkey](https://github.com/valkey-io/valkey) `src/geohash.c`,
  `src/geohash_helper.c`, `src/geo.c`
- [s2geometry.io](https://s2geometry.io) — S2 cell hierarchy docs
- [h3geo.org](https://h3geo.org) — H3 hex grid docs
