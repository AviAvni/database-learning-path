# rax: a radix tree packed into cache lines

Redis's compressed radix tree — behind stream IDs, client tracking keys, and
cluster slot→key maps — is what a trie looks like when memory is the corner
of the RUM triangle you're defending: one variable-size node layout,
deliberately unaligned pointers, path-compressed runs. This chapter builds
the trie idea from zero, compresses it, then packs it byte by byte the way
rax does — before sending you into the layout comment and the walk. Read for
the *layout* (~45 min, skim the insert logic); it's the memory-first contrast
case for the ART paper that follows.

## The problem in one sentence

Redis keeps *millions* of small string-keyed maps (one per stream, per
client-tracking table, per cluster slot), so a per-node overhead of even 48
bytes — a textbook trie node — multiplies into gigabytes; the index must cost
close to the bytes of the keys themselves.

## The concepts, step by step

### Step 1 — the trie: the key's bytes ARE the path

A **trie** (radix tree) is a tree where you find a key not by *comparing*
keys but by *spelling* them: each node branches on the next byte of the key,
so the path from the root spells the key out. Lookup depth = key length, not
log n; there is no hash function and no full-key comparisons — just one
branch decision per byte:

```
keys "foo", "for":        root
                           │f
                          [f]
                           │o
                          [o]
                          / \
                        o    r          depth = key length (3),
                       [●]  [●]         independent of how many keys exist
```

What you gain over a hash table: sorted iteration and prefix scans for free
(all keys under "fo" live in one subtree — topic 23's inverted index will
want this). What it costs so far: one node *per byte* of every key — a
3-level chain of allocations to store "foo". That's the memory disaster to
fix.

### Step 2 — path compression: collapse single-child chains into runs

Most trie nodes in real data have exactly one child (long unique key tails,
shared prefixes) — a chain of one-child nodes spelling "oot" is pure
overhead. **Path compression** replaces any such chain with a single node
holding the whole byte run:

```
radix tree (rax), keys "foo", "foobar", "footer":

        [f o o]  ← compressed run (iscompr): one node holds the shared prefix
           │
        (key: "foo")
         ┌─┴──┐
        [b]  [t]
         │    │
       [a r] [e r]   compressed tails
```

Now depth ≈ the number of *branch points*, not key length, and node count ≈
distinct branches. A compressed node stores a multi-byte run ("foo") with a
**single** child pointer. The remaining question is what one node costs in
bytes — rax's real contribution.

### Step 3 — the node: a 4-byte header and one flexible array

rax spends four bytes of header, then packs **everything** — child bytes,
child pointers, and the optional value pointer — into one flexible array in
a single allocation. `rax.h:78–111`:

```c
typedef struct raxNode {
    uint32_t iskey:1;     /* this node terminates a key             */
    uint32_t isnull:1;    /* key has no associated value            */
    uint32_t iscompr:1;   /* node is a compressed run               */
    uint32_t size:29;     /* # children (or run length if iscompr)  */
    unsigned char data[]; /* EVERYTHING else lives here             */
} raxNode;
```

```
non-compressed, size=3 ("abc" branches):        compressed run "xyz" (iscompr=1):

┌header┐┌── data[] ─────────────────────┐       ┌header┐┌── data[] ────────────┐
│4 bytes││a b c pad│ A* │ B* │ C* │ V*? │       │4 bytes││x y z pad│ Z* │ V*?  │
└──────┘└─────────┴────┴────┴────┴─────┘       └──────┘└─────────┴────┴──────┘
          ▲ char bytes first (dense filter!)      whole run = ONE child pointer
          then pointers, then value if iskey       (points past the run)
```

The layout comment at rax.h:83–109 is the spec — read it in full. Note the
order: the branch *characters* come first, densely packed, then the
pointers. Choosing a branch scans only the char bytes — the same "dense
filter, fat payload" move as SwissTable control bytes (README §4): the data
you probe is dense; the data you follow is touched once, on a match.

### Step 4 — unaligned pointers, on purpose

Because chars come first and there's no padding, the 8-byte child pointers
in `data[]` may start at **any byte offset** — they are not aligned. Redis
reads and writes them with `memcpy` (the `raxNodeFirstChildPtr` /
`raxNodeLastChildPtr` helpers; rax.h:90, 99). Why tolerate that?

- One allocation per node; header + chars + pointers usually fit **one cache
  line** for small fanouts (4 + 3 + 3×8 = 31 bytes for the 3-child node
  above).
- Alignment padding would spread the node across lines; modern ARM/x86 do
  unaligned loads nearly free, so the cache line saved is worth more than
  the alignment lost.

A deliberate trade of CPU convention for memory locality — the whole chapter
in one decision.

### Step 5 — the walk: the tree's entire read path

Every rax operation starts with `raxLowWalk`: consume the key byte by byte,
scanning char bytes in branching nodes and matching prefixes in compressed
runs. It returns how much of the key it consumed and — crucially for insert —
`splitpos`, where the key diverged *inside* a compressed run:

```rust
// returns (bytes of key consumed, split position inside a compressed run)
fn low_walk(mut node: &RaxNode, key: &[u8]) -> (usize, usize) {
    let mut i = 0;
    while i < key.len() {
        if node.iscompr() {
            let run = node.chars();                  // e.g. "oot" — one node
            let m = common_prefix(run, &key[i..]);
            if m < run.len() { return (i + m, m); }  // diverged MID-run: splitpos
            i += m;
            node = node.child(0);                    // whole run = ONE pointer
        } else {
            match node.chars().iter().position(|&c| c == key[i]) { // dense scan:
                Some(j) => { node = node.child(j); i += 1; }       //   chars only,
                None => return (i, 0),                             //   ptrs untouched
            }
        }
    }
    (i, 0)      // consumed the whole key: node.iskey ⇒ hit
}
```

Cost model: one dependent pointer hop per *node* (not per byte, thanks to
compression), and within a node the scan touches only the dense char prefix.

### Step 6 — insert = split machinery

Inserting a key that diverges mid-run must cut the run at `splitpos`, create
a small branching node, and re-hang the tails. `raxGenericInsert`
(rax.c:515–658, skim) enumerates the cases in its long comment; the picture:

```
insert "first" into node ["footer"]:   split the run at splitpos=1
              [f]                ← shared prefix survives as run (or single node)
             ┌─┴─┐
        ["ooter"] ["irst"]       ← two compressed tails, new branching node
```

Every case is "cut the run, make a 2-child branching node, re-hang the
tails". Don't memorize the five cases — just verify the invariant: **after
any insert, no node has exactly one child unless it's compressed** (otherwise
it would be merged into a run). That invariant is what keeps Step 2's
compression from decaying under writes.

### Step 7 — the contrast: rax vs ART, opposite RUM corners

The next chapter's ART is the same structure tuned for the opposite corner:

| | rax | ART (Leis 2013) |
|---|-----|-----|
| node sizes | one variable-size layout | adaptive Node4/16/48/256 |
| child search | linear scan of char bytes | SIMD (Node16), direct index (Node256) |
| pointers | unaligned, memcpy'd | aligned arrays |
| optimized for | memory (redis: millions of tiny trees) | lookup speed (main-memory index) |

Same structure, opposite RUM corner: rax minimizes M, ART minimizes R. Keep
this table in mind while reading the paper.

## Where each step lives in the code

- **Steps 3–4** — `raxNode` struct: rax.h:78–111; the layout spec comment:
  rax.h:83–109 (read in full before any function); unaligned-pointer helpers
  `raxNodeFirstChildPtr` / `raxNodeLastChildPtr`: rax.h:90, 99.
- **Step 5** — `raxLowWalk`: the read path every operation shares.
- **Step 6** — `raxGenericInsert`: rax.c:515–658 (skim; the case-enumeration
  comment above the code is the map).

## Questions to answer in notes.md

1. Why does rax put the char bytes *before* the pointers instead of interleaving
   (char,ptr) pairs? (Branch decision reads only chars — one dense scan.)
2. A radix tree has no hash function and no key comparisons — what does it give
   up vs a hash table? (Point-lookup cost ∝ key length; but you gain prefix scans
   and ordered iteration — which topic 23's inverted index will want.)

## Done when

You can sketch a compressed vs non-compressed node's `data[]` layout from memory
and say why the pointers are unaligned on purpose.

## References

**Code**
- [redis](https://github.com/redis/redis) `src/rax.h`, `src/rax.c` —
  the layout comment at rax.h:83–109 is the spec; read it in full before
  the functions
