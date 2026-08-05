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

Source pin: every anchor is RediSearch at `87276ca`, re-verified with
`tools/pinned-source.py`; all paths are under `src/redisearch_rs/`.

## The problem in one sentence

tantivy absorbs one new document by buffering it and eventually
flushing a whole immutable segment; a Redis module must make a
freshly indexed document searchable *within the same command*, with
no background merge infrastructure and readers potentially holding
cursors into the very lists being appended — the entire crate is
the fallout of that requirement.

## The concepts, step by step

### Step 1 — the constraint: mutable NOW, or nothing

> **In:** a write command inside Redis's single-threaded command loop.
> **Out:** the document is queryable when the command returns — which rules out immutable-segments + background merge, and forces one mutable posting list per term.

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

> **In:** the "append must be cheap" constraint from Step 1.
> **Out:** each term is a `ThinVec` of variable-length `IndexBlock`s, each an append-tail-mutable byte buffer — coarse per-block skipping, no fixed-width repack.

Each term's index (`inverted_index/src/index/core.rs:30`,
`InvertedIndex<E>`) is a `ThinVec<IndexBlock>` plus counters
(`n_unique_docs`), flags, a `gc_marker: AtomicU32`, and a
`unique_id`. An `IndexBlock` (`core.rs:75`) is `{ first_doc_id,
last_doc_id, num_entries: u16, buffer: Vec<u8> }` — a growable byte
buffer of varint-encoded entries, chained one after another.
Contrast tantivy: blocks here are **variable-length and
append-tail-mutable**, not fixed 128-wide bitpacked — because an
append must be O(1) bytes written, not a block re-pack. The block
chain still gives coarse skipping (`first_doc_id`/`last_doc_id` per
block), which is what a mutable index can afford instead of skip
files.

### Step 3 — the write path: varint deltas, new block on overflow

> **In:** a new `(doc_id, record)` for a term whose last block ends at some doc.
> **Out:** the delta `doc_id − delta_base` varint-appended to that block — unless the codec can't represent the delta, which chains a fresh block starting at delta 0.

Appending a posting means **varint**-encoding (a byte-at-a-time
variable-length integer encoding — small deltas take 1 byte) the
delta from the block's `delta_base` (usually its last doc id) into
the last block's buffer. One edge case drives the block-chaining: a
delta too large for the codec's representable range starts a fresh
block at delta 0. The real path, quoted:

```rust
// RediSearch src/redisearch_rs/inverted_index/src/index/core.rs:195-243 (elided)
   195  pub fn add_record(&mut self, record: &RSIndexResult) -> std::io::Result<AddRecordOutcome> {
   196      let doc_id = record.doc_id;
   216      let mut block = self.take_block(doc_id, same_doc);   // last block, or a fresh one
   219      let delta_base = E::delta_base(&block);              // defaults to block.last_doc_id
   224      let delta = doc_id.wrapping_sub(delta_base);
   226      let delta = match E::Delta::from_u64(delta) {        // None ⇒ too big for this codec
   227          Some(delta) => delta,
   228          None => {                                        // start a NEW block at delta 0
   231              let new_block = IndexBlock::new(doc_id);
   234              mem_growth += self.add_block(block);
   235              block = new_block;
   237              E::Delta::zero()
   238          }
   239      };
   242      let writer = block.writer();
   243      let _bytes_written = E::encode(writer, delta, record)?;   // codec writes the varint
```

The `from_u64 → None` overflow arm lives in the codec's `IdDelta`
trait (`codec/mod.rs:36`, trait at :29-40; the comment at :33-35
notes None means "new block per doc"). Simple and robust — and the
cost is exactly topic 17's lesson worked on our densest list
(`t0`, delta ≈ 1):

```
varint:   delta 1 → 1 byte  = 8 bits / posting
bitpack:  block max delta 1 → 1 bit / posting (tantivy)
          → varint is ~8× larger on a dense list, and its per-byte
            decode loop is branchy where bitpacking is branch-free SIMD
```

Cheap writes were bought with slower, bulkier scans (question 4).

### Step 4 — the codec ladder: one trait, ten codecs, chosen at compile time

> **In:** the question "what does a posting carry — just ids? +freqs? +fields? +positions?".
> **Out:** that granularity is a codec module implementing one `Encoder` trait, selected as a *type parameter* so the per-record branch is monomorphized away.

What a posting *carries* (Zobel & Moffat's granularity ladder: ids →
frequencies → fields → positions) is a codec choice: `trait Encoder`
(`codec/mod.rs:53`) declares `encode(writer, delta, record)` (:74)
and `delta_base(block)` (:81, defaulting to the block's last doc id
:82), with `RECOMMENDED_BLOCK_ENTRIES: u16 = 100` (:70). There are
**ten codec modules** in `codec/` (`codec/mod.rs:10-19`):
`doc_ids_only`, `raw_doc_ids_only`, `freqs_only`, `freqs_fields`,
`freqs_offsets`, `fields_only`, `fields_offsets`, `offsets_only`,
`full`, `numeric` — the granularity ladder as a directory listing,
over one shared varint wire format (`varint/src/lib.rs:98`,
`VarintEncode`; `write_as_varint` :101). Each codec may override the
block-size policy: `doc_ids_only` sets `RECOMMENDED_BLOCK_ENTRIES =
1000` (`doc_ids_only.rs:26`) because id-only postings are tiny
(question 1).

The encoder is a *type parameter* (`InvertedIndex<E>`,
`PhantomData<E>`), so codec choice is compile-time. This is the
Rust rewrite earning its keep: the C original dispatched on
`IndexFlags` at runtime *per record*; the Rust one monomorphizes the
codecs and lets FFI pick the concrete type once —
`NewInvertedIndex_Ex` (`c_entrypoint/inverted_index_ffi/src/lib.rs:105`)
matches the flags to a concrete arm like `InvertedIndex::Full(...)`
(:116) — so the per-posting branch simply no longer exists.

### Step 5 — deletes and readers: GC, gc_marker, unique_id

> **In:** deleted docs accumulating in a mutable, in-place index that no merge ever rewrites.
> **Out:** a GC pass that compacts blocks in place, plus two integers (`gc_marker`, `unique_id`) that let concurrent readers detect that their cursor went stale.

A mutable index can't do tantivy's "alive-bitmap now, purge at
merge" — there is no merge. Instead a **GC pass** (`gc.rs`) rewrites
blocks in place to purge deleted docs — compaction for a mutable
index (`repair` :139, `scan_gc` :214, `apply_gc` :242) — which
invalidates any cursor mid-list. Two validation devices protect
readers:

- `gc_marker` (an atomic counter bumped by GC — `apply_gc` calls
  `gc_marker_inc`, gc.rs:340) — a cursor compares its saved marker
  and knows its position is stale;
- `unique_id` — ABA detection (the "freed, then something new
  allocated at the same address" hazard): if the whole index was
  dropped and reallocated at the same pointer, cursors notice via
  id mismatch — a very Redis-module concern.

This is the mutable-world tax: tantivy readers get snapshot
isolation free (a segment never changes under you); RediSearch buys
an approximation of it with two integers and a protocol (question 2
maps this onto FalkorDB's delta-matrix `wait`/version story).

### Step 6 — the deltas vs tantivy, and what M23 should copy

> **In:** the two indexes side by side — immutable-batch vs mutable-now.
> **Out:** a per-axis diff, and a copy/avoid list for M23's own index.

The whole comparison, one line per axis:

```
                     tantivy/Lucene              RediSearch
  mutability     immutable segments + merge   ONE mutable chained-block list per term
  encoding       128-block bitpack (SIMD)     varint per entry (byte-at-a-time)
  deletes        alive-bitmap, purge on merge GC pass rewrites blocks in place
  concurrency    segment = snapshot           gc_marker + unique_id cursor validation
  granularity    postings files per field     codec picked at compile time (10 modules)
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
| `core.rs:195` `add_record`; delta logic :219-243 | `delta_base` (:219) → `wrapping_sub` (:224) → `from_u64` None ⇒ new block at delta 0 (:226-238) → `E::encode` (:243) (3) |
| `codec/mod.rs:53` `trait Encoder` | `encode(writer, delta, record)` (:74), `delta_base(block)` (:81), `RECOMMENDED_BLOCK_ENTRIES = 100` (:70) (4) |
| `codec/mod.rs:10-19` | ten codec modules: `doc_ids_only` / `raw_doc_ids_only` / `freqs_only` / `freqs_fields` / `freqs_offsets` / `fields_only` / `fields_offsets` / `offsets_only` / `full` / `numeric` — Zobel & Moffat's granularity ladder as a directory listing (4) |
| `codec/doc_ids_only.rs:26` | `RECOMMENDED_BLOCK_ENTRIES = 1000` override — id-only postings pack more per block (1, 4) |
| `varint/src/lib.rs:98` `VarintEncode` | the wire format under most codecs; `write_as_varint` :101 (3, 4) |
| `inverted_index/src/gc.rs` | `repair` :139, `scan_gc` :214, `apply_gc` :242 rewrite blocks to purge deleted docs; `gc_marker_inc` :340 tells live readers their cursor is stale (5) |
| `c_entrypoint/inverted_index_ffi/src/lib.rs:105` `NewInvertedIndex_Ex` | C picks the monomorphized type once, e.g. `InvertedIndex::Full(...)` :116 (4, 5) |

Read order: `core.rs` top-to-bottom (it's the smallest core file in
this topic), then `codec/mod.rs` + one concrete codec
(`doc_ids_only`), then `gc.rs`, then peek at the FFI seam in
`c_entrypoint/inverted_index_ffi` to see how C picks the
monomorphized type.

## Questions (answer in notes.md)

1. `num_entries: u16` and buffer-growth: what's the effective block
   size policy (default `RECOMMENDED_BLOCK_ENTRIES = 100`, but
   `doc_ids_only` overrides to 1000), and why does variable block
   length make block-max metadata harder to bolt on than tantivy's
   fixed 128?
2. The `gc_marker`/`unique_id` cursor-validation dance: map it onto
   FalkorDB's delta-matrix `wait` + version story. What does each
   protect against, and which is stricter?
3. Ten codecs vs tantivy's one postings format + fast fields:
   which RediSearch codecs correspond to "positions" and "doc
   values" in the Lucene taxonomy?
4. Varint vs bitpacked at df=99888/100K docs (delta≈1, one byte
   each): compute bytes/posting for both. Where does varint actually
   WIN?
5. Sketch M23's native replacement: which parts of this crate would
   you lift verbatim into falkordb-rs-next-gen, and where does the
   graph (node ids = doc ids, roaring hit-sets into masked mxv)
   change the design?

## Done when

Answer each before unfolding it.

- [ ] You can state the constraint that shapes the whole design: mutable now, or nothing.
  <details><summary>the single-threaded command loop</summary>
  A Redis write command must leave the document queryable when it
  returns, inside a (mostly) single-threaded loop with no merge
  threads. That rules out immutable-segments + background merge and
  forces one in-place-mutable posting list per term.
  </details>
- [ ] You can describe the chained growable block structure per term and the write path through it.
  <details><summary>ThinVec of byte-buffer blocks</summary>
  `InvertedIndex<E>` (core.rs:30) holds a `ThinVec<IndexBlock>`; each
  `IndexBlock` (core.rs:75) is a growable `Vec<u8>` of varint entries.
  `add_record` (core.rs:195) varint-appends `doc_id − delta_base`
  (:219-224) to the last block, chaining a fresh block at delta 0
  when `from_u64` returns None (:226-238).
  </details>
- [ ] You can explain the codec ladder — one trait, many encoders, chosen at compile time — and why that is a codegen decision rather than a runtime one.
  <details><summary>Encoder as a type parameter</summary>
  Ten codec modules (codec/mod.rs:10-19) each `impl Encoder`
  (:53, `encode` :74). `InvertedIndex<E>` carries the codec as a type
  parameter (`PhantomData<E>`), so the compiler monomorphizes it and
  the per-record `IndexFlags` branch the C code paid disappears; FFI
  picks the concrete type once (inverted_index_ffi/src/lib.rs:105).
  </details>
- [ ] You can explain how GC, `gc_marker` and `unique_id` let readers survive concurrent deletes.
  <details><summary>compact-in-place + two guards</summary>
  GC (gc.rs: `scan_gc` :214, `apply_gc` :242) rewrites blocks to drop
  deleted docs, invalidating cursors. `gc_marker` (bumped at :340) is
  compared by a cursor to detect a stale position; `unique_id`
  catches ABA (index dropped and reallocated at the same address).
  </details>
- [ ] You wrote answers to all questions in notes.md.
  <details><summary>check</summary>
  Five answers in notes.md, including the varint-vs-bitpack
  bytes/posting computation (Q4) and the FalkorDB cursor-validation
  mapping (Q2).
  </details>

## References

**Code**
- [RediSearch](https://github.com/RediSearch/RediSearch) `@87276ca`
  `src/redisearch_rs/` — `inverted_index/src/index/core.rs`
  (`InvertedIndex` :30, `IndexBlock` :75, `add_record` :195-243),
  `inverted_index/src/codec/mod.rs` (`trait Encoder` :53, ten modules
  :10-19), `inverted_index/src/codec/doc_ids_only.rs:26`,
  `varint/src/lib.rs:98`, `inverted_index/src/gc.rs:139,214,242`, and
  the FFI seam `c_entrypoint/inverted_index_ffi/src/lib.rs:105`
