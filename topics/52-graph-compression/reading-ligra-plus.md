# Ligra+: compression that is faster, because the machine was waiting on memory

Everything else in this topic prices compression in bits. Ligra+ is the paper
that prices it in *seconds*, on a shared-memory graph framework with real
algorithms (BFS, PageRank, connected components…), and reports the result that
makes compression interesting for a performance-minded engine: on a big
multicore machine, running on the **compressed** graph was on average as fast as
or faster than running on the uncompressed one — and slower, single-threaded,
on most of them. This guide reads the paper and the byte-code source side by
side, because the format is small enough to read whole.

**"Smaller and Faster: Parallel Processing of Compressed Graphs with Ligra+"**
— Julian Shun, Laxman Dhulipala, Guy E. Blelloch. DCC 2015
([pdf](https://people.csail.mit.edu/jshun/ligra+.pdf)); section, table and
figure numbers are from that PDF. Code anchors are `ligra` at `8763202`
(`github.com/jshun/ligra`, the pin in `resources/codebases.md`), files
`ligra/byte.h` and `ligra/byteRLE.h`.

## The problem in one sentence

Graph algorithms are memory-bound, so a smaller graph should be faster — but
decoding costs instructions and branch mispredictions; when does the saved
memory traffic win?

## The concepts, step by step

### Step 1 — the baseline and the workloads

> **In:** nothing. **Out:** what "uncompressed" and "Ligra" mean in the
> comparison, and what each application does with the edges.

Ligra stores a graph as an **adjacency array**: one array of edge targets
(`uintE`, 32 bits) and one of per-vertex offsets, plus a second pair for the
in-edges if the graph is directed. That is CSR with 32-bit targets: 32 bits
per edge, which the paper uses as the reference ("the uncompressed graph in
Ligra requires 32 bits per edge", §4). Its `edgeMap` has two forms: `edgeMapSparse`
loops over the out-neighbours of every vertex in a frontier, `edgeMapDense`
loops over the in-neighbours of every vertex and stops early once a vertex is
satisfied (§2.1, Figures 1–2); it switches when the frontier plus its out-degree
exceeds `|E|/20`. Compression replaces only the neighbour loop.

The experiments (§4) run six applications — BFS, betweenness centrality,
radii, connected components, PageRank (one iteration) and Bellman–Ford — on a
40-core (80-thread) machine with 4 × 10-core E7-8870 Xeons, a 30 MB L3 cache and 256 GB;
times are the median of three trials. Inputs are synthetic (`randLocal`, a
`3D-grid`) and real (`soc-LJ`, `cit-Patents`, `com-LJ`, `com-Orkut`,
`nlpkkt240`, `Twitter`, `uk-union`, `Yahoo`); Twitter and uk-union are
directed, the rest symmetrised.

### Step 2 — the k-bit code and its difference encoding

> **In:** Step 1's sorted neighbour lists. **Out:** the byte code, worked on
> three numbers and then read in the source.

Neighbours are sorted and stored as **differences**: the first as
`target − source` (it can be negative), each later one as
`target − previous target` (always positive). A **k-bit code** (Blandford et
al.) writes an integer as a sequence of `k`-bit blocks, each with one
**continue bit** (1 means "the next block is also part of this integer"). With
`k = 8` blocks are bytes: seven data bits and a continue bit. The first edge
carries an extra **sign bit** in its first block, leaving six data bits.

Worked, with this repo's lane-1 convention in mind (Ligra stores the plain
difference; this topic's `VarintGraph` stores `difference − 1`, saving a bit of
range):

* A later difference of **300**: binary `1 0010 1100`. Low seven bits
  `010 1100` = 44, more to come, so byte one is `0x80 | 44 = 0xAC`; the rest,
  `300 >> 7 = 2`, is byte two `0x02`. Two bytes: `AC 02`.
* A first edge of source 100, target **5000**: difference +4900. Low six bits
  `4900 mod 64 = 36`, more to come (continue bit `0x80`, sign clear): first
  byte `0xA4`; `4900 >> 6 = 76 < 128`: second byte `0x4C`. Decoding: `0x24 = 36`,
  continue, `76 << 6 = 4864`, `36 + 4864 = 4900`, sign clear, `100 + 4900 =
  5000`.
* A first edge of source 100, target **90**: difference −10: `10 & 0x3f = 10`,
  sign bit `0x40` set, nothing more: single byte `0x4A`.

Now the source. The decoder for any edge after the first (`byte.h`, line
numbers as they occupy the file):

```c
// ligra/byte.h — eatEdge, 84-97
  84  inline uintE eatEdge(uchar* &start) {
  85    uintE edgeRead = 0;
  86    int shiftAmount = 0;
  87  
  88    while (1) {
  89      uchar b = *start++;
  90      edgeRead += ((b & 0x7f) << shiftAmount);
  91      if (LAST_BIT_SET(b))
  92        shiftAmount += EDGE_SIZE_PER_BYTE;
  93      else
  94        break;
  95    }
  96    return edgeRead;
  97  }
```

The line to look at is 91: `LAST_BIT_SET(b)` is `b & 0x80` (`byte.h:36`), the
**continue** bit, despite its name — the name is the paper's "last bit" of the
block. A branch on a data-dependent bit, per byte, per edge: that is the cost
the next step removes. The loop that uses it, `decode` (`byte.h:104-123`):

```c
// ligra/byte.h — decode, 104-123
 104  template <class T>
 105    inline void decode(T t, uchar* edgeStart, const uintE &source, const uintT &degree, const bool par=true) {
 106    size_t edgesRead = 0;
 107    if (degree > 0) {
 108      // Eat first edge, which is compressed specially
 109      uintE startEdge = eatFirstEdge(edgeStart,source);
 110      if (!t.srcTarg(source,startEdge,edgesRead)) {
 111        return;
 112      }
 113      for (edgesRead = 1; edgesRead < degree; edgesRead++) {
 114        // Eat the next 'edge', which is a difference, and reconstruct edge.
 115        uintE edgeRead = eatEdge(edgeStart);
 116        uintE edge = startEdge + edgeRead;
 117        startEdge = edge;
 118        if (!t.srcTarg(source, edge, edgesRead)) {
 119          break;
 120        }
 121      }
 122    }
 123  }
```

`t.srcTarg(...)` is the application's per-edge function, inlined; returning
false stops early (line 118–120), which is how `edgeMapDense` keeps its
"break once satisfied" optimisation on a compressed list. The first edge is
read by `eatFirstEdge` (`byte.h:63-79`), which tests `fb & 0x40` for the sign at
line 78.

### Step 3 — run-length byte codes: trade bits for branch-free decoding

> **In:** Step 2's byte code. **Out:** the `byte-RLE` variant (§3), its format
> and its cost.

The paper's observation: the continue bit is what makes the decoder branchy.
Instead of marking each integer's length, mark the length of a **group** of
consecutive integers that all need the same number of bytes. Each group is one
header byte — "2 bits … for the number of bytes each element requires … and 6
bits for the size of the group (which allows for groups of up to size 64)" —
followed by the elements as raw little-endian bytes with no continue bit. The
decoder reads the header and then runs an inner loop whose width is known
(`byteRLE.h:99-148`):

```c
// ligra/byteRLE.h — run-length decode loop, 99-109
  99      uintT i = 0;
 100      edgesRead = 1;
 101      while(1) {
 102        if(edgesRead == degree) return;
 103        uchar header = edgeStart[i++];
 104        uint numbytes = 1 + (header & 0x3);
 105        uint runlength = 1 + (header >> 2);
 106        switch(numbytes) {
 107        case 1:
 108  	for(uint j = 0; j < runlength; j++) {
 109  	  uintE edge = (uintE) edgeStart[i++] + startEdge;
```

Lines 104–105 are the whole format. The encoder side is
`sequentialCompressEdgeSet` (`byteRLE.h:232-297`): it measures each difference
against 256, 65,536 and 16,777,216 (lines 244, 253, 262), extends the current
group while the width matches, and flushes when the width changes or the group
reaches 64 (line 282). The header is built at `byteRLE.h:195-197`
(`numBytes − 1 | (runlength − 1) << 2`).

The price, worked. Differences `3, 5, 300, 7`: the plain byte code spends
`1 + 1 + 2 + 1 = 5` bytes. The RLE code needs three groups — `[3, 5]` at one
byte (1 header + 2 data = 3 bytes), `[300]` at two bytes (1 + 2 = 3), `[7]`
at one byte (1 + 1 = 2) — **8 bytes**. When the widths alternate, RLE is worse;
when runs are long (a hub's sorted neighbours, differences all under 256),
the header is amortised over up to 64 elements and each element costs one byte
up to 255, where the plain byte code needs two bytes above 127. The paper's Figure 5 puts byte-RLE last in
size among the three schemes: "nibble codes require the least space, followed
by byte codes, and finally byte-RLE codes".

A **nibble** code is the same scheme with `k = 4` (a half byte: three data bits
and a continue bit): smaller on small differences, slower, "due to the extra
bit arithmetic required" (§3).

### Step 4 — parallel decoding of long lists

> **In:** Step 2's sequential decode. **Out:** why a high-degree vertex does
> not serialise the machine (§3, "Parallel Decoding").

A compressed list can only be decoded front to back. In a power-law graph one
vertex can hold more neighbours than a whole core's share, and the compressed
form makes the obvious fix (a parallel loop over its neighbours) impossible.
Ligra+ splits any list with more than `T` edges into chunks of `T`; **the
first edge of every chunk is encoded relative to the source vertex** (so a
chunk decodes without its predecessor), and the offsets of all chunks but the
first are stored. `T = 1,000`; performance "similar across a wide range of T
(from 100 to 10,000)". The extra storage is at most `|E|/T − |V|` offsets.
Vertex degrees are stored uncompressed (Ligra derives degrees from offsets;
compressed offsets no longer give them).

### Step 5 — what it measured: size

> **In:** the format. **Out:** the paper's space results and the caveat on
> reordering.

"Ligra+ is able to represent a variety of synthetic and real-world graphs
using 49–56% of its original size on average, depending on the compression
scheme" (§1): about 53% for byte codes, 56% for byte-RLE and 49% for nibble
(§4). For graphs "with good compression and/or low vertex-to-edge ratio, such
as `nlpkkt240` and `uk-union`, the space savings are up to 3× for byte and
byte-RLE coding and 4× for nibble". The savings are smaller on `3D-grid`
because Ligra+ "does not compress vertices": the offset and degree arrays are
a larger share of a sparse graph. Bits per edge (Figure 5) are at most 19 on
these inputs, against 32 for Ligra.

The sentence this topic is built to test: "We experiment with several graph
reordering schemes … to improve the locality … and hence compression of the
graphs. While for most graphs, applying the best reordering algorithm improves
compression, **the locality of our real-world graphs is already quite good
without reordering**. In our experiments, we use the best ordering for each
graph, but confirmed that reordering is not always necessary to obtain good
compression." (§4, "Compression Quality"). The graphs came with
crawl-ordered ids; the measurement says nothing about graphs whose ids arrive
in an arbitrary order. Lane 1 provides that case.

### Step 6 — what it measured: time

> **In:** Table 2 of the paper. **Out:** the shape of the result, read from
> specific rows.

Table 2 reports sequential (`T1`) and 40-core (`T40`) seconds. Read three
rows (seconds, Ligra / byte / byte-RLE / nibble):

| row | T1 | T40 |
|---|---|---|
| `soc-LJ` BFS | 0.634 / 0.677 / 0.676 / 0.902 | 0.028 / 0.027 / 0.026 / 0.031 |
| `Twitter` PageRank | 48.6 / 73.5 / 70.5 / 95.2 | 2.68 / 2.74 / 2.66 / 3.15 |
| `uk-union` PageRank | 74.4 / 56.9 / 52.3 / 64.8 | 4.89 / 2.26 / 2.24 / 2.65 |

Single-threaded, compression usually costs: on Twitter PageRank the byte code
is `73.5 / 48.6 = 1.51×` slower. On 40 cores it vanishes (2.74 vs 2.68) and on
`uk-union`, whose ids are crawl-ordered and which compresses 3×, the compressed
graph wins even single-threaded (56.9 vs 74.4) and 2.2× at 40 threads
(4.89 / 2.26). The paper's summary: the performance of Ligra+ "ranges from 2.2x
faster to 1.1x slower than the original Ligra system … about 14% faster on
average when using the fastest compression scheme" (§1), and the stated
reason: "graph algorithms are memory-bound, and memory is a larger bottleneck in
parallel due to multiple cores competing for bandwidth … decoding becomes less
of an overhead as it has better parallel scalability relative to the rest of
the computation".

What the table does **not** support: a claim that compression is always
faster. `randLocal` BFS at one thread is 1.46 s for Ligra and 1.93 s for the byte
code (1.32× slower), and `3D-grid` BFS at 40 threads is 0.214 against 0.216: no
gain at all. Whether it wins depends on whether the workload is bound by
memory bandwidth at that thread count — a property of the machine, not of the
codec.

## How to read the paper and the code (with the concepts in hand)

Read §3 for the format and parallel decoding, §4's "Compression Quality"
paragraph, then Table 2 against the Step-6 rows. In `byte.h` read `eatEdge`,
`eatFirstEdge`, `decode`, then `compressFirstEdge` (`byte.h:151-184`: the
mirror of the decoder, building the first byte with `0x3f`, `0x40`, `0x80` at
lines 159–165); `byteRLE.h` adds the header and the three-width test at lines
244–262. `nibble.h` holds the half-byte version.

## Where each step lives in this topic

| step | here |
|---|---|
| byte code | `gapcode::VarintGraph` (lane 1 columns `varint`, `var ns/e`) |
| decode cost on a scan | lane 1 `var ns/e` against `CSR ns/e` |
| memory-bound vs decode-bound | README, "the problem, measured" |
| reordering already good | lane 1 `planted (oracle)` vs `random` |

## Questions (answer in notes.md)

1. Encode the differences `3, 5, 300, 7` by hand in the plain byte code and in
   byte-RLE. Which is larger, and for which difference pattern does RLE win?
2. Your machine's lane-1 `var ns/e` against `CSR ns/e`: is the decode cost
   visible in one thread? What would you change to make the scan memory-bound
   (hint: the paper's machine has 80 threads on four sockets)?
3. `uk-union` PageRank is faster compressed even at `T1`, `Twitter` is not.
   What differs about the two graphs' ids and sizes?
4. Why must the first edge of each parallel chunk be encoded relative to the
   source?

## Done when

Answer each before unfolding it.

- [ ] You can encode an integer in the byte code and decode it back.

  <details><summary>Answer</summary>

  Split into seven-bit groups, least significant first; set the 0x80 continue
  bit on every byte except the last. 300 is `AC 02` (44 | 0x80, then 2). A
  first edge has six data bits and a 0x40 sign bit: source 100 to target 5000
  is `A4 4C` (36 | 0x80, then 76), decoding to 36 + (76 << 6) = 4900 and
  100 + 4900 = 5000 (`byte.h:63-79`, `84-97`).

  </details>

- [ ] You can explain what byte-RLE changes, what it costs, and what it buys.

  <details><summary>Answer</summary>

  It stores groups of up to 64 consecutive differences that need the same
  number of bytes (1–4) behind a one-byte header (2 bits of width, 6 bits of
  length), with no continue bits (`byteRLE.h:103-105`, encoder `232-297`). The
  decoder runs a fixed-width inner loop, avoiding a data-dependent branch per
  byte. It is the largest of the three codes because headers and
  all-eight-bits-used widths cost more when widths alternate: differences 3, 5,
  300, 7 take 8 bytes against 5.

  </details>

- [ ] You can state when compression made the paper's algorithms faster and
  when it did not.

  <details><summary>Answer</summary>

  Single-threaded it is usually slower (Twitter PageRank 73.5 s against 48.6 s
  with byte codes). At 40 cores with hyper-threading the gap closes or
  reverses (2.74 against 2.68), because memory bandwidth is shared and decoding
  parallelises; on average the fastest scheme was about 14% faster (§1). On
  uk-union, which compresses 3×, it won even at one thread (56.9 against 74.4).
  The result depends on how memory-bound the machine is.

  </details>

- [ ] You can quote what the paper says about vertex reordering and say what
  it leaves unmeasured.

  <details><summary>Answer</summary>

  "The locality of our real-world graphs is already quite good without
  reordering" (§4); the best reordering was used per graph but "reordering is
  not always necessary". The graphs arrived in crawl or dataset order, so the
  paper never measures compression on a graph whose ids are arbitrary — the
  case this topic's lane 1 constructs.

  </details>

## References

- Shun, Dhulipala & Blelloch, *Smaller and Faster: Parallel Processing of
  Compressed Graphs with Ligra+*, DCC 2015 — §1, §2.1, §3, §4 (Table 1,
  Table 2, Figures 5–6).
- Blandford, Blelloch & Kash, *Compact representations of separable graphs*
  (SODA 2003) and *An experimental analysis of a compact graph representation*
  (2004) — the k-bit codes; not read.
- `ligra` at `8763202`: `ligra/byte.h`, `ligra/byteRLE.h`, `ligra/nibble.h`,
  `ligra/encoder.C`.
- [topic 24's reading-ligra.md](../24-graph-algorithms/reading-ligra.md) — Ligra's
  `edgeMap`, without compression.
