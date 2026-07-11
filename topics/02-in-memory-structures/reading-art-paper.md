# Reading guide — Leis et al., "The Adaptive Radix Tree" (ICDE 2013)

Paper: *The Adaptive Radix Tree: ARTful Indexing for Main-Memory Databases*,
Leis, Kemper, Neumann. ~2 h. This is the index inside HyPer and DuckDB — a radix
tree tuned until it beats hash tables on some workloads *while staying sorted*.

## The problem it solves

Plain radix trees waste memory: a 256-pointer node with 3 children is 2KB of
nulls. Binary-comparison trees (B-tree, T-tree) waste *time*: every level is a
key comparison + dependent cache miss. ART's move: **make node size adapt to
fanout**, so space ≈ compact and depth ≈ radix.

## The four node types (§III.A — the core of the paper)

```
Node4        keys[4]   ┌k┬k┬k┬k┐          linear scan, fits in
             ptrs[4]   └●┴●┴●┴●┘          one cache line

Node16       keys[16]  ┌k×16────────┐     SIMD compare — literally the
             ptrs[16]  └●×16────────┘     SwissTable group probe trick

Node48       index[256]┌256 × 1-byte ─┐   byte-indexed indirection:
             ptrs[48]  └48 × 8-byte  ─┘   index[c] → slot in ptrs

Node256      ptrs[256] ┌●×256────────┐    direct array — no search at all
```

Nodes grow/shrink between types as children are added/removed. Note the
progression of *search strategy*: linear → SIMD → indexed → direct. Each type
picks the cheapest search its density allows.

## Reading order

1. **§III.A–B** — node types + lazy expansion / path compression. Map both onto
   rax: lazy expansion ≈ rax storing the key tail in a compressed node;
   path compression ≈ `iscompr`. ART's per-node prefix is capped (8 bytes,
   "pessimistic" overflow re-checks the full key) — rax's is unbounded. Why does
   ART cap it? (Fixed-size headers ⇒ no variable-size node layouts.)
2. **§III.C–D** — insert/delete with node-type transitions. Skim.
3. **§III.E + §IV — binary-comparable keys.** Don't skip this. To make ints,
   floats, strings radix-able you transform them so bytewise order = logical
   order (flip sign bit, big-endian, etc.). This idea is *everywhere*: RocksDB
   comparators, FoundationDB tuples, your capstone's composite (entity,attr)
   keys in M2.
4. **§V — evaluation.** Read Fig. 8/9 with topic-0 eyes: where does ART beat the
   hash table (dense integer keys — short paths, no hash cost) and where does it
   lose (long random strings — depth ∝ length)?

## Space guarantee worth remembering

§III.B proves worst-case **52 bytes per key** regardless of key distribution —
the adaptive nodes + path compression make the bound possible. Compare: your
skiplist's per-node cost (1.33 pointers avg + key) has no such bound story.

## Questions to answer in notes.md

1. Node16 search is the SwissTable group probe (compare 16 bytes in one SIMD op).
   What's the *structural* difference between how ART and SwissTable use the
   result? (ART: index into child pointers; Swiss: candidate slots to verify.)
2. Height of ART on 8-byte integer keys is ≤ 8 regardless of n. At what n does
   log₂(n) exceed that — i.e., where does a B-tree start losing on depth alone?
3. For the capstone: would ART beat your M2 hash-based attribute store for
   (entity id, attr id) → value? Sketch the key encoding and the RUM trade.

## Done when

You can name the four node types with their search strategies from memory, and
explain binary-comparable key encoding well enough to encode (u64, u16) pairs.
