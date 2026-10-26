# webgraph-rs: the BV format as code, and the reordering that feeds it

[reading-webgraph-framework.md](reading-webgraph-framework.md) derives the
format from the paper's tables. This guide reads the Rust implementation that
writes it today and checks each concept against code: the greedy reference
choice, the copy-block routine, the interval split, the default parameters
(which are *not* the ones in the paper's Table 7), and — in the same
repository — **Layered Label Propagation**, the community-based ordering that
the compressor assumes you ran first. That last piece is the practical answer
to "what do I number the vertices by".

Code anchors are `vigna/webgraph-rs` at commit
`8ce86026b9c1beca249d580d9274af2f78bfb74d` (2026-10-26, crate `webgraph`
0.7.0), which is not yet in `resources/codebases.md`'s pin table. Files:
`webgraph/src/graphs/bvgraph/comp/bvcomp.rs`, `.../flags.rs`, and
`algo/src/llp/`.

## The problem in one sentence

Given the BV format's three tools (reference, interval, residual), how does a
compressor choose, for every vertex, among up to `W + 1` candidate encodings
without trying a combinatorial number of them — and what does the same
repository do about the vertex order that decides how good any of them can be?

## The concepts, step by step

### Step 1 — the parameters and their defaults

> **In:** the paper's three parameters `W`, `R`, `L_min` and its codes.
> **Out:** the crate's defaults, which differ from the paper's experiments.

`CompFlags::default()` (`flags.rs:42-55`):

```rust
// webgraph/src/graphs/bvgraph/comp/flags.rs — CompFlags::default, 42-55
  42  impl core::default::Default for CompFlags {
  43      fn default() -> Self {
  44          CompFlags {
  45              outdegrees: Codes::Gamma,
  46              references: Codes::Unary,
  47              blocks: Codes::Gamma,
  48              intervals: Codes::Gamma,
  49              residuals: Codes::Zeta(3),
  50              min_interval_length: 4,
  51              compression_window: 7,
  52              max_ref_count: 3,
  53          }
  54      }
  55  }
```

Compare with the paper (§4, Table 7): `L_min = 3`, `W ∈ {1, 3, 7}`,
`R ∈ {∞, 3, 1}`, residuals in ζ₃. The crate matches `W = 7`, `R = 3` — the
`R = 3, W = 7` cell, 3.00 bits per link on the `.uk` snapshot — and the ζ₃
residuals, but chooses `L_min = 4`. A **ζ code** (ζ_k) writes `x + 1` as a unary
class (`h` zeros then a one, `h = ⌊log₂(x+1)/k⌋`) followed by a minimal-binary
remainder; it is the "flat" code tuned to the power-law gaps of Figure 1 in the
paper — ζ₃ spends `h + 1` bits on the class and then `3h + 2` or `3h + 3` bits of
remainder, where Elias-γ spends `2·⌊log₂(x+1)⌋ + 1`. Worked for `x = 718`
(`x + 1 = 719`, `⌊log₂ 719⌋ = 9`): γ = 19 bits; ζ₃ has `h = ⌊9/3⌋ = 3`, so `4`
bits of class and, since `719 − 2⁹ = 207` is below the class's cutoff of 512,
`3·3 + 2 = 11` bits of remainder: 15 bits. The references are written in **unary**:
`r` costs `r + 1` bits, cheap because `r ≤ W = 7`.

### Step 2 — choosing a reference: the window loop

> **In:** one vertex's list and the previous `W` lists. **Out:** the
> reference with the fewest bits, chosen greedily (`BvComp::push`).

`push` (`bvcomp.rs:438-554`) is the heart. First it compresses the list with
no reference and measures the bits that would be written (lines 462–492, via an
`estimator` that counts instead of writing). Then it tries every candidate in
the window (`bvcomp.rs:496-534`):

```rust
// webgraph/src/graphs/bvgraph/comp/bvcomp.rs — the window loop in push, 496-512
 496          let deltas = 1 + self
 497              .compression_window
 498              .min(self.curr_node - self.start_node);
 499          // compression windows is not zero, so compress the current node
 500          for delta in 1..deltas {
 501              let ref_node = self.curr_node - delta;
 502              // If the reference node is too far, we don't consider it
 503              let count = self.ref_counts[ref_node];
 504              if count >= self.max_ref_count {
 505                  continue;
 506              }
 507              // Get the neighbors of this previous ref_node
 508              let ref_list = &self.backrefs[ref_node];
 509              // No neighbors, no compression
 510              if ref_list.is_empty() {
 511                  continue;
 512              }
```

and keeps the strictly smaller (`bits < min_bits`, line 529: "the nearest one in
the case of multiple equal ones"). Line 504 is the paper's `R` (Step 5 of the
framework guide): a candidate whose own chain is already `max_ref_count` long
is skipped. Line 510 skips empty lists. The `backrefs` buffer holds the last
`W` lists (a circular buffer); the cost of compressing is therefore `W + 1`
trial encodings per vertex, which is why the paper says the window is a
"slower and more memory-consuming compression" trade.

This is exactly what lane 3's reference does, and where its cost goes: the
bench's `enc ms` column is `W + 1 = 8` trial encodings per vertex. The crate also
has `BvCompZ` (`bvcompz.rs`), "an alternative reference-selection strategy
based on dynamic programming" (`bvcomp.rs:78-79`) — the paper's conclusion
named "find a simple way to get the optimal compression" as open work.

### Step 3 — the copy blocks, as a merge

> **In:** a list and its reference. **Out:** `diff_comp`'s blocks and extra
> nodes, matching the framework guide's Step 3 row for vertex 16.

`diff_comp` (`bvcomp.rs:313-393`) walks both sorted lists with two cursors
(`j` into the current list, `k` into the reference) in two states: *copying*
(lines 325–351) and *ignoring* (352–376). The three comparisons in the copying
state are the whole story:

```rust
// webgraph/src/graphs/bvgraph/comp/bvcomp.rs — diff_comp, copying state, 325-350
 325              if copying {
 326                  match curr_list[j].cmp(&ref_list[k]) {
 327                      Ordering::Greater => {
// ... 328-329: comments ...
 330                          self.blocks.push(curr_block_len);
 331                          copying = false;
 332                          curr_block_len = 0;
 333                      }
 334                      Ordering::Less => {
// ... 335-338: comments ...
 339                          self.extra_nodes.push(curr_list[j]);
 340                          j += 1;
 341                      }
 342                      Ordering::Equal => {
// ... 343-346: comments ...
 347                          j += 1;
 348                          k += 1;
 349                          curr_block_len += 1;
 350                      }
```

Equal: the reference entry is kept (extend the copy run). Less: the current
list has an entry the reference lacks (an extra node). Greater: the reference
has entries the current list skips (end the copy run, start an ignore run).
The omitted last block is at lines 378–382 (`if copying && k < ref_list.len()
{ push }`), and the "add a 1 to the first block" convention at 389–392 is the
paper's "first block is not decremented, the rest are": the `write` method
subtracts one from every block (`bvcomp.rs:194`: `writer.write_block((self.blocks[i] - 1) …)`).
Run it on `S(15)` / `S(16)` from the paper and the raw run lengths are
`0, 1, 3, 2, 2, 1, 1`; line 391 adds 1 to the first so that `write`'s uniform
`− 1` gives `0, 0, 2, 1, 1, 0, 0` — seven blocks, the paper's Table 4 row.

### Step 4 — intervals and residuals, and the record's order

> **In:** the extra nodes. **Out:** `intervalize` and the order in which
> `write` emits a record.

`intervalize` (`bvcomp.rs:283-309`) finds runs of consecutive extra nodes,
`extra_nodes[i] + 1 == extra_nodes[i + 1]` (lines 289, 291), and keeps those
of at least `min_interval_length` as `(left, len)` pairs (297–300); the rest
become residuals (303–305). `write` (`bvcomp.rs:175-239`) then emits the record
in the paper's Table 6 order: outdegree (185), reference (189) and blocks
(191–196), intervals (201–223), residuals (225–235). Two details match the
paper's formulas exactly. The first interval start is
`(left − curr_node).to_nat()` (line 206): the zig-zag of the signed distance
from the vertex. Later starts are `left[i] − prev − 1` where `prev = left +
len` of the previous interval (lines 211, 215, 220), i.e. `left − (right_end +
1) − 1 = left − right_end − 2`, the paper's `E_{k+1} − E_k − L_k − 1` once
`E_k + L_k = right_end + 1`. Interval lengths are written minus `L_min`
(209, 218). The first residual is `(res − curr_node).to_nat()` (227), later
ones `res[i] − res[i−1] − 1` (232).

### Step 5 — the zig-zag, the one place the paper and code differ

> **In:** Step 4's `.to_nat()`. **Out:** which formula the crate implements.

`to_nat` is `dsi_bitstream::codes::ToNat`, implemented as
`(x << 1) ^ (x >> 63)` — documented as "nonnegative `x` to `2x` and a negative
`x` to `−2x − 1`" (`dsi-bitstream-rs`, `src/codes/mod.rs`, read at `main`). So
`ν(−2) = 3`, the paper's formula; the paper's worked example (and its Table 5)
prints 5. The code and the paper's equation agree; the paper's example is the
outlier.

### Step 6 — the other half of the repository: LLP, the ordering

> **In:** a symmetric graph. **Out:** a permutation that makes Steps 2–4 work,
> and the objective it reports.

The compressor assumes the numbering is already good; `algo/src/llp/mod.rs`
produces one. **Layered Label Propagation** (Boldi, Rosa, Santini & Vigna,
WWW 2011 — not read; this section describes the crate's documentation and
code): each vertex holds a label and repeatedly adopts the label that
maximises an objective based on the label frequencies among its neighbours and
a **resolution** γ. Low γ yields few large communities, high γ many small ones.
LLP runs label propagation for several γ ("exponentially-spaced values, for
example γ ∈ {1, 1/2, 1/4, …}") and combines the labelings, so ids reflect
community structure at several scales; the graph is then permuted in label
order (`mod.rs:28-40`). It requires a **symmetric, loopless** graph
(`mod.rs:18-19`), which is why a directed graph needs `symmetrize_seq` first.
Memory: "two `usize` and a boolean per node" plus the graph (`mod.rs:23`).

It also reports the objective of Dhulipala et al.'s paper. `compute_log_gap_cost`
(`algo/src/llp/gap_cost.rs:18-44`) sums `⌊log₂(gap)⌋` over each list's gaps —
and the first gap as `⌊log₂(|x − s₀| + 1)⌋` — with the comment "this
implementation uses the *floor* of the base-2 logarithm as a measure of cost,
whereas the Java implementation uses the *ceiling*". Dhulipala et al. define
`log(x) = 1 + ⌊log₂ x⌋` (bits needed), which is neither; the three differ by
at most one bit per gap, and the lane-1 `LogGap` column uses the paper's
bit-length. Compare numbers across tools only after checking which logarithm
each used.

Which gives the missing piece for a database: **the ordering step is a
community-detection step.** Topic 24 builds Leiden communities for analytics
([reading-louvain-leiden.md](../24-graph-algorithms/reading-louvain-leiden.md));
sorting vertices by Leiden community id and by degree within a community is the
cheap version of what LLP produces, and is the order lane 1 calls `planted
(oracle)`, computed here by the generator and, in a real graph, by Leiden.

## How to read the crate (with the concepts in hand)

Start at `flags.rs:42-55`, then `BvComp::push` (`bvcomp.rs:438-554`), then
`diff_comp`, `intervalize` and `write` in that order. Then `algo/src/llp/mod.rs`
lines 8–40 for the idea and `gap_cost.rs` for the objective. The decoder
(`random_access.rs`, `sequential.rs`, `masked_iter.rs`) holds the paper's lazy
iteration (framework guide Step 6) and was not read here.

## Where each step lives in this topic

| step | here |
|---|---|
| defaults and codes | lane 3's `encode(&g, 7, 3, 4)` |
| greedy reference choice | `refcode::encode`'s per-vertex loop (stub) |
| `diff_comp` | the copy-block computation in lane 3 |
| LLP as ordering | lane 2's bisection plays the same role |

## Questions (answer in notes.md)

1. Why does the window loop cost `W + 1` trial encodings per vertex, and what
   in the loop bounds a reference chain by `R`?
2. The crate's default `L_min` is 4 and the paper's experiments use 3. On a
   list with a run of 3 consecutive extras, which representation is shorter in
   γ-coded bits at `L_min = 3` and at 4? Work one example.
3. Where does the first residual's sign enter, and what does it cost for a
   vertex whose neighbours all have smaller ids?
4. LLP requires a symmetric graph. What does that cost for a directed graph's
   ordering quality, and what would you do in a database that stores
   directed relationships?

## Done when

Answer each before unfolding it.

- [ ] You can list the crate's default parameters and say which match the
  paper's best cell.

  <details><summary>Answer</summary>

  γ for outdegrees, blocks and intervals, unary for references, ζ₃ for
  residuals, `L_min = 4`, `W = 7`, `R = 3` (`flags.rs:42-55`). The paper's
  Table 7 uses ζ₃ and `L_min = 3`; the crate's `W = 7`, `R = 3` is the cell with
  3.00 bits per link on the .uk snapshot.

  </details>

- [ ] You can describe how `push` picks a reference.

  <details><summary>Answer</summary>

  It compresses the list with no reference as the baseline, then for each
  delta in 1..=W (bounded by the nodes available) skips nodes whose reference
  count already reaches `max_ref_count` or whose list is empty, compresses
  against each remaining candidate, measures the bits with an estimator, and
  keeps the strictly smaller (`bvcomp.rs:496-534`). Cost: W + 1 trial
  encodings.

  </details>

- [ ] You can trace `diff_comp`'s three comparisons.

  <details><summary>Answer</summary>

  Equal extends the copy run and advances both cursors; Less pushes the current
  entry to the extra nodes; Greater ends the copy run (pushing its length) and
  starts a skip run, in which each Greater advances the reference cursor and
  extends the run and an Equal ends it. The last block is not recorded unless
  copying stopped before the reference's end (`bvcomp.rs:313-393`).

  </details>

- [ ] You can say what LLP produces and what it requires.

  <details><summary>Answer</summary>

  A vertex labelling from label propagation at several resolutions, combined;
  permuting in label order puts community members at nearby ids. It requires a
  symmetric loopless graph, so directed graphs are symmetrised first
  (`algo/src/llp/mod.rs`).

  </details>

## References

- `vigna/webgraph-rs` at `8ce86026b9c1beca249d580d9274af2f78bfb74d`:
  `webgraph/src/graphs/bvgraph/comp/{bvcomp,flags}.rs`, `algo/src/llp/`.
- `vigna/dsi-bitstream-rs` `src/codes/mod.rs` (`ToNat`), read at `main`.
- Boldi & Vigna, WWW 2004 ([reading-webgraph-framework.md](reading-webgraph-framework.md)).
- Boldi, Rosa, Santini & Vigna, *Layered Label Propagation*, WWW 2011; not read.
- Dhulipala et al., KDD 2016 ([reading-recursive-bisection.md](reading-recursive-bisection.md)).
