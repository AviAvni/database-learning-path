# rax: a radix tree packed into cache lines

Redis's compressed radix tree — behind stream IDs, client tracking keys, and
cluster slot→key maps — is what a trie looks like when memory is the corner
of the RUM triangle you're defending: one variable-size node layout,
deliberately unaligned pointers, path-compressed runs. Read for the *layout*
(~45 min, skim the insert logic); it's the memory-first contrast case for the
ART paper that follows.

## 1. The node — rax.h:78–111

```c
typedef struct raxNode {
    uint32_t iskey:1;     /* this node terminates a key             */
    uint32_t isnull:1;    /* key has no associated value            */
    uint32_t iscompr:1;   /* node is a compressed run               */
    uint32_t size:29;     /* # children (or run length if iscompr)  */
    unsigned char data[]; /* EVERYTHING else lives here             */
} raxNode;
```

Four bytes of header, then one flexible array holding **child bytes, child
pointers, and the optional value pointer**, all packed:

```
non-compressed, size=3 ("abc" branches):        compressed run "xyz" (iscompr=1):

┌header┐┌── data[] ─────────────────────┐       ┌header┐┌── data[] ────────────┐
│4 bytes││a b c pad│ A* │ B* │ C* │ V*? │       │4 bytes││x y z pad│ Z* │ V*?  │
└──────┘└─────────┴────┴────┴────┴─────┘       └──────┘└─────────┴────┴──────┘
          ▲ char bytes first (dense filter!)      whole run = ONE child pointer
          then pointers, then value if iskey       (points past the run)
```

- Layout comment at rax.h:83–109 — read it in full; it's the spec.
- A compressed node stores a multi-byte run ("foo") with a **single** child
  pointer — that's the path compression that keeps depth ≈ distinct branches,
  not key length.

## 2. The unaligned-pointer aha — rax.h:90, 99

The child pointers in `data[]` are **not aligned**: chars come first, so a pointer
may start at any byte offset. Redis reads/writes them with `memcpy`
(`raxNodeLastChildPtr`, `raxNodeFirstChildPtr` helpers). Why tolerate that?

- One allocation per node; header + chars + pointers usually fit **one cache line**
  for small fanouts.
- Scanning the char bytes to pick a branch touches only the dense prefix of the
  node — same "dense filter, fat payload" move as SwissTable control bytes
  (README §4). Alignment padding would spread the node across lines.

Modern ARM/x86 do unaligned loads nearly free; the cache line saved is worth more.

## 3. Insert = split machinery — rax.c:515–658 (skim)

`raxGenericInsert` walks with `raxLowWalk`, which returns `splitpos` — where the
new key diverges *inside* a compressed run. The walk itself is the tree's whole
read path:

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

The long comment before the insert code
enumerates the cases; the picture:

```
insert "first" into node ["footer"]:   split the run at splitpos=1
              [f]                ← shared prefix survives as run (or single node)
             ┌─┴─┐
        ["ooter"] ["irst"]       ← two compressed tails, new branching node
```

Every case is "cut the run, make a 2-child branching node, re-hang the tails".
Don't memorize the five cases — just verify the invariant: **after any insert,
no node has exactly one child unless it's compressed** (otherwise it would be
merged into a run).

## 4. Contrast with ART (next reading)

| | rax | ART (Leis 2013) |
|---|-----|-----|
| node sizes | one variable-size layout | adaptive Node4/16/48/256 |
| child search | linear scan of char bytes | SIMD (Node16), direct index (Node256) |
| pointers | unaligned, memcpy'd | aligned arrays |
| optimized for | memory (redis: millions of tiny trees) | lookup speed (main-memory index) |

Same structure, opposite RUM corner: rax minimizes M, ART minimizes R.

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
