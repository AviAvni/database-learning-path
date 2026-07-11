# Reading guide — "The Ubiquitous B-Tree" (Comer, ACM Computing Surveys 1979)

Forty-five years old and still the cleanest B-tree exposition in print. ~15 pages,
2 h. Everything in turso's `btree.rs` is a footnote to this paper.

## Read in this order

1. **§1–2 (the problem + the structure)** — why balanced trees on disk need high
   fanout: tree height = number of IOs, and height = log_fanout(n). A 4KB page
   holding ~100 keys ⇒ 1 billion rows in height 5, of which 3–4 levels cache-resident.
   This is the whole game.
2. **§2.1–2.2 (insertion/deletion)** — split on overflow, merge/borrow on underflow.
   Map to turso: `balance_non_root` (btree.rs:2995) is the "borrow from siblings
   first" refinement — Comer calls redistribution out as reducing splits.
3. **§3 (B+-tree, B*-tree variants)** — the section that matters most:

```
B-tree:  keys+values in ALL nodes          B+tree: values ONLY in leaves
         ┌─────k,v─────┐                          ┌──────k──────┐  routing only
      ┌─k,v─┐       ┌─k,v─┐                    ┌──k──┐       ┌──k──┐
      ...                                     [k,v|k,v] ↔ [k,v|k,v]  linked leaves
                                                     └── range scan = list walk
```

   Why every real engine chose B+: (a) interior nodes hold only keys → higher fanout
   → shorter tree; (b) leaf-level linked list → range scans without re-descending;
   (c) uniform "all data at leaf depth" simplifies everything.
4. **§4 (applications: VSAM, etc.)** — skim for flavor; 1979's product landscape.

## Questions to answer in notes.md

1. Why do B-trees guarantee ≥50% page occupancy, and what's the *measured average*
   (~69%, ln 2)? Connect to space amplification in the README.
2. B*-tree defers splits by redistributing into siblings. What does turso implement —
   B+, B*, or a hybrid?
3. Comer's B-trees assume one page write is atomic. It isn't (torn writes). Which
   later machinery patches this hole? (WAL — topic 5; checksums — topic 3.)

## The one-line takeaway

The B-tree is the memory hierarchy turned into a data structure: node size = transfer
unit, fanout = whatever fits, height = the IO budget.
