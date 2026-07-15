# ART: sorted like a tree, probed like a hash table

The index inside HyPer and DuckDB — a radix tree tuned until it beats hash
tables on some workloads *while staying sorted*. Where rax spends its design
budget on memory, ART spends it on lookup speed: node layouts that adapt to
fanout, each picking the cheapest search its density allows. It is also where
this topic's SwissTable and radix-tree threads literally meet, in Node16's
SIMD probe. This chapter builds the paper's ideas one at a time — the
sparse-node waste, the four adaptive layouts, the two compression tricks, the
key encoding that makes everything radix-able — then routes you through the
sections.

## The problem in one sentence

A radix tree that branches on full bytes needs 256 child pointers per node —
2 KB — and a real node averages a handful of children, so a naive
main-memory radix index burns **~98% of its space on null pointers**; shrink
the nodes naively and every level becomes a search, i.e. a B-tree with extra
steps.

## The concepts, step by step

### Step 1 — the tension: radix depth vs radix memory

Recall from the rax chapter: a radix tree finds keys by *spelling* them —
one branch decision per key byte, depth = key length, no comparisons, no
hashing. Branching on a full byte (span 8 bits) keeps depth minimal — ≤8
levels for an 8-byte integer key, regardless of n — but demands room for 256
children per node. Binary-comparison trees (B-tree, T-tree) have the
opposite problem: compact nodes, but every level costs a key comparison plus
a dependent cache miss, and depth grows as log₂(n). ART's move: keep the
byte-wise branching, but **make the node's physical size adapt to how many
children it actually has**.

### Step 2 — the four node types: one logical node, four layouts

An ART node is logically always "up to 256 children indexed by byte"; its
physical layout is whichever of four types fits the current child count
(§III.A — the core of the paper):

```
Node4        keys[4]   ┌k┬k┬k┬k┐          linear scan, fits in
             ptrs[4]   └●┴●┴●┴●┘          one cache line

Node16       keys[16]  ┌k×16────────┐     SIMD compare — literally the
             ptrs[16]  └●×16────────┘     SwissTable group probe trick

Node48       index[256]┌256 × 1-byte ─┐   byte-indexed indirection:
             ptrs[48]  └48 × 8-byte  ─┘   index[c] → slot in ptrs
             
Node256      ptrs[256] ┌●×256────────┐    direct array — no search at all
```

Nodes grow and shrink between types as children are added or removed — a
Node4 gaining a fifth child is copied into a Node16, and so on. Space now
tracks density: a 3-child node costs ~56 bytes, not 2 KB.

### Step 3 — search strategy per type: pay only what density demands

Each layout picks the cheapest search its density allows — the progression is
linear → SIMD → indexed → direct, and one `match` carries the whole idea:

```rust
fn find_child(node: &Node, byte: u8) -> Option<&Node> {
    match node {
        Node4 { keys, ptrs, n } =>              // ≤4 children: linear scan,
            (0..*n).find(|&i| keys[i] == byte)  //   one cache line
                   .map(|i| &ptrs[i]),
        Node16 { keys, ptrs, .. } => {
            let hits = simd_eq(keys, byte);     // the SwissTable group probe
            one_bit(hits).map(|i| &ptrs[i])     //   (≤1 hit here: keys unique)
        }
        Node48 { index, ptrs } =>               // byte-indexed indirection
            slot(index[byte as usize]).map(|s| &ptrs[s]),
        Node256 { ptrs } =>                     // direct — no search at all
            ptrs[byte as usize].as_ref(),
    }
}
```

Node16 is where the topic's two threads meet: compare 16 candidate bytes
against one search byte in a single SIMD instruction — the SwissTable group
probe, reused as tree navigation. Cost per level in every case: at most one
or two cache lines and no branch mispredictions worth naming.

### Step 4 — lazy expansion and path compression: kill the boring levels

Two tricks remove nodes that exist only to spell out bytes (§III.B) — both
are rax ideas with fixed-size discipline:

- **Lazy expansion**: a subtree containing a *single* key isn't expanded at
  all — the leaf stores the key's remaining bytes. (rax equivalent: storing
  the key tail as a compressed run.)
- **Path compression**: a chain of one-child inner nodes is collapsed; each
  node carries a **prefix** of the skipped bytes. (rax's `iscompr`.) ART caps
  the stored prefix at 8 bytes — beyond that it goes "pessimistic": skip the
  bytes optimistically and re-check the full key at the leaf. Why cap it?
  Fixed-size node headers — ART refuses variable-size node layouts, which is
  exactly the trade rax took the other way.

Together these make depth ≈ number of *distinguishing* bytes, not key
length.

### Step 5 — binary-comparable keys: the encoding that makes it universal

A radix tree returns keys in **byte order**, so for sorted iteration and
range scans to be *correct*, byte order must equal logical order. §III.E +
§IV show the transformations: store integers big-endian (most significant
byte first), flip the sign bit for signed ints, massage IEEE floats, null-
terminate strings, concatenate fields for composite keys. Example: as
little-endian bytes, 256 (`00 01 00 ...`) sorts *before* 1 (`01 00 ...`) —
big-endian fixes it. Don't skip this section: the idea is everywhere —
RocksDB comparators, FoundationDB tuples, and your capstone's composite
(entity, attr) keys in M2 are all binary-comparable encodings.

### Step 6 — the space guarantee: 52 bytes per key, worst case

Adaptive nodes plus path compression buy a provable bound (§III.B):
worst-case **52 bytes per key** regardless of key distribution — no
adversarial key set can blow the structure up. Compare: your skiplist's
per-node cost (1.33 pointers average + key) is fine on average but has no
such bound story, and a naive radix tree has no bound at all. Bounds like
this are what let a database *promise* memory budgets.

## How to read the paper (with the concepts in hand)

1. **§III.A–B** — node types (Steps 2–3) + lazy expansion / path compression
   (Step 4). Map both tricks onto rax as you read; note where ART's 8-byte
   prefix cap diverges from rax's unbounded runs and why.
2. **§III.C–D** — insert/delete with node-type transitions. Skim — it's
   Step 2's grow/shrink mechanics spelled out.
3. **§III.E + §IV — binary-comparable keys** (Step 5). Don't skip; work the
   encodings until you could encode (u64, u16) pairs cold.
4. **§V — evaluation.** Read Fig. 8/9 with topic-0 eyes: where does ART beat
   the hash table (dense integer keys — short paths, no hash cost) and where
   does it lose (long random strings — depth ∝ length)?

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

## References

**Papers**
- Leis, Kemper, Neumann — "The Adaptive Radix Tree: ARTful Indexing for
  Main-Memory Databases" (ICDE 2013) —
  [PDF](https://db.in.tum.de/~leis/papers/ART.pdf) — ~2 h; §III.A is the
  core, don't skip §III.E/§IV (binary-comparable keys), read §V's
  figures with topic-0 eyes
