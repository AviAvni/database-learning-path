# rax: a radix tree packed into cache lines

Redis's compressed radix tree — behind stream entries, client-tracking tables,
the blocked-client timeout index and the errors table — is what a trie looks
like when memory is the corner of the RUM triangle you are defending: one
variable-size node layout sized to the byte, path-compressed runs, a padding
rule that buys pointer alignment for at most seven bytes, and — when every key
has the same length — no leaf node at all. This chapter builds the trie idea
from zero, compresses it, then packs it byte by byte the way rax does — before
sending you into the layout comment and the walk. Read for the *layout*
(~45 min, skim the insert logic); it is the memory-first contrast case for the
ART paper that follows.

Everything below is read against **redis/redis at `e87dda2a4`** (the
`unstable` branch), where `src/rax.h` is 268 lines and `src/rax.c` is 3028.
Line numbers move between releases; re-check yours with:

```
tools/pinned-source.py ref redis
tools/pinned-source.py show redis src/rax.h -r 87:136
tools/pinned-source.py show redis src/rax.c -r 126:155
```

If a number below does not match your checkout, trust the checkout and record
the drift in `notes.md` — that is the exercise.

## The problem in one sentence

Redis keeps *millions* of small string-keyed maps — one radix tree per stream,
one per tracking prefix, one for every blocked client's timeout — so a
per-node overhead of even 48 bytes multiplies into gigabytes; the index must
cost close to the bytes of the keys themselves.

## The concepts, step by step

### Step 1 — the trie: the key's bytes ARE the path

> **In:** a set of byte-string keys and the need to look one up.
> **Out:** a structure with no hash function and no whole-key comparisons —
> depth proportional to key length — and one glaring cost: a node per byte.

A **trie** (radix tree) finds a key not by *comparing* keys but by *spelling*
them: each edge is labelled with the next byte, so the path from the root
spells the key out. Lookup depth is key length, not log n; there is no hash
function and no full-key comparison — just one branch decision per byte:

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

Note where the character lives. rax's own header is emphatic about it, because
it is the thing everybody gets backwards: the character is stored **in the
parent's edge**, not in the child.

What you gain over a hash table: sorted iteration and prefix scans for free —
every key beginning "fo" lives in one subtree (topic 23's inverted index will
want exactly this). What it costs so far: one node *per byte* of every key. A
three-level chain of allocations to store "foo". That is the memory disaster
the rest of the chapter fixes.

### Step 2 — path compression: collapse single-child chains into runs

> **In:** the per-byte node chain from Step 1.
> **Out:** one node per *run* of bytes, so node count tracks branch points
> rather than key length — and the exact invariant that keeps it that way
> under writes.

Most trie nodes in real data have exactly one child — long unique key tails
and shared prefixes both produce chains. **Path compression** replaces any
such chain with a single node holding the whole byte run. rax's header draws
it for the keys "foo", "foobar", "footer":

```
// src/rax.h — header comment, the compressed representation, 44-50
    44   *                  ["foo"] ""
    45   *                     |
    46   *                  [t   b] "foo"
    47   *                  /     \
    48   *        "foot" ("er")    ("ar") "foob"
    49   *                 /          \
    50   *       "footer" []          [] "foobar"
```

Square brackets mark a node that **is a key**, parentheses one that is not
(`rax.h:18-19`); a compressed node shows its whole run inside the delimiters.
Six nodes for three keys, where the uncompressed trie at `rax.h:23-35` needed
ten. Depth is now the number of *branch points*, not the key length.

The invariant that keeps compression from decaying is stated where it is
enforced — on the *delete* side, not the insert side:

```
// src/rax.c — recompression rationale in raxRemove, 1440-1447
  1440      /* Recompression: if trycompress is true, 'h' points to a radix tree node
  1441       * that changed in a way that could allow to compress nodes in this
  1442       * sub-branch. Compressed nodes represent chains of nodes that are not
  1443       * keys and have a single child, so there are two deletion events that
  1444       * may alter the tree so that further compression is needed:
  1445       *
  1446       * 1) A node with a single child was a key and now no longer is a key.
  1447       * 2) A node with two children now has just one child.
```

Read lines 1442-1443 carefully: a compressible chain is nodes that are **not
keys** *and* have a single child. Both clauses matter. A single-child node
that *is* a key cannot be folded into a run, because a run has one value slot
at its end and nowhere to hang a value in the middle. Insert splits runs apart
(Step 6); delete is where they get glued back (`raxRemove`: the loop at
1492-1498 walks up to the highest compressible node, and the loop at 1515-1528
then walks forward along the chain collecting it). Insertion never needs to
merge, because it only ever adds branch points.

### Step 3 — the node: a 4-byte header and one flexible array

> **In:** the compressed tree from Step 2, still made of unspecified "nodes".
> **Out:** the concrete byte layout — a 32-bit header and a single flexible
> array holding characters, then child pointers, then an optional value
> pointer — and the size formula that follows from it.

rax spends **four bytes** of header, then packs everything else into one
flexible array in a single allocation:

```
// src/rax.h — raxNode, 87-92 and 120-121
    87  #define RAX_NODE_MAX_SIZE ((1<<29)-1)
    88  typedef struct raxNode {
    89      uint32_t iskey:1;     /* Does this node contain a key? */
    90      uint32_t isnull:1;    /* Associated value is NULL (don't store it). */
    91      uint32_t iscompr:1;   /* Node is compressed. */
    92      uint32_t size:29;     /* Number of children, or compressed string len. */
    ... 93-119: the data layout comment — the spec, quoted next ...
   120      unsigned char data[];
   121  } raxNode;
```

Three bits and a 29-bit count in one word, so `sizeof(raxNode)` is 4 and
`RAX_NODE_MAX_SIZE` = 2²⁹ − 1 = **536,870,911** — the largest fanout or
compressed run the `size` field can express. `isnull` earns its bit: a key
whose value is `NULL` stores no value pointer at all, saving 8 bytes on the
very common "membership set" use.

The layout comment is the spec. Read it in full before any function:

```
// src/rax.h — data layout comment, 93-118
    93      /* Data layout is as follows:
    ... 94: blank comment line ...
    95       * If node is not compressed we have 'size' bytes, one for each children
    96       * character, and 'size' raxNode pointers, point to each child node.
    97       * Note how the character is not stored in the children but in the
    98       * edge of the parents:
    99       *
   100       * [header iscompr=0][abc][a-ptr][b-ptr][c-ptr](value-ptr?)
   101       *
   102       * if node is compressed (iscompr bit is 1) the node has 1 child.
   103       * In that case the 'size' bytes of the string stored immediately at
   104       * the start of the data section, represent a sequence of successive
   105       * nodes linked one after the other, for which only the last one in
   106       * the sequence is actually represented as a node, and pointed to by
   107       * the current compressed node.
   108       *
   109       * [header iscompr=1][xyz][z-ptr](value-ptr?)
   ... 110-114: both kinds can carry a key at any level ...
   115       * If the node has an associated key (iskey=1) and is not NULL
   116       * (isnull=0), then after the raxNode pointers pointing to the
   117       * children, an additional value pointer is present (as you can see
   118       * in the representation above as "value-ptr" field).
```

The two lines to hold are 100 and 109: the two node shapes. So, with the
padding rule from Step 4 filled in:

```
non-compressed, size=3 ("abc" branches):        compressed run "xyz" (iscompr=1):

┌header┐┌──────── data[] ───────────────┐       ┌header┐┌──── data[] ─────────┐
│4 bytes││a b c │p│ A* │ B* │ C* │ V*?  │       │4 bytes││x y z │p│ Z* │ V*?  │
└──────┘└──────┴─┴────┴────┴────┴──────┘       └──────┘└──────┴─┴────┴──────┘
          ▲ char bytes first (dense filter)       whole run = ONE child pointer
          │ then padding, then pointers           (points at the node after it)
          │ then value pointer if iskey&&!isnull
   32 bytes total                                  16 bytes total
```

Note the order: the branch *characters* come first, densely packed. Choosing a
branch scans only the char bytes — the same "dense filter, fat payload" move
as SwissTable's control bytes (README §4): the data you probe is dense and
small; the data you follow is touched once, on a match.

### Step 4 — the padding rule: rax pays for aligned pointers

> **In:** a `data[]` array whose pointer section starts after a variable number
> of character bytes.
> **Out:** the 0–7 byte padding that restores 8-byte alignment, the exact node
> size formula, and worked sizes for real nodes.

Because the characters come first and their count is arbitrary, the pointer
section would land at an arbitrary offset. rax does **not** accept that. It
inserts padding:

```
// src/rax.c — size macros, 126-155
   126  /* Return the padding needed in the characters section of a node having size
   127   * 'nodesize'. The padding is needed to store the child pointers to aligned
   128   * addresses. Note that we add 4 to the node size because the node has a four
   129   * bytes header. */
   130  #define raxPadding(nodesize) ((sizeof(void*)-(((nodesize)+4) % sizeof(void*))) & (sizeof(void*)-1))
   ... 131-133: comment for raxNodeLastChildPtr ...
   134  #define raxNodeLastChildPtr(n) ((raxNode**) ( \
   135      ((char*)(n)) + \
   136      raxNodeCurrentLength(n) - \
   137      sizeof(raxNode*) - \
   138      (((n)->iskey && !(n)->isnull) ? sizeof(void*) : 0) \
   139  ))
   140
   141  /* Return the pointer to the first child pointer. */
   142  #define raxNodeFirstChildPtr(n) ((raxNode**) ( \
   143      (n)->data + \
   144      (n)->size + \
   145      raxPadding((n)->size)))
   ... 146-149: comment: the second line computes the padding after the string ...
   150  #define raxNodeCurrentLength(n) ( \
   151      sizeof(raxNode)+(n)->size+ \
   152      raxPadding((n)->size)+ \
   153      ((n)->iscompr ? sizeof(raxNode*) : sizeof(raxNode*)*(n)->size)+ \
   154      (((n)->iskey && !(n)->isnull)*sizeof(void*)) \
   155  )
```

Line 127-128 says it outright: "The padding is needed to store the child
pointers to aligned addresses." Line 145 is where `raxNodeFirstChildPtr` skips
it. `malloc` returns 8-aligned memory, the header is 4 bytes, and
`raxPadding(size)` (line 130) is chosen so that `4 + size + padding ≡ 0
(mod 8)` — so every child pointer in the array is 8-aligned. **rax's pointers
are aligned, by construction, and the padding is the price.**

That price is at most 7 bytes per node:

```
raxPadding(size) = (8 - ((size + 4) mod 8)) & 7

 size:     0  1  2  3  4  5  6  7  8
 padding:  4  3  2  1  0  7  6  5  4
```

**Work a node.** `raxNodeCurrentLength` (line 150-155) is
`4 + size + padding + (iscompr ? 8 : 8·size) + (iskey && !isnull ? 8 : 0)`.

```
 non-compressed, size=3, not a key : 4 + 3 + 1 + 24 + 0 = 32 bytes
 compressed "xyz", not a key       : 4 + 3 + 1 +  8 + 0 = 16 bytes
 compressed "xyz", key with value  : 4 + 3 + 1 +  8 + 8 = 24 bytes
 non-compressed, size=1, not a key : 4 + 1 + 3 +  8 + 0 = 16 bytes
 leaf: size=0, key with value      : 4 + 0 + 4 +  0 + 8 = 16 bytes
 full fanout, size=256, not a key  : 4 + 256 + 4 + 2048 = 2312 bytes  (9.03 B/child)
```

Now price Step 2's whole tree — the three keys "foo", "foobar", "footer",
15 bytes of key data:

```
 compressed (rax.h:44-50, six nodes)          uncompressed trie (ten nodes)
   ["foo"]   compr size=3, non-key   16         7 × single-child non-key   112
   [t b]     size=2, key             32         [b t] size=2, key           32
   ("er")    compr size=2, non-key   16         2 × leaf size=0, key        32
   ("ar")    compr size=2, non-key   16
   [] × 2    size=0, key          16+16
                                   ─────                                  ─────
                                     112                                    176

 saving: (176 − 112) / 176 = 36.4%
```

Compression buys 36% here, and the gap widens with key length: every extra
byte of a unique tail costs 16 bytes uncompressed and 1 byte inside a run.
Note also the honest direction of the comparison — 112 bytes of index for 15
bytes of keys is *not* cheap in absolute terms. Radix trees pay a fixed price
per branch point; they win when keys are long and share prefixes, which is
exactly the stream-ID and tracking-key shape redis uses them for. Two of those
112 bytes' worth of nodes — the 16-byte leaves at the bottom — hold nothing but
a value pointer; Step 8 is how rax removes them when it can.

rax also keeps its own byte count, which tells you how seriously the memory
corner is taken here:

```
// src/rax.c — raxNewNode, 161-174
   161  raxNode *raxNewNode(rax *rax, size_t children, int datafield) {
   162      size_t nodesize = sizeof(raxNode)+children+raxPadding(children)+
   163                        sizeof(raxNode*)*children;
   164      if (datafield) nodesize += sizeof(void*);
   165      size_t usable;
   166      raxNode *node = rax_malloc_usable(nodesize,&usable);
   ... 167-171: NULL check and header init ...
   172      if (rax->alloc_size) *rax->alloc_size += usable;
   173      return node;
   174  }
```

Line 166 asks the allocator for the *usable* size, not the requested one, and
line 172 accumulates it into a caller-supplied counter (`rax->alloc_size`,
`rax.h:127`) — so redis reports the true allocator-rounded footprint of a
tree, malloc slack included, rather than the sum of its `nodesize` arguments.
The same knowledge is spent, not just reported, when a branching node grows:
`raxAddChild` checks `rax_malloc_usable_size(n) < newlen` (`rax.c:286`) and
skips the `realloc` entirely when the allocator's size class already has room
for one more character and pointer (the comment at 283-285 names jemalloc's
size-class rounding as the common case).

### Step 5 — the walk: the tree's entire read path

> **In:** a key `s` of `len` bytes and the tree root.
> **Out:** a `raxNodeLink` recording how many bytes were consumed, the node
> where the walk stopped, the parent slot that points at it, and `splitpos` —
> the offset *inside* a compressed run where it stopped, which is what insert
> needs to cut — plus a 1/0 answer to "is this key present?".

Every rax operation starts with `raxLowWalk`. It is the whole read path —
lookup, the first half of insert, and the descent of delete:

```
// src/rax.c — raxLowWalk, 633-722
   633  static inline int raxLowWalk(rax *rax, unsigned char *s, size_t len,
   634                               void **value, raxNodeLink *link, raxStack *ts) {
   635      raxNode *h = rax->head;
   636      raxNode **parentlink = &rax->head;
   637
   638      size_t i = 0; /* Position in the string. */
   639      size_t j = 0; /* Position in the node children (or bytes if compressed).*/
   ... 640-645: stop-state defaults: link->splitpos = 0, link->leafSlot = -1 ...
   646      while(h->size && i < len) {
   ... 647: debug hook ...
   648          unsigned char *v = h->data;
   649
   650          if (h->iscompr) {
   651              for (j = 0; j < h->size && i < len; j++, i++) {
   652                  if (v[j] != s[i]) break;
   653              }
   654              if (j != h->size) break;
   655          } else {
   656              /* Children are sorted. Check the last child first: for
   657               * sequential inserts the match is almost always at the end,
   658               * and for random keys the extra compare is negligible vs
   659               * the O(n) scan that follows on miss. */
   660              if (v[h->size - 1] == s[i]) {
   661                  j = h->size - 1;
   662              } else if (s[i] > v[h->size - 1]) {
   663                  j = h->size;
   664                  break;
   665              } else {
   666                  /* Lookup the matching child edge. memchr() is used instead
   667                   * of an open-coded scalar loop because libc implementations
   668                   * on most platforms are SIMD-optimized (SSE2/AVX2 on x86,
   669                   * NEON on arm64), which is significantly faster than a
   670                   * byte-by-byte scan at the small fan-out sizes (<= 256)
   671                   * that rax nodes can have. */
   672                  unsigned char *p = memchr(v, s[i], h->size);
   673                  if (p == NULL) break;
   674                  j = (size_t)(p - v);
   675              }
   676              i++;
   677          }
   678
   ... 679-695: the fixed-length leaf-parent stop — Step 8 ...
   696
   697          if (ts) raxStackPush(ts,h); /* Save stack of parent nodes. */
   698          raxNode **children = raxNodeFirstChildPtr(h);
   699          if (h->iscompr) j = 0; /* Compressed node only child is at index 0. */
   700          memcpy(&h,children+j,sizeof(h));
   701          parentlink = children+j;
   ... 702-705: reset j to 0 for the next iteration ...
   706      }
   ... 707-714: publish stopnode, parentlink, consumed; splitpos = j when h is compressed ...
   715      /* Match: query fully consumed, clean stop at a node boundary (not
   716       * mid-prefix on a compressed node), and h is a key. */
   717      if (i != len || (h->iscompr && link->splitpos != 0) || !h->iskey)
   718          return 0;
   719
   720      if (value != NULL) *value = raxGetData(h);
   721      return 1;
   722  }
```

Four places carry the design.

**Lines 648 and 660-674** — choosing a child reads `h->data`, the *character*
prefix, and nothing else. `children` (line 698) is not even computed until a
match is found. That is the dense-filter payoff made concrete: a 256-way
branching node is 2312 bytes, but choosing a child touches only the first 256
of them, and usually far fewer.

**Lines 656-664 — the last edge first.** Because a node's edge bytes are kept
sorted, the last one is the largest. One compare against it settles two
cases before any scan: a match (line 660 — take the last child), or a byte
*larger* than every edge (line 662), which cannot be present anywhere in the
node, so the walk stops as a miss with no scan at all. This is not a
heuristic for typical data; for one workload it is exact. Suppose the key
being looked up is larger than every key already in the tree — a new stream
ID, which redis encodes big-endian (`streamEncodeID`, `t_stream.c:620-625`) so
that byte order is time order. At every branching node on its path, the walk
is following the tree's *largest* key, which always leaves through the last
edge; the new key's byte either equals that edge (shared prefix — line 660) or
exceeds it (first point of difference — line 662). An append-only insert
therefore never reaches line 672.

**Line 672 — `memchr`, not binary search.** Everything else falls to a
**linear scan**: libc's `memchr`, which finds the first occurrence of a byte in
a buffer, over the `size` edge bytes. The edges are sorted, so a **binary
search** — halve the range at each compare — would be legal, and the code
still does not use one. The reason is the layout from Step 3: the edges are
contiguous bytes, so even a full 256-edge node is four 64-byte cache lines
read in order, and a vectorised `memchr` compares 16 or 32 bytes per
instruction (the comment at 666-671 names SSE2/AVX2 and NEON), whereas a
binary search is log₂(256) = 8 dependent, unpredictable branches over the same
memory. Topic 0's lesson — branch misprediction and locality dominate
instruction count at these sizes. Note what the comment does *not* contain: a
measurement. "Significantly faster … at the small fan-out sizes" is a claim,
and the question it leaves open is what happens at a fanout of 2 or 3, where
a function call is being made to scan two bytes. This is also exactly where
ART diverges: it replaces this scan with a fixed-width SIMD compare (Node16)
or a direct index (Node256), chosen per node size.

**Line 700** — the child pointer is read with `memcpy` even though Step 4's
padding guarantees it is aligned. That is not an unaligned-access workaround;
it is the standard C idiom for reading a `raxNode*` out of a `char`-typed
buffer without violating **strict aliasing** (the C rule that an object may
only be read through a pointer of a compatible type), and every compiler turns
it into a single load.

**Line 717 — the hit test.** The loop ends (line 646) when the key runs out
*or* the node has no children, and the walk itself then decides whether that
counts as a hit, in one expression of three failure modes: the key was not
fully consumed; or it *was* consumed but the walk halted part-way through a
compressed run, so the key is a strict prefix of that run and no node
represents it; or a node does represent it but was never marked a key. The
middle clause exists only because runs exist — it is compression's tax on the
lookup path, and it is one comparison. `raxFind` (`rax.c:1242-1245`) is
nothing more than this walk under another name, returning its 1 or 0; Step 6
shows why the walk was made to fill in a `raxNodeLink` instead of returning
bare numbers.

Cost model: one dependent pointer hop per *node* (not per byte, thanks to
compression), and inside a node the scan touches only the dense character
prefix — or, on the last edge, a single byte of it.

### Step 6 — insert = find the link, then split at it

> **In:** the `raxNodeLink` Step 5's walk produced for a key that is not in the
> tree — often one that diverges part-way through a compressed run, with the
> `splitpos` that says where.
> **Out:** the two-call insert API that walks the tree once, two algorithms —
> one for a mismatch inside a run, one for a key that *ends* inside a run —
> and the reason you should read the comment rather than the code.

Insert is split in two at the point where reading stops and writing starts.
The record that crosses that boundary is a **`raxNodeLink`** — the walk's stop
state, saved:

```
// src/rax.h — raxNodeLink, 210-230
   210  #define RAX_LEAF_PARENT_STOP (-1)  /* on fixed-len, when walk stop at a leaf parent */
   211  typedef struct raxNodeLink {
   212      raxNode  *stopnode;     /* Stop node. */
   213      raxNode **parentlink;   /* Slot in stopnode's parent that holds h. */
   214      size_t    consumed;     /* Bytes of key consumed at stop. */
   215      int       splitpos;     /* Split position inside stopnode's compressed
   ... 216-223: only meaningful when stopnode->iscompr; RAX_LEAF_PARENT_STOP is the fixed-length sentinel ...
   224      int       leafSlot;     /* Fixed-length leaf-parent stop only
   ... 225-229: index of the inlined value slot (Step 8) ...
   230  } raxNodeLink;
```

`parentlink` (line 213) is the field that makes the split possible: it is the
address of the pointer that leads *to* the stop node, so insert can replace
that node (after a `realloc` moves it, or when a split puts a new node in
front of it) by overwriting one slot in the parent. Nodes have no parent
pointer — Step 3's 4-byte header has no room for one — so the walk hands the
parent slot over instead.

The public operations are thin compositions of the two halves:

```
// src/rax.c — raxFindLink, raxInsert, raxFind, 1209-1245
  1209  int raxFindLink(rax *rax, unsigned char *s, size_t len,
  1210                  void **value, raxNodeLink *link) {
  ... 1211: debug hook ...
  1212      return raxLowWalk(rax, s, len, value, link, NULL);
  1213  }
  ... 1214-1216: comment: overwriting insert, one walk then commit ...
  1217  int raxInsert(rax *rax, unsigned char *s, size_t len, void *data, void **old) {
  1218      raxNodeLink link;
  1219      raxFindLink(rax, s, len, NULL, &link);
  1220      return raxInsertAt(rax,s,len,data,old,&link);
  1221  }
  ... 1222-1241: raxTryInsert (returns early when the find hits) and raxFind's comment ...
  1242  int raxFind(rax *rax, unsigned char *s, size_t len, void **value) {
  1243      raxNodeLink link;
  1244      return raxFindLink(rax, s, len, value, &link);
  1245  }
```

The line to focus on is 1220: `raxInsertAt` starts from the recorded link
instead of walking again. That matters most to callers, not to `raxInsert`
itself. A **find-or-insert** — look a key up, and create its entry only on a
miss — is the commonest pattern in redis's rax users, and it costs one
walk: `tracking.c:143-147` calls `raxFindLink` on the prefix table and, on a
miss, builds the entry and calls `raxInsertAt` with the same link; the stream
code does the same for consumer groups and pending entries
(`t_stream.c:3718` and `3738`, `4413` and `4440`). The price is a contract,
spelled out at `rax.h:203-209`: the link holds interior pointers into the
tree, so it goes stale on *any* intervening rax mutation, and the commit must
follow the find immediately. Debug builds enforce it by walking again and
comparing (`raxLinkStillValid`, `rax.c:729-737`).

`raxInsertAt` (`rax.c:753-1196`) is the longest function in the file outside
the test suite, and more than a quarter of it is a comment enumerating the
cases on the example word "ANNIBALE":

```
// src/rax.c — the case enumeration, 837-868
   837       * When inserting we may face the following cases. Note that all the cases
   838       * require the insertion of a non compressed node with exactly two
   839       * children, except for the last case which just requires splitting a
   840       * compressed node.
   841       *
   842       * 1) Inserting "ANNIENTARE"
   843       *
   844       *               |B| -> "ALE" -> "SCO" -> []
   845       *     "ANNI" -> |-|
   846       *               |E| -> (... continue algo ...) "NTARE" -> []
   ... 847-865: cases 2-4, all mid-run mismatches at different offsets ...
   866       * 5) Inserting "ANNI"
   867       *
   868       *     "ANNI" -> "BALE" -> "SCO" -> []
```

Cases 1-4 are the same event at different offsets: the key mismatched inside
the run, so cut the run at `splitpos`, insert a two-child branching node, and
re-hang both tails. Case 5 is different in kind: the key *ran out* inside the
run with no mismatch, so there is nothing to branch — just split the run into
a prefix and a postfix and mark the prefix as a key. That is exactly why the
code has two labelled algorithms:

- **ALGO 1** (`rax.c:945`, guarded by `if (h->iscompr && i != len)`) — the
  mismatch cases. Steps are spelled out at lines 873-915: save `$NEXT`, build
  the split node, trim or replace the original depending on whether
  `$SPLITPOS == 0`, build a postfix node if the remainder is non-empty, then
  fall through to the ordinary insertion for the key's own tail.
- **ALGO 2** (comment at lines 916-941, code from 1053) — case 5, the "key
  ends inside a run" case: build the postfix node, trim the current node to
  `$SPLITPOS` characters, and mark the trimmed node as the key.

Two details from the code are worth carrying away. First, `if (h->size == 0 &&
len-i > 1)` at line 1130: when a fresh tail of more than one byte has to be
appended, rax creates it as a *compressed* node immediately rather than as a
chain it would have to fold later — compression is maintained at write time,
not by a cleanup pass. Second, lines 1133-1134 clamp a new run to
`RAX_NODE_MAX_SIZE`, so a key longer than 2²⁹ − 1 bytes simply becomes several
runs; nothing overflows the 29-bit `size` field.

Do not memorise the five cases. Verify the invariant instead — the one from
Step 2, at `rax.c:1442-1443`: compressed nodes are chains of nodes that are
**not keys** and have a **single child**. Every case above either preserves it
or is the delete-side repair that restores it.

### Step 7 — binary-comparable keys, in production

> **In:** a radix tree that orders keys byte-wise, and a workload that wants
> them ordered *numerically*.
> **Out:** the encoding trick that makes those the same thing, a verified
> list of where redis relies on it, and the observation that most of those
> keys have one fixed length.

A radix tree iterates in **byte-lexicographic** order. That is only useful for
numbers if the number's bytes sort the same way the number does — which
little-endian integers emphatically do not. redis's blocked-client timeout
index shows the fix in four lines:

```
// src/timeout.c — encodeTimeoutKey, 75-83
    75  #define CLIENT_ST_KEYLEN 16    /* 8 bytes mstime + 8 bytes client ID. */
    76
    77  /* Given client ID and timeout, write the resulting radix tree key in buf. */
    78  void encodeTimeoutKey(unsigned char *buf, uint64_t timeout, client *c) {
    79      timeout = htonu64(timeout);
    80      memcpy(buf,&timeout,sizeof(timeout));
    81      memcpy(buf+8,&c,sizeof(c));
    ... 82: zero padding for 32-bit targets ...
    83  }
```

Line 79 converts the millisecond timeout to **big-endian** before it becomes a
key. Now the most significant byte is byte 0, so the radix tree's byte order
*is* numeric order, and `handleBlockedClientsTimeout` can walk the tree from
the smallest key and stop at the first entry not yet due — an ordered scan
over a structure that never compares whole keys. The client pointer at bytes
8-15 is only a tiebreaker, keeping keys unique.

That transformation has a name in the next chapter: ART calls it
**binary-comparable keys**, and devotes a section to producing them for signed
integers, floats and compound keys. redis got there first, informally, in a
`htonu64` call — and again in `streamEncodeID`, which writes both halves of a
stream ID big-endian.

Verified users of rax in this checkout, so you can see the workload shape it
was built for. The last column is the third argument each tree is created
with — `raxNewEx(metaSize, alloc_size, keyFixedLen)` — which Step 8 explains;
0 (or plain `raxNew()`) means keys of any length:

| Where | What the tree holds | Key length at creation |
|-------|---------------------|------------------------|
| `src/stream.h:41` | `rax *rax;` — the stream itself, keyed by entry ID | `sizeof(streamID)` = 16 (`t_stream.c:77`) |
| `src/stream.h:48-49` | consumer groups by name; message ID → group | 0; 16 (`t_stream.c:3716`, `3528`) |
| `src/tracking.c:24-25` | `TrackingTable`, `PrefixTable` — client-side caching | 0 (`tracking.c:177-178`) |
| `src/server.h:2202` | `clients_timeout_table` — the ordered index above | 16 (`server.c:3093`) |
| `src/server.h:2219` | `clients_index` — active clients by ID | 8 (`server.c:3084`) |
| `src/server.h:2138` | `errors` — the error-statistics table | 0 (`server.c:3077`) |
| `src/server.h:1497` | `blocks_index` — replication backlog blocks | 8 (`replication.c:274`) |

Every one of them is either long shared-prefix keys (stream IDs, tracking
keys) or a small map that there may be thousands of. Neither is a case where a
`dict` would win. And four of the seven rows, plus half of a fifth — the
stream itself among them, as are the consumer groups' pending-entry lists
(`t_stream.c:3726`, also 16) — have keys that are all exactly the same length,
because they are binary encodings of fixed-width integers.

### Step 8 — fixed-length keys: the leaf node disappears

> **In:** Step 4's size table, where every key ends in a 16-byte leaf holding
> only a value pointer, and Step 7's observation that most of redis's trees
> have keys of one fixed length.
> **Out:** leaf inlining — the value stored in the parent's child slot, one
> allocation fewer per key — and its cost: the meaning of a slot depends
> on depth, which every walker must track.

Look again at a leaf from Step 4: `size = 0`, a key with a value, 4 + 0 + 4 +
0 + 8 = 16 bytes, whose only payload is one 8-byte value pointer. Its parent
already spends an 8-byte slot pointing at it. If the tree knew that *this*
node can have no children, the parent's slot could hold the value itself. In
general it cannot know — "foo" might later get a child "foobar" — but if every
key has exactly `keyFixedLen` bytes, then no key is a prefix of another, and
every node at depth `keyFixedLen` is a childless leaf. rax's header states the
optimization:

```
// src/rax.h — header comment, FIXED LENGTH KEY OPT, 75-83
    75   * FIXED LENGTH KEY OPT
    76   * --------------------
    77   * Another optimization applies when all keys share a known fixed length, set at
    78   * construction via raxNewEx()'s keyFixedLen. In that mode the leaf raxNode is
    79   * skipped: the value pointer is stored directly in the parent's child-pointer
    80   * slot. The node at depth keyFixedLen-1 becomes a "leaf parent" whose slots hold
    81   * inlined values instead of raxNode pointers, saving one raxNode allocation per
    82   * key. In fact, fixed length keys are a common case in Redis so this
    83   * implementation is very useful.
```

Here **depth** means the number of key bytes consumed to reach a node, and a
**leaf parent** is a node whose children would sit at depth `keyFixedLen` —
lines 80-81. Its byte layout is Step 3's, unchanged; only what the slots
*mean* differs:

```
// src/rax.c — fixed-length helpers, 412-430
   412  /* Layout of nodes leaves is unchanged; only slot interpretation differs. Depth
   413   * is tracked implicitly: either by raxLowWalk or raxIterator.
   ... 414-419: each child slot [Xptr] becomes the value [Xval]; no trailing value pointer ...
   420   *   iscompr=0:  [HDR*][abde][Aval][Bval][Dval][Eval]   (one slot per edge byte)
   421   *   iscompr=1:  [HDR*][xyz][val]                       (single slot, leafSlot=0)
   422   */
   ... 423-427: blank line and comment for raxSlotsAreValues ...
   428  static inline int raxSlotsAreValues(const rax *rax, size_t childDepth) {
   429      return rax->keyFixedLen && childDepth == (size_t)rax->keyFixedLen;
   430  }
```

The line to focus on is 413: nothing in a node says "my slots are values".
`raxSlotsAreValues` (line 429) answers it from the depth the *caller* has
counted. So the walk checks it after every matched step, and stops one node
early:

```
// src/rax.c — raxLowWalk, the leaf-parent stop, 686-695
   686          if (raxSlotsAreValues(rax, i)) {
   687              debugnode("Lookup leaf-parent stop node is", h);
   688              link->stopnode = h;
   689              link->parentlink = parentlink;
   690              link->consumed = i;
   691              link->splitpos = RAX_LEAF_PARENT_STOP;
   692              link->leafSlot = h->iscompr ? 0 : (int)j;
   693              if (value != NULL) *value = raxLeafParentReadSlot(h, link->leafSlot);
   694              return 1;
   695          }
```

`i` is the depth the next child would sit at, so when it equals `keyFixedLen`
the slot at index `j` (line 692) holds the value, and line 693 reads it with
the same `memcpy` idiom as Step 5's line 700. Insert has the mirror image: at
`rax.c:1135-1145` a fresh compressed tail is fused with its value
(`raxCompressNodeWithValue`, a `[hdr][suffix][pad][value]` node), and at
1155-1161 a new edge at the leaf-parent depth gets its value written into the
new slot (`raxAddSlot`) — in both cases the function returns without ever
allocating a leaf.

**Work the saving.** Take three 3-byte keys "fob", "foo", "for", first in an
ordinary tree and then in one created with `keyFixedLen = 3`, using Step 4's
formula:

```
 ordinary tree (5 nodes)                       keyFixedLen = 3 (2 nodes)
   "fo"     compr size=2, non-key     16         "fo"    compr size=2, non-key  16
   [b o r]  size=3, non-key           32         [b o r] size=3, leaf parent    32
   [] × 3   size=0, key with value  3×16            (slots hold the 3 values)
                                     ────                                      ───
                                       96                                       48
```

Half the bytes and three allocations fewer, because each of the three leaves
was a whole 16-byte node existing to hold one pointer — and the leaf parent is
the same 32 bytes either way, since a value fits the slot a child pointer used
to occupy. The saving is per key: one 16-byte allocation (before allocator
rounding) for every key in the tree, which for the stream and timeout trees in
Step 7 is every entry.

The cost is the coupling to depth. Every routine that descends the tree now
has to count, or it will dereference a value as if it were a node: the
iterator carries a `leaf_slot_idx` for its position on a "virtual leaf"
(`rax.h:189-194`), and `raxRemove` rebuilds a depth from its parent stack
(`rax.c:1501-1509`) so that the recompression scan of Step 2 can stop at a
leaf parent (line 1519-1521) rather than fold it into a run. The debug printer
`raxRecursiveShow` (`rax.c:2410-2438`) does not count, and follows every slot
as a child node — it is only safe on variable-length trees. The keys
themselves are unaffected: Step 7's big-endian argument and Steps 3-4's node
layout are exactly the same for both kinds of tree.

### Step 9 — the contrast: rax vs ART, opposite RUM corners

> **In:** rax's single variable-size node with a last-edge check and a `memchr`
> scan.
> **Out:** the axis along which ART differs, and the specific claims to check
> against the paper in the next chapter.

The next chapter's ART is the same structure tuned for the opposite corner:

| | rax (`e87dda2a4`) | ART (Leis et al., ICDE 2013) |
|---|-----|-----|
| node sizes | one layout, sized to `size` exactly | four fixed layouts: Node4/16/48/256 |
| child search | last-edge compare, then `memchr` over the char prefix (`rax.c:660-674`) | SIMD compare, indirection array, or direct index |
| pointer alignment | padded to 8 (`rax.c:130`) | aligned arrays |
| path compression | full runs, arbitrary length | pessimistic/optimistic, bounded prefix |
| optimised for | memory — millions of tiny trees | lookup latency — one big main-memory index |

Same structure, opposite RUM corner: rax minimises M, ART minimises R. Two
concrete numbers to carry into the paper: a rax node with 4 children costs
4 + 4 + 0 + 32 = **40 bytes**, and with 16 children 4 + 16 + 4 + 128 =
**152 bytes** — check those against ART's Node4 and Node16, which are fixed
sizes regardless of how many slots are occupied. That difference *is* the RUM
trade, in bytes.

## Where each step lives in the code

| File | Lines | What | Step |
|------|-------|------|------|
| `src/rax.h` | 16-85 | header comment: notation, vanilla trie, compression, splitting, fixed-length keys | 1, 2, 8 |
| `src/rax.h` | 44-50 | the compressed representation of foo/foobar/footer | 2 |
| `src/rax.h` | 87 | `RAX_NODE_MAX_SIZE` = 2²⁹ − 1 | 3 |
| `src/rax.h` | 88-121 | `raxNode` — 4-byte header + flexible `data[]` | 3 |
| `src/rax.h` | 93-119 | **the layout spec** — read in full before any function | 3 |
| `src/rax.h` | 123-136 | `rax` — head, counts, `alloc_size`, `keyFixedLen`, metadata | 4, 8 |
| `src/rax.h` | 138-149 | `raxStack` — parents, because nodes have no parent pointer | 2 |
| `src/rax.h` | 197-230 | `raxNodeLink` and its invalidation contract | 6 |
| `src/rax.c` | 126-130 | `raxPadding` — the alignment rule and its rationale | 4 |
| `src/rax.c` | 134-145 | `raxNodeLastChildPtr` / `raxNodeFirstChildPtr` | 4 |
| `src/rax.c` | 150-155 | `raxNodeCurrentLength` — the node size formula | 4 |
| `src/rax.c` | 161-174 | `raxNewNode` — one allocation, usable-size accounting | 4 |
| `src/rax.c` | 270-293 | `raxAddChild` — skips `realloc` when the size class has room | 4 |
| `src/rax.c` | 411-430 | fixed-length layout and `raxSlotsAreValues` | 8 |
| `src/rax.c` | 583-622 | `raxCompressNode` — build a run | 2 |
| `src/rax.c` | 624-632 | `raxLowWalk` doc comment — what it returns and fills in | 5 |
| `src/rax.c` | 633-722 | `raxLowWalk` — the entire read path | 5 |
| `src/rax.c` | 753-1196 | `raxInsertAt` | 6 |
| `src/rax.c` | 820-942 | the case enumeration and both algorithms, as a comment | 6 |
| `src/rax.c` | 945, 1130-1134 | ALGO 1's guard; compressed-tail creation at write time | 6 |
| `src/rax.c` | 1135-1161 | fixed-length inserts: fused tail, `raxAddSlot` | 8 |
| `src/rax.c` | 1209-1245 | `raxFindLink`, `raxInsert`, `raxTryInsert`, `raxFind` | 5, 6 |
| `src/rax.c` | 1440-1447 | the compression invariant, stated on the delete path | 2 |
| `src/rax.c` | 1488-1528 | recompression: walk up, then collect the chain | 2, 8 |
| `src/timeout.c` | 75-83 | `encodeTimeoutKey` — big-endian keys for ordered scan | 7 |

A route through it that builds rather than jumps:

1. `rax.h:16-74`. The header comment is a tutorial with pictures — trie, then
   compressed trie, then the split that "foo"/"first" forces. Read it before
   any code. Line 18-19 defines the notation: `[]` is a key, `()` is not.
2. `rax.h:87-121`. Struct, then the layout comment. Write the two layouts on
   paper from lines 100 and 109.
3. `rax.c:126-155`. Four macros. Compute `raxNodeCurrentLength` by hand for a
   non-compressed 3-child node and check you get 32.
4. `rax.c:633-722`. `raxLowWalk`. Trace a lookup of "footer" against the tree
   at `rax.h:44-50`: which nodes are visited, which of lines 660, 662 and 672
   resolves each branching step, and what are `i`, `j` and `splitpos` at each
   stop?
5. Line 717. Now trace "foot" — a strict prefix — and find which of the hit
   test's three clauses rejects it.
6. **Aha:** the padding at `rax.c:130` exists so the child pointers are
   *aligned*, and it costs 0-7 bytes per node. Once you see that, re-read the
   Step 4 size table and notice that a compressed node costs 16 bytes no
   matter whether its run is 1 or 3 bytes long — padding absorbs the
   difference. The natural run lengths for a memory-tuned structure are
   therefore not 1; they are 3, 11, 19, … Every design decision in this file
   is that kind of arithmetic.
7. Only then skim `rax.c:820-942`. Read the comment, not the code.
8. Last, `rax.h:75-83` and `rax.c:411-430`, then find every place in
   `raxLowWalk` and `raxInsertAt` that calls `raxSlotsAreValues`.

**Contrast case.** Read `raxLowWalk`'s branching arm (lines 656-674) beside
this repo's own `reading-hashbrown.md`, where the same "find the matching byte"
question is answered with a 16-wide SIMD compare over control bytes. Both are
scanning a dense byte array for a match, and both end in vector instructions —
rax's via libc's `memchr`, hashbrown's via `_mm_cmpeq_epi8` inline. The
difference is fanout and fixedness: hashbrown always compares exactly one
group, so it is a few instructions with no length to handle; rax's scan covers
`size` bytes where `size` varies per node and is often under 8, so it is a
general-purpose call that must handle any length — and rax puts a single
scalar compare (the last edge) in front of it. Whether `memchr` beats a
plain loop at a fanout of 2-4 is the comment's claim, not a measured fact;
it is the same question that leads ART to have both a Node4 *and* a Node16
rather than one SIMD node.

## Questions to answer in notes.md

1. Why does rax put the char bytes *before* the pointers instead of
   interleaving (char, ptr) pairs? Answer in terms of what `raxLowWalk` lines
   660-674 touch versus what they do not, how many cache lines each layout
   would read for a 32-child node, and whether `memchr` could be used at all on
   the interleaved layout.
2. `raxPadding` costs 0-7 bytes per node to keep child pointers aligned. Using
   the size table in Step 4, compute the total padding in the six-node tree
   for foo/foobar/footer, and say what the tree would cost with the pointers
   left unaligned. Was the trade worth it here?
3. Lines 666-671 claim `memchr` is "significantly faster than a byte-by-byte
   scan" at rax's fan-outs. Construct the case where that claim is weakest —
   what fanout, and what would you have to measure to show it? Then say which
   inserts never reach line 672 at all because of lines 660-664.
4. The hit test at `rax.c:717` has three failure clauses. Give a concrete key
   and tree for each, and say which one exists only because of path
   compression.
5. A radix tree has no hash function and no whole-key comparison. Compare
   against this repo's measured `lookup_shootout` at n = 10⁶ — `hashmap
   8.8 ns`, `btreemap 26.6 ns` — and say what rax buys that neither offers,
   naming the two redis subsystems from Step 7 that need it.
6. A tree created with `keyFixedLen = 16` holds a million stream IDs. Using
   Step 8, how many bytes of leaf nodes did leaf inlining remove (before
   allocator rounding)? What would break if one caller inserted a 15-byte key?

## Takeaway

rax is a radix tree that treats every byte as negotiable. Four bytes of header
carry three flags and a 29-bit count; characters and pointers share one
flexible array in one allocation; a compressed node folds an arbitrary run
into a single child pointer; a `NULL` value costs no pointer at all; and when
every key has the same length, the leaf node itself is gone and the value
lives in its parent's slot. The one place it *spends* is `raxPadding` — up to
seven bytes per node so the child pointers stay 8-aligned — which is a good
reminder that "memory-optimised" never means "no padding", it means every
byte was priced. The read path touches only the dense character prefix of
each node — often just its last byte — and hands its stop state to insert so
a find-or-insert walks the tree once; the entire insert complexity is the
price of keeping runs merged. When you meet ART next, the question to hold is
not "which is better" but "which corner": rax has one node shape sized to the
byte, ART has four shapes sized for the probe.

## Done when

Answer each before unfolding it.

- [ ] Compute `raxNodeCurrentLength` by hand for (a) a non-compressed node with
      3 children that is not a key, and (b) a compressed node holding "xyz"
      that is a key with a non-NULL value.

<details>
<summary>Answer</summary>

The formula (`rax.c:150-155`) is
`4 + size + raxPadding(size) + (iscompr ? 8 : 8·size) + (iskey && !isnull ? 8 : 0)`,
and `raxPadding(3) = (8 − ((3+4) mod 8)) & 7 = 1`.

(a) 4 + 3 + 1 + 8×3 + 0 = **32 bytes**.
(b) 4 + 3 + 1 + 8 + 8 = **24 bytes**.

The compressed node holds three characters *and* a value in less space than the
branching node needs for three pointers — which is the whole point of Step 2.

</details>

- [ ] Are rax's child pointers aligned or unaligned? Point at the line that
      decides it, and say what it costs.

<details>
<summary>Answer</summary>

**Aligned.** `raxPadding` at `rax.c:130` inserts 0-7 bytes after the character
section so that `4 + size + padding` is a multiple of 8; since `malloc` returns
8-aligned memory, every child pointer is 8-aligned.
`raxNodeFirstChildPtr` (line 142-145) skips exactly that padding. The cost is
the padding itself — 4 bytes for a leaf, 3 for a 1-child node, 0 for a
4-child node, averaging 3.5 bytes per node over uniform sizes. The `memcpy`
at `rax.c:700` is a strict-aliasing idiom, not evidence of unaligned access.

</details>

- [ ] Trace a lookup of "foot" against the tree at `rax.h:44-50`. Where does
      `raxLowWalk` stop, and which clause of the hit test at `rax.c:717`
      rejects it?

<details>
<summary>Answer</summary>

`raxLowWalk` matches "foo" in the compressed root (lines 650-654), then
descends to the branching node, whose edges are stored sorted as `b`, `t`. The
key byte 't' equals the *last* edge, so line 660 resolves it with one compare
and no `memchr`; the walk descends into the compressed node `("er")`. Now
`i == 4 == len`, so the loop at line 646 exits on the key-exhausted condition.
The stop node is `("er")`, which is compressed, and `splitpos` is 0 — the walk
entered the run but consumed none of it (`j` was reset to 0 after the
descent, and line 713 copies it).

At line 717 the first clause passes (`i == len`) and the second passes
(`splitpos == 0`), so the rejection comes from the **third**: `!h->iskey`. The
node `("er")` is not a key, because "foot" was never inserted. Had the walk
stopped one byte *into* the run — say looking up "foote" — the second clause
`(h->iscompr && link->splitpos != 0)` would have rejected it instead, and that
clause exists only because runs exist.

</details>

- [ ] State the compression invariant precisely, and explain why a
      single-child node that is a key cannot be folded into a run.

<details>
<summary>Answer</summary>

From `rax.c:1442-1443`: compressed nodes represent chains of nodes that are
**not keys** *and* have a **single child**. A run stores `size` characters and
exactly one child pointer, plus at most one value pointer at the very end — so
there is exactly one position, the end of the run, at which a value can hang.
Folding a mid-chain node that carries a value would leave nowhere to store it.
That is also why removing a key can *create* a compression opportunity (case 1
at line 1446): the node stops being a key, so the chain becomes foldable, and
`raxRemove` walks up at lines 1492-1498 to find the highest node that now
qualifies, then collects the chain forward at 1515-1528.

</details>

- [ ] The characters come before the pointers. Name the other structure in
      this topic that makes the same choice, and the one number that decides
      whether a scalar or SIMD scan of that dense region is faster.

<details>
<summary>Answer</summary>

SwissTable/hashbrown: one dense byte of control tag per slot, with the fat
key/value slots elsewhere (README §4 calls it "dense filter, fat payload";
ART Node16's 16-byte key array is the third instance). The deciding number is
the **fanout** — how many bytes the scan must cover. hashbrown always scans a
full group (16 bytes with SSE2, 8 with NEON or the generic fallback), so a
single fixed-width compare pays for itself. rax's non-compressed nodes
usually have a handful of children; rax hands the scan to libc's `memchr` and
asserts at `rax.c:666-671` that the SIMD version is "significantly faster"
even at these sizes, while lines 660-664 resolve the last edge — and every
append of an ever-larger key — with one scalar compare before `memchr` is
called. The comment states its claim without a measurement, which makes it a
good exercise: pick a fanout and measure.

</details>

- [ ] What does `keyFixedLen` change about a rax tree's memory and about the
      code that walks it?

<details>
<summary>Answer</summary>

Memory: when every key is exactly `keyFixedLen` bytes, no key can be a prefix
of another, so every node at depth `keyFixedLen` would be a childless leaf —
4 + 0 + 4 + 0 + 8 = 16 bytes holding one value pointer. Leaf inlining
(`rax.h:75-83`) stores that value in the parent's 8-byte child slot instead,
so one 16-byte allocation per key disappears and the parent's size does not
change. For "fob", "foo", "for" that is 96 → 48 bytes and 5 → 2 nodes.

Code: a slot's meaning — child pointer or value — is not written in the
node; it follows from depth (`raxSlotsAreValues`, `rax.c:428-430`). So
`raxLowWalk` stops one node early and returns the slot (`rax.c:686-695`),
insert writes values into slots instead of allocating leaves (1135-1161), the
iterator tracks a virtual leaf (`leaf_slot_idx`, `rax.h:189-194`), and
`raxRemove` recomputes depth from its stack (`rax.c:1501-1509`) so that
recompression stops at a leaf parent. A walker that does not count depth —
`raxRecursiveShow` is one — will dereference a value as a node.

</details>

## References

**Code**

- [redis](https://github.com/redis/redis) at `e87dda2a4` (`unstable`) —
  verify with `tools/pinned-source.py ref redis`.

| File | Lines | What |
|------|-------|------|
| `src/rax.h` | 16-85 | header comment — the tutorial, the splitting example, the fixed-length optimization |
| `src/rax.h` | 87-121 | `RAX_NODE_MAX_SIZE`, `raxNode`, and the layout spec at 93-119 |
| `src/rax.h` | 123-149 | `rax` (with `alloc_size` and `keyFixedLen`) and `raxStack` |
| `src/rax.h` | 197-230 | `raxNodeLink` — the walk's stop state |
| `src/rax.c` | 126-155 | `raxPadding`, the child-pointer macros, `raxNodeCurrentLength` |
| `src/rax.c` | 161-181 | `raxNewNode` / `raxFreeNode` — usable-size accounting |
| `src/rax.c` | 411-430 | fixed-length slot layout and `raxSlotsAreValues` |
| `src/rax.c` | 624-722 | `raxLowWalk` and its doc comment |
| `src/rax.c` | 753-1196 | `raxInsertAt`; the case enumeration is 820-942 |
| `src/rax.c` | 1209-1245 | `raxFindLink`, `raxInsert`, `raxTryInsert`, `raxFind` |
| `src/rax.c` | 1440-1528 | the compression invariant and the recompression walk |
| `src/timeout.c` | 75-83 | `encodeTimeoutKey` — big-endian for byte-order = numeric order |
| `src/t_stream.c` | 620-625 | `streamEncodeID` — the same trick for stream IDs |
| `src/stream.h` | 41, 48-49 | streams and consumer groups, the largest rax users |
| `src/tracking.c` | 24-25, 143-147 | `TrackingTable` and `PrefixTable`; a find-or-insert on one walk |

**Measured in this repo**

- `topics/00-performance-toolbox/notes.md`, `lookup_shootout` at n = 10⁶:
  `hashmap 8.8 ns`, `btreemap 26.6 ns`, `vec_binary_search 25.8 ns`. rax has
  no lane of its own here — its win is not point-lookup latency, it is bytes
  per tree and prefix iteration, neither of which that lane measures. If you
  want a number, the exercise is to build one.
- `topics/00-performance-toolbox/notes.md`, cache ladder ~1 / 5 / 100 ns —
  the prices behind "one dependent hop per node".

**Companion chapters**

- [`reading-art-paper.md`](reading-art-paper.md) — the same structure with the
  opposite RUM priority. Bring the 40-byte and 152-byte numbers from Step 9.
- [`reading-hashbrown.md`](reading-hashbrown.md) — the other "dense filter,
  fat payload" layout in this topic, and the fixed-width SIMD answer to
  Step 5's scan.
