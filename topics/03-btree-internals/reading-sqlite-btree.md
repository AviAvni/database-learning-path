# btree.c: twenty years of production scars

You already know the format from turso — this guided skim (2 h) reads **the
original** for the parts turso simplified and for comments that carry two
decades of production experience: the `balance_quick` fast path, the measured
"about 25% faster" tweak, pointer maps, predecessor-swap deletes. This chapter
builds those production tricks one step at a time — the parsed page, the
descent, in-page free space, the overflow-cell dodge, the three balance paths,
the reverse index, and the delete that forks into two — then hands you the
reading route. Don't read its 11,633 lines linearly.

Every anchor below is SQLite at the commit this repo pins, **`sqlite/sqlite@951de30`**
(confirm with `tools/pinned-source.py ref sqlite`), where `src/btree.c` is
11,633 lines and `src/btreeInt.h` is 746. All arithmetic uses
`SQLITE_DEFAULT_PAGE_SIZE` = 4096 (`src/sqliteLimit.h:214`) with zero reserved
bytes, so *usable size* = *page size* = 4096.

## The problem in one sentence

btree.c must make the single most common write on Earth — appending the next
sequential rowid to a table — cost one new page and one cell copy instead of
rewriting three siblings' worth of cells (114 of them, at 4 KB pages and
100-byte rows), inside 11,633 lines of C that also survive every crash,
corrupt page and pathological key distribution that twenty years on billions
of devices can produce.

## The concepts, step by step

### Step 1 — MemPage: parse the page once, dispatch without branching

> **In:** 4096 raw bytes from the pager.
> **Out:** a `MemPage` whose header fields are decoded, whose per-cell
> operations are already bound to the right function, and whose free-byte
> count is deliberately *not* computed. Every later step reads this struct
> instead of the bytes.

The bytes have a fixed shape, and btreeInt.h documents it better than any
other open-source file documents anything:

```c
// sqlite/sqlite src/btreeInt.h — the page layout and header table, 111-134
   111  **      | file header    |   100 bytes.  Page 1 only.
   112  **      |----------------|
   113  **      | page header    |   8 bytes for leaves.  12 bytes for interior nodes
   114  **      |----------------|
   115  **      | cell pointer   |   |  2 bytes per cell.  Sorted order.
   116  **      | array          |   |  Grows downward
   117  **      |                |   v
   118  **      |----------------|
   119  **      | unallocated    |
   120  **      | space          |
   121  **      |----------------|   ^  Grows upwards
   122  **      | cell content   |   |  Arbitrary order interspersed with freeblocks.
   123  **      | area           |   |  and free space fragments.
   124  **      |----------------|
   125  **
   126  ** The page headers looks like this:
   127  **
   128  **   OFFSET   SIZE     DESCRIPTION
   129  **      0       1      Flags. 1: intkey, 2: zerodata, 4: leafdata, 8: leaf
   130  **      1       2      byte offset to the first freeblock
   131  **      3       2      number of cells on this page
   132  **      5       2      first byte of the cell content area
   133  **      7       1      number of fragmented free bytes
   134  **      8       4      Right child (the Ptr(N) value).  Omitted on leaves.
```

A **cell** is one key+payload entry; a **slot** is what a cell costs in total,
its 2-byte pointer (:115) plus its body. Note line 134: the right-child
pointer is 4 bytes at header offset 8, which is why the interior header is
12 bytes and the leaf header 8 — that difference is `MemPage.childPtrSize`,
"0 if leaf==1. 4 if leaf==0" (btreeInt.h:282), and it reappears as a literal
`+4` in Step 7.

`MemPage` (btreeInt.h:273-304) is the decoded form, built once when the page
enters the cache. Three of its 30 fields carry production decisions:

```c
// sqlite/sqlite src/btreeInt.h — inside struct MemPage, 288-303
   288    int nFree;           /* Number of free bytes on the page. -1 for unknown */
   // ... 289-292: nCell, maskPage, aiOvfl[4] ...
   293    u8 *apOvfl[4];       /* Pointers to the body of overflow cells */
   // ... 294-301: pBt, aData, aDataEnd, aCellIdx, aDataOfst, pDbPage ...
   302    u16 (*xCellSize)(MemPage*,u8*);             /* cellSizePtr method */
   303    void (*xParseCell)(MemPage*,u8*,CellInfo*); /* btreeParseCell method */
```

- **Lines 302-303 are devirtualized dispatch, 1994 style.** `xCellSize` and
  `xParseCell` are function pointers chosen once per page at init — a table
  leaf gets the table-leaf parser, an index interior gets the index-interior
  parser — so the per-cell inner loop never re-tests "what kind of page am
  I?". A descent parses O(log N × log₂ F) cells; the branch it avoids is the
  most-executed branch in the engine.
- **Line 288 is laziness with a stated sentinel.** `nFree` is held at −1 until
  someone needs it, because computing it means walking the freeblock chain
  (Step 3) and most page visits never ask. `btreeComputeFreeSpace` is called
  on demand — you can watch it happen at btree.c:9212 and :9994.
- **Line 293 is Step 4's entire mechanism**, and note the array bound: **four**
  overflow cells, not an arbitrary number.

`CellInfo` (btreeInt.h:480-486) is what `xParseCell` fills in — `nKey`,
`pPayload`, `nPayload`, `nLocal`, `nSize`. The pair `nPayload` (:483) and
`nLocal` (:484) is where a too-big cell forks into a local part and an
overflow chain; the arithmetic of that split belongs to
[`reading-sqlite-file-format.md`](reading-sqlite-file-format.md).

Why it matters: cell parsing is the innermost loop of every search, insert and
balance. This is where cycles go.

### Step 2 — the search path: descend, binary-search, bias the first probe

> **In:** a root page number and a search key.
> **Out:** a leaf page and an index into its cell pointer array — plus a count
> of pages touched, which is the tree's height.

A lookup descends from the root, binary-searching each page's sorted cell
pointer array to pick the child to follow. Here is the whole loop's skeleton
for a rowid table:

```c
// sqlite/sqlite src/btree.c — inside sqlite3BtreeTableMoveto, 5917-5968
  5917      lwr = 0;
  5918      upr = pPage->nCell-1;
  5919      assert( biasRight==0 || biasRight==1 );
  5920      idx = upr>>(1-biasRight); /* idx = biasRight ? upr : (lwr+upr)/2; */
  5921      for(;;){
  // ... 5922-5951: decode this cell's rowid, compare, narrow lwr/upr,
  //                and return early on an exact hit at a leaf ...
  5952        assert( lwr+upr>=0 );
  5953        idx = (lwr+upr)>>1;  /* idx = (lwr+upr)/2; */
  5954      }
  // ... 5955-5963: if this is a leaf, we are done ...
  5964  moveto_table_next_layer:
  5965      if( lwr>=pPage->nCell ){
  5966        chldPg = get4byte(&pPage->aData[pPage->hdrOffset+8]);
  5967      }else{
  5968        chldPg = get4byte(findCell(pPage, lwr));
```

Two production marks, and the first one is narrower than it is usually
described:

- **Line 5920 is the bias hint, and it biases exactly one probe.** With
  `biasRight = 1` the first `idx` is `upr` — the rightmost cell — instead of
  the midpoint. Every *subsequent* probe is the ordinary midpoint, line 5953.
  So an appending caller does not skip the binary search; it wins the common
  case in one comparison and otherwise pays the usual log₂. (The parameter is
  declared at :5840, "If true, bias the search to the high end".)
- **Lines 5965-5966 are the rightmost pointer.** When the key sorts past every
  cell, the descent follows the 4-byte right child at header offset 8 —
  the field from Step 1's line 134.

`sqlite3BtreeIndexMoveto` (btree.c:6068) is the index counterpart; it compares
full records through an `xRecordCompare` callback specialized per key shape,
the same devirtualization move as Step 1.

Now price the descent, because "height" is the currency. Symbols: `P` = page
size, `H` = page header bytes, `F` = fanout (children per interior page),
`L` = entries per leaf page, `N` = row count.

```
P = 4096, zero reserved bytes  (SQLITE_DEFAULT_PAGE_SIZE, sqliteLimit.h:214)

table leaf   H = 8   (btreeInt.h:113)
  slot = 2 (cell pointer)  + 1 (payload-size varint, 100 fits in one byte)
       + 3 (rowid varint, 10^6 needs 3) + 100 (payload)          = 106 B
  L = floor((4096 - 8) / 106) = floor(4088 / 106) = 38 rows/leaf

table interior  H = 12  (the extra 4 = right child, btreeInt.h:134)
  slot = 2 (cell pointer) + 4 (child pgno) + 3 (rowid varint)    =   9 B
  F = floor((4096 - 12) / 9) = floor(4084 / 9) = 453 children/page

height at N = 10^6:
  leaves = ceil(N / L)      = ceil(10^6 / 38)        = 26,316
  interior levels = ceil(log_F(N/L)) = ceil(log_453(26,315.8))
                  = ceil(ln 26,315.8 / ln 453) = ceil(10.178 / 6.116)
                  = ceil(1.6642) = 2
  total pages touched per lookup = 2 + 1 leaf = 3

at N = 10^9 the rowid varint grows to 5 B, so F = floor(4084/11) = 371:
  leaves = ceil(10^9 / 38) = 26,315,790
  ceil(log_371(26,315,790)) = ceil(17.086 / 5.916) = ceil(2.8879) = 3
  total pages touched = 4
```

For an *index* b-tree with a 16-byte key the slot is
`2 + 4 + 1 (payload-size varint) + 16 = 23`, so `F = floor(4084/23) = 177` —
and with the textbook 8-byte child pointer instead of SQLite's 4-byte one it
would be `floor(4084/24) = 170`. Fanout is not delicate: halving the pointer
width moved it 4%.

**A warning this topic has measured.** Height is the count of pages a lookup
*touches*; it is not the time a lookup takes. This topic's own benchmark holds
height at 3 across 10⁶ → 4×10⁶ keys and still watches lookups climb from
862 ns to 1101 ns — see the measured block in [README.md](README.md) and the
ladder in [notes.md](notes.md). Height is a step function; latency is not,
because what a touch *costs* depends on cache residency. Use the arithmetic
above to predict pages read, and nothing else.

Why it matters: comparisons are the entire CPU cost of a descent, and picking
the specialized comparator once per query instead of branching per comparison
is free money.

### Step 3 — free space within a page: freeblocks, merged on free

> **In:** a page with cells deleted out of it over time, and a request for
> `nByte` contiguous bytes.
> **Out:** an offset — reached by one of three routes, in a fixed order — or
> a failure that hands control to Step 4.

Deleting a cell turns its bytes into a **freeblock**: a hole inside the cell
content area, threaded into a singly linked list so later inserts can reuse
it. The format is four bytes of self-description, and it has a floor:

```c
// sqlite/sqlite src/btreeInt.h — the freeblock and fragment rules, 152-163
   152  ** Unused space within the cell content area is collected into a linked list of
   153  ** freeblocks.  Each freeblock is at least 4 bytes in size.  The byte offset
   154  ** to the first freeblock is given in the header.  Freeblocks occur in
   155  ** increasing order.  Because a freeblock must be at least 4 bytes in size,
   156  ** any group of 3 or fewer unused bytes in the cell content area cannot
   157  ** exist on the freeblock chain.  A group of 3 or fewer free bytes is called
   158  ** a fragment.  The total number of bytes in all fragments is recorded.
   159  ** in the page header at offset 7.
   160  **
   161  **    SIZE    DESCRIPTION
   162  **      2     Byte offset of the next freeblock
   163  **      2     Bytes in this freeblock
```

Lines 161-163 explain line 153: a freeblock must hold its own 2-byte next
pointer and 2-byte size, so 4 bytes is the minimum it can describe. Anything
smaller is a **fragment**, unreachable and merely counted — in a *one-byte*
header field (Step 1's line 133), so at most 255 fragment bytes can even be
represented on a page.

`allocateSpace` (btree.c:1846) then tries three routes, in this order:

```c
// sqlite/sqlite src/btree.c — the three routes in allocateSpace, 1889-1928
  1889    if( (data[hdr+2] || data[hdr+1]) && gap+2<=top ){
  1890      u8 *pSpace = pageFindSlot(pPage, nByte, &rc);
  // ... 1891-1903: if a freeblock fit, return its offset ...
  1904
  1905    /* The request could not be fulfilled using a freelist slot.  Check
  1906    ** to see if defragmentation is necessary.
  1907    */
  // ... 1908 ...
  1909    if( gap+2+nByte>top ){
  // ... 1910-1911: asserts ...
  1912      rc = defragmentPage(pPage, MIN(4, pPage->nFree - (2+nByte)));
  // ... 1913-1916: recheck top ...
  1917    }
  // ... 1918-1924: comment on why the gap allocation is now safe ...
  1925    top -= nByte;
  1926    put2byte(&data[hdr+5], top);
  // ... 1927 ...
  1928    *pIdx = top;
```

Read it as: **freeblock chain first** (:1889-1890, gated on the chain being
non-empty), **compact only if the gap is too small** (:1909-1912), then always
**allocate from the gap** (:1925-1928) between the pointer array and the cell
content. Defrag is the last resort, not a third allocation route.

`freeSpace` (btree.c:1945) is the other half, and its contract is one line of
comment at :1937 — "Adjacent freeblocks are coalesced." Deletes actively fight
fragmentation as they happen, rather than deferring all of it.

`defragmentPage` (btree.c:1640) has a fast path worth seeing, because it is
the same instinct as `balance_quick`: at :1674, if the page has at most two
freeblocks and at most `nMaxFrag` fragment bytes, it slides the cells with one
or two `memmove`s (:1693, :1701) and adds a fixed offset to each affected cell
pointer (:1702-1706), instead of rebuilding the page through the temp buffer
at :1712-1740. `allocateSpace` passes `nMaxFrag = MIN(4, ...)` at :1912, so
the fast path only fires on nearly clean pages.

Why it matters: this machinery is what makes delete cheap — unlink a 2-byte
pointer, thread a hole — while keeping pages usable through decades of churn
with no vacuum. Contrast LMDB, which has no freeblocks at all and pays a full
`memmove` on every delete instead; see
[`reading-lmdb.md`](reading-lmdb.md) Step 7.

### Step 4 — the overflow-cell trick: a page is never physically overfull

> **In:** an insert that Step 3 could not place, even after defragmenting.
> **Out:** a page that is logically overfull but physically valid, plus an
> obligation on the caller to run Step 5 before releasing it. Crucially, the
> on-disk format never learns that overfull pages exist.

When a cell will not fit, SQLite does not grow the page, reallocate it, or
invent an "overfull" on-disk representation. The incoming cell is parked **in
memory, beside the page**, in `MemPage.apOvfl[4]` (Step 1's line 293), with
`aiOvfl[i]` recording which non-overflow cell it belongs before
(btreeInt.h:291-292).

```rust
// ILLUSTRATION — not quoted from SQLite; the real code is insertCell,
// sqlite/sqlite src/btree.c:7363, with the array at src/btreeInt.h:293
fn insert_cell(page: &mut MemPage, i: usize, cell: Cell) {
    match page.allocate_space(cell.len()) {     // btree.c:1846 — freeblocks,
        Some(off) => page.write_cell(off, i, &cell), //  defrag, then the gap
        None => {
            page.ap_ovfl.push((i, cell));       // btreeInt.h:293 — 4 slots,
                                                //  parked IN MEMORY
            // caller must run balance() (btree.c:9162) before the page is
            // released; balance drains ap_ovfl while redistributing, so the
            // on-disk format never needs an "overfull" representation
        }
    }
}
```

Why it matters: every page on disk is always structurally valid, at every
instant, so a crash can never expose a page shape the reader does not
understand. That crash-safety and simplicity win is bought with one 4-element
in-memory array — and the bound of 4 is itself a statement, since Step 5 is
guaranteed to run before a fifth could be needed.

### Step 5 — balance: read it for the engineering, not the algorithm

> **In:** a page carrying overflow cells from Step 4 (or one left too empty by
> Step 7).
> **Out:** pages that are all physically valid and within the fill rules, and
> new separator keys pushed into the parent — possibly making *it* overfull,
> which is why `balance` is a loop.

**Balance** pools the cells of the problem page and its neighbours,
redistributes them, and pushes separators up. The `balance()` dispatcher
(btree.c:9162) picks between three paths.

**`balance_quick` (btree.c:8039)** is the append fast path, and its gate is
five exact conditions:

```c
// sqlite/sqlite src/btree.c — the balance_quick gate inside balance(), 9216-9222
  9216  #ifndef SQLITE_OMIT_QUICKBALANCE
  9217        if( pPage->intKeyLeaf
  9218         && pPage->nOverflow==1
  9219         && pPage->aiOvfl[0]==pPage->nCell
  9220         && pParent->pgno!=1
  9221         && pParent->nCell==iIdx
  9222        ){
```

Read: a rowid-table leaf (:9217) with exactly one overflow cell (:9218) that
belongs *after* every existing cell (:9219), on a non-root parent (:9220),
and which is that parent's rightmost child (:9221). That is precisely "the
next sequential rowid, appended". The rationale is stated where the function
lives, and it is the argument the previous edition of this chapter attached to
the wrong comment:

```c
// sqlite/sqlite src/btree.c — the balance_quick header comment, 8022-8037
  8022  ** Instead of trying to balance the 3 right-most leaf pages, just add
  8023  ** a new page to the right-hand side and put the one new entry in
  8024  ** that page.  This leaves the right side of the tree somewhat
  8025  ** unbalanced.  But odds are that we will be inserting new entries
  8026  ** at the end soon afterwards so the nearly empty page will quickly
  8027  ** fill up.  On average.
  // ... 8028-8032: pPage must be the right-most leaf, with one overflow ...
  8033  ** The pSpace buffer is used to store a temporary copy of the divider
  8034  ** cell that will be inserted into pParent. Such a cell consists of a 4
  8035  ** byte page number followed by a variable length integer. In other
  8036  ** words, at most 13 bytes. Hence the pSpace buffer must be at
  8037  ** least 13 bytes in size.
```

Lines 8033-8037 also give you the divider cell's exact budget: 4 bytes of page
number plus a rowid varint of at most 9, so **at most 13 bytes** enter the
parent per split. Price the fast path against the general one at the leaf
geometry of Step 2 (L = 38 rows/leaf):

```
balance_quick    : allocate 1 page, copy 1 cell, insert ≤13 B into the parent
balance_nonroot  : pool NB = 3 siblings ⇒ 3 × 38 = 114 cells re-encoded and
                   rewritten, plus the parent's dividers rebuilt
saving per append: 114 cell copies → 1
```

**`balance_nonroot` (btree.c:8277)** is the general case: pool the overfull
page with up to `NB = 3` siblings (`#define NB 3` at btree.c:7552, commented
"(NN*2+1): Total pages involved in the balance") and redistribute. Two of its
comments are worth the trip, and they are *different* optimizations:

```c
// sqlite/sqlite src/btree.c — the measured 25%, and what it is about, 8730-8741
  8730    /*
  8731    ** Reassign page numbers so that the new pages are in ascending order.
  8732    ** This helps to keep entries in the disk file in order so that a scan
  8733    ** of the table is closer to a linear scan through the file. That in turn
  8734    ** helps the operating system to deliver pages from the disk more rapidly.
  8735    **
  8736    ** An O(N*N) sort algorithm is used, but since N is never more than NB+2
  8737    ** (5), that is not a performance concern.
  8738    **
  8739    ** When NB==3, this one optimization makes the database about 25% faster
  8740    ** for large insertions and deletions.
  8741    */
```

**This is a page-number reassignment, not a fill-bias.** The block that
follows (:8742-8770) is an O(N²) selection sort over at most five pages
(:8747-8751) that renumbers the freshly balanced siblings into ascending page
order, so that a later table scan reads the file closer to sequentially. The
25% is a *physical locality* result — a topic-0 lesson in the wild, and a
sibling of topic 1's fillseq-vs-fillrandom gap.

The fill bias is a separate thing, twenty lines earlier, and it is explicitly
*not* an optimization:

```c
// sqlite/sqlite src/btree.c — the packing adjustment in balance_nonroot, 8636-8646
  8636    /*
  8637    ** The packing computed by the previous block is biased toward the siblings
  8638    ** on the left side (siblings with smaller keys). The left siblings are
  8639    ** always nearly full, while the right-most sibling might be nearly empty.
  8640    ** The next block of code attempts to adjust the packing of siblings to
  8641    ** get a better balance.
  8642    **
  8643    ** This adjustment is more than an optimization.  The packing above might
  8644    ** be so out of balance as to be illegal.  For example, the right-most
  8645    ** sibling might be completely empty.  This adjustment is not optional.
  8646    */
```

So the left bias is an accident of the greedy first-fit packing loop
(:8580-8634), and the loop at :8647-8688 exists to *correct* it for
correctness, not for speed. Attributing "packs left so the right page has room
for the next append" to the 25% comment fuses two unrelated pieces of code —
the previous edition of this chapter did exactly that, and it was wrong.

**`balance_deeper` (btree.c:9081)** is the root split: the root's content moves
into a fresh child and the tree grows *up* by one level. It is the only
operation that increases height, it is called at :9190, and the assertion at
:9188 records that it can happen at most once per `balance()` call.

Why it matters: the algorithm is in every textbook; the five-condition fast
path, the bound `NB = 3`, and the *measured* 25% renumbering are what two
decades of production look like.

### Step 6 — pointer maps: the reverse index turso doesn't have

> **In:** the observation that a B-tree has only downward pointers.
> **Out:** a permanent format tax, paid on every page allocation and every
> split, that makes one management operation — auto-vacuum's page relocation —
> possible at all.

A **pointer map** is a reverse index: for each page number, who points *at*
it. Relocating a page (which vacuum must do to shrink the file) would
otherwise require searching the whole tree for the parent. Entries are five
bytes — a one-byte type plus a four-byte page number — as the offset macro
shows: `PTRMAP_PTROFFSET(pgptrmap, pgno) = 5*(pgno-pgptrmap-1)`
(btreeInt.h:630). The five types are `PTRMAP_ROOTPAGE` … `PTRMAP_BTREE`
(btreeInt.h:664-668), documented at :647-662; note `PTRMAP_OVERFLOW2` (:657),
which chains overflow pages backwards so an overflow page can find its
predecessor.

The density follows directly:

```c
// sqlite/sqlite src/btree.c — ptrmapPageno, 1063-1075
  1063  static Pgno ptrmapPageno(BtShared *pBt, Pgno pgno){
  // ... 1064-1067: locals, mutex assert, and pgno<2 returns 0 ...
  1068    nPagesPerMapPage = (pBt->usableSize/5)+1;
  1069    iPtrMap = (pgno-2)/nPagesPerMapPage;
  1070    ret = (iPtrMap*nPagesPerMapPage) + 2;
  1071    if( ret==PENDING_BYTE_PAGE(pBt) ){
  1072      ret++;
  1073    }
  1074    return ret;
  1075  }
```

Line 1068 is the whole cost model. Evaluate it:

```
usable = 4096
entries per ptrmap page      = floor(4096 / 5)        = 819
pages covered by one group   = 819 + 1                = 820
space overhead               = 1 / 820                = 0.122 %
```

0.122% of the file is a rounding error. The real price is elsewhere: every
page allocation, every split, and every overflow-chain change must also
*write* its ptrmap page (`ptrmapPut`, btree.c:1087), which turns one dirtied
page into two and gives the balance code an extra failure mode.

Why it matters: it is a clean example of paying a permanent format tax for one
management operation — and of why turso has not implemented it yet. See
[`reading-turso-btree-deep.md`](reading-turso-btree-deep.md).

### Step 7 — the fork: one interior delete becomes two page mutations

> **In:** a delete positioned on an *interior* page.
> **Out:** two independent structural edits — a cell dropped from the interior
> page and a cell promoted out of a leaf — each of which can require its own
> `balance()` call. This is the one place a single logical operation forks into
> two physical ones, and the code is shaped entirely around that.

An interior cell is not only data; it is also the separator routing searches
between two subtrees. Deleting it therefore cannot just remove it. SQLite's
answer, and its reason, are in the comment:

```c
// sqlite/sqlite src/btree.c — inside sqlite3BtreeDelete, 9948-9959
  9948    /* If the page containing the entry to delete is not a leaf page, move
  9949    ** the cursor to the largest entry in the tree that is smaller than
  9950    ** the entry being deleted. This cell will replace the cell being deleted
  9951    ** from the internal node. The 'previous' entry is used for this instead
  9952    ** of the 'next' entry, as the previous entry is always a part of the
  9953    ** sub-tree headed by the child page of the cell being deleted. This makes
  9954    ** balancing the tree following the delete operation easier.  */
  9955    if( !pPage->leaf ){
  9956      rc = sqlite3BtreePrevious(pCur, 0);
  // ... 9957-9958: assert and error check ...
  9959    }
```

Lines 9951-9953 are the part textbooks skip: **predecessor rather than
successor**, because the predecessor is guaranteed to live in the subtree
under the child pointer of the very cell being removed. That containment is
what keeps the subsequent rebalancing local.

The fork then plays out in order: drop the interior cell first (`dropCell` at
:9980), then promote the leaf's *last* cell (`findCell(pLeaf, pLeaf->nCell-1)`
at :10003) into the interior page, then drop it from the leaf:

```c
// sqlite/sqlite src/btree.c — the promotion, inside sqlite3BtreeDelete, 10003-10013
 10003      pCell = findCell(pLeaf, pLeaf->nCell-1);
  // ... 10004-10010: corruption check, size, temp space, make the leaf writable ...
 10011        rc = insertCell(pPage, iCellIdx, pCell-4, nCell+4, pTmp, n);
  // ... 10012 ...
 10013      dropCell(pLeaf, pLeaf->nCell-1, nCell, &rc);
```

Line 10011 carries a detail worth its own sentence: `pCell-4` and `nCell+4`.
A leaf cell promoted to an interior page **grows by exactly four bytes**,
because interior cells carry a child page number and leaf cells do not — Step
1's `childPtrSize`, "0 if leaf==1. 4 if leaf==0" (btreeInt.h:282). The four
bytes are taken from in front of the cell and filled with `n`, the child pgno.

Both halves then have to be repaired, so `balance()` can be called twice:

```c
// sqlite/sqlite src/btree.c — the two balance calls after a delete, 10032-10048
 10032    assert( pCur->pPage->nOverflow==0 );
 10033    assert( pCur->pPage->nFree>=0 );
 10034    if( pCur->pPage->nFree*3<=(int)pCur->pBt->usableSize*2 ){
 10035      /* Optimization: If the free space is less than 2/3rds of the page,
 10036      ** then balance() will always be a no-op.  No need to invoke it. */
 10037      rc = SQLITE_OK;
 10038    }else{
 10039      rc = balance(pCur);
 10040    }
 10041    if( rc==SQLITE_OK && pCur->iPage>iCellDepth ){
  // ... 10042-10047: walk the cursor back up to the interior page ...
 10048      rc = balance(pCur);
```

Line 10034 states SQLite's underflow threshold in closed form:
`nFree × 3 ≤ usable × 2`, i.e. balance is skipped unless **more than two
thirds of the page is free** — a page under one third full. At usable = 4096
that is `nFree > 2730`. Compare LMDB, which merges below 25% full
(`FILL_THRESHOLD` 250 tenths of a percent, `mdb.c:1136`); SQLite tolerates
emptier pages before doing structural work. Line 10039 repairs the leaf; line
10048 walks back up and repairs the interior page, but only if the first
balance did not already propagate far enough (:10041).

Why it matters: every delete's structural work happens at leaf level, where
Step 5 already knows what to do — one mechanism instead of two — and the
price of that reuse is this fork, the two `dropCell`s, and the possibility of
two balances.

## Where each step lives in the code

**Start with btreeInt.h:1-215** — the file-format spec as a comment: page
layout diagram, header table, cell formats, freeblock list, overflow, freelist.
It is the best on-disk-format documentation in open source. Read it entire
before any function.

| File | Lines | What | Step |
|---|---|---|---|
| `sqliteLimit.h` | 214 | `SQLITE_DEFAULT_PAGE_SIZE 4096` | all |
| `btreeInt.h` | 110-134 | page layout + the 8/12-byte header table | 1 |
| `btreeInt.h` | 152-163 | freeblocks ≥ 4 B, fragments ≤ 3 B | 3 |
| `btreeInt.h` | 273-304 | `MemPage`: `nFree` −1 at :288, `apOvfl[4]` at :293, `xCellSize`/`xParseCell` at :302-303, `childPtrSize` at :282 | 1, 4, 7 |
| `btreeInt.h` | 480-486 | `CellInfo`: `nKey`, `pPayload`, `nPayload`, `nLocal`, `nSize` | 1 |
| `btreeInt.h` | 630, 647-668 | `PTRMAP_PTROFFSET` (5 B/entry) and the five entry types | 6 |
| `btree.c` | 1063-1075 | `ptrmapPageno` — the density formula at :1068 | 6 |
| `btree.c` | 1087, 1146 | `ptrmapPut`, `ptrmapGet` | 6 |
| `btree.c` | 1640 | `defragmentPage`; the ≤2-freeblock fast path at :1674-1708 | 3 |
| `btree.c` | 1774 | `pageFindSlot` — the freeblock-chain search | 3 |
| `btree.c` | 1846-1930 | `allocateSpace` — freeblocks :1889, defrag :1912, gap :1925 | 3 |
| `btree.c` | 1945 | `freeSpace` — "Adjacent freeblocks are coalesced" (:1937) | 3 |
| `btree.c` | 5837 | `sqlite3BtreeTableMoveto`; `biasRight` declared :5840, used :5920; midpoint :5953; right child :5965-5966 | 2 |
| `btree.c` | 6068 | `sqlite3BtreeIndexMoveto` — `xRecordCompare` per key shape | 2 |
| `btree.c` | 7106 | `fillInCell` — builds the overflow chain before insertion | 4 |
| `btree.c` | 7363 | `insertCell` — the `apOvfl[]` parking | 4 |
| `btree.c` | 7552 | `#define NB 3` | 5 |
| `btree.c` | 8022-8037 | `balance_quick`'s rationale and the ≤13-byte divider | 5 |
| `btree.c` | 8039 | `balance_quick` | 5 |
| `btree.c` | 8277 | `balance_nonroot` | 5 |
| `btree.c` | 8636-8646 | the left-packing bias and why correcting it is "not optional" | 5 |
| `btree.c` | 8730-8741 | the page renumbering that is "about 25% faster" | 5 |
| `btree.c` | 9081 | `balance_deeper` — the only height increase | 5 |
| `btree.c` | 9162 | `balance()` — the dispatcher loop; quick-balance gate :9216-9222 | 5 |
| `btree.c` | 9873 | `sqlite3BtreeDelete` | 7 |
| `btree.c` | 9948-9959 | why the *predecessor*, not the successor | 7 |
| `btree.c` | 10003-10013 | the promotion, growing the cell by 4 bytes at :10011 | 7 |
| `btree.c` | 10034-10048 | the ⅔-free skip, and the two `balance()` calls | 7 |

## Questions to answer in notes.md

1. `fillInCell` (btree.c:7106) builds the overflow chain BEFORE the cell is
   inserted into the page. What crash-safety property makes that ordering safe?
   (Pages only become durable at commit via pager/WAL — nothing here is.)
2. Why does `balance_quick` exist when `balance_nonroot` handles the same case?
   Estimate the work saved for a fillseq insert (pages touched, cells copied),
   then check your estimate against Step 5's `114 → 1` and against the
   five-condition gate at :9216-9222 — which of the five would a `fillrandom`
   workload violate first?
3. SQLite computes `nFree` lazily (btreeInt.h:288) and validates cells only
   under `SQLITE_DEBUG`. What does that say about where btree.c sits on the
   trust-the-page-vs-verify spectrum, and what's the corruption story?
   (`PRAGMA integrity_check` exists for a reason.)
4. The 25% comment at :8739 is about page *renumbering* (:8730-8734), not about
   packing. Which topic-1 measurement does that make it a sibling of, and what
   would you expect the 25% to become on an NVMe drive where sequential and
   random reads differ by far less than on the 2004 hardware it was measured
   on?

## Done when

Answer each before unfolding it.

- [ ] You can explain why `NB = 3`, and say what bounds it buys.

  <details><summary>Answer</summary>

  `#define NB 3` at btree.c:7552, commented "(NN*2+1): Total pages involved in
  the balance" — `NN = 1` sibling on each side, plus the page itself. It bounds
  the work per split to a constant: at most 3 sibling pages are read, pooled
  and rewritten, at most `NB+2 = 5` pages are renumbered (the comment at
  :8736-8737 relies on that bound to justify an O(N²) sort), and the parent
  gains at most a bounded number of dividers.

  What it buys is that adjacent redistribution usually *avoids* a split
  entirely — the cells simply spread across three pages instead of two — so
  the tree grows in height far less often than a naive "split at 100% full"
  scheme, without the unbounded cascade that pooling *all* siblings would
  cause. The trade is fill factor: three-way redistribution leaves pages
  fuller than a plain split does, which is why an append run would degrade
  under it, which is why Step 5's fast path exists.

  </details>

- [ ] You can name the two fast paths that serve sequential inserts, and say where each one lives.

  <details><summary>Answer</summary>

  (1) The **bias hint**: `biasRight` (declared btree.c:5840, used at :5920,
  `idx = upr>>(1-biasRight)`). It makes the *first* binary-search probe the
  rightmost cell rather than the midpoint, so an append finds its position in
  one comparison. Note the narrow scope — every later probe is the ordinary
  midpoint at :5953, so this does not skip the search.

  (2) **`balance_quick`** (btree.c:8039), gated by five conditions at
  :9216-9222: rowid-table leaf, exactly one overflow cell, the overflow cell
  sorts after every existing cell, the parent is not page 1, and the page is
  the parent's rightmost child. It allocates one page, puts the single new
  cell there, and pushes a ≤13-byte divider (:8033-8037) into the parent —
  instead of re-encoding ~114 cells across three siblings. Its own comment
  (:8022-8027) admits the tree is left "somewhat unbalanced" and bets that the
  nearly empty page will fill up: "On average."

  </details>

- [ ] You can say what the "about 25% faster" comment is actually about, and what it is *not* about.

  <details><summary>Answer</summary>

  It is at btree.c:8739 and it belongs to the block at :8730-8741, which
  **reassigns page numbers so the freshly balanced siblings end up in
  ascending order** — an O(N²) selection sort over at most 5 pages
  (:8747-8751) — "so that a scan of the table is closer to a linear scan
  through the file" (:8732-8734). It is a physical-locality optimization,
  the same phenomenon topic 1 measures as fillseq vs fillrandom.

  It is *not* about packing pages fuller on the left, and it is not about
  leaving room for the next append. The left-packing bias is a different
  thing, at :8636-8646, and the code that follows it exists to *undo* it: "This
  adjustment is more than an optimization. The packing above might be so out
  of balance as to be illegal... This adjustment is not optional." The
  leave-room-for-the-next-append argument is `balance_quick`'s, at :8022-8027.
  Three separate ideas within a hundred lines of each other; the previous
  edition of this chapter merged the first two and got both wrong.

  </details>

- [ ] You can state how many pages a lookup touches in a 10⁶-row table, show the arithmetic, and say why that is not a latency prediction.

  <details><summary>Answer</summary>

  With `P` = 4096 (`sqliteLimit.h:214`), a 100-byte payload and rowids below
  2²¹: a table leaf has an 8-byte header and 106-byte slots
  (2 pointer + 1 payload-size varint + 3 rowid varint + 100), so
  `L = floor(4088/106) = 38`. A table interior page has a 12-byte header
  (the extra 4 being the right child, btreeInt.h:134) and 9-byte slots
  (2 + 4 + 3), so `F = floor(4084/9) = 453`. Then
  `leaves = ceil(10⁶/38) = 26,316` and
  `ceil(log_453 26,315.8) = ceil(10.178/6.116) = ceil(1.6642) = 2` interior
  levels, for **3 pages touched**. At 10⁹ rows the rowid varint grows to 5
  bytes, `F` falls to `floor(4084/11) = 371`, and the answer is 4.

  Why it is not a latency prediction: this topic measured it. Height stays at
  3 from 10⁶ to 4×10⁶ keys while lookups climb 862 ns → 1101 ns (see the
  measured block in README.md and the ladder in notes.md). Height is a step
  function of `N`; latency is smooth, because height counts pages *touched*
  and says nothing about whether a touch hits L2, L3 or DRAM. Predict pages
  read with the arithmetic; predict time with a benchmark.

  </details>

- [ ] You can say what an interior delete costs that a leaf delete does not.

  <details><summary>Answer</summary>

  A leaf delete is one `dropCell` and possibly one `balance()`. An interior
  delete forks (Step 7): `sqlite3BtreePrevious` at btree.c:9956 walks down to
  the predecessor — chosen over the successor because it is guaranteed to sit
  in the subtree under the deleted cell's own child pointer (:9951-9953), which
  keeps the repair local. Then there are two `dropCell`s, at :9980 (the
  interior cell) and :10013 (the leaf cell), one `insertCell` at :10011 that
  grows the promoted cell by exactly 4 bytes (`pCell-4, nCell+4` — an interior
  cell carries a child pgno, a leaf cell does not, btreeInt.h:282), and up to
  **two** `balance()` calls: :10039 for the leaf, then :10048 after walking the
  cursor back up, if the first did not propagate far enough (:10041).

  Both balances are guarded by the threshold at :10034,
  `nFree*3 <= usableSize*2` — balance is skipped unless more than two thirds of
  the page is free, i.e. the page is under one third full. At usable = 4096
  that means `nFree > 2730`.

  </details>

- [ ] You can state the pointer map's cost in both space and writes.

  <details><summary>Answer</summary>

  Space: entries are 5 bytes (1 type + 4 page number), from
  `PTRMAP_PTROFFSET(pgptrmap, pgno) = 5*(pgno-pgptrmap-1)` at btreeInt.h:630,
  and `ptrmapPageno` groups the file into runs of
  `nPagesPerMapPage = (usableSize/5)+1` (btree.c:1068). At usable = 4096 that
  is `819 + 1 = 820`, so one page in 820 is a ptrmap page — **0.122%**. A
  rounding error.

  Writes: that is the real cost. Every page allocation, every split that moves
  a page, and every change to an overflow chain must also call `ptrmapPut`
  (btree.c:1087) and dirty the covering ptrmap page — turning one dirtied page
  into two, adding a second failure point to the balance code, and adding
  write traffic to the pager on the hottest paths. That is why it is
  compile-time optional (`SQLITE_OMIT_AUTOVACUUM`, btree.c:1053) and why turso
  has not implemented it.

  </details>

## References

**Code**

| File | Lines | What |
|---|---|---|
| `src/btreeInt.h` | 1-215 | the on-disk format as a comment — read it entire, before any function |
| `src/btreeInt.h` | 273-304 | `MemPage` — every production decision of Step 1 |
| `src/btree.c` | 1846-1930 | `allocateSpace` — the three routes, in order |
| `src/btree.c` | 5917-5968 | the descent loop and the one-probe bias |
| `src/btree.c` | 8022-8037 | why the append fast path exists |
| `src/btree.c` | 8636-8646 | the packing bias, and why fixing it is not optional |
| `src/btree.c` | 8730-8741 | the measured 25% — page renumbering, not packing |
| `src/btree.c` | 9948-10013 | the predecessor swap and the 4-byte promotion |

- [sqlite](https://github.com/sqlite/sqlite), pinned at `sqlite/sqlite@951de30`
  — `src/btree.c` (11,633 lines; don't read linearly) and `src/btreeInt.h`
  (746 lines).

**In this curriculum**
- [`reading-lmdb.md`](reading-lmdb.md) — the opposite design: no freeblocks,
  no pointer map, no WAL, and a page that is compacted on every delete.
- [`reading-turso-btree-deep.md`](reading-turso-btree-deep.md) — the same
  format reimplemented, with the simplifications this chapter is reading
  *against*.
- [README.md](README.md) §3 — the splits-and-balance diagram, and the measured
  height ladder that Step 2's arithmetic must be read alongside.
