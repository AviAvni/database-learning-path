# Reading guide — Gustavson '78 + Buluç & Gilbert SpGEMM survey

"Two Fast Algorithms for Sparse Matrices" (Gustavson, TOMS 1978) —
the 1978 paper every modern SpGEMM still implements — plus the
Buluç/Gilbert framing of the design space.

## 1. The problem: C = A*B when everything is sparse

Dense matmul is three nested loops; sparse kills two of them.
The inner-product view (C(i,j) = A(i,:)·B(:,j)) does nnz(A)·nnz(B)
intersection work mostly producing zeros. Gustavson's row-wise
view:

```
 for i in rows(A):                      # one output row at a time
   for k where A(i,k) ≠ 0:              # A's row pattern
     for j where B(k,j) ≠ 0:            # B's row k
       SPA[j] += A(i,k) * B(k,j)        # scatter-accumulate
   C(i,:) = gather nonzeros of SPA      # then reset SPA
```

Work = flops = Σᵢₖ nnz(B(k,:)) over A's entries — *optimal*: you
touch exactly the multiplications that exist. The entire algorithm
design space since is "what data structure is SPA":

```
 SPA = dense array + occupied list   (Gustavson '78)
       O(1) scatter, O(m) alloc, gather via occupied list
 SPA = hash table                    (saxpy3 hash task)
       O(1)-ish scatter, O(flops) alloc — wins for huge m
 SPA = heap / sorted-list merge      (merge k sorted rows of B)
       output comes out SORTED — no gather/sort pass
```

## 2. The two-pointer / two-phase trick (the "two" in the title)

Output size nnz(C) is unknown before you compute it. Gustavson:
symbolic phase (pattern only — compute row counts, allocate exact)
then numeric phase (fill). Every system in this curriculum that
meets sparse output rediscovers this: saxpy3's flopcount pre-pass,
cudf's size/retrieve, Gunrock's degree scan. Alternative: guess +
grow (topic 17's simdjson over-allocate answer). Our stub does
symbolic+numeric; the HashMap reference does guess-free
accumulation and pays for it in allocator traffic.

## 3. Buluç & Gilbert's axes (the survey's map)

- **formulation**: row-wise (Gustavson) / outer-product (column of
  A × row of B → rank-1 updates, needs merging) / inner-product
- **accumulator**: SPA / hash / heap / merge — pick by density of
  the output row and size of m
- **parallelism**: rows are independent (row-wise ⇒ embarrassingly
  parallel over i) BUT power-law graphs make row costs wildly
  unequal ⇒ saxpy3's coarse/fine split, Gunrock's merge_path — the
  same load-balance problem at every layer of this curriculum
- **compression**: masked SpGEMM (C<M>=A*B) can skip work only in
  dot formulation; Gustavson's mask only prunes writes

## 4. Cost intuition to carry

For RMAT/power-law A²: flops concentrate in hub rows (row i's cost
∝ Σ degrees of i's neighbors — degree-squared weighting). A few
rows are 1000× the median: static row partitioning dies, which is
why every real implementation has the fine-task path.

## Questions for notes.md

1. Derive: why is Gustavson's total work exactly
   Σ_{(i,k)∈A} nnz(B(k,:)) and why can no SpGEMM do fewer
   multiplications (each is a necessary term — unless the SEMIRING
   short-circuits: ANY_PAIR reachability can stop early — where?).
2. The dense SPA costs m bytes×2 (value + mark) per thread. For
   m = 10M that's cold DRAM per row. Compute the crossover row
   density where hash beats SPA using topic 13's cache numbers
   (SPA touches nnz_out random cells of an 80 MB array; hash
   touches nnz_out cells of a 2×flops table that fits L2).
3. Symbolic+numeric does the pattern walk TWICE. When is
   guess-and-grow cheaper (flops/row small and uniform — the
   variance argument; connect to topic 16's ddmin determinism
   requirement... no wait, to cudf's retrieve-skip answer)?
4. Outer-product SpGEMM produces k rank-1 updates that must be
   merged — which topic 3 structure is that (LSM: sorted runs +
   merge), and why does it win out-of-core / distributed
   (sequential I/O, no random SPA)?
5. Masked Gustavson can't skip work; masked dot can. Show it on
   triangle counting C<L>=L*L: what does each formulation compute
   per wedge, and reconcile with LAGraph shipping BOTH Sandia_LL
   (saxpy) and Sandia_LUT (dot) as the fastest per-graph choices.
