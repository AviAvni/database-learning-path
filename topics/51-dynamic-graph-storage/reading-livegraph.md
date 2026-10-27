# LiveGraph: an append-only log per vertex, and two timestamps on every edge

Sortledton, Teseo and PCSR all keep neighbours **sorted** and pay for it on
insert. LiveGraph makes the opposite bet: do not sort at all. Each vertex's
neighbours are an append-only **log**, an insert is a write at the tail, and a
scan is a pure sequential read of the log from one end to the other. The price
is on the other side of the ledger — every entry carries two timestamps, so
every scan checks every entry, and "does this edge exist?" is a scan unless a
Bloom filter says no. This guide reads that trade-off in the paper and in the
code, because it is the design a delta matrix is *not*: a log per vertex,
forever, instead of one small log in front of an immutable array.

**"LiveGraph: A Transactional Graph Storage System with Purely Sequential
Adjacency List Scans"** — Xiaowei Zhu, Guanyu Feng, Marco Serafini, Xiaosong
Ma, Jiping Yu, Lei Xie, Ashraf Aboulnaga, Wenguang Chen. PVLDB 13(7):
1020–1034, 2020 (the Sortledton paper's reference [57]);
[arXiv:1910.05773v2](https://arxiv.org/pdf/1910.05773), 29 Aug 2020, is the PDF
read here. Section and figure numbers are that PDF's. The code is
`github.com/thu-pacman/LiveGraph` at
`eea5a40ce6ee443f901e44323c1119f59113eaf3`, cloned to `~/repos/LiveGraph`
(not in the pin table; the commit is stated here). One caution: the paper
says its adjacency-list entries are 28 bytes (§6) and the repository's
`EdgeEntry` fields add to 24 (Step 2). They may be different revisions; this
guide quotes each where it is used and does not reconcile them.

```
 one TEL block (size = 2^order bytes)

 +--------+------------------------+  free  +---------------------+-------------+
 | header | property bytes  ->     | space  |  <-  edge log       |  Bloom      |
 |        | (grow forward)         |        | (grow backward)     |  1/16       |
 +--------+------------------------+--------+---------------------+-------------+
                                                 newest entry first    only if
                                                                       order >= 10
 each EdgeEntry: length | dst (48 bit) | creation_time | deletion_time   (24 B)
 visible to a reader at T  iff  creation_time <= T < deletion_time
```

## The problem in one sentence

A transactional graph store has to hold several versions of an adjacency list
so readers see a snapshot, and the usual ways of doing that — per-version
copies, a pointer chain per edge, a B+ tree — turn a neighbour scan into
random memory access (§2.1).

## The concepts, step by step

### Step 1 — the Transactional Edge Log (TEL) and double timestamps (§3, §4)

> **In:** the problem sentence. **Out:** the block layout, why a scan over it
> is sequential, and the one operation that is not O(1).

A **Transactional Edge Log (TEL)** is one fixed-size block of memory holding a
vertex's whole adjacency list as a **log**: a header, then entries appended at
the tail. An insert of a new edge appends an entry. An update or delete of an
existing edge appends a new entry *and* sets an **invalidation timestamp** on
the previous entry for that edge. Each entry therefore carries two timestamps:
a **creation timestamp** (when it became visible) and an **invalidation
timestamp** (when it stopped being visible), the **double timestamps**.

A reader with **read timestamp** T (its snapshot's time) considers an entry
visible exactly when `creation ≤ T < invalidation`. So a scan walks the log
once, skips the invisible entries, and never follows a pointer: "a transaction
can check the validity of an edge entry simply by checking the embedded
timestamp information" (§4). That is what the title means by *purely
sequential*.

Not everything is O(1): inserting a brand-new edge is an append, but an update
or delete must find the previous entry to invalidate it — "which entails
scanning the log" (§4). LiveGraph avoids the scan for true inserts with a
**Bloom filter** in the TEL header (a compact bit array that can say "this
edge is definitely not here" but never "definitely here"). A "no" means the
operation is a plain append; a "possibly yes" means a tail-to-head scan,
which is cheap "in practice … edges appended most recently are most likely to
be accessed" (§4).

TELs are fitted to power-of-two sizes by a buddy-system allocator, starting
at 64 bytes — "a 36-byte header plus a 28-byte log entry" (§6) — and a TEL
that fills is **upgraded**: a block of twice the size is allocated and the
log is copied (§1, §4). Compaction runs per vertex, not as a global merge: every
65,536 transactions (the paper's default) each worker thread walks its set of
"dirty" vertices and rewrites each block, copying only entries still visible
to any running or future transaction (§6) — "unlike LSMT, LiveGraph never
needs to compact multiple on-disk files through merging".

Why it matters: this is *the* log-structured alternative to a delta design.
Both defer the structural work; LiveGraph defers it per vertex and pays a
visibility check per entry for the life of the data, a delta defers it for the
whole graph and pays one extra probe per row until the flush.

### Step 2 — the code: what an entry is, and how the log grows (blocks.hpp)

> **In:** Step 1's description. **Out:** the entry's bytes, the direction the
> log grows, and the Bloom filter's sizing rule — each with its line.

```cpp
// core/blocks.hpp — class EdgeEntry, 191-225 (elided 194-217)
   191      class EdgeEntry
   192      {
   193      public:
   ...
   219      private:
   220          uint16_t length;
   221          uint16_t dst_high;
   222          uint32_t dst_low;
   223          timestamp_t creation_time;
   224          timestamp_t deletion_time;
   225      };
```

An entry is `length` (2 bytes, the size of the edge's property data), a
48-bit destination id split into `dst_high` (16) and `dst_low` (32) bits, and
two `timestamp_t`s — `int64_t` (`core/types.hpp:26`) — so **2 + 2 + 4 + 8 + 8 =
24 bytes**. Against a 4-byte CSR edge that is 6× the bytes a scan reads per
neighbour (and 3× an 8-byte id), before the Bloom filter. Note the field name:
`deletion_time` here, "invalidation timestamp" in the paper.

Appending:

```cpp
// core/blocks.hpp — EdgeBlockHeader::has_space and append, 291-322 (elided 292, 306)
   291          bool has_space(EdgeEntry entry, size_t num_entries, size_t data_length) const
   293              size_t block_size = get_block_size();
   294              size_t bloom_filter_size;
   295              if (get_order() < BLOOM_FILTER_THRESHOLD)
   296                  bloom_filter_size = 0;
   297              else
   298                  bloom_filter_size = block_size >> BLOOM_FILTER_PORTION;
   299              if (sizeof(*this) + (num_entries + 1) * sizeof(entry) + data_length + entry.get_length() +
   300                      bloom_filter_size >
   301                  get_block_size())
   302                  return false;
   ...
   307          EdgeEntry *append(EdgeEntry entry, const char *data, BloomFilter &filter)
   308          {
   ...
   313              *(get_entries() - num - 1) = entry;
   314              for (size_t i = 0; i < entry.get_length(); i++)
   315                  (get_data() + length)[i] = data[i];
   ...
   319              if (filter.valid())
   320                  filter.insert(entry.get_dst());
   321              return get_entries() - num - 1;
```

Read three lines. **313** is the whole insert for a new edge: a store at
`get_entries() - num - 1` — the entry log grows **backwards from the end of
the block** while the property bytes (314–315) grow **forwards from the
front**, "from the opposite ends of TEL blocks" (§4), so all fixed-size
entries and their cache-aligned timestamps sit together. **295–298** is the
Bloom filter's sizing rule: a block of order ≥ `BLOOM_FILTER_THRESHOLD` (10,
`blocks.hpp:372`) reserves `block_size >> 4` bytes — one sixteenth — for the
filter (`BLOOM_FILTER_PORTION` = 4, line 373). Block size is `1 << order`
(line 43), so the threshold is a **1,024-byte block**; the paper says "for
each block larger than 256 bytes" (§4) — a discrepancy of two orders, and the
code is what runs. **320** inserts the edge's destination into the filter on
every append.

Worked on a 4,096-byte block (order 12): the filter takes 4,096 ÷ 16 = 256
bytes, leaving 3,840 for the header, entries and properties; with no
properties that is at most (3,840 − header) ÷ 24 ≤ 160 entries, so the filter
holds ≥ 256 × 8 ÷ 160 = **12.8 bits per edge**. (The header's size is not
subtracted — the true count is a little lower and the bits per edge a little
higher.)

And the scan:

```cpp
// core/edge_iterator.hpp — EdgeIterator constructor, forward direction, 53-65
    53              if (!reverse)
    54              {
    55                  while (valid())
    56                  {
    57                      if (cmp_timestamp(entries_cursor->get_creation_time_pointer(), read_epoch_id, local_txn_id) <= 0 &&
    58                          cmp_timestamp(entries_cursor->get_deletion_time_pointer(), read_epoch_id, local_txn_id) > 0)
    59                      {
    60                          break;
    61                      }
    62                      data_cursor -= entries_cursor->get_length();
    63                      entries_cursor++;
    64                  }
```

Lines 57–58 are the visibility test, `creation ≤ read epoch < deletion`, and
they run for **every** entry the scan touches; 62–63 skip an invisible entry
(and its property bytes). There is no sorted order and no block index — the
iterator's only job is to walk and test.

Why it matters: this loop is the thing a delta design removes from the
settled data. Two `cmp_timestamp` calls and a branch per edge are what the
Revisiting paper's "α" multiplies (see [reading-revisiting-dgs.md](reading-revisiting-dgs.md)
Step 1), and the 24-byte entry is its "3 × w".

### Step 3 — what the paper measured, and what an independent bench found (§2.1; Revisiting §6.1.2, §6.4)

> **In:** LiveGraph's own micro-benchmark and the Revisiting paper's
> re-measurement. **Out:** two checkable statements and a contradiction
> resolved by reading what each one measured.

The LiveGraph micro-benchmark (§2.1) performs 10⁸ adjacency-list scans from
power-law-chosen start vertices on Kronecker graphs of 2²⁰–2²⁶ vertices with
mean degree 4, single-threaded, and compares data structures. Its statements:
"TEL has a scan speedup larger than 29× over linked list, 20× over LSMT, and
6× over B+ tree" on *edge scan* latency (Fig. 1b); "CSR's latency for scans
is 43% that of TEL", the gap being "mainly from TEL's larger memory footprint
and the overheads of checking edge visibility"; and TEL "triggers 2.42× more
LLC-misses" than CSR because "a single edge in TEL takes 2× memory than CSR in
our micro-benchmark". Worked: 1 ÷ 0.43 = 2.33 — TEL's edge scan is about 2.3×
CSR's latency by LiveGraph's own account.

The independent bench ([reading-revisiting-dgs.md](reading-revisiting-dgs.md))
re-runs LiveGraph's *structure* inside a common framework and finds:

- scans: AdjLst and LiveGraph "perform the best due to their continuous
  storage" (§6.1.2), but on the kernels LiveGraph is 1.0–1.9× *slower* than a
  plain adjacency list "because LiveGraph scans from the end of the array,
  preventing automatic vectorization by the compiler" (§6.1.3);
- memory: with versions ("w" columns of its Table 9), LiveGraph needs 4.9× to
  16.7× the CSR's memory on the eight graphs (e.g. 6.24 GB against 0.67 GB on
  lj: 9.3×; 24.62 against 3.95 on g5: 6.2×);
- searches and inserts are slow because the log is unsorted: "LiveGraph
  checks whether the element to be inserted already exists, requiring a
  search operation … although LiveGraph uses a bloom filter to reduce
  searches for non-existent edges, it struggles with existing edges, and
  false positives arise due to the filter's limited memory space" (§6.1.2).

Reconciling "TEL is 2.3× CSR on scan" (a 2020 micro-benchmark, mean degree 4,
one thread) with "LiveGraph is up to 16× the memory and slower than an
adjacency list on kernels" (a 2025 framework, real graphs, up to 32 threads):
they measure different things — a single scan's latency against whole-kernel
time and resident size — and agree on the direction: the version bytes and
the per-entry check are paid on every scan, and the log is a poor structure
for membership queries.

Why it matters: LiveGraph is the lower bound on insert cost (an append) and
an upper bound on scan cost for an *unsorted* design. Where a delta matrix
sits between the two is exactly lane 1: an insert that is a small sorted
insert into a side structure, and a scan that is a pure array read plus one
probe.

## How to read the paper (with the concepts in hand)

1. **§1–2.1 with Figure 1 and Table 1** — the structures compared; do not
   skip Table 1's seek-versus-scan columns.
2. **§3 with Figures 2–3** — the block layout; Figure 3 is the picture of
   the two-ended block.
3. **§4** — single-threaded operations and the Bloom filter paragraph.
4. **§5** — the epoch-based transaction manager; read it for the claim that
   reads take no locks and the commit is one timestamp write.
5. **§6** — compaction and the buddy allocator; the 64-byte minimum TEL.

Read the code in this order: `core/blocks.hpp:191` (the entry), `:291` (append
and the Bloom sizing), `:372` (the constants), then `core/edge_iterator.hpp:53`
(the visibility loop).

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | — | the log-per-vertex design is not a lane; `Vec<Vec<u32>>` unsorted append is its closest model |
| 2 | bench lane 1f | bytes per edge: 4.26 for CSR, 24+ for a TEL entry |
| 3 | [reading-revisiting-dgs.md](reading-revisiting-dgs.md) Steps 3–4 | the same layout, priced |

## Questions (answer in notes.md)

1. The code's Bloom filter exists only for blocks of order ≥ 10 (≥ 1,024 B);
   the paper says > 256 B. At 24 bytes per entry, how many entries does a
   256-byte block hold, and what do the two thresholds imply for the false
   positive rate of a small hub?
2. A scan skips an invisible entry with `entries_cursor++` and a `data_cursor`
   adjustment (lines 62–63). If a vertex's log is 50% invalidated entries,
   what fraction of the bytes a scan reads is useful, and when does the
   compaction of Step 1 bring it back?
3. The log is scanned from one end because recent entries are the likeliest
   to be updated. Why does that direction prevent compiler auto-vectorisation
   in the Revisiting paper's account, and what would a manual SIMD loop have
   to do about the visibility test?
4. Compare LiveGraph's per-vertex compaction with a delta flush: what does
   each one rewrite, and which has a pause proportional to |E|?

## Done when

Answer each before unfolding it.

- [ ] You can explain a TEL and what makes its scan sequential.

  <details><summary>Answer</summary>

  A TEL is one block holding a vertex's whole adjacency list as an
  append-only log of fixed-size entries, each with a creation and an
  invalidation timestamp. A scan walks the log and keeps entries with
  creation ≤ read timestamp < invalidation (edge_iterator.hpp:57–58); no
  pointer is followed and no index is consulted, so the access pattern is
  one forward pass over contiguous memory (§3–§4).

  </details>

- [ ] You can say which edge operation is not constant time and how the Bloom filter helps.

  <details><summary>Answer</summary>

  Updating or deleting an existing edge, because the previous entry's
  invalidation timestamp must be set, which needs a scan of the log. The
  Bloom filter separates true inserts (a "no" answer — a plain O(1) append)
  from possible updates (a "possibly yes" — a tail-to-head scan, cheap when
  the edge was touched recently). It is sized at one sixteenth of the block
  (blocks.hpp:298, 373) for blocks of at least 1,024 bytes (line 372).

  </details>

- [ ] You can state the per-edge bytes in the code and what they cost a scan.

  <details><summary>Answer</summary>

  EdgeEntry is 2 (length) + 2 + 4 (packed 48-bit destination) + 8 + 8 (two
  int64 timestamps) = 24 bytes (blocks.hpp:220–224), 6× a 4-byte CSR edge,
  plus one sixteenth of a large block for the filter. A scan reads all of it
  for every entry, visible or not; the paper attributes CSR's 43% scan
  latency relative to a TEL to the larger footprint and the visibility
  checks (§2.1).

  </details>

- [ ] You can place LiveGraph against a delta design on insert, scan and reclamation.

  <details><summary>Answer</summary>

  Insert: LiveGraph appends (O(1) for new edges, a scan for updates); a delta
  inserts into a small sorted side structure, with a membership check against
  the settled array. Scan: LiveGraph pays the 24-byte entry and the
  timestamp test on every edge for the life of the data; a delta scans the
  settled array unversioned and pays one probe per row of the side
  structure. Reclamation: LiveGraph compacts one vertex's block at a time;
  a delta flush rewrites the whole settled array, a pause proportional to |E|.

  </details>

- [ ] You can name the one place the code and the paper disagree about the Bloom filter and say which runs.

  <details><summary>Answer</summary>

  The paper says the filter is 1/16 of the TEL "for each block larger than
  256 bytes" (§4); the code's `BLOOM_FILTER_THRESHOLD` is order 10 — a
  1,024-byte block — with `BLOOM_FILTER_PORTION` 4 giving the 1/16
  (blocks.hpp:372–373, 295–298). The code runs.

  </details>

## References

- Zhu, Feng, Serafini, Ma, Yu, Xie, Aboulnaga, Chen. *LiveGraph: A
  Transactional Graph Storage System with Purely Sequential Adjacency List
  Scans.* PVLDB 13(7): 1020–1034, 2020; arXiv:1910.05773v2.
- Code: `github.com/thu-pacman/LiveGraph` at
  `eea5a40ce6ee443f901e44323c1119f59113eaf3`.
- Putze, Sanders, Singler. *Cache-, Hash- and Space-Efficient Bloom Filters.*
  (the blocked Bloom filter the TEL uses, per `bloom_filter.hpp:21`; not read
  for this guide.)
- [reading-revisiting-dgs.md](reading-revisiting-dgs.md),
  [reading-sortledton.md](reading-sortledton.md).
