# Geo indexes: 2D queries through the 1D index you already have

Spatial search looks like it demands a new index structure — valkey's
GEO commands prove it doesn't: interleave the coordinate bits into one
integer and a plain sorted index becomes a spatial one. This chapter
walks that trick, why the curve you pick matters (Z-order vs Hilbert),
and the families that *do* build real spatial structures (R-tree,
S2, H3).

## The valkey GEO trick: no spatial index at all

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

The interleave is five magic-mask rounds (geohash.c:52 does exactly this):

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

Two ideas worth stealing:
1. **Reuse the index you have.** A sorted structure + a
   space-filling-curve key = a spatial index. FalkorDB could do the
   same over any sorted node-property index.
2. **Candidate-then-verify.** The 9-cell scan over-fetches
   (corners of the square aren't in the circle); the exact filter
   fixes it. One-sided error, then verification — a bloom filter's
   control flow, applied to geometry.

## Why Z-order has seams (and Hilbert doesn't)

```
 Z-order visits cells:        Hilbert visits cells:
   0 ─ 1     4 ─ 5              0 ─ 1     E ─ F
       │   ╱     │              │       │
   2 ─ 3     6 ─ 7              3 ─ 2   D ─ C
        BIG JUMP                 neighbors stay
   (3 → 4 crosses the           1 apart on the
    whole quadrant)              curve, mostly
```

Adjacent cells can be far apart on the Z-curve, so one bounding box
decomposes into many score ranges (valkey caps it by scanning the
fixed 3×3 neighborhood instead of decomposing precisely). Hilbert
curves keep neighbors closer at the cost of a more expensive
encode — the trade S2 takes (Hilbert on a cube projected to the
sphere).

## The other families

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
