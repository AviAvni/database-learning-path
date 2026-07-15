# The SQLite file format: decode a row by hand

The normative spec for what btree.c writes — and the one document in this
topic you read with a hex dump open beside it. After two codebases' worth of
slotted pages, this chapter builds the format bottom-up in five steps —
header, page, varint, cell, record — verifies your mental model against the
official text, and ends with the exercise that makes the format yours:
labeling every byte of one cell in a real database file. ~1.5 h.

## The problem in one sentence

A two-row table is an **8,192-byte** file (two 4 KB pages), and by the end of
this chapter you must be able to point at every byte that encodes the row
`(500, 'world')` in a raw hex dump — page size, page type, cell pointer,
varints, serial types, payload.

## The concepts, step by step

### Step 1 — the file is an array of pages, and byte 0 starts a 100-byte header

An SQLite database file is nothing but fixed-size pages laid end to end —
page 1 begins at byte 0, page N at byte `(N−1) × page_size` — and the first
100 bytes of page 1 are the **file header** that says how to read everything
else. The fields to find in your dump:

- **page size** at offset 16 (big-endian u16 — most significant byte first;
  4096 is stored as `0x10 0x00`);
- the **file change counter** at offset 24, bumped on every write
  transaction;
- the **freelist** head page and count at offsets 32–39 (the chain of
  wholly-unused pages);
- the **schema cookie**, bumped whenever the schema changes.

Why it matters: everything downstream is addressed in page units, and the
page size that defines those units lives in exactly one place — these two
bytes.

### Step 2 — the b-tree page: one type byte, then the slotted layout

Every page that stores table or index data is a **b-tree page**: its first
byte declares the page type (`0x0D` table leaf, `0x05` table interior,
`0x0A` index leaf, `0x02` index interior), followed by the slotted-page
header you now know from two codebases — cell count, offset where cell
content starts, first-freeblock pointer, fragmented-bytes counter — then the
sorted array of 2-byte cell pointers, a gap, and the cells themselves packed
from the page's end.

```
 ┌───────────────┬─────────────────────┬───────┬───────────────────┐
 │ 8/12 B header │ cell ptr array (2B  │ free  │ cells, packed     │
 │ type,ncell,   │ each, sorted order) │ gap   │ from the right    │
 │ content-start │        →grows       │       │        ←grows     │
 └───────────────┴─────────────────────┴───────┴───────────────────┘
```

Verify your mental model against the normative text — especially the
**freeblock** rules (a freeblock is a reusable hole left by a delete): a
freeblock must be at least 4 bytes, and leftovers too small to be freeblocks
are counted in the header's fragment counter, capped at 60 before the page
must be defragmented.

Why it matters: this is where the spec is law — the codebases you read are
correct *because* they match these rules, not the other way around.

### Step 3 — the varint: SQLite's variable-length integer

A **varint** is an integer encoded in 1–9 bytes, 7 payload bits per byte,
where a set high bit means "more bytes follow" — small numbers (the common
case: short lengths, low rowids) cost one byte instead of eight. SQLite's
flavor is **big-endian** (most significant group first — unlike protobuf),
and a 9th byte, if reached, contributes all 8 of its bits.

Every cell starts with varints, so carry the decoder in your head into the
exercise:

```rust
// SQLite varint: 7 bits/byte, BIG-endian (unlike protobuf), max 9 bytes
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
// rowid 500 = 0x83 0x74 → (0b0000011 << 7) | 0b1110100 — find it in the dump
```

Why it matters: you cannot find *anything* inside a cell without decoding
varints, because every length that tells you where the next field starts is
one.

### Step 4 — the cell: payload size, rowid, record

A **cell** is one row's on-disk container. In a table leaf it is exactly
three parts, in order: a varint giving the payload size, a varint giving
the **rowid** (the table's hidden 64-bit integer key), then the payload —
the encoded row itself (Step 5).

Concretely, for the row `(500, 'world')`: payload-size varint, then rowid
500 as the two bytes `0x83 0x74` (check the 7-bit encoding), then the
record. The cell pointer in Step 2's array is what tells you where this cell
begins.

Why it matters: the cell is the unit the b-tree machinery moves, splits, and
points at — and for an INTEGER PRIMARY KEY table, the rowid varint here *is*
the primary key (question 2 below).

### Step 5 — the record: serial types make pages schema-free

The payload is a **record**: a varint giving the header length, then one
**serial type** varint per column (a single number encoding both the
column's type *and* its byte length), then the column values back to back —
so a page can be decoded with no schema in hand.

The serial-type table is the heart of §2. Note especially:

- types **8 and 9** mean literal integer 0 and 1 with **zero bytes of
  payload** — the value is entirely in the type number;
- text and blob lengths ride inside the type number via the odd/even
  encoding: text of length n has serial type `2n+13` (decode with
  `(n−13)/2`), blobs use even numbers (`(n−12)/2`). So `'hello'` (text,
  length 5) has serial type 2·5+13 = 23.

Why it matters: this is the exercise's final boss — once you can read a
serial type and count value bytes, the whole file is legible.

## How to read the document (with the concepts in hand)

Read in this order:

1. **§1 The database file** — Step 1's 100-byte file header: page size
   (offset 16), file change counter, freelist head + count (offsets 32–39),
   schema cookie.
2. **§1.6 B-tree pages** — Step 2: the slotted-page spec you now know from
   two codebases; verify your mental model against the normative text (esp.
   freeblock rules: min 4 bytes, fragment cap 60).
3. **§2 Record format** — Steps 3–5: the serial types table. Note types 8/9
   (literal 0 and 1 — zero bytes of payload!) and the odd/even text/blob
   length encoding `(n−13)/2` / `(n−12)/2`.
4. **§1.5 Pointer maps**, **§4.1 WAL vs rollback journal** — skim; WAL is
   topic 5.

## The exercise (30 min, do it)

```bash
sqlite3 /tmp/t.db "create table t(a integer primary key, b text);
                   insert into t values (1,'hello'),(500,'world');"
xxd /tmp/t.db | head -80
```

Find by hand, writing offsets in notes.md:
- page size at offset 16 (big-endian u16);
- page 2's header byte `0x0D` (table leaf), cell count, content-area start;
- the two cell pointers, then decode cell 1: payload-size varint, rowid varint
  (rowid 500 needs 2 bytes — check the 7-bit encoding), record header, serial
  type for 'hello' (text len 5 ⇒ type 2·5+13 = 23).

If you can decode a row from a hex dump, the format is yours.

## Questions to answer in notes.md

1. Why does the format store the cell CONTENT area offset in the header instead
   of deriving it from the cell pointers? (Cheap free-space check: `content_start
   − ptr_array_end` without scanning.)
2. INTEGER PRIMARY KEY tables store the key only as the rowid varint — the
   column itself is NULL in the record. What does this alias buy in bytes/row
   and what does it forbid? (WITHOUT ROWID tables exist for the other case.)
3. The change counter (offset 24) and version-valid-for (92) — how do they let
   a reader detect a stale in-memory schema without locks?

## Done when

Your notes contain the annotated hex dump with every byte of one cell labeled.

## References

**Papers**
- SQLite team — "The SQLite Database File Format" (official
  documentation) — https://www.sqlite.org/fileformat2.html — the
  normative spec for what btree.c writes; read side-by-side with a real
  database file and a hex dump
