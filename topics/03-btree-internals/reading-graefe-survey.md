# Reading guide — Graefe, "Modern B-Tree Techniques" (2011)

Foundations and Trends in Databases, ~200 pages. **Do not read it all.** It's a
survey/reference; this guide picks the ~50 pages that matter for this topic and
the capstone. Budget: 3 h.

## Why this survey

Every "B-trees are simple" take dies here. Graefe catalogs what production
B-trees actually do: compression, latching, logging interactions, bulk loads.
You'll come back to it in topics 5 (logging), 8/9 (latching), 12 (columnar).

## Read now (this topic)

| Section | Pages (approx) | Why |
|---|---|---|
| §2 Basic techniques | skim | you know this from the code |
| **§3.1–3.3 Prefix + suffix truncation** | read | your experiment; separators need only be *shorter than both neighbors*, not real keys |
| **§3.4 Normalized keys** | read | binary-comparable encoding again (ART §III.E) — one memcmp replaces typed comparison |
| §3.5 Poor man's normalized key | read | first bytes of the key cached IN the pointer array slot — dense filter pattern yet again |
| **§4.2 Overflow / variable-length records** | skim | you saw SQLite's version |
| §5.1–5.2 Node sizes | read | why 4KB? (it's not sacred — CPU cache vs disk trade; big nodes + in-node structure) |

## Defer (note where, come back later)

- §6 latching & B-link trees → topic 9 (concurrency)
- §7 logging & recovery interplay (fence keys, ghost records) → topic 5
- §8 bulk load / index creation → topic 12/22

## The three ideas to extract

```
1. suffix truncation:  separator between "smith,bob" and "smyth,al"
                       needs only "smy" — interior keys shrink ⇒ fanout grows
                       ⇒ height shrinks ⇒ every lookup saves a page

2. prefix truncation:  page stores common prefix once
   page ["foo/aaa".."foo/zzz"]: header prefix="foo/", cells store "aaa"…

3. normalized keys:    encode (type,collation,composite) into memcmp-able bytes
                       — comparison becomes branch-free byte compare (SIMD-able,
                       topic 17)
```

Fanout arithmetic to internalize: 4KB page, 16-byte keys + 8-byte child pointers
≈ 170 fanout ⇒ 1B keys in height 4. Truncate separators to 4 bytes ⇒ fanout ~340
⇒ height 4 still, but at 10B keys. **Height is the metric; fanout is the lever;
key size is what you control.**

## Questions to answer in notes.md

1. Why does suffix truncation apply to interior separators but prefix truncation
   mostly to leaf pages? (Separators are synthetic; leaf keys must be exact.)
2. SQLite/turso do neither. Given SQLite's design goals (simplicity, robustness,
   integer rowids as the common key), argue whether that's the right call.
3. Poor man's normalized key = SwissTable h2 = skiplist tower = pointer-array-as-
   filter. Write the general principle in one sentence for the capstone notes.

## Done when

You can do the fanout→height arithmetic cold, and you've marked which sections
you'll return to in topics 5 and 9.
