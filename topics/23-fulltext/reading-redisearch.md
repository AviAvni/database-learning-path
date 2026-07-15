# RediSearch in Rust: a mutable inverted index

Home turf: this is what FalkorDB delegates full-text to, and the
interesting part is that the C core is being strangler-figged into
Rust crates behind FFI (`c_entrypoint/inverted_index_ffi`,
`varint_ffi`) — the exact migration pattern falkordb-rs-next-gen
lives. Read the `inverted_index` crate as a *mutable, in-memory*
counterpart to tantivy's immutable segments
([reading-tantivy.md](reading-tantivy.md)). Before pointing at the
code, this chapter builds the design one constraint at a time —
every delta from tantivy falls out of "updates must be cheap NOW" —
then hands you the anchors.

## The problem in one sentence

tantivy absorbs one new document by buffering it and eventually
flushing a whole immutable segment; a Redis module must make a
freshly indexed document searchable *within the same command*, with
no background merge infrastructure and readers potentially holding
cursors into the very lists being appended — the entire crate is
the fallout of that requirement.

## The concepts, step by step

### Step 1 — the constraint: mutable NOW, or nothing

A Redis module runs inside Redis's (mostly) single-threaded command
loop: no fleet of merge threads, no "visible after the next flush"
— a write command returns and the data is queryable. That kills the
Lucene/tantivy design (immutable segments + background merge,
topic 4's LSM) at the root. The alternative: **one mutable posting
list per term**, appended in place, with deletion handled by
periodic in-place garbage collection. Every structure below is this
choice worked out; the cost — weaker compression, no block-max
metadata, cursor-invalidation protocols — is the running theme.

### Step 2 — the structure: chained growable blocks per term

Each term's index (`core.rs:30`, `InvertedIndex<E>`) is a
`ThinVec<IndexBlock>` plus counters (`n_unique_docs`), flags, a
`gc_marker: AtomicU32`, and a `unique_id`. An `IndexBlock`
(`core.rs:75`) is `{ first_doc_id, last_doc_id, num_entries: u16,
buffer: Vec<u8> }` — a growable byte buffer of varint-encoded
entries, chained one after another. Contrast tantivy: blocks here
are **variable-length and append-tail-mutable**, not fixed 128-wide
bitpacked — because an append must be O(1) bytes written, not a
block re-pack. The block chain still gives coarse skipping
(`first_doc_id`/`last_doc_id` per block), which is what a mutable
index can afford instead of skip files.

### Step 3 — the write path: varint deltas, new block on overflow

Appending a posting means varint-encoding (a byte-at-a-time
variable-length integer encoding — small deltas take 1 byte) the
delta from the block's last doc id into the last block's buffer.
One edge case drives the block-chaining: a delta too large for the
codec's representable range starts a fresh block at delta 0
(`core.rs:229`, the `IdDelta::from_u64` → None path,
codec/mod.rs:28-44):

```rust
// append one posting: varint-encode the delta into the last block;
// a delta the codec can't represent starts a NEW block at delta 0
fn add<E: Encoder>(&mut self, doc_id: u64, rec: &Record) {
    let block = self.blocks.last_mut().unwrap();
    match E::delta(doc_id, block) {          // None ⇒ overflow for this codec
        Some(delta) => {
            E::write(&mut block.buffer, rec, delta);  // byte-at-a-time varint
            block.last_doc_id = doc_id;
            block.num_entries += 1;
        }
        None => {
            self.blocks.push(IndexBlock::new(doc_id)); // chain a fresh block
            self.add::<E>(doc_id, rec);                //   — simple, robust
        }
    }
    self.n_unique_docs += 1;
}
```

Simple and robust — and the cost is exactly topic 17's lesson: the
branchy per-byte varint decode loop caps read-side GB/s, versus
tantivy's branchless 128-at-a-time SIMD unpack. Cheap writes were
bought with slower scans.

### Step 4 — the codec ladder: one trait, eleven encoders, chosen at compile time

What a posting *carries* (Zobel-Moffat's granularity ladder: ids →
frequencies → fields → positions) is a codec choice: `trait
Encoder` (`codec/mod.rs:53` — `write(record, delta)`,
`delta_base(block)`) has eleven implementations in `codec/` —
`doc_ids_only` / `raw_doc_ids_only` / `freqs_only` / `freqs_fields`
/ `fields_offsets` / `full` / `numeric` … — the granularity ladder
as a directory listing, over one shared varint wire format
(`varint/src/lib.rs:98`, `VarintEncode`).

The encoder is a *type parameter* (`InvertedIndex<E>`,
`PhantomData<E>`), so codec choice is compile-time. This is the
Rust rewrite earning its keep: the C original dispatched on
`IndexFlags` at runtime *per record*; the Rust one monomorphizes
eleven codecs and lets FFI pick the concrete type once
(`c_entrypoint/inverted_index_ffi`) — the per-posting branch simply
no longer exists.

### Step 5 — deletes and readers: GC, gc_marker, unique_id

A mutable index can't do tantivy's "alive-bitmap now, purge at
merge" — there is no merge. Instead a **GC pass** (`gc.rs`)
rewrites blocks in place to purge deleted docs — compaction for a
mutable index — which invalidates any cursor mid-list. Two
validation devices protect readers:

- `gc_marker` (an atomic counter bumped by GC) — a cursor compares
  its saved marker and knows its position is stale;
- `unique_id` — ABA detection (the "freed, then something new
  allocated at the same address" hazard): if the whole index was
  dropped and reallocated at the same pointer, cursors notice via
  id mismatch — a very Redis-module concern.

This is the mutable-world tax: tantivy readers get snapshot
isolation free (a segment never changes under you); RediSearch buys
an approximation of it with two integers and a protocol (question 2
maps this onto FalkorDB's delta-matrix `wait`/version story).

### Step 6 — the deltas vs tantivy, and what M23 should copy

The whole comparison, one line per axis:

```
                     tantivy/Lucene              RediSearch
  mutability     immutable segments + merge   ONE mutable chained-block list per term
  encoding       128-block bitpack (SIMD)     varint per entry (byte-at-a-time)
  deletes        alive-bitmap, purge on merge GC pass rewrites blocks in place
  concurrency    segment = snapshot           gc_marker + unique_id cursor validation
  granularity    postings files per field     codec picked per index flags (11 variants)
  why            batch search workloads       a Redis module: single-threaded-ish,
                                              updates must be cheap NOW, no background
                                              merge infrastructure
```

For M23's own index:

- Copy: codec ladder (doc-ids-only for filters, freqs for ranked),
  new-block-on-delta-overflow (simple, robust), GC marker protocol
  for readers over a mutable index (FalkorDB's matrices already
  have the delta/wait analogue).
- Avoid: per-entry varint for the ranked lane — topic 17 says the
  branchy byte-decode loop caps GB/s; 128-block bitpacking + block
  maxima buy WAND. RediSearch itself has no block-max WAND; scoring
  unions walk everything (why `FT.SEARCH` with scores is expensive
  on big result sets).

## Where each step lives in the code

All under `src/redisearch_rs/` — `inverted_index/src/index/core.rs`
unless noted:

| anchor | what (step) |
|---|---|
| `core.rs:30` `InvertedIndex<E>` | `blocks: ThinVec<IndexBlock>`, `n_unique_docs`, `flags: IndexFlags`, `gc_marker: AtomicU32`, `unique_id` — encoder is a type parameter (`PhantomData<E>`), so codec choice is compile-time (2, 4) |
| `core.rs:75` `IndexBlock` | `{ first_doc_id, last_doc_id, num_entries: u16, buffer: Vec<u8> }` — a growable byte buffer of varint-encoded entries, chained, NOT fixed 128-wide bitpacked (2) |
| `core.rs:229` | a delta too large for the codec ⇒ start a new block with delta 0 (`IdDelta::from_u64` → None path, codec/mod.rs:28-44) (3) |
| `codec/mod.rs:53` `trait Encoder` | `write(record, delta)`, `delta_base(block)` — one trait, eleven codecs (4) |
| `codec/` | `doc_ids_only` / `raw_doc_ids_only` / `freqs_only` / `freqs_fields` / `fields_offsets` / `full` / `numeric` … — the granularity ladder from Zobel-Moffat §3 as a directory listing (4) |
| `varint/src/lib.rs:98` `VarintEncode` | the wire format under most codecs (3, 4) |
| `gc.rs` | garbage collection rewrites blocks to purge deleted docs — compaction for a mutable index; `gc_marker` tells live readers their cursor is stale (5) |
| `unique_id` (core.rs comment) | ABA detection: index freed + reallocated at same address ⇒ cursors notice via id mismatch — a very Redis-module concern (5) |

Read order: `core.rs` top-to-bottom (it's the smallest core file in
this topic), then `codec/mod.rs` + one concrete codec
(`doc_ids_only`), then `gc.rs`, then peek at the FFI seam in
`c_entrypoint/inverted_index_ffi` to see how C picks the
monomorphized type.

## Questions (answer in notes.md)

1. `num_entries: u16` and buffer-growth: what's the effective block
   size policy, and why does variable block length make block-max
   metadata harder to bolt on than tantivy's fixed 128?
2. The `gc_marker`/`unique_id` cursor-validation dance: map it onto
   FalkorDB's delta-matrix `wait` + version story. What does each
   protect against, and which is stricter?
3. Eleven codecs vs tantivy's one postings format + fast fields:
   which RediSearch codecs correspond to "positions" and "doc
   values" in the Lucene taxonomy?
4. Varint vs bitpacked at df=99888/100K docs (delta≈1, one byte
   each): compute bytes/posting for both. Where does varint actually
   WIN?
5. Sketch M23's native replacement: which parts of this crate would
   you lift verbatim into falkordb-rs-next-gen, and where does the
   graph (node ids = doc ids, roaring hit-sets into masked mxv)
   change the design?

## References

**Code**
- [RediSearch](https://github.com/RediSearch/RediSearch)
  `src/redisearch_rs/` — `inverted_index/src/index/core.rs` (the
  structure), `inverted_index/src/codec/` (eleven codecs, one
  trait), `varint/src/lib.rs`, `inverted_index/src/gc.rs`, and the
  FFI seam in `c_entrypoint/inverted_index_ffi`
