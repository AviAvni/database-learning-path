# Reading guide — RediSearch's inverted index, the Rust rewrite (`~/repos/RediSearch/src/redisearch_rs/`)

Home turf: this is what FalkorDB delegates full-text to, and the
interesting part is that the C core is being strangler-figged into
Rust crates behind FFI (`c_entrypoint/inverted_index_ffi`,
`varint_ffi`) — the exact migration pattern falkordb-rs-next-gen
lives. Read `inverted_index` as a *mutable, in-memory* counterpart
to tantivy's immutable segments.

## The structure (`inverted_index/src/index/core.rs`)

| anchor | what |
|---|---|
| `core.rs:30` `InvertedIndex<E>` | `blocks: ThinVec<IndexBlock>`, `n_unique_docs`, `flags: IndexFlags`, `gc_marker: AtomicU32`, `unique_id` — encoder is a type parameter (`PhantomData<E>`), so codec choice is compile-time |
| `core.rs:75` `IndexBlock` | `{ first_doc_id, last_doc_id, num_entries: u16, buffer: Vec<u8> }` — a growable byte buffer of varint-encoded entries, chained, NOT fixed 128-wide bitpacked |
| `core.rs:229` | a delta too large for the codec ⇒ start a new block with delta 0 (`IdDelta::from_u64` → None path, codec/mod.rs:28-44) |
| `codec/mod.rs:53` `trait Encoder` | `write(record, delta)`, `delta_base(block)` — one trait, eleven codecs |
| `codec/` | `doc_ids_only` / `raw_doc_ids_only` / `freqs_only` / `freqs_fields` / `fields_offsets` / `full` / `numeric` … — the granularity ladder from Zobel-Moffat §3 as a directory listing |
| `varint/src/lib.rs:98` `VarintEncode` | the wire format under most codecs |
| `gc.rs` | garbage collection rewrites blocks to purge deleted docs — compaction for a mutable index; `gc_marker` tells live readers their cursor is stale |
| `unique_id` (core.rs comment) | ABA detection: index freed + reallocated at same address ⇒ cursors notice via id mismatch — a very Redis-module concern |

## Design deltas vs tantivy (worth internalizing)

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

The `Encoder`-as-type-parameter design is the Rust rewrite earning
its keep: the C original dispatched on `IndexFlags` at runtime per
record; the Rust one monomorphizes eleven codecs and lets FFI pick
the concrete type once (`c_entrypoint/inverted_index_ffi`).

## What M23 should copy vs avoid

- Copy: codec ladder (doc-ids-only for filters, freqs for ranked),
  new-block-on-delta-overflow (simple, robust), GC marker protocol
  for readers over a mutable index (FalkorDB's matrices already
  have the delta/wait analogue).
- Avoid: per-entry varint for the ranked lane — topic 17 says the
  branchy byte-decode loop caps GB/s; 128-block bitpacking + block
  maxima buy WAND. RediSearch itself has no block-max WAND; scoring
  unions walk everything (why `FT.SEARCH` with scores is expensive
  on big result sets).

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
