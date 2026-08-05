# The SQLite file format: decode a row by hand

The normative spec for what btree.c writes — and the one document in this
topic you read with a hex dump open beside it. After two codebases' worth of
slotted pages, this chapter builds the format bottom-up in six steps —
header, page, varint, cell, record, and the fork where a payload outgrows its
page — verifies your mental model against the official text, and ends with the
exercise that makes the format yours: labelling every byte of one cell in a
real database file. ~1.5 h.

Two kinds of anchor appear below. Section numbers such as **§1.3.2** are
sections of *The SQLite Database File Format*,
<https://www.sqlite.org/fileformat.html>, which is normative. Line numbers
such as `btreeInt.h:130` are SQLite at the commit this repo pins,
**`sqlite/sqlite@951de30`** (confirm with `tools/pinned-source.py ref sqlite`),
where the same rules appear as the implementation's own comments. When the two
disagree, the document wins — but they do not disagree, and reading them
side by side is the point.

## The problem in one sentence

A two-row table is an **8,192-byte** file — two 4,096-byte pages — and by the
end of this chapter you must be able to point at every byte that encodes the
row `(500, 'world')` in a raw hex dump: page size, page type, cell pointer,
the two varints `0x83 0x74`, the record header, the serial type `0x17`, and
the five bytes `w o r l d`.

## The concepts, step by step

### Step 1 — the file is an array of pages, and byte 0 starts a 100-byte header

> **In:** a file, and nothing else — no schema, no catalogue, no side file.
> **Out:** one number, the page size, from which every other address in the
> file is computed. Steps 2-6 all address in page units.

An SQLite database file is fixed-size pages laid end to end (§1.2): page 1
begins at byte 0, page *N* at byte `(N−1) × page_size`. The first 100 bytes of
page 1 are the **database header** (§1.3) — and note the asymmetry, page 1 is
the only page that carries it, which is why `MemPage.hdrOffset` is documented
as "100 for page 1. 0 otherwise" (btreeInt.h:281).

The fields to find in your dump, with the offsets §1.3 gives them:

| Offset | Size | Field | Why you care |
|---|---|---|---|
| 0 | 16 | `"SQLite format 3\0"` | how a file(1)-style detector recognises it |
| **16** | **2** | **page size**, big-endian | the unit for every address below (§1.3.2) |
| 18, 19 | 1, 1 | write / read format version | 1 = legacy journal, 2 = WAL |
| **20** | **1** | **reserved bytes per page** | *usable size* = page size − this (§1.3.4) |
| 21, 22, 23 | 1 each | payload fractions, must be 64 / 32 / 32 | Step 6's overflow thresholds (§1.3.5) |
| 24 | 4 | file change counter | bumped on every write txn (§1.3.6) |
| 28 | 4 | in-header database size, in pages | §1.3.7 |
| 32 | 4 | first freelist **trunk** page | head of the free-page chain (§1.3.8) |
| 36 | 4 | total freelist pages | §1.3.8 |
| 40 | 4 | schema cookie | bumped when the schema changes (§1.3.9) |
| 92 | 4 | version-valid-for number | pairs with offset 24 — question 3 (§1.3.16) |

Two things the older edition of this chapter left out, and both bite in the
exercise:

- **Page size is big-endian and 4096 is `0x10 0x00`.** §1.3.2 adds a wrinkle:
  the value must be a power of two between 512 and 32768, *or the literal
  value 1*, which means 65536 — because 65536 does not fit in two bytes.
- **Offset 20 is usually 0 but is not always.** §1.3.4 calls it "Bytes of
  unused 'reserved' space at the end of each page. Usually 0." It is
  subtracted from the page size to give the **usable size**, which is the
  number every later formula actually uses. On macOS's system `sqlite3` (an
  Apple build, `3.51.0 …apl`) this byte reads `0x0c`, so usable = 4096 − 12 =
  4084. On a stock build it is 0 and usable = 4096. Check your own byte before
  trusting any arithmetic below.

Why it matters: everything downstream is addressed in page units, and the two
numbers that define those units — offsets 16 and 20 — live in exactly one
place each.

### Step 2 — the b-tree page: one type byte, then the slotted layout

> **In:** a page number and the page size from Step 1.
> **Out:** a page type, a cell count, and an array of 2-byte offsets — enough
> to find any cell on the page without decoding a single one of them.

Every page holding table or index data is a **b-tree page** (§1.6). Its first
byte declares the type, and the four legal values are worth memorising because
you will read them off a dump constantly:

| Byte | Page |
|---|---|
| `0x02` | interior **index** |
| `0x05` | interior **table** |
| `0x0a` | leaf **index** |
| `0x0d` | leaf **table** |

"Any other value for the b-tree page type is an error" (§1.6). The header that
follows is the slotted-page header you now know from two codebases, and
btreeInt.h states it in the same layout the document does:

```c
// sqlite/sqlite src/btreeInt.h — the b-tree page header, 126-134
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

Line 134 is why interior pages have a 12-byte header and leaves an 8-byte one.
Then comes the sorted array of 2-byte cell pointers, a gap, and the cells
packed from the page's end:

```
 offset 0        8 or 12      +2·nCell                  content-start    usable
 ┌───────────────┬─────────────────────┬───────────────┬───────────────────┐
 │ page header   │ cell ptr array (2 B │ unallocated   │ cells, packed     │
 │ type, nCell,  │ each, sorted by KEY │ gap           │ from the right    │
 │ content-start │        → grows      │               │        ← grows    │
 └───────────────┴─────────────────────┴───────────────┴───────────────────┘
        ↑ header offset 5 points here ─────────────────┘
```

The pointer array is sorted by *key*; the cells themselves are in arbitrary
physical order. That separation is the whole point of a slotted page — an
insert in the middle moves 2·k bytes of pointers, never a byte of payload.

Now verify your model against the normative text, because this is where two
rules are stated with a force people usually get wrong (§1.6):

> A freeblock requires at least 4 bytes of space. If there is an isolated
> group of 1, 2, or 3 unused bytes within the cell content area, those bytes
> comprise a fragment. … In a well-formed b-tree page, the total number of
> bytes in fragments may not exceed 60.

**That 60 is a well-formedness invariant, not a defragmentation trigger.** A
page carrying 61 fragment bytes is *corrupt*, not merely untidy. Defragmenting
is described separately and permissively — "SQLite **may** from time to time
reorganize a b-tree page so that there are no freeblocks or fragment bytes"
— and in the implementation it happens only when an allocation cannot
otherwise be satisfied (`allocateSpace`, btree.c:1909-1912; see
[`reading-sqlite-btree.md`](reading-sqlite-btree.md) Step 3). The previous
edition of this chapter said the counter was "capped at 60 before the page
must be defragmented", which fuses a validity rule with an unrelated policy.

Two more details from §1.6 that the implementation comment (btreeInt.h:152-163)
repeats: a freeblock's 2-byte size field counts **including the 4-byte
header**, and freeblocks are chained "in order of increasing offset".

Why it matters: this is where the spec is law — the codebases you read are
correct *because* they match these rules, not the other way around.

### Step 3 — the varint: SQLite's variable-length integer

> **In:** a byte offset inside a cell.
> **Out:** a 64-bit value *and* a length, so you know where the next field
> begins. Without this you cannot advance a single field in Steps 4-6.

A **varint** is, in §2.1's words, "a static Huffman encoding of 64-bit
twos-complement integers that uses less space for small positive values". It
is 1 to 9 bytes: seven payload bits per byte, high bit set meaning "more
follows", most significant group **first** — big-endian, unlike protobuf — and
a ninth byte, if reached, contributes all 8 of its bits. btreeInt.h states the
rule and then, unusually, hands you a test vector:

```c
// sqlite/sqlite src/btreeInt.h — the varint rule and its worked examples, 170-184
   170  ** Cell content makes use of variable length integers.  A variable
   171  ** length integer is 1 to 9 bytes where the lower 7 bits of each 
   172  ** byte are used.  The integer consists of all bytes that have bit 8 set and
   173  ** the first byte with bit 8 clear.  The most significant byte of the integer
   174  ** appears first.  A variable-length integer may not be more than 9 bytes long.
   175  ** As a special case, all 8 bits of the 9th byte are used as data.  This
   176  ** allows a 64-bit integer to be encoded in 9 bytes.
   177  **
   178  **    0x00                      becomes  0x00000000
   179  **    0x7f                      becomes  0x0000007f
   180  **    0x81 0x00                 becomes  0x00000080
   181  **    0x82 0x00                 becomes  0x00000100
   182  **    0x80 0x7f                 becomes  0x0000007f
   183  **    0x81 0x91 0xd1 0xac 0x78  becomes  0x12345678
   184  **    0x81 0x81 0x81 0x81 0x01  becomes  0x10204081
```

Line 182 is the one to stare at: `0x80 0x7f` and `0x7f` both decode to 127.
The encoding is not canonical, so a decoder must not assume minimal length.

Carry the decoder into the exercise:

```rust
// ILLUSTRATION — not quoted from SQLite. The rule is src/btreeInt.h:170-176;
// the real decoder is sqlite3GetVarint in src/util.c.
fn read_varint(buf: &[u8]) -> (u64, usize) {
    let mut v = 0u64;
    for i in 0..8 {
        v = (v << 7) | (buf[i] & 0x7f) as u64;
        if buf[i] < 0x80 {
            return (v, i + 1);        // high bit clear = last byte
        }
    }
    ((v << 8) | buf[8] as u64, 9)     // 9th byte contributes all 8 bits
}
```

Work the case you will meet in the dump, by hand:

```
rowid 500 = 0b1_1111_0100
  split into 7-bit groups, most significant first:
    group 1 = 500 >> 7      = 3   = 0b000_0011
    group 0 = 500 &  0x7f   = 116 = 0b111_0100
  set the continuation bit on every group but the last:
    byte 0 = 0x80 | 3       = 0x83
    byte 1 =        116     = 0x74
  encoded: 0x83 0x74        (2 bytes, versus 8 for a fixed u64)
  decode:  (3 << 7) | 116   = 384 + 116 = 500  ✓
```

Why it matters: you cannot find *anything* inside a cell without decoding
varints, because every length that tells you where the next field starts is
one.

### Step 4 — the cell: payload size, rowid, record

> **In:** one 2-byte entry from Step 2's pointer array.
> **Out:** a payload byte-range and a rowid — the row's identity and its
> contents, still undecoded.

A **cell** is one row's on-disk container. btreeInt.h gives the general shape
for all four page types at once:

```c
// sqlite/sqlite src/btreeInt.h — the general cell layout, 189-196
   189  ** The content of a cell looks like this:
   190  **
   191  **    SIZE    DESCRIPTION
   192  **      4     Page number of the left child. Omitted if leaf flag is set.
   193  **     var    Number of bytes of data. Omitted if the zerodata flag is set.
   194  **     var    Number of bytes of key. Or the key itself if intkey flag is set.
   195  **      *     Payload
   196  **      4     First page of the overflow chain.  Omitted if no overflow
```

For a table leaf (`0x0d`) three of those five rows survive: no left child
(line 192, it is a leaf), a payload-size varint (193), and — because
`intkey` is set — line 194 becomes *the rowid itself*, as a varint, rather
than a key length. Then the payload, and the 4-byte overflow pointer only if
Step 6 fired.

So a table-leaf cell is exactly:

```
  varint  payload size, in bytes (the record of Step 5)
  varint  rowid — the table's hidden 64-bit integer key
  bytes   the record, `payload size` of them
  [4 B]   first overflow page, present only when the payload did not fit
```

Concretely, for `(500, 'world')` in the exercise below, that reads
`08 | 83 74 | <8 bytes of record>` — payload size 8, rowid 500 as Step 3's two
bytes, then the record. Total cell size 11 bytes.

Why it matters: the cell is the unit the b-tree machinery moves, splits and
points at — and for an `INTEGER PRIMARY KEY` table the rowid varint *is* the
primary key, which is question 2 below and which you will see confirmed by a
`0x00` in the record.

### Step 5 — the record: serial types make pages schema-free

> **In:** the payload byte-range from Step 4.
> **Out:** typed column values — obtained without consulting the schema,
> which is the property that makes a page self-describing.

The payload is a **record** (§2.1): a varint giving the header length, then
one **serial type** varint per column, then the column values back to back. A
serial type is a single number encoding both the column's type *and* its byte
length, so the decoder never needs the table definition to know where one
value ends.

| Serial type *T* | Content bytes | Meaning |
|---|---|---|
| 0 | 0 | NULL |
| 1, 2, 3, 4 | 1, 2, 3, 4 | big-endian twos-complement integer |
| 5 | 6 | 48-bit integer |
| 6 | 8 | 64-bit integer |
| 7 | 8 | IEEE-754 double |
| **8** | **0** | **the integer 0** — value is entirely in the type |
| **9** | **0** | **the integer 1** — likewise |
| 10, 11 | var | reserved; never in a well-formed file |
| *T* ≥ 12, even | (*T*−12)/2 | BLOB of that length |
| *T* ≥ 13, odd | (*T*−13)/2 | text of that length, **no NUL terminator** |

Three points the older edition of this chapter got loose, and one it omitted:

- **The header-length varint counts itself.** §2.1: "The varint value is the
  size of the header in bytes *including the size varint itself*." Off-by-one
  here is the single most common error in the exercise.
- **Name the variable.** Going *up*, a text value of length `n` gets serial
  type `T = 2n + 13`; going *down*, a serial type `T` yields length
  `(T − 13)/2`. Those are inverses of each other, not two rules — and the
  previous edition wrote both with the same letter `n`, which makes them look
  contradictory. For `'hello'`, `n = 5`, so `T = 2·5 + 13 = 23 = 0x17`.
- **Types 8 and 9 have a version floor.** §2.1 marks both "(Only available for
  schema format 4 and higher.)" — the schema format number is at file offset
  44 (§1.3.10). A booleans-heavy table on an older schema format pays a byte
  per value that a modern one does not.
- **Five types are zero-length**, not two: §2.1 lists 0, 8, 9, 12 and 13 —
  the last two being the empty blob and the empty string. "If all columns are
  of these types then the body section of the record is empty."

Why it matters: this is the exercise's final boss — once you can read a serial
type and count value bytes, the whole file is legible without a schema.

### Step 6 — the fork: when a payload outgrows the page

> **In:** a record from Step 5 that is larger than a page can hold.
> **Out:** two things instead of one — a *local* prefix that stays in the
> cell, and an *overflow chain* of pages holding the rest. Every later reader
> must handle both halves, which is why this fork touches search, delete and
> vacuum alike.

A cell must fit on a page, and a record need not. §1.7 resolves this by
splitting the payload: the cell keeps a prefix, and the remainder goes into a
linked list of overflow pages, addressed by the 4-byte pointer from Step 4's
line 196.

```c
// sqlite/sqlite src/btreeInt.h — the overflow chain format, 198-204
   198  ** Overflow pages form a linked list.  Each page except the last is completely
   199  ** filled with data (pagesize - 4 bytes).  The last page can have as little
   200  ** as 1 byte of data.
   201  **
   202  **    SIZE    DESCRIPTION
   203  **      4     Page number of next overflow page
   204  **      *     Data
```

The threshold is not a constant; it is computed from Step 1's payload
fractions at header offsets 21, 22 and 23 — the bytes §1.3.5 requires to be
64, 32 and 32, each a fraction of 255. The implementation turns them into
four limits in one place:

```c
// sqlite/sqlite src/btree.c — the payload limits, inside sqlite3BtreeSetPageSize, 3471-3474
  3471    pBt->maxLocal = (u16)((pBt->usableSize-12)*64/255 - 23);
  3472    pBt->minLocal = (u16)((pBt->usableSize-12)*32/255 - 23);
  3473    pBt->maxLeaf = (u16)(pBt->usableSize - 35);
  3474    pBt->minLeaf = (u16)((pBt->usableSize-12)*32/255 - 23);
```

Evaluate them, naming every symbol. `U` = usable size = page size − reserved
bytes (Step 1, offsets 16 and 20). The `−12` is the interior page header; the
`−23` is a worst-case cell overhead allowance; `64/255` and `32/255` are the
header's payload fractions.

```
stock build, reserved = 0, U = 4096:
  maxLocal = floor((4096-12) × 64 / 255) - 23
           = floor(4084 × 64 / 255) - 23 = floor(261376/255) - 23
           = 1025 - 23 = 1002    ← index cells spill past 1002 B
  minLocal = floor(4084 × 32 / 255) - 23 = floor(130688/255) - 23
           = 512 - 23 = 489      ← at least this much always stays local
  maxLeaf  = 4096 - 35 = 4061    ← a TABLE leaf keeps up to 4061 B locally

Apple's system sqlite3, reserved = 12 (offset 20 = 0x0c), U = 4084:
  maxLocal = floor(4072 × 64 / 255) - 23 = 1022 - 23 = 999
  maxLeaf  = 4084 - 35 = 4049
```

The asymmetry at line 3473 is the design: a *table* leaf keeps almost the
whole page locally (4061 of 4096 bytes), because a table b-tree is where big
rows live and chasing an overflow chain to read one row would be miserable.
An *index* page keeps at most 1002 bytes — roughly a quarter of the page —
because an index page is searched, and search wants fanout, and fanout wants
small cells. Same format, two policies, both derived from three bytes in the
file header.

`minLocal` (line 3472) is the anti-thrash floor: at least 489 bytes always
stay local, so a payload that only just exceeds `maxLocal` cannot produce an
overflow page holding four bytes of data. The exact spill formula, and the
"≥ 4 cells per page" reasoning behind the `64/255`, belong to
[`reading-turso-btree-deep.md`](reading-turso-btree-deep.md), which reads a
reimplementation of these same four lines.

Why it matters: this fork is the reason `CellInfo` has both `nPayload` and
`nLocal` (btreeInt.h:483-484), why the pointer map needs two overflow entry
types (`PTRMAP_OVERFLOW1`/`2`, btreeInt.h:666-667), and why a delete has to
free a chain before it frees a cell.

## How to read the document (with the concepts in hand)

Section numbers below are the real ones on
<https://www.sqlite.org/fileformat.html>; the previous edition of this
chapter had three of them wrong, listed under "corrections" at the end.

| Read | Section | For | Step |
|---|---|---|---|
| 1st | **§1.2 Pages**, **§1.3 The Database Header** | the 100-byte header table; page size §1.3.2, reserved bytes §1.3.4, payload fractions §1.3.5, change counter §1.3.6, free page list §1.3.8, schema cookie §1.3.9, version-valid-for §1.3.16 | 1 |
| 2nd | **§1.6 B-tree Pages** | the slotted-page spec you know from two codebases — verify against it, especially the freeblock minimum of 4 bytes and the 60-byte fragment *validity* limit | 2 |
| 3rd | **§2.1 Record Format** | varints, the serial-type chart, the header-length-includes-itself rule | 3, 4, 5 |
| 4th | **§1.7 Cell Payload Overflow Pages** | the fork, and its interaction with §1.3.5's fractions | 6 |
| skim | **§1.5 The Freelist**, **§1.8 Pointer Map Pages** | free-page reuse and the reverse index; the ptrmap cost model is in [`reading-sqlite-btree.md`](reading-sqlite-btree.md) Step 6 | — |
| skim | **§3 The Rollback Journal**, **§4 The Write-Ahead Log** | how any of this becomes durable — topic 5 does it properly | — |

**Corrections to the previous edition of this chapter**, all verified against
the live document's table of contents: the record format is **§2.1**, not §2
(§2 is "Schema Layer"); pointer maps are **§1.8**, not §1.5 (§1.5 is "The
Freelist"); the rollback journal is **§3** and the WAL is **§4**, so "§4.1 WAL
vs rollback journal" was two sections conflated (§4.1 is "WAL File Format").
The canonical URL is `fileformat.html`; `fileformat2.html` also resolves but
is not the name the document uses for itself.

## The exercise (30 min, do it)

Write the scratch database inside the repo, not `/tmp`:

```bash
mkdir -p .cache/scratch && rm -f .cache/scratch/t.db
sqlite3 .cache/scratch/t.db \
  "create table t(a integer primary key, b text);
   insert into t values (1,'hello'),(500,'world');"
ls -l .cache/scratch/t.db          # expect exactly 8192 bytes = 2 × 4096
xxd -l 112       .cache/scratch/t.db   # the file header (Step 1)
xxd -s 4096 -l 16 .cache/scratch/t.db  # page 2's header  (Step 2)
xxd -s 8154 -l 38 .cache/scratch/t.db  # the two cells    (Steps 3-5)
```

Find by hand, writing offsets in notes.md:

1. **Offset 16** — the page size as a big-endian u16. **Offset 20** — your
   build's reserved bytes; compute `usable = page_size − reserved` and use it
   everywhere below.
2. **Page 2, offset 0** — the type byte. Then offsets 3-4 (cell count), 5-6
   (cell content area start), 1-2 (first freeblock) and 7 (fragments).
3. **Page 2, offsets 8-11** — the two 2-byte cell pointers. They are page
   offsets, so cell *k* begins at file byte `4096 + pointer[k]`. Note which
   pointer is larger, and explain it: cells grow leftward from the page's end,
   so the *second* row inserted sits at the *lower* offset.
4. Decode both cells: payload-size varint, rowid varint (rowid 500 needs the
   two bytes of Step 3), then the record — header-length varint (remember it
   counts itself), one serial type per column, then the values.
5. **Close the arithmetic.** Add each cell's total length to its pointer. The
   largest such sum must equal your usable size from step 1 — the first cell
   ends exactly at the usable boundary. If it does not, you mis-decoded a
   varint or misread offset 20.

Two things to notice while you are in there. The `a integer primary key`
column decodes to serial type **`0x00`, NULL** — the value is not stored
twice; it lives only in the cell's rowid varint (question 2). And the record
header is 3 bytes for both rows: one for its own length, one for each column's
serial type.

If you can decode a row from a hex dump, the format is yours.

## Questions to answer in notes.md

1. Why does the format store the cell content area offset in the header
   (§1.6, offset 5) instead of deriving it from the cell pointers? (Cheap
   free-space check: `content_start − (header + 2·nCell)` without scanning —
   and compare `MemPage.nFree`'s −1 sentinel, btreeInt.h:288.)
2. `INTEGER PRIMARY KEY` tables store the key only as the rowid varint — the
   column itself decodes to serial type 0, NULL, as you just saw in the dump.
   What does this alias buy in bytes per row for the exercise's table, and
   what does it forbid? (`WITHOUT ROWID` tables, §2.4, exist for the other
   case.)
3. The change counter (offset 24, §1.3.6) and the version-valid-for number
   (offset 92, §1.3.16) — how do they let a reader detect a stale in-memory
   schema without taking a lock? What has to be true about the *order* in
   which those two fields are written?
4. Offset 20's reserved bytes shrink the usable size, and every formula in
   Step 6 is denominated in usable size. If a build reserved 32 bytes per page
   for a checksum, recompute `maxLocal` and `maxLeaf` at a 4096-byte page, and
   say how many more index cells per page you would need to lose before the
   tree gained a level (use the fanout arithmetic in
   [`reading-sqlite-btree.md`](reading-sqlite-btree.md) Step 2).

## Done when

Answer each before unfolding it.

- [ ] Your notes contain the annotated hex dump with every byte of one cell labelled.

  <details><summary>Answer</summary>

  For `(500, 'world')` on a stock build, the cell is 11 bytes and reads
  `08 83 74 03 00 17 77 6f 72 6c 64`:

  | Bytes | Field | Value |
  |---|---|---|
  | `08` | payload-size varint (Step 4) | 8 bytes of record follow the rowid |
  | `83 74` | rowid varint (Step 3) | `(3 << 7) \| 116` = 500 |
  | `03` | record header length (§2.1) | 3 — **and it counts itself** |
  | `00` | serial type, column `a` | NULL — the rowid alias (question 2) |
  | `17` | serial type, column `b` | 23, odd ⇒ text of length (23−13)/2 = 5 |
  | `77 6f 72 6c 64` | body | `w o r l d`, no NUL terminator |

  Check it closes: header 3 + body 0 + 5 = 8 = the payload size. And
  1 + 2 + 8 = 11 = the cell's total length, which is what you add to its
  pointer in exercise step 5.

  </details>

- [ ] You can state the two numbers in the file header that every later formula depends on, and where they are.

  <details><summary>Answer</summary>

  **Offset 16** (2 bytes, big-endian) is the page size — §1.3.2, a power of
  two from 512 to 32768, or the literal `1` meaning 65536 because 65536 does
  not fit in two bytes. **Offset 20** (1 byte) is the reserved bytes per page
  — §1.3.4, "Usually 0".

  Usable size = page size − reserved bytes, and *that* is the quantity every
  formula uses: the cell content area's upper bound, `maxLocal`/`minLocal`/
  `maxLeaf` (btree.c:3471-3474), the ptrmap group size (btree.c:1068), and the
  fanout arithmetic. It is worth checking your own byte: macOS's system
  `sqlite3` reports `0x0c` there, so usable is 4084, not 4096, and every
  derived number shifts.

  </details>

- [ ] You can say what the 60-byte fragment limit means, and what it does not.

  <details><summary>Answer</summary>

  §1.6: "In a well-formed b-tree page, the total number of bytes in fragments
  may not exceed 60." It is a **validity invariant** — a page whose one-byte
  counter at header offset 7 exceeds 60 is corrupt, and `PRAGMA
  integrity_check` will say so.

  It is *not* a defragmentation trigger. §1.6 describes defragmenting
  separately and permissively — SQLite "may from time to time reorganize a
  b-tree page so that there are no freeblocks or fragment bytes" — and the
  implementation only does it when an allocation cannot be satisfied any other
  way (`allocateSpace`, btree.c:1909-1912). A fragment exists at all only
  because a freeblock needs 4 bytes to hold its own next-pointer and size
  (§1.6, btreeInt.h:152-163), so 1-3 stranded bytes have no way to describe
  themselves and can only be counted.

  </details>

- [ ] You can explain why a page is decodable without the schema, and name the one thing that is not.

  <details><summary>Answer</summary>

  Because the record format (§2.1) puts a **serial type** varint in front of
  every value, and a serial type encodes both the datatype and the byte length:
  `T ≥ 13` odd means text of length `(T−13)/2`, `T ≥ 12` even means a blob of
  `(T−12)/2`, `T` in 1-7 are fixed widths, and 0, 8, 9 carry their value
  entirely in the type number and occupy zero bytes. So a decoder can walk
  every column of every row with no table definition in hand — which is what
  makes `xxd` a usable tool here at all.

  What is *not* recoverable: the column **names**, their declared types and
  affinities, and the table's name. Those live only in the `sqlite_schema`
  table on page 1 (§2.6), as ordinary rows holding the original `CREATE TABLE`
  text. A page tells you a value is a 5-byte string; only the schema tells you
  the string is called `b`.

  </details>

- [ ] You can say when a payload forks into an overflow chain, with the number evaluated.

  <details><summary>Answer</summary>

  When the record exceeds the page's local limit, computed at btree.c:3471-3474
  from the three payload fractions at file header offsets 21, 22, 23 — required
  by §1.3.5 to be 64, 32, 32, each over 255. With `U` = usable size = 4096
  (reserved = 0):

  - a **table leaf** keeps up to `maxLeaf = U − 35 = 4061` bytes locally;
  - an **index** page keeps up to
    `maxLocal = floor((U−12)×64/255) − 23 = floor(261376/255) − 23 = 1025 − 23 = 1002`;
  - and whatever spills, at least
    `minLocal = floor((U−12)×32/255) − 23 = 512 − 23 = 489` bytes stay behind,
    so a marginal overflow cannot create a nearly empty overflow page.

  The asymmetry is deliberate: table leaves hold rows and want them local;
  index pages are searched and want fanout, so they cap cells at about a
  quarter of a page. The chain itself is a singly linked list, each page a
  4-byte next-pointer followed by data, every page but the last completely
  full (btreeInt.h:198-204).

  </details>

## References

**The document**
- SQLite team — *The SQLite Database File Format*,
  <https://www.sqlite.org/fileformat.html> — normative. Read side by side with
  a real file and a hex dump.

| Section | What to take |
|---|---|
| §1.2 | pages are fixed size; page *N* starts at `(N−1) × page_size` |
| §1.3 | the 100-byte header table — offsets 16, 20, 21-23, 24, 32, 36, 40, 92 |
| §1.6 | b-tree page header; freeblocks ≥ 4 B; fragments ≤ 60 B for *validity* |
| §1.7 | cell payload overflow pages |
| §2.1 | varints, the serial-type chart, header length includes itself |
| §2.4 | `WITHOUT ROWID` tables — question 2's other case |

**Code (the same rules, as the implementation's own comments)**

| File | Lines | What |
|---|---|---|
| `src/btreeInt.h` | 110-134 | page layout and the header offset table |
| `src/btreeInt.h` | 152-163 | freeblock and fragment rules |
| `src/btreeInt.h` | 170-184 | the varint rule *and* seven worked examples |
| `src/btreeInt.h` | 189-204 | cell layout and the overflow chain |
| `src/btree.c` | 3471-3474 | `maxLocal` / `minLocal` / `maxLeaf` — Step 6's thresholds |

Pinned at `sqlite/sqlite@951de30`; confirm with `tools/pinned-source.py ref sqlite`.

**In this curriculum**
- [`reading-sqlite-btree.md`](reading-sqlite-btree.md) — the code that writes
  this format, including what it does when a page fills.
- [`reading-turso-btree-deep.md`](reading-turso-btree-deep.md) — the same
  format decoded by a second implementation, where Step 6's spill formula is
  worked in full.
