# Inside the slotted page: freeblocks, overflow, balance

Topic 1's turso chapter traced the cursor/seek/insert surface; this one
descends into the page mechanics that surface glossed over — the freeblock
chain, the exact overflow-spill formulas, the resumable balance state
machines, varints, and the whole-page freelist. This chapter builds each
mechanism step by step, then maps every step to its anchors. Budget: 2–3 h
across `core/storage/btree.rs`, `core/storage/sqlite3_ondisk.rs`,
`core/storage/pager.rs`, and `core/types.rs`.

Every anchor below is turso at `tursodatabase/turso@dd775bc`. Confirm the pin
with `tools/pinned-source.py ref turso` before you start; if it prints a
different SHA, the line numbers in this guide are for a different tree and you
should navigate by the symbol names, which are given for every anchor. Read a
range with, for example:

```
tools/pinned-source.py show turso core/storage/btree.rs -r 7592:7687
```

Turso is a Rust rewrite of SQLite that is byte-compatible with the SQLite file
format, so this guide and [reading-sqlite-btree.md](reading-sqlite-btree.md)
describe the *same* format through two implementations. Where turso simplifies,
clarifies, or renames something, this guide says so — those diffs are the
cheapest way to see what is essential in the format and what is C-era
incidental. The on-disk field definitions themselves are in
[reading-sqlite-file-format.md](reading-sqlite-file-format.md); that guide
defers the overflow-spill arithmetic to this one, and Step 6 below pays that
debt.

## The problem in one sentence

A **page** — a fixed-size chunk of the file, 4,096 bytes by default — must
absorb variable-length rows, up to and including a 100 KB payload, plus
arbitrary deletes and re-inserts, forever, without the on-disk format ever
needing a special case; freeblocks, overflow chains, and 3-sibling balancing
are the entire toolkit.

Two size symbols recur throughout and are worth pinning now. `P` is the **page
size** (4,096 by default). `R` is the **reserved region** at the end of every
page, a byte count stored at file-header offset 20 that the b-tree layer is
forbidden to touch. `U = P − R` is the **usable space** — the part of the page
the b-tree actually gets. Almost every formula below is written in `U`, not
`P`, and turso's parameter for it is literally named `usable_space`. A stock
build has `R = 0` and therefore `U = P = 4096`; Apple's system SQLite ships
`R = 12`, so `U = 4084` there. This guide works every number at `U = 4096` and
flags where `R` would move it.

## The concepts, step by step

### Step 1 — the slotted page: a header, a growing pointer array, and a shrinking content area

> **In:** a raw 4,096-byte page and nothing else. **Out:** the meaning of all
> seven header fields, and the fanout and tree height those bytes buy —
> evaluated, not asserted.

A **slotted page** stores variable-length records in a fixed-size block by
splitting the block in three: a header at the front, an array of 2-byte
offsets ("slots", or the **cell pointer array**) growing rightward from the
header, and the **cell content area** — the records themselves — growing
leftward from the end. The unallocated gap between the two is what is left.
A **cell** is one record's on-disk container: for a table leaf, one row.

Turso names every header offset in one module, `btree.rs:84–124`, with the
layout drawn in the doc comment above it at `btree.rs:76–83`:

| offset | width | field | meaning |
|---|---|---|---|
| 0 | 1 B | `BTREE_PAGE_TYPE` | leaf/interior × table/index |
| 1 | 2 B | `BTREE_FIRST_FREEBLOCK` | head of the freeblock chain, 0 = none |
| 3 | 2 B | `BTREE_CELL_COUNT` | number of slots in the pointer array |
| 5 | 2 B | `BTREE_CELL_CONTENT_AREA` | offset of the lowest live cell byte |
| 7 | 1 B | `BTREE_FRAGMENTED_BYTES_COUNT` | unusable scraps, ≤ 60 |
| 8 | 4 B | `BTREE_RIGHTMOST_PTR` | interior pages only |

So the header is **8 bytes on a leaf** and **12 bytes on an interior page** —
the last field exists only where there is an extra child to point at. Turso
gives both, plus the two other geometry constants, real names:

```rust
// tursodatabase/turso@dd775bc — core/storage/sqlite3_ondisk.rs
// the four constants every page-arithmetic formula in this guide is built from
    80  pub const CELL_PTR_SIZE_BYTES: usize = 2;
    81  pub const INTERIOR_PAGE_HEADER_SIZE_BYTES: usize = 12;
    82  pub const LEAF_PAGE_HEADER_SIZE_BYTES: usize = 8;
    83  pub const LEFT_CHILD_PTR_SIZE_BYTES: usize = 4;
```

Two of those constants are easy to skip past and both matter below. Every cell
costs **2 bytes of pointer** in addition to its own bytes — that 2 is
`CELL_PTR_SIZE_BYTES`, and forgetting it is the classic way to overcount
fanout by a few percent. And an interior cell carries a **4-byte child page
number**, `LEFT_CHILD_PTR_SIZE_BYTES`, before anything else.

Now the arithmetic. Define:

- `U` = usable space per page = 4096 (stock build).
- `H_leaf` = 8, `H_int` = 12 — the header widths above.
- `p` = 2 — `CELL_PTR_SIZE_BYTES`.
- `c` = 4 — `LEFT_CHILD_PTR_SIZE_BYTES`.
- `L` = **leaf fanout**: how many rows fit on one leaf page.
- `F` = **interior fanout**: how many children one interior page can name.

Take a concrete row: a table with a 100-byte payload and rowids below
2²¹ = 2,097,152, so the rowid varint (Step 4) is 3 bytes and the payload-size
varint is 1 byte. One table-leaf slot therefore costs:

```text
slot_leaf = p + size_varint + rowid_varint + payload
          = 2 + 1            + 3            + 100
          = 106 bytes

L = floor((U − H_leaf) / slot_leaf)
  = floor((4096 − 8) / 106)
  = floor(4088 / 106)
  = floor(38.57)
  = 38 rows per leaf
```

A table-*interior* cell has no payload at all (Step 5): it is a 4-byte child
pointer and a rowid varint, and nothing else.

```text
slot_int_table = p + c + rowid_varint
               = 2 + 4 + 3
               = 9 bytes

F = floor((U − H_int) / slot_int_table)
  = floor((4096 − 12) / 9)
  = floor(4084 / 9)
  = floor(453.8)
  = 453 children per interior page
```

That 453 is the number to remember, and it is why the original guide's
hand-wave — "table interior cells are ~13 bytes, so table trees have enormous
fanout" — is worth replacing with a figure. (13 is the *worst case*: 4 bytes of
child plus a 9-byte rowid varint, which only occurs above rowid 2⁵⁶. At that
point `F` falls to `floor(4084/15) = 272`.)

Height follows. Let `N` be the row count and `d` the number of **pages a
lookup touches**, root included:

```text
N = 1,000,000
leaves = ceil(N / L) = ceil(1000000 / 38) = 26,316

interior levels = ceil( log(leaves) / log(F) )
                = ceil( log(26316) / log(453) )
                = ceil( 10.178 / 6.116 )
                = ceil( 1.6642 )
                = 2

d = 2 interior + 1 leaf = 3 pages touched
```

At `N = 10⁹` the rowid varint grows to 5 bytes, so `F = floor(4084/11) = 371`,
`leaves = ceil(10⁹/38) = 26,315,790`, and
`ceil(log(26315790)/log(371)) = ceil(17.086/5.916) = ceil(2.888) = 3`, giving
**4 pages touched**. A thousandfold more data costs exactly one more page
touch. That is the whole argument for B-trees over binary trees, and it is
also, deliberately, only half the story — see the caveat at the end of this
step.

Turso encodes the same arithmetic as a corruption check:

```rust
// tursodatabase/turso@dd775bc — core/storage/btree.rs
// the depth bound, and the two balance-width constants used in Step 7
   126  /// Maximum depth of an SQLite B-Tree structure. Any B-Tree deeper than
   127  /// this will be declared corrupt. This value is calculated based on a
   128  /// maximum database size of 2^31 pages a minimum fanout of 2 for a
   129  /// root-node and 3 for all other internal nodes.
   130  ///
   131  /// If a tree that appears to be taller than this is encountered, it is
   132  /// assumed that the database is corrupt.
   133  pub const BTCURSOR_MAX_DEPTH: usize = 20;
   134
   135  /// Maximum number of sibling pages that balancing is performed on.
   136  pub const MAX_SIBLING_PAGES_TO_BALANCE: usize = 3;
   137
   138  /// We only need maximum 5 pages to balance 3 pages, because we can guarantee that cells from 3 pages will fit in 5 pages.
   139  pub const MAX_NEW_SIBLING_PAGES_AFTER_BALANCE: usize = 5;
```

Check the 20 against its own stated premises — worst case, not the 453 above:

```text
root fanout 2, internal fanout 3, so with `k` edges from root to leaf:
    leaves ≤ 2 · 3^(k−1)

k = 19:  2 · 3^18 =   774,840,978  <  2^31 = 2,147,483,648
k = 20:  2 · 3^19 = 2,324,522,934  >  2^31
```

So 20 edges is the first depth that can address a 2³¹-page file under the
worst legal fanout, and anything deeper is by definition corrupt. The constant
is exact, not a round number someone picked.

**The caveat, and it is this topic's headline.** Everything above computes
pages *touched*. It does not compute time. This topic's `README.md` records
lookups climbing **862 → 1101 ns** between 1e6 and 4e6 keys with height pinned
at 3 the whole way — a 28% slowdown with no change in `d` at all. Height sets
how many pages a lookup touches; what a touch *costs* is set by whether that
page is in CPU cache, and at 270 MB it is not. Read the measured block in
`README.md` and the height ladder in `notes.md` before you let the tidy
logarithm above convince you that fanout is the only lever.

Why it matters: `L` and `F` are the only two numbers in this entire guide that
the format designer actually controls. Steps 2 through 6 are all, in the end,
about protecting them.

### Step 2 — the freeblock chain: free space is a linked list in the dead bytes

> **In:** a page whose header (Step 1) says where the content area starts.
> **Out:** what a delete costs, and the exact rule that turns leftover bytes
> into unusable fragments.

When a cell is deleted, its bytes become a **freeblock** — a hole inside the
content area, threaded into a singly linked list *through the dead space
itself*. Each freeblock's first 4 bytes are a 2-byte offset of the next
freeblock (0 = end) and a 2-byte size **that includes those 4 header bytes**.
The list head is `BTREE_FIRST_FREEBLOCK` in the page header, and the chain is
kept in **ascending offset order** — `find_free_slot` treats a non-ascending
next-pointer as corruption (`btree.rs:7629–7631`).

Two rules keep the bookkeeping inside 4 bytes. A freeblock must be at least
**4 bytes**, because anything smaller cannot hold its own next-pointer and
size (`CELL_SIZE_MIN` at `btree.rs:7597`). And leftovers below that threshold
are instead counted in the header's 1-byte fragment counter — bytes that are
free but unaddressable, because nothing can point at them.

Allocation is first-fit down the chain, in `find_free_slot`
(`btree.rs:7592–7687`). The interesting branch is what happens when the
request *almost* fills a block:

```rust
// tursodatabase/turso@dd775bc — core/storage/btree.rs — find_free_slot
  7636          let new_size = size - amount;
  7637          // If the freeblock's new size is < CELL_SIZE_MIN, the freeblock is deleted and the remaining bytes
  7638          // become fragmented free bytes.
  7639          if new_size < CELL_SIZE_MIN {
  7640              if page_ref.num_frag_free_bytes() > 57 {
  7641                  // SQLite has a fragmentation limit of 60 bytes.
  7642                  // check sqlite docs https://www.sqlite.org/fileformat.html#:~:text=A%20freeblock%20requires,not%20exceed%2060
  7643                  return Ok(None);
  7644              }
```

Read the guard at 7640 carefully, because the constant is **57, not 60**. The
format's invariant is that the fragment counter never exceeds 60. The leftover
about to be absorbed here is `new_size`, which this branch has just established
is 1, 2, or 3 bytes. So refusing at `> 57` guarantees `57 + 3 = 60` at worst:
the code checks the *pre-*state against a bound that leaves room for the
largest legal increment. Refusing returns `None`, which sends the caller to
Step 3.

Otherwise the block is carved and — this is the neat part —
`btree.rs:7669–7682` shrinks the block in place and returns
`Ok(Some(cur + new_size))`, i.e. the allocation is taken from the block's
**tail**. The freeblock's 4-byte header stays at its original offset, so
neither the chain's ascending order nor its predecessor's next-pointer needs
touching. A first-fit allocation that hits this branch relinks nothing at all.

The mirror operation is `free_cell_range` (`btree.rs:8097–…`), which may
coalesce the freed range into the next freeblock, the previous one, or both.
It also has a case worth noticing at `btree.rs:8118–8125`: if the chain is
empty *and* the freed range starts exactly at the content-area boundary, no
freeblock is created — the content-area pointer simply moves right and the
bytes rejoin the unallocated gap. Deleting the most recently inserted cell
leaves no trace.

Why it matters: a delete costs a 2-byte pointer-array edit plus threading one
hole. All the cleanup is deferred to Step 3 and paid only when space actually
runs short. That is the same bargain LMDB makes at page granularity and
log-structured stores make at file granularity — defer the compaction, pay it
in a batch.

### Step 3 — defragmentation: compact the holes when first-fit fails

> **In:** a page where Step 2's first-fit returned `None` even though the
> total free byte count is sufficient. **Out:** why that situation is possible
> at all, and which of two algorithms turso picks.

**Defragmentation** rewrites all live cells contiguously against the page's
end, then zeroes the freeblock chain and the fragment counter — turning many
scattered holes into one usable gap. It is the answer to the question Step 2
leaves open: total free space can exceed a request while *no single freeblock*
does.

The entry point is `defragment_page` (`btree.rs:8422`), and it chooses between
two algorithms at `btree.rs:8435–8440`:

- the **fast path**, `defragment_page_fast` (`btree.rs:8273`), used when there
  are **at most 2 freeblocks** and the fragment count is within the caller's
  `max_frag_bytes` budget. Its doc comment (`btree.rs:8268–8272`) gives the
  reasoning: with one or two holes it is cheaper to `memmove` the two or three
  surviving runs of cells and add a fixed delta to each affected pointer than
  to rebuild the page. Note the last line of that comment — the fast path
  **does not reduce the fragment count**, it only moves cells.
- the **full path**, reached otherwise, which reconstructs the page cell by
  cell. `defragment_page_full` (`btree.rs:8399–8401`) forces it by passing
  `max_frag_bytes = -1`: since `num_frag_free_bytes()` is unsigned and
  compared as `isize`, `x <= -1` is never true, so the fast-path test at 8436
  always fails. A sentinel, not a budget.

Question to hold while reading: what triggers defrag, and why is it correct to
move cells but never the pointer array? (The pointer array *is* the sorted
index — cells are only ever reached through it, so rewriting cell offsets in
place is invisible to every reader of the page. The array's *order* is the
data structure; the cells' positions are an implementation detail.)

Why it matters: defrag is O(page size) — the fee for Step 2's cheap deletes,
charged rarely and all at once. The fast path exists because the common case
after a single delete-then-insert is exactly one freeblock, and paying a full
4 KB rebuild for that would make the deferred-cleanup bargain a bad one.

### Step 4 — varints and the record format

> **In:** a byte range that Step 5 will identify as one cell's payload.
> **Out:** the integers and column values inside it, decoded without a schema.

A **varint** is an integer encoded in 1–9 bytes, big-endian (most significant
group first), 7 payload bits per byte with the high bit meaning "another byte
follows". The 9th byte, if reached, contributes a full 8 bits — so the ceiling
is 8×7 + 8 = 64 bits, exactly a `u64`, in at most 9 bytes. Small numbers —
short lengths, low rowids — cost 1 byte instead of 8, which is precisely why
`L = 38` and `F = 453` in Step 1 rather than the smaller figures fixed-width
integers would give.

```rust
// tursodatabase/turso@dd775bc — core/storage/sqlite3_ondisk.rs — read_varint
  1304  pub fn read_varint(buf: &[u8]) -> Result<(u64, usize)> {
  1305      let mut v: u64 = 0;
  1306      for i in 0..8 {
  1307          match buf.get(i) {
  1308              Some(c) => {
  1309                  v = (v << 7) + (c & 0x7f) as u64;
  1310                  if (c & 0x80) == 0 {
  1311                      return Ok((v, i + 1));
  1312                  }
  1313              }
```

Eight iterations of 7 bits, then a separate ninth-byte case at
`sqlite3_ondisk.rs:1320–1331` that shifts by 8 rather than 7. That case also
carries a canonicalization check at `:1326`: a 9-byte encoding whose top 8 bits
are zero is rejected as corrupt, because such a value had a shorter encoding
and a well-formed writer would have used it. The encoder is `write_varint`
(`sqlite3_ondisk.rs:1379`).

On top of varints sits the **record** — the encoding of one row. Its shape is:
a header-size varint, then one **serial type** varint per column, then the raw
column values back to back. A serial type is a single number that encodes both
the column's type *and* its byte length; text of length `n` is `2n+13` and a
blob of length `n` is `2n+12`, so lengths ride inside the type tag and no
separate length field exists. Five serial types (0, 8, 9, 12, 13) occupy **zero
bytes** in the value area — NULL, integer 0, integer 1, and the empty
blob/string carry their entire value in the tag.

Turso's record header walk is in `core/types.rs`, and it pins down the one
detail everyone gets wrong on the first read:

```rust
// tursodatabase/turso@dd775bc — core/types.rs — record header parse
  1651          let (header_size, header_varint_len) = read_varint(payload)?;
  1652          let header_size = header_size as usize;
  1653
  1654          if header_size > payload.len()
  1655              || header_varint_len > payload.len()
  1656              || header_varint_len > header_size
  1657          {
```

The slice taken from this is `&payload[header_varint_len..header_size]` (the
same computation at `types.rs:1196`). Both bounds are measured from the *start
of the record*, which means **the header-size varint counts itself**. The check
at 1656 — `header_varint_len > header_size` is corrupt — is exactly the
statement that the header must be at least big enough to contain its own length
field. Per-serial-type decoding is `read_value_serial_type`
(`sqlite3_ondisk.rs:1101`) and `read_value` (`sqlite3_ondisk.rs:973`).

Why it matters: serial types are why pages are schema-less — any page can be
fully decoded with no catalogue in hand, which is what lets the b-tree layer,
the pager, and every recovery tool work on bytes alone. Every cell begins with
varints, and every balance or overflow computation below starts by decoding
them.

### Step 5 — the four cell formats

> **In:** a page type byte from Step 1 and a slot offset from the pointer
> array. **Out:** which of exactly four layouts to parse, and what that choice
> costs in fanout.

There are two b-tree flavours — **table** trees keyed by rowid, **index** trees
keyed by column values — times two page levels, giving exactly four cell
layouts. Turso declares them as four structs at
`sqlite3_ondisk.rs:774–812`, and the field lists *are* the format:

| cell | layout | struct |
|---|---|---|
| table interior | `child u32 ∥ rowid varint` | `:782–785` |
| table leaf | `size varint ∥ rowid varint ∥ payload` | `:788–795` |
| index interior | `child u32 ∥ size varint ∥ payload` | `:798–804` |
| index leaf | `size varint ∥ payload` | `:807–812` |

Three of the four structs also carry `first_overflow_page: Option<u32>` — the
tail of Step 6's chain. The table-interior cell is the exception and carries no
payload at all, so it can never overflow. Parsing is `read_btree_cell`
(`sqlite3_ondisk.rs:816`).

Two consequences fall straight out of the table:

1. A table-interior cell is 4 bytes of child plus a rowid varint, so the
   `slot_int_table = 9` and `F = 453` of Step 1 hold. Table trees are shallow
   because their interior cells are nearly empty.
2. An index-interior cell carries the **whole key**. Redo Step 1's interior
   arithmetic for a 16-byte index key: the slot costs
   `p + c + size_varint + key = 2 + 4 + 1 + 16 = 23` bytes, so
   `F = floor(4084 / 23) = 177` — **2.6× worse than the table tree's 453**,
   from key bytes alone.

Note there is **no prefix or suffix truncation anywhere**: turso, like SQLite,
stores full keys in interior cells. Graefe's survey treats suffix truncation as
standard practice and
[reading-graefe-survey.md](reading-graefe-survey.md) works through what it
buys; this topic's `notes.md` measures the same gap from the other end, where a
32-byte key costs 2.5× the interior slots of an 8-byte key. That missing
optimization is your experiment's opening.

Why it matters: fanout is not a property of the page size, it is a property of
the *key*, and only index trees pay. This is question 1 below.

### Step 6 — overflow: the exact spill formulas

> **In:** a payload from Step 4 that does not fit the page. **Out:** exactly
> how many bytes stay local, how many overflow pages result, and why the
> constants are 64/255 and 32/255.

When a payload is too big for its page, the excess **overflows** into a chain
of dedicated overflow pages, each holding a 4-byte next-page number (0
terminates) followed by `U − 4` payload bytes. Only a prefix stays "local" in
the cell, and the last 4 bytes of that local region are the first overflow
page number — verified at `sqlite3_ondisk.rs:951–957`, which reads
`unread[cell_len-4 .. cell_len]` as a big-endian `u32` and hands back
`&unread[..cell_len-4]` as the local payload.

Two thresholds govern the decision, both at `btree.rs:9010–9043`:

- `max_local` — the largest payload that stays entirely local.
  - index pages: `(U − 12) · 64/255 − 23`
  - table pages: `U − 35`
- `min_local` — the smallest local prefix a spilled payload may keep:
  `(U − 12) · 32/255 − 23`, the **same formula for all four page types**
  (`btree.rs:9040–9042`; the `page_type` parameter is `_page_type`, unused).

At `U = 4096`:

```text
max_local(index) = floor((4096 − 12) · 64 / 255) − 23
                 = floor(4084 · 64 / 255) − 23
                 = floor(261376 / 255) − 23
                 = 1025 − 23
                 = 1002 bytes

min_local        = floor(4084 · 32 / 255) − 23
                 = floor(130688 / 255) − 23
                 = 512 − 23
                 = 489 bytes

max_local(table) = 4096 − 35 = 4061 bytes
```

These are the identical figures `reading-sqlite-btree.md` derives from
`sqlite/sqlite@951de30` `src/btree.c:3471–3474`, which is the point: the
formulas are file-format constants, not implementation choices, and a rewrite
that changed them would produce unreadable files.

**Why 64/255 and 32/255?** The doc comment states the design goal outright at
`btree.rs:9015`: "Give a minimum fanout of 4 for index b-trees". Check it. Four
index-interior cells at the maximum local size cost

```text
per cell:  max_local(index) + p + c + size_varint(1002 is 2 bytes)
        =  1002            + 2 + 4 + 2
        =  1010 bytes
4 cells =  4040 bytes  ≤  U − H_int = 4084     ✓  (44 bytes spare)
5 cells =  5050 bytes  >  4084                 ✗
```

So four maximal cells fit and five cannot — the fraction 64/255 = 0.25098 is
"just over a quarter", and the −23 is a conservative allowance for cell
overhead (the real overhead here is 8). The second stated goal at
`btree.rs:9016–9017` is the one usually forgotten: keep enough payload local
that **the record header of Step 4 can normally be read without following the
chain**, so a query that only needs column types never touches an overflow
page.

Now the spill rule itself, which the doc comment states at
`btree.rs:9034–9036` and the code implements:

```rust
// tursodatabase/turso@dd775bc — core/storage/sqlite3_ondisk.rs — payload_overflows
  2138      if payload_size <= payload_overflow_threshold_max {
  2139          return (false, 0);
  2140      }
  2141
  2142      let mut space_left = payload_overflow_threshold_min
  2143          + (payload_size - payload_overflow_threshold_min) % (usable_size - 4);
  2144      if space_left > payload_overflow_threshold_max {
  2145          space_left = payload_overflow_threshold_min;
  2146      }
  2147      (true, space_left + 4)
```

Naming the symbols: `P_size` is the total payload, `M = min_local`,
`X = max_local`, and `K = M + (P_size − M) mod (U − 4)` is the candidate local
size at 2142–2143. The rule is **two-branch**, and the second branch at
2144–2145 is the one usually left out of summaries:

- if `K ≤ X`, keep `K` bytes local;
- **otherwise keep exactly `M` bytes local.**

The `+ 4` at 2147 is the overflow page pointer, which the cell must also hold.

The point of the `K` branch is that `P_size − K` is then an exact multiple of
`U − 4`, so **every overflow page including the last is completely full** — no
partly-used page in the chain. Work a case where it holds, a 5,000-byte table
row:

```text
K = 489 + (5000 − 489) mod 4092
  = 489 + 4511 mod 4092
  = 489 + 419
  = 908                       ≤ 4061 = X  →  first branch
remainder = 5000 − 908 = 4092 = exactly one full overflow page
```

Now work the guide's own headline case, a 100 KB row, and watch the property
fail:

```text
P_size = 102,400
K = 489 + (102400 − 489) mod 4092
  = 489 + 101911 mod 4092
  = 489 + 3703
  = 4192                      > 4061 = X  →  SECOND branch
local     = M = 489 bytes
remainder = 102400 − 489 = 101,911
pages     = ceil(101911 / 4092) = 25 overflow pages
last page = 101911 − 24·4092 = 3,703 of 4,092 bytes used
```

So "sized so the last overflow page is exactly full" is a property of the first
branch only. Roughly one remainder in eight lands above `X` for a table leaf
(`K > 4061` requires the modulus to exceed 3,572, i.e. 519 of 4,092 possible
values) and takes the fallback, where the chain does end with a partial page.
Do not state the packing property unconditionally.

Why it matters: overflow trades extra page reads for one fat value against
tree height for everyone else. Without it, a single 100 KB row would force a
page size that wrecks `L` and `F` for the other million rows.

### Step 7 — balance as a resumable state machine

> **In:** a page that Step 2 and Step 3 together could not make room on.
> **Out:** which pages get rewritten, how many come out, and what the async
> rewrite costs in invariants.

**Balancing** is what runs when an insert overflows a page: pool the cells of
the overfull page, up to two siblings, and the **divider cells** between them
(the parent's separator entries), then redistribute the pool evenly. Turso's
twist on SQLite is that balancing is a **resumable state machine** returning
`IOResult` rather than synchronous recursion, because every page touch may
yield for async IO.

The dispatcher is `balance` (`btree.rs:2793`), whose match arms at
`btree.rs:2895–2904` route to three routines:

- `balance_quick` (`btree.rs:2895–2897`) — the append fast path, when the
  overflowing page is the rightmost leaf of its subtree. Its doc comment at
  `btree.rs:2909–2915` spells out the four steps: allocate a new right sibling,
  put the overflow cell in it alone, insert one divider into the parent, and
  move the parent's rightmost pointer. No cells are redistributed at all.
  `reading-sqlite-btree.md` measures what this saves on a sequential load.
- `balance_root` (`btree.rs:4774`) — root overflow. Allocate a child, copy the
  root's contents into it, and the root becomes an interior page pointing at
  it. This is the *only* operation that increases tree height, which is why
  B-trees grow at the root rather than the leaves. Note `btree.rs:4789–4790`:
  when the root is page 1 the copy must skip the 100-byte file header, which is
  the whole reason root splits copy rather than simply reusing the page.
- `balance_non_root` (`btree.rs:2995–4309`) — everything else.

`balance_non_root` is a five-phase state machine, and knowing the phase
boundaries is the difference between reading it and drowning in it:

| sub-state | lines | what it does |
|---|---|---|
| `NonRootPickSiblings` | 3014–3271 | choose up to `MAX_SIBLING_PAGES_TO_BALANCE` = 3 neighbours |
| `NonRootDoBalancing` | 3272–3814 | pool cells, size the output pages, decide the split points |
| `NonRootDoBalancingAllocate` | 3815–3855 | allocate any new sibling pages needed |
| `NonRootDoBalancingFinish` | 3856–4281 | write cells into the new pages, rewrite parent dividers |
| `FreePages` | 4282–4309 | return now-empty siblings to Step 8's freelist |

Inside the pooling phase there is a distinction that is easy to miss and is one
of the more elegant things in the format. At `btree.rs:3507–3517`, **for table
leaves the divider cells are not pooled at all** — they stay in the parent as
bookkeeping — while for index and interior pages they *are* pooled. The reason
is Step 5's table: a table-interior divider is only `(child, rowid)`, and after
redistribution the correct rowid is simply the largest one on the page to its
left, so the divider can be *regenerated* rather than moved. An index divider
carries a real key that exists nowhere else and must be redistributed like any
other cell. The assertions at 3518–3529 exist to catch getting this backwards.

The output width is bounded by `MAX_NEW_SIBLING_PAGES_AFTER_BALANCE = 5`
(`btree.rs:139`), asserted at `btree.rs:3597–3600` with the message "it is
corrupt to require more than 5 pages to balance 3 siblings". So the contract is
**≤3 pages in, ≤5 pages out**, and every balance is O(1) pages regardless of
tree size.

The sizing itself happens in two passes, and turso quotes SQLite verbatim for
the second:

```rust
// tursodatabase/turso@dd775bc — core/storage/btree.rs — balance_non_root, sizing pass 2
  3689                      // Comment borrowed from SQLite src/btree.c
  3690                      // The packing computed by the previous block is biased toward the siblings
  3691                      // on the left side (siblings with smaller keys). The left siblings are
  3692                      // always nearly full, while the right-most sibling might be nearly empty.
  3693                      // The next block of code attempts to adjust the packing of siblings to
  3694                      // get a better balance.
  3695                      //
  3696                      // This adjustment is more than an optimization.  The packing above might
  3697                      // be so out of balance as to be illegal.  For example, the right-most
  3698                      // sibling might be completely empty.  This adjustment is not optional.
```

Pass one (`btree.rs:3588–3676`) greedily packs cells left until each page is
full, spilling into a new page when needed. Pass two (`btree.rs:3699–3793`)
walks the pages right-to-left moving cells back. The comment is worth taking at
its word: the first pass can produce an *illegal* page, not merely an ugly one,
so the second pass is a correctness step. This is the same comment
`reading-sqlite-btree.md` anchors at `sqlite/sqlite@951de30`
`src/btree.c:8636–8646`; the C original and the Rust copy agree line for line.

```text
 balance_non_root, 2 siblings + overfull page:

 parent:      [ ... D1 ... D2 ... ]         D = divider cells
                 │      │      │
        [sib L]   [OVERFULL]   [sib R]
        └──────── pool: L + D1 + full + D2 + R ────────┘
             redistribute ⇒ up to 5 pages, new dividers up
             (table leaves: D1, D2 stay in the parent — btree.rs:3511)
```

Why it matters: pooling ≤3 siblings bounds the work per balance while leaving
pages fuller than a naive half/half split would. And the state-machine shape
forces every intermediate state to be resumable — which is question 3 below.

### Step 8 — the freelist: recycling whole pages

> **In:** the pages Step 7's `FreePages` phase emptied, plus whole trees
> dropped by DDL. **Out:** where those pages go, and why the file never
> shrinks.

Separately from Step 2's *within-page* holes, whole pages freed by drops and
balances go on the **freelist** — a chain of **trunk pages**, each holding a
next-trunk `u32`, a leaf-count `u32`, and then an array of free page numbers.
The "leaves" are just page IDs; their contents are never read. Turso documents
the layout in a comment and four constants:

```rust
// tursodatabase/turso@dd775bc — core/storage/sqlite3_ondisk.rs
    85  // Freelist trunk page layout:
    86  // - Bytes 0-3: Page number of next freelist trunk page (0 if none)
    87  // - Bytes 4-7: Number of leaf page pointers on this trunk page
    88  // - Bytes 8+: Array of 4-byte leaf page pointers
    89  pub const FREELIST_TRUNK_OFFSET_NEXT_TRUNK_PTR: usize = 0;
    90  pub const FREELIST_TRUNK_OFFSET_LEAF_COUNT: usize = 4;
    91  pub const FREELIST_TRUNK_OFFSET_FIRST_LEAF_PTR: usize = 8;
    92  pub const FREELIST_TRUNK_HEADER_SIZE: usize = 8;
    93  pub const FREELIST_LEAF_PTR_SIZE: usize = 4;
```

Freeing is `Pager::free_page` (`pager.rs:5019–5154`) — note the name, because
there is no `add_page_to_freelist` in this tree. Its capacity test is at
`pager.rs:5103–5104`:

```text
max_free_list_entries = U / FREELIST_LEAF_PTR_SIZE − RESERVED_SLOTS
                      = 4096 / 4 − 2
                      = 1024 − 2
                      = 1022 leaf pointers per trunk page
```

The `RESERVED_SLOTS = 2` (`pager.rs:5022`) are the next-trunk and leaf-count
`u32`s. If there is room, the page number is appended to the current trunk
(`pager.rs:5106–5126`); if not, the page being freed **becomes a new trunk**
pointing at the old one (`pager.rs:5130–5148`). So the freelist's own index
structure costs nothing extra — it is built out of the pages it is tracking.
One trunk per 1,022 free pages is an overhead of `1/1023` ≈ **0.098%** of the
freed space.

Allocation is `Pager::allocate_page` (`pager.rs:5250`), which prefers reuse:
read the first trunk (`pager.rs:5301–5314`), and if it has leaves, pop one
(`ReuseFreelistLeaf`, `pager.rs:5390–5447`). Only when the freelist is empty
(`pager.rs:5302–5303`) does it fall through to `AllocateNewPage`
(`pager.rs:5450`) and grow the file. When a trunk runs out of leaves the trunk
page ITSELF is handed out as the allocation — the list consumes its own
skeleton.

Why it matters: the file never shrinks on delete; it recycles. This is the
page-granularity mirror of Step 2's freeblock story — same deferred-cleanup
bargain, three orders of magnitude up — and it is the structure your capstone's
pager will need too. It is also why `VACUUM` exists as a separate, explicit,
whole-file operation.

## Where each step lives in the code

All anchors are `tursodatabase/turso@dd775bc`. Symbol names are given so the
anchors survive a re-pin.

- **Step 1 — slotted page geometry**: header offset module `btree.rs:84–124`
  with the layout diagram at `:76–83`; the four size constants
  `sqlite3_ondisk.rs:80–83`; `BTCURSOR_MAX_DEPTH` and the two balance-width
  constants `btree.rs:126–139`.
- **Step 2 — freeblocks**: `find_free_slot` `btree.rs:7592–7687` (ascending-order
  check `:7629–7631`; the 57-byte fragment guard `:7640`; tail-carve return
  `:7669–7682`); `free_cell_range` `btree.rs:8097–…` with the
  no-freeblock-needed case at `:8118–8125`; `compute_free_space`
  `btree.rs:8689`.
- **Step 3 — defragment**: dispatcher `defragment_page` `btree.rs:8422`, path
  choice `:8435–8440`; `defragment_page_fast` `btree.rs:8273` (rationale
  `:8268–8272`); `defragment_page_full` `btree.rs:8399–8401`;
  `defragment_page_for_insert` `btree.rs:8412`.
- **Step 4 — varints + records**: `read_varint` `sqlite3_ondisk.rs:1304–1337`
  (9-byte case `:1320–1331`, canonicalization check `:1326`); `write_varint`
  `sqlite3_ondisk.rs:1379`; record header parse `core/types.rs:1650–1660` and
  `:1187–1196`; `read_value_serial_type` `sqlite3_ondisk.rs:1101`; `read_value`
  `sqlite3_ondisk.rs:973`.
- **Step 5 — cell formats**: `BTreeCell` enum `sqlite3_ondisk.rs:774–779`, the
  four structs `:782–812`, parser `read_btree_cell` `:816`.
- **Step 6 — overflow**: `payload_overflow_threshold_max` `btree.rs:9019–9028`
  and `payload_overflow_threshold_min` `btree.rs:9040–9043`, with the design
  rationale in the doc comments at `:9010–9018` and `:9030–9038`; the spill
  rule `payload_overflows` `sqlite3_ondisk.rs:2132–2148` (fallback branch
  `:2144–2145`); chain pointer extraction `sqlite3_ondisk.rs:951–957`.
- **Step 7 — balance**: dispatcher `balance` `btree.rs:2793`, arms `:2895–2904`;
  `balance_quick` doc `:2909–2915`; `balance_root` `btree.rs:4774` (page-1
  header offset `:4789–4790`); `balance_non_root` `btree.rs:2995–4309` with
  sub-states `NonRootPickSiblings` `:3014`, `NonRootDoBalancing` `:3272`,
  `NonRootDoBalancingAllocate` `:3815`, `NonRootDoBalancingFinish` `:3856`,
  `FreePages` `:4282`; table-leaf divider rule `:3507–3517`; five-page assert
  `:3597–3600`; the borrowed SQLite comment and rebalancing pass `:3689–3793`.
- **Step 8 — freelist**: trunk layout `sqlite3_ondisk.rs:85–93`;
  `Pager::free_page` `pager.rs:5019–5154` (capacity `:5103–5104`, append
  `:5106–5126`, new trunk `:5130–5148`); `Pager::allocate_page` `pager.rs:5250`
  (trunk read `:5301–5314`, `ReuseFreelistLeaf` `:5390–5447`,
  `AllocateNewPage` `:5450`).

## Questions to answer in notes.md

1. Why do table-btree interior cells store only rowids (no payload) while
   index-btree interior cells carry the full key? What does that do to fanout?
   Use Step 1's and Step 5's numbers: 453 versus 177 at a 16-byte key.
2. The freeblock minimum is 4 bytes and the fragment counter is capped at 60 —
   what goes wrong without defragmentation? Describe a page where the total
   free space exceeds a request but `find_free_slot` still returns `None`.
3. Turso's balance yields mid-operation for IO. What invariant must hold at
   every yield point so a concurrent reader (or a crash) never sees a broken
   tree? (Hint: WAL — pages aren't durable until commit; in-memory the cursor
   holds refs.)
4. Step 6's spill rule has two branches. Construct a payload size that takes
   each, and say what the chain's last overflow page looks like in both cases.

## Done when

Answer each before unfolding it.

- [ ] Write the byte layout of a table-leaf page holding two cells and one
  freeblock, from memory — every header field, the pointer array, and the
  freeblock's own 4 bytes.

<details>
<summary>Answer</summary>

Header, 8 bytes (`btree.rs:84–124`): byte 0 page type (`0x0d` = table leaf);
bytes 1–2 first freeblock offset; bytes 3–4 cell count = 2; bytes 5–6 cell
content area start; byte 7 fragment count. No rightmost pointer — that field
exists only on interior pages, which is what makes the leaf header 8 rather
than 12.

Bytes 8–11: the cell pointer array, two 2-byte big-endian offsets, in **key
order**, not allocation order.

Then the unallocated gap, then the content area growing leftward from `U`. The
freeblock sits inside the content area: 2 bytes next-offset (0 if it is the
only one) then 2 bytes size, and **that size includes these 4 header bytes**.
The header's byte 1–2 field points at it.

Sanity check the arithmetic closes: content-area start + (sum of live cell
sizes) + (sum of freeblock sizes) + fragment count = `U`.
</details>

- [ ] Explain what `balance_non_root` pools, and why the bound is 3.

<details>
<summary>Answer</summary>

It pools the cells of the overfull page plus up to two neighbouring siblings,
plus the divider cells separating them in the parent — except for table leaves,
where dividers stay in the parent because a `(child, rowid)` divider can be
regenerated from the largest rowid on the page to its left
(`btree.rs:3507–3517`).

The bound is `MAX_SIBLING_PAGES_TO_BALANCE = 3` (`btree.rs:136`), with output
bounded by `MAX_NEW_SIBLING_PAGES_AFTER_BALANCE = 5` (`btree.rs:139`), asserted
at `:3597–3600`. Three is the smallest window that lets an underfull page draw
from *both* neighbours, so it can usually be fixed without changing the
parent's cell count; and holding the window at a constant makes every balance
O(1) pages no matter how big the tree is. Wider windows pack pages better but
make each insert's worst case worse.
</details>

- [ ] Compute the interior fanout `F` of a table b-tree on a 4,096-byte page
  with rowids under 2²¹, and the number of pages a lookup touches at
  `N = 10⁶`. Name every term.

<details>
<summary>Answer</summary>

`U = 4096`, interior header `H_int = 12`, cell pointer `p = 2`, child pointer
`c = 4`, rowid varint 3 bytes at that magnitude. Slot = `2 + 4 + 3 = 9`, so
`F = floor((4096 − 12)/9) = floor(4084/9) = 453`.

Leaves: with a 100-byte payload the leaf slot is `2 + 1 + 3 + 100 = 106` and
`L = floor(4088/106) = 38`, so `leaves = ceil(10⁶/38) = 26,316`. Interior
levels = `ceil(log 26316 / log 453) = ceil(10.178/6.116) = ceil(1.664) = 2`,
so **3 pages touched** including the leaf.

And the point of Step 1's caveat: that 3 does not predict latency. This topic's
`README.md` shows lookups going 862 → 1101 ns while `d` stays at 3.
</details>

- [ ] State the fragment-counter guard in `find_free_slot` and explain why the
  constant is 57.

<details>
<summary>Answer</summary>

`if page_ref.num_frag_free_bytes() > 57 { return Ok(None) }`
(`btree.rs:7640`). The branch it guards absorbs a leftover of 1, 2, or 3 bytes
into the fragment counter, and the format's invariant is that the counter never
exceeds 60. Testing the pre-state against 57 leaves headroom for the largest
legal increment: `57 + 3 = 60`. Refusing returns `None`, which sends the caller
to defragmentation (Step 3).

Note what the 60 is: a **validity invariant** — a page whose counter exceeds it
is corrupt — not a threshold that triggers anything by itself.
</details>

- [ ] Give the two branches of the overflow spill rule, and say which one
  leaves a partially-filled last overflow page.

<details>
<summary>Answer</summary>

With `M = min_local`, `X = max_local`, `P_size` the payload and
`K = M + (P_size − M) mod (U − 4)` (`sqlite3_ondisk.rs:2142–2143`): if `K ≤ X`
keep `K` local; otherwise keep exactly `M` (`:2144–2145`). Either way the cell
also holds a 4-byte overflow pointer (`:2147`).

The first branch makes `P_size − K` an exact multiple of `U − 4`, so every
overflow page including the last is full. The **second** branch is the one that
leaves a partial page: a 100 KB row at `U = 4096` gives `K = 4192 > 4061 = X`,
falls back to 489 local bytes, and its 25th and last overflow page holds
3,703 of 4,092 bytes.
</details>

- [ ] Say where a page freed by a balance goes, and why the file does not
  shrink.

<details>
<summary>Answer</summary>

`balance_non_root`'s `FreePages` sub-state (`btree.rs:4282–4309`) calls
`Pager::free_page` (`pager.rs:5019`), which appends the page number to the
current freelist trunk if it has room — 1,022 leaf pointers at `U = 4096`,
from `U/4 − 2` at `pager.rs:5103–5104` — or turns the freed page into a new
trunk pointing at the old one (`:5130–5148`).

The file does not shrink because nothing ever truncates it: `allocate_page`
(`pager.rs:5250`) reuses freelist pages first and only extends the file when
the list is empty (`:5302–5303` → `AllocateNewPage` `:5450`). Reclaiming the
space to the filesystem is a separate explicit operation, `VACUUM`.
</details>

## References

**Code**
- [turso](https://github.com/tursodatabase/turso), pinned at `dd775bc` —
  `core/storage/btree.rs` (slotted-page ops, overflow thresholds, balance state
  machines), `core/storage/sqlite3_ondisk.rs` (cell formats, varints, spill
  rule, freelist trunk layout), `core/storage/pager.rs` (page allocation and
  freeing), `core/types.rs` (record header walk). Local clone at `~/repos/turso`;
  confirm the pin with `tools/pinned-source.py ref turso`.
- Extends topic 1's
  [reading-turso-btree.md](../01-storage-engine-landscape/reading-turso-btree.md),
  which covers the cursor/seek/insert surface this guide descends beneath.
- [sqlite](https://github.com/sqlite/sqlite), pinned at `951de30` — the C
  original. `src/btree.c:3471–3474` carries the same overflow formulas and
  `:8636–8646` the same rebalancing comment turso quotes at `btree.rs:3689`.

**In this topic**
- [reading-sqlite-btree.md](reading-sqlite-btree.md) — the same algorithms in
  C, including `balance_quick` and the page-number-ordering optimization turso
  has not adopted.
- [reading-sqlite-file-format.md](reading-sqlite-file-format.md) — the on-disk
  field definitions, byte offset by byte offset; it defers the spill arithmetic
  to Step 6 here.
- [reading-graefe-survey.md](reading-graefe-survey.md) — what suffix truncation
  would buy the index-interior fanout of Step 5, and why neither SQLite nor
  turso implements it.
- `README.md` and `notes.md` — this topic's measured height ladder, and the
  reason Step 1's logarithm is only half the story.

**Docs**
- [SQLite file format](https://www.sqlite.org/fileformat.html) — §1.6 for the
  b-tree page header and the 60-byte fragment bound turso cites verbatim at
  `btree.rs:7641–7642`.
