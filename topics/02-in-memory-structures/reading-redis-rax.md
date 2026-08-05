# rax: a radix tree packed into cache lines

Redis's compressed radix tree — behind stream entries, client-tracking tables,
the blocked-client timeout index and the errors table — is what a trie looks
like when memory is the corner of the RUM triangle you are defending: one
variable-size node layout sized to the byte, path-compressed runs, and a
padding rule that buys pointer alignment for at most seven bytes. This chapter
builds the trie idea from zero, compresses it, then packs it byte by byte the
way rax does — before sending you into the layout comment and the walk. Read
for the *layout* (~45 min, skim the insert logic); it is the memory-first
contrast case for the ART paper that follows.

Everything below is read against **redis/redis at `a176d1225`**, where
`src/rax.h` is 204 lines and `src/rax.c` is 2098. Line numbers move between
releases; re-check yours with:

```
tools/pinned-source.py ref redis
tools/pinned-source.py show redis src/rax.h -r 77:119
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
// src/rax.c — recompression rationale in raxRemove, 1107-1114
  1107      /* Recompression: if trycompress is true, 'h' points to a radix tree node
  1108       * that changed in a way that could allow to compress nodes in this
  1109       * sub-branch. Compressed nodes represent chains of nodes that are not
  1110       * keys and have a single child, so there are two deletion events that
  1111       * may alter the tree so that further compression is needed:
  1112       *
  1113       * 1) A node with a single child was a key and now no longer is a key.
  1114       * 2) A node with two children now has just one child.
```

Read line 1109 carefully: a compressible chain is nodes that are **not keys**
*and* have a single child. Both clauses matter. A single-child node that *is*
a key cannot be folded into a run, because a run has one value slot at its
end and nowhere to hang a value in the middle. Insert splits runs apart
(Step 6); delete is where they get glued back (`raxRemove`, the loop at
1150-1175 walks up to the highest compressible node and then forward along the
chain). Insertion never needs to merge, because it only ever adds branch
points.

### Step 3 — the node: a 4-byte header and one flexible array

> **In:** the compressed tree from Step 2, still made of unspecified "nodes".
> **Out:** the concrete byte layout — a 32-bit header and a single flexible
> array holding characters, then child pointers, then an optional value
> pointer — and the size formula that follows from it.

rax spends **four bytes** of header, then packs everything else into one
flexible array in a single allocation:

```
// src/rax.h — raxNode, 77-82 and 110-111
    77  #define RAX_NODE_MAX_SIZE ((1<<29)-1)
    78  typedef struct raxNode {
    79      uint32_t iskey:1;     /* Does this node contain a key? */
    80      uint32_t isnull:1;    /* Associated value is NULL (don't store it). */
    81      uint32_t iscompr:1;   /* Node is compressed. */
    82      uint32_t size:29;     /* Number of children, or compressed string len. */
    ... 83-109: the data layout comment — the spec, quoted next ...
   110      unsigned char data[];
   111  } raxNode;
```

Three bits and a 29-bit count in one word, so `sizeof(raxNode)` is 4 and
`RAX_NODE_MAX_SIZE` = 2²⁹ − 1 = **536,870,911** — the largest fanout or
compressed run the `size` field can express. `isnull` earns its bit: a key
whose value is `NULL` stores no value pointer at all, saving 8 bytes on the
very common "membership set" use.

The layout comment is the spec. Read it in full before any function:

```
// src/rax.h — data layout comment, 83-108
    83      /* Data layout is as follows:
    ...
    85       * If node is not compressed we have 'size' bytes, one for each children
    86       * character, and 'size' raxNode pointers, point to each child node.
    87       * Note how the character is not stored in the children but in the
    88       * edge of the parents:
    89       *
    90       * [header iscompr=0][abc][a-ptr][b-ptr][c-ptr](value-ptr?)
    91       *
    92       * if node is compressed (iscompr bit is 1) the node has 1 child.
    93       * In that case the 'size' bytes of the string stored immediately at
    94       * the start of the data section, represent a sequence of successive
    95       * nodes linked one after the other, for which only the last one in
    96       * the sequence is actually represented as a node, and pointed to by
    97       * the current compressed node.
    98       *
    99       * [header iscompr=1][xyz][z-ptr](value-ptr?)
    ... 100-104: both kinds can carry a key at any level ...
   105       * If the node has an associated key (iskey=1) and is not NULL
   106       * (isnull=0), then after the raxNode pointers pointing to the
   107       * children, an additional value pointer is present (as you can see
   108       * in the representation above as "value-ptr" field).
```

So, with the padding rule from Step 4 filled in:

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
   ... 132-133: comment for raxNodeLastChildPtr ...
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
   ... 147-149: comment: the second line computes the padding after the string ...
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
`raxPadding(size)` is chosen so that `4 + size + padding ≡ 0 (mod 8)` — so
every child pointer in the array is 8-aligned. **rax's pointers are aligned,
by construction, and the padding is the price.**

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
exactly the stream-ID and tracking-key shape redis uses them for.

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
`rax.h:117`) — so redis reports the true allocator-rounded footprint of a
tree, malloc slack included, rather than the sum of its `nodesize` arguments.

### Step 5 — the walk: the tree's entire read path

> **In:** a key `s` of `len` bytes and the tree root.
> **Out:** how many bytes were consumed, the node where the walk stopped, and
> `splitpos` — the offset *inside* a compressed run where it stopped — which
> is what insert needs to cut.

Every rax operation starts with `raxLowWalk`. It is 42 lines and it is the
whole read path:

```
// src/rax.c — raxLowWalk, 465-506
   465  static inline size_t raxLowWalk(rax *rax, unsigned char *s, size_t len, raxNode **stopnode, raxNode ***plink, int *splitpos, raxStack *ts) {
   466      raxNode *h = rax->head;
   467      raxNode **parentlink = &rax->head;
   468
   469      size_t i = 0; /* Position in the string. */
   470      size_t j = 0; /* Position in the node children (or bytes if compressed).*/
   471      while(h->size && i < len) {
   ... 472-473: debug hook; unsigned char *v = h->data ...
   475          if (h->iscompr) {
   476              for (j = 0; j < h->size && i < len; j++, i++) {
   477                  if (v[j] != s[i]) break;
   478              }
   479              if (j != h->size) break;
   480          } else {
   481              /* Even when h->size is large, linear scan provides good
   482               * performances compared to other approaches that are in theory
   483               * more sounding, like performing a binary search. */
   484              for (j = 0; j < h->size; j++) {
   485                  if (v[j] == s[i]) break;
   486              }
   487              if (j == h->size) break;
   488              i++;
   489          }
   490
   491          if (ts) raxStackPush(ts,h); /* Save stack of parent nodes. */
   492          raxNode **children = raxNodeFirstChildPtr(h);
   493          if (h->iscompr) j = 0; /* Compressed node only child is at index 0. */
   494          memcpy(&h,children+j,sizeof(h));
   495          parentlink = children+j;
   ... 496-499: reset j to 0 for the next iteration ...
   500      }
   ... 501-504: publish stopnode, plink, and splitpos when h is compressed ...
   505      return i;
   506  }
```

Three lines carry the design.

**Line 473 and 484-486** — the scan reads `h->data`, the *character* prefix,
and nothing else. `children` (line 492) is not even computed until a match is
found. That is the dense-filter payoff made concrete: a 256-way branching node
is 2312 bytes, but choosing a child touches only the first 256 of them, and
usually far fewer.

**Lines 481-483** — the comment defends a linear scan over binary search, "even
when `h->size` is large". Believe it, and know why: the characters are
contiguous bytes, so a linear scan is a sequential read over at most four
cache lines with a perfectly predictable stride, while a binary search is
log₂(256) = 8 unpredictable branches over the same memory. Topic 0's lesson —
branch misprediction and locality dominate instruction count at these sizes.
This is also the exact place where ART diverges: it replaces this loop with a
SIMD compare (Node16) or a direct index (Node256).

**Line 494** — the child pointer is read with `memcpy` even though Step 4's
padding guarantees it is aligned. That is not an unaligned-access workaround;
it is the standard C idiom for reading a `raxNode*` out of a `char`-typed
buffer without violating strict aliasing, and every compiler turns it into a
single load.

The stop condition at line 471 is worth a second: the loop ends when the key
runs out *or* the node has no children. `raxFind` then decides whether that
counts as a hit, in one line:

```
// src/rax.c — raxFind, 931-941
   931  int raxFind(rax *rax, unsigned char *s, size_t len, void **value) {
   ... 932-935: locals and debug ...
   936      size_t i = raxLowWalk(rax,s,len,&h,NULL,&splitpos,NULL);
   937      if (i != len || (h->iscompr && splitpos != 0) || !h->iskey)
   938          return 0;
   939      if (value != NULL) *value = raxGetData(h);
   940      return 1;
   941  }
```

Line 937 is three failure modes in one expression: the key was not fully
consumed; or it *was* consumed but the walk halted part-way through a
compressed run, so the key is a strict prefix of that run and no node
represents it; or a node does represent it but was never marked a key. The
middle clause exists only because runs exist — it is compression's tax on the
lookup path, and it is one comparison.

Cost model: one dependent pointer hop per *node* (not per byte, thanks to
compression), and inside a node the scan touches only the dense character
prefix.

### Step 6 — insert = split machinery

> **In:** a key that diverges from the tree part-way through a compressed run,
> plus the `splitpos` that says where.
> **Out:** two algorithms — one for a mismatch inside a run, one for a key that
> *ends* inside a run — and the reason you should read the comment rather than
> the code.

`raxGenericInsert` (`rax.c:515-913`) is the longest function in the file, and
roughly a quarter of it is a comment enumerating the cases on the example word
"ANNIBALE":

```
// src/rax.c — the case enumeration, 577-608
   577       * When inserting we may face the following cases. Note that all the cases
   578       * require the insertion of a non compressed node with exactly two
   579       * children, except for the last case which just requires splitting a
   580       * compressed node.
   581       *
   582       * 1) Inserting "ANNIENTARE"
   583       *
   584       *               |B| -> "ALE" -> "SCO" -> []
   585       *     "ANNI" -> |-|
   586       *               |E| -> (... continue algo ...) "NTARE" -> []
   ... 588-605: cases 2-4, all mid-run mismatches at different offsets ...
   606       * 5) Inserting "ANNI"
   607       *
   608       *     "ANNI" -> "BALE" -> "SCO" -> []
```

Cases 1-4 are the same event at different offsets: the key mismatched inside
the run, so cut the run at `splitpos`, insert a two-child branching node, and
re-hang both tails. Case 5 is different in kind: the key *ran out* inside the
run with no mismatch, so there is nothing to branch — just split the run into
a prefix and a postfix and mark the prefix as a key. That is exactly why the
code has two labelled algorithms:

- **ALGO 1** (`rax.c:684-685`, guarded by `if (h->iscompr && i != len)`) — the
  mismatch cases. Steps are spelled out at lines 613-654: save `$NEXT`, build
  the split node, trim or replace the original depending on whether
  `$SPLITPOS == 0`, build a postfix node if the remainder is non-empty, then
  fall through to the ordinary insertion for the key's own tail.
- **ALGO 2** (lines 656-681) — case 5, the "key ends inside a run" case: build
  the postfix node, trim the current node to `$SPLITPOS` characters, and mark
  the trimmed node as the key.

Two details from the code are worth carrying away. First, `if (h->size == 0 &&
len-i > 1)` at line 867: when a fresh tail of more than one byte has to be
appended, rax creates it as a *compressed* node immediately rather than as a
chain it would have to fold later — compression is maintained at write time,
not by a cleanup pass. Second, line 870-871 clamps a new run to
`RAX_NODE_MAX_SIZE`, so a key longer than 2²⁹ − 1 bytes simply becomes several
runs; nothing overflows the 29-bit `size` field.

Do not memorise the five cases. Verify the invariant instead — the one from
Step 2, at `rax.c:1109-1110`: compressed nodes are chains of nodes that are
**not keys** and have a **single child**. Every case above either preserves it
or is the delete-side repair that restores it.

### Step 7 — binary-comparable keys, in production

> **In:** a radix tree that orders keys byte-wise, and a workload that wants
> them ordered *numerically*.
> **Out:** the encoding trick that makes those the same thing, and a verified
> list of where redis relies on it.

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
`htonu64` call.

Verified users of rax in this checkout, so you can see the workload shape it
was built for:

| Where | What the tree holds |
|-------|---------------------|
| `src/stream.h:37` | `rax *rax;` — the stream itself, keyed by entry ID |
| `src/stream.h:44-45` | consumer groups by name; message ID → group |
| `src/tracking.c:24-25` | `TrackingTable`, `PrefixTable` — client-side caching |
| `src/server.h:1999` | `clients_timeout_table` — the ordered index above |
| `src/server.h:2003` | `clients_index` — active clients by ID |
| `src/server.h:1935` | `errors` — the error-statistics table |
| `src/server.h:1349` | `blocks_index` — replication backlog blocks |

Every one of them is either long shared-prefix keys (stream IDs, tracking
keys) or a small map that there may be thousands of. Neither is a case where a
`dict` would win.

### Step 8 — the contrast: rax vs ART, opposite RUM corners

> **In:** rax's single variable-size node with a linear scan.
> **Out:** the axis along which ART differs, and the specific claims to check
> against the paper in the next chapter.

The next chapter's ART is the same structure tuned for the opposite corner:

| | rax (`a176d1225`) | ART (Leis et al., ICDE 2013) |
|---|-----|-----|
| node sizes | one layout, sized to `size` exactly | four fixed layouts: Node4/16/48/256 |
| child search | linear scan of the char prefix (`rax.c:484-486`) | SIMD compare, indirection array, or direct index |
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
| `src/rax.h` | 16-75 | header comment: notation, vanilla trie, compression, splitting | 1, 2 |
| `src/rax.h` | 44-50 | the compressed representation of foo/foobar/footer | 2 |
| `src/rax.h` | 77 | `RAX_NODE_MAX_SIZE` = 2²⁹ − 1 | 3 |
| `src/rax.h` | 78-111 | `raxNode` — 4-byte header + flexible `data[]` | 3 |
| `src/rax.h` | 83-109 | **the layout spec** — read in full before any function | 3 |
| `src/rax.h` | 113-119 | `rax` — head, counts, `alloc_size`, metadata | 4 |
| `src/rax.h` | 121-130 | `raxStack` — parents, because nodes have no parent pointer | 2 |
| `src/rax.c` | 126-130 | `raxPadding` — the alignment rule and its rationale | 4 |
| `src/rax.c` | 134-145 | `raxNodeLastChildPtr` / `raxNodeFirstChildPtr` | 4 |
| `src/rax.c` | 150-155 | `raxNodeCurrentLength` — the node size formula | 4 |
| `src/rax.c` | 161-174 | `raxNewNode` — one allocation, usable-size accounting | 4 |
| `src/rax.c` | 403-434 | `raxCompressNode` — build a run | 2 |
| `src/rax.c` | 436-464 | `raxLowWalk` doc comment — what `splitpos` means | 5 |
| `src/rax.c` | 465-506 | `raxLowWalk` — the entire read path | 5 |
| `src/rax.c` | 515-913 | `raxGenericInsert` | 6 |
| `src/rax.c` | 560-682 | the case enumeration and both algorithms, as a comment | 6 |
| `src/rax.c` | 684, 867-877 | ALGO 1's guard; compressed-tail creation at write time | 6 |
| `src/rax.c` | 931-941 | `raxFind` — the three-clause hit test | 5 |
| `src/rax.c` | 1107-1121 | the compression invariant, stated on the delete path | 2 |
| `src/rax.c` | 1150-1175 | recompression: walk up, then collect the chain | 2 |
| `src/timeout.c` | 75-83 | `encodeTimeoutKey` — big-endian keys for ordered scan | 7 |

A route through it that builds rather than jumps:

1. `rax.h:16-75`. The header comment is a tutorial with pictures — trie, then
   compressed trie, then the split that "foo"/"first" forces. Read it before
   any code. Line 18-19 defines the notation: `[]` is a key, `()` is not.
2. `rax.h:77-111`. Struct, then the layout comment. Write the two layouts on
   paper from lines 90 and 99.
3. `rax.c:126-155`. Four macros. Compute `raxNodeCurrentLength` by hand for a
   non-compressed 3-child node and check you get 32.
4. `rax.c:465-506`. `raxLowWalk`. Trace a lookup of "footer" against the tree
   at `rax.h:44-50`: which nodes are visited, and what are `i`, `j` and
   `splitpos` at each stop?
5. `rax.c:931-941`. `raxFind`. Now trace "foot" — a strict prefix — and find
   which of line 937's three clauses rejects it.
6. **Aha:** the padding at `rax.c:130` exists so the child pointers are
   *aligned*, and it costs 0-7 bytes per node. Once you see that, re-read the
   Step 4 size table and notice that a compressed node costs 16 bytes no
   matter whether its run is 1 or 3 bytes long — padding absorbs the
   difference. The natural run lengths for a memory-tuned structure are
   therefore not 1; they are 3, 11, 19, … Every design decision in this file
   is that kind of arithmetic.
7. Only then skim `rax.c:560-682`. Read the comment, not the code.

**Contrast case.** Read `raxLowWalk`'s branching arm (lines 484-486) beside
this repo's own `reading-hashbrown.md`, where the same "find the matching byte"
question is answered with a 16-wide SIMD compare over control bytes. Both are
scanning a dense byte array for a match; one uses a scalar loop and defends it
in a comment, the other uses `_mm_cmpeq_epi8`. The difference is fanout: rax's
scan is over `size` bytes where `size` is usually under 8, hashbrown's is over
a fixed 16. Below about a group width, the scalar loop wins on setup cost
alone — which is precisely why ART introduces Node4 *and* Node16 rather than
one SIMD node.

## Questions to answer in notes.md

1. Why does rax put the char bytes *before* the pointers instead of
   interleaving (char, ptr) pairs? Answer in terms of what `raxLowWalk` line
   484-486 touches versus what it does not, and how many cache lines each
   layout would read for a 32-child node.
2. `raxPadding` costs 0-7 bytes per node to keep child pointers aligned. Using
   the size table in Step 4, compute the total padding in the six-node tree
   for foo/foobar/footer, and say what the tree would cost with the pointers
   left unaligned. Was the trade worth it here?
3. Lines 481-483 defend a linear scan over binary search "even when `h->size`
   is large". Construct the case where that comment is wrong — what fanout,
   and what would you have to measure to show it? (Then check what ART chose.)
4. `raxFind` line 937 has three failure clauses. Give a concrete key and tree
   for each, and say which one exists only because of path compression.
5. A radix tree has no hash function and no whole-key comparison. Compare
   against this repo's measured `lookup_shootout` at n = 10⁶ — `hashmap
   8.8 ns`, `btreemap 26.6 ns` — and say what rax buys that neither offers,
   naming the two redis subsystems from Step 7 that need it.

## Takeaway

rax is a radix tree that treats every byte as negotiable. Four bytes of header
carry three flags and a 29-bit count; characters and pointers share one
flexible array in one allocation; a compressed node folds an arbitrary run
into a single child pointer; a `NULL` value costs no pointer at all. The one
place it *spends* is `raxPadding` — up to seven bytes per node so the child
pointers stay 8-aligned — which is a good reminder that "memory-optimised"
never means "no padding", it means every byte was priced. The read path is 42
lines and touches only the dense character prefix of each node, and the entire
insert complexity is the price of keeping runs merged. When you meet ART next,
the question to hold is not "which is better" but "which corner": rax has one
node shape sized to the byte, ART has four shapes sized for the probe.

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
at `rax.c:494` is a strict-aliasing idiom, not evidence of unaligned access.

</details>

- [ ] Trace a lookup of "foot" against the tree at `rax.h:44-50`. Where does
      `raxLowWalk` stop, and which clause of `raxFind` line 937 rejects it?

<details>
<summary>Answer</summary>

`raxLowWalk` matches "foo" in the compressed root, descends to `[t b]`, matches
't' at index 0, and descends into the compressed node `("er")`. Now `i == 4 ==
len`, so the loop at line 471 exits on the key-exhausted condition. The stop
node is `("er")`, which is compressed, and `splitpos` is 0 — the walk entered
the run but consumed none of it.

At line 937 the first clause passes (`i == len`) and the second passes
(`splitpos == 0`), so the rejection comes from the **third**: `!h->iskey`. The
node `("er")` is not a key, because "foot" was never inserted. Had the walk
stopped one byte *into* the run — say looking up "foote" — the second clause
`(h->iscompr && splitpos != 0)` would have rejected it instead, and that
clause exists only because runs exist.

</details>

- [ ] State the compression invariant precisely, and explain why a
      single-child node that is a key cannot be folded into a run.

<details>
<summary>Answer</summary>

From `rax.c:1109-1110`: compressed nodes represent chains of nodes that are
**not keys** *and* have a **single child**. A run stores `size` characters and
exactly one child pointer, plus at most one value pointer at the very end — so
there is exactly one position, the end of the run, at which a value can hang.
Folding a mid-chain node that carries a value would leave nowhere to store it.
That is also why removing a key can *create* a compression opportunity (case 1
at line 1113): the node stops being a key, so the chain becomes foldable, and
`raxRemove` walks up at lines 1159-1165 to find the highest node that now
qualifies.

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
single SIMD compare pays for itself; rax's non-compressed nodes usually have a
handful of children, where loading a vector register costs more than the loop
it replaces. `rax.c:481-483` states this as a claim without a measurement,
which makes it a good exercise: pick a fanout and measure.

</details>

## References

**Code**

- [redis](https://github.com/redis/redis) at `a176d1225` — verify with
  `tools/pinned-source.py ref redis`.

| File | Lines | What |
|------|-------|------|
| `src/rax.h` | 16-75 | header comment — the tutorial, with the splitting example |
| `src/rax.h` | 77-111 | `RAX_NODE_MAX_SIZE`, `raxNode`, and the layout spec at 83-109 |
| `src/rax.h` | 113-130 | `rax` (with `alloc_size`) and `raxStack` |
| `src/rax.c` | 126-155 | `raxPadding`, the child-pointer macros, `raxNodeCurrentLength` |
| `src/rax.c` | 161-181 | `raxNewNode` / `raxFreeNode` — usable-size accounting |
| `src/rax.c` | 436-506 | `raxLowWalk` and its doc comment |
| `src/rax.c` | 515-913 | `raxGenericInsert`; the case enumeration is 560-682 |
| `src/rax.c` | 931-941 | `raxFind` |
| `src/rax.c` | 1107-1175 | the compression invariant and the recompression walk |
| `src/timeout.c` | 75-83 | `encodeTimeoutKey` — big-endian for byte-order = numeric order |
| `src/stream.h` | 37, 44-45 | streams and consumer groups, the largest rax users |
| `src/tracking.c` | 24-25 | `TrackingTable` and `PrefixTable` |

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
  opposite RUM priority. Bring the 40-byte and 152-byte numbers from Step 8.
- [`reading-hashbrown.md`](reading-hashbrown.md) — the other "dense filter,
  fat payload" layout in this topic, and the SIMD answer to Step 5's scan.
