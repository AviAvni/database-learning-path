# The SQLite file format: decode a row by hand

The normative spec for what btree.c writes — and the one document in this
topic you read with a hex dump open beside it. After two codebases' worth of
slotted pages, this chapter verifies your mental model against the official
text and ends with the exercise that makes the format yours: labeling every
byte of one cell in a real database file. ~1.5 h.

## Read in this order

1. **§1 The database file** — 100-byte file header: page size (offset 16),
   file change counter, freelist head + count (offsets 32–39), schema cookie.
2. **§1.6 B-tree pages** — the slotted-page spec you now know from two codebases;
   verify your mental model against the normative text (esp. freeblock rules:
   min 4 bytes, fragment cap 60).
3. **§2 Record format** — serial types table. Note types 8/9 (literal 0 and 1
   — zero bytes of payload!) and the odd/even text/blob length encoding
   `(n−13)/2` / `(n−12)/2`.
4. **§1.5 Pointer maps**, **§4.1 WAL vs rollback journal** — skim; WAL is topic 5.

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
