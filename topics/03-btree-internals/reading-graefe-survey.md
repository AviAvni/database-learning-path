# Modern B-tree techniques: height is the metric, fanout is the lever

Every "B-trees are simple" take dies in Graefe's ~200-page survey of what
production B-trees actually do — compression, latching, logging interactions,
bulk loads. **Do not read it all.** This chapter builds the survey's core
ideas one step at a time — the fanout arithmetic first, then the three
key-compression tricks that move it — and then hands you the ~50 pages that
matter for this topic and the capstone (budget: 3 h); you'll come back for
more in topics 5 (logging), 8/9 (latching), and 12 (columnar).

## The problem in one sentence

A B-tree lookup pays one page read per level of the tree, so on a
billion-key tree every byte you shave off the keys stored in interior pages
raises fanout — and shaving 16-byte keys down to 4-byte separators is the
difference between height 4 holding **1 billion** keys and holding **10
billion**.

## The concepts, step by step

### Step 1 — height is priced in page reads

A B-tree stores its keys in fixed-size **pages** (disk blocks, typically
4 KB): **interior pages** hold keys plus child-page pointers, **leaf pages**
hold the actual data, and a point lookup reads one page per level from root
to leaf. The number of children an interior page holds is the **fanout**, and
the tree's **height** — the number of levels — is log-base-fanout of the key
count.

The arithmetic to internalize: 4KB page, 16-byte keys + 8-byte child pointers
≈ 170 fanout ⇒ 170⁴ ≈ 1B keys in height 4.

Why it matters: height is the *only* number a lookup pays for, and it moves
in whole units — an entire page read gained or lost across *every* lookup.
Every technique in this survey is ultimately a lever on fanout, hence on
height.

### Step 2 — separators are synthetic: suffix truncation

An interior page never needs to store real keys — it stores **separators**,
values whose only job is to route a search left or right, so a separator
between two leaf keys can be *any* string that sorts between them, including
one much shorter than either.

```
suffix truncation:  separator between "smith,bob" and "smyth,al"
                    needs only "smy" — interior keys shrink ⇒ fanout grows
                    ⇒ height shrinks ⇒ every lookup saves a page
```

Suffix truncation is small enough to write down whole — the point is that a
separator is *synthetic*, so it only has to sort between its neighbors:

```rust
// separator between leaf keys "smith,bob" and "smyth,al" → "smy"
fn shortest_separator(left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut i = 0;
    while i < left.len() && left[i] == right[i] {
        i += 1;                       // skip the shared prefix
    }
    right[..=i].to_vec()              // one byte past divergence: > left, ≤ right
}
// shorter separators ⇒ more fit per interior page ⇒ fanout up ⇒ height down —
// and height is priced in page reads, so EVERY lookup collects the saving
```

Redo Step 1's arithmetic with truncation: separators cut to 4 bytes ⇒ fanout
~340 ⇒ height 4 still, but now at 10B keys. **Height is the metric; fanout is
the lever; key size is what you control.** This is your experiment for this
topic — and note that separators need only be *shorter than both neighbors*,
not real keys. (Survey §3.1–3.3.)

### Step 3 — prefix truncation: store the shared prefix once

Leaf keys must stay exact — they *are* the data — so leaves compress
differently: when every key on a page shares a common prefix, store that
prefix once in the page header and keep only the distinct tails in the cells
(the per-key entries within a page).

```
prefix truncation:  page stores common prefix once
  page ["foo/aaa".."foo/zzz"]: header prefix="foo/", cells store "aaa"…
```

Same lever, different region of the tree: more keys per leaf ⇒ fewer leaves ⇒
fewer interior entries ⇒ (eventually) a shorter tree. The cost: the common
prefix must be recomputed whenever a split or merge changes the page's key
range. (Survey §3.1–3.3.)

### Step 4 — normalized keys: comparison becomes one memcmp

A **normalized key** is a re-encoding of a typed, possibly composite key
(say, an integer column plus a case-insensitive string column) into a single
byte string whose plain byte-by-byte order equals the intended sort order —
so one branch-free `memcmp` replaces a typed comparison that dispatches on
column types and collations.

```
normalized keys:    encode (type,collation,composite) into memcmp-able bytes
                    — comparison becomes branch-free byte compare (SIMD-able,
                    topic 17)
```

You've met this idea already: it's the binary-comparable encoding of ART
§III.E. Why it matters: within a page the lookup cost is CPU comparisons, not
IO, and a branch-free byte compare is what hardware executes fast — and what
SIMD can widen later (topic 17). (Survey §3.4.)

### Step 5 — poor man's normalized key: a filter inside the pointer array

Cache the first few bytes of each cell's normalized key directly inside that
cell's slot in the page's pointer array (the small sorted array of cell
offsets), so binary search usually decides from the slot alone and touches
the actual cell only on a near-tie.

This is the dense-filter pattern yet again — first bytes of the key cached
IN the pointer array slot, the same move as SwissTable's h2 byte and the
skiplist tower. Why it matters: the pointer array is contiguous and hot in
cache; the cells are scattered across the page — each avoided dereference is
an avoided cache miss. (Survey §3.5. Question 3 below asks you to state the
general principle in one sentence.)

### Step 6 — node size is a trade, not a constant

Nothing makes 4 KB sacred: page size trades IO efficiency (bigger pages
amortize seeks and raise fanout) against CPU cache behavior (binary search
over a huge page thrashes cache lines) and write cost (one dirty byte
rewrites the whole page).

The survey's resolution (§5.1–5.2): big nodes *plus in-node structure* — a
mini-index inside the page — get both: large IO units for the disk,
cache-sized search steps for the CPU. Hold onto this when topic 12 makes
columnar pages megabytes wide.

## How to read the paper (with the concepts in hand)

Read now (this topic):

| Section | Pages (approx) | Why |
|---|---|---|
| §2 Basic techniques | skim | Step 1 — you know this from the code |
| **§3.1–3.3 Prefix + suffix truncation** | read | Steps 2–3; your experiment; separators need only be *shorter than both neighbors*, not real keys |
| **§3.4 Normalized keys** | read | Step 4; binary-comparable encoding again (ART §III.E) — one memcmp replaces typed comparison |
| §3.5 Poor man's normalized key | read | Step 5; first bytes of the key cached IN the pointer array slot — dense filter pattern yet again |
| **§4.2 Overflow / variable-length records** | skim | you saw SQLite's version |
| §5.1–5.2 Node sizes | read | Step 6; why 4KB? (it's not sacred — CPU cache vs disk trade; big nodes + in-node structure) |

Defer (note where, come back later):

- §6 latching & B-link trees → topic 9 (concurrency)
- §7 logging & recovery interplay (fence keys, ghost records) → topic 5
- §8 bulk load / index creation → topic 12/22

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

## References

**Papers**
- Graefe — "Modern B-Tree Techniques" (Foundations and Trends in
  Databases, 2011) — ~200 pages; do NOT read it all — follow the
  section table above (§3 truncation + normalized keys and §5 node
  sizes now; §6–§8 deferred to topics 9, 5, and 12/22)
