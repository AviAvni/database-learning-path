# Reading guide — parallelism: SuiteSparse's OpenMP vs rayon

Code: SuiteSparse `Source/mxm/GB_AxB_saxpy3.c`,
`GB_AxB_saxpy3_slice_balanced.c`, `GB_AxB_saxpy3_flopcount.c`
(all in [`~/repos/GraphBLAS`](https://github.com/DrTimothyAldenDavis/GraphBLAS)); rayon `rayon-core/src/join/mod.rs`
and `rayon-core/src/registry.rs` ([`~/repos/rayon`](https://github.com/rayon-rs/rayon)).

## Two answers to "who does which slice of the multiply?"

```
 SuiteSparse (plan first, execute statically):

   flopcount pass ──► total_flops, per-column flops
        │              (GB_AxB_saxpy3_flopcount.c:80; itself
        │               parallel: omp schedule(dynamic,1) at :219)
        ▼
   nthreads = GB_nthreads(total_flops, chunk, nthreads_max)
        │              (slice_balanced.c:418 — tiny job ⇒ 1 thread)
        ▼
   slice B into tasks, balanced by flops       (:434, :456)
     coarse task: one thread owns whole columns of B
     fine task:   a TEAM splits one fat column; Gustavson
                  workspace shared, atomics coordinate
        ▼
   #pragma omp parallel — every thread grabs its task list

 rayon (split lazily, steal dynamically):

   par_iter over rows ──► join(left, right)   (join/mod.rs:93)
     caller runs left inline, pushes right onto ITS deque (:115)
     idle worker steals right          (registry.rs:248, Stealer)
     each stolen half splits again — recursion IS the scheduler
```

Same problem — power-law column weights mean equal-column-count
slices are wildly unbalanced — solved with a cost model in one
world and with theft in the other.

## What to read, in order

1. `GB_AxB_saxpy3.c:22-48` — the header comment is a scheduling
   essay: coarse/fine taxonomy, Gustavson-vs-hash per task.
2. `GB_AxB_saxpy3_slice_balanced.c:309` (entry), :418 (nthreads
   from flops), :456 (target task size). Note what is *not* here:
   no dynamic load balancing at execution time.
3. `GB_AxB_saxpy3_flopcount.c:80` — exact flops per column of B,
   cheap because it only walks pattern, not values.
4. rayon `join/mod.rs:93-140` — `join_context`: inline + push +
   steal-back. The "potential parallelism" framing: `join` costs
   ~nothing when no thread is idle.
5. `registry.rs:10-60, :248` — one `Worker` deque per thread,
   `Stealer` handles crossed between them; the sleep/wake protocol
   is why idle rayon threads don't spin.

## The trade in one table

| axis | static (SuiteSparse) | stealing (rayon) |
|---|---|---|
| needs a cost model | yes (flopcount) | no |
| skew response | pre-balanced or fine-task atomics | automatic |
| per-task overhead | ~zero at runtime | deque push + potential steal |
| determinism of schedule | high | none |
| lines of scheduler code you own | many | zero (but tune min_len) |

## Questions

1. saxpy3's flopcount pass costs O(nnz(B) + flops-pattern-walk)
   before any multiply happens. For which matrix shapes is that
   pre-pass a bad deal, and what does rayon do instead of paying it?
2. Fine tasks share one Gustavson workspace with atomics. What is
   the rayon-idiomatic equivalent for one fat row — and why does
   "split the row, each half gets its own SPA, merge after" change
   the memory bill?
3. `GB_nthreads(work, chunk, nthreads_max)` returns 1 for small
   work. Write the rayon equivalent — where does `with_min_len`
   go, and what happens if you omit it on a 1000×1000 multiply
   with 5K nonzeros?
4. Work-stealing is nondeterministic: two runs assign rows to
   threads differently. Which GraphBLAS semirings make that
   visible in the OUTPUT (hint: floating-point ⊕), and how does
   SuiteSparse's static schedule sidestep the question?
5. rustgraphblas-style FFI bindings inherit SuiteSparse's OpenMP
   pool; your Rust process also has a rayon pool. What goes wrong
   when both are sized to num_cpus and a rayon task calls GrB_mxm?
6. **M20 mapping**: pick the M20 kernel list (SpMV, SpMSpV, masked
   dot-SpGEMM, delta_mxm fold). For each, decide: par_iter over
   what axis, does it need a flopcount-style pre-pass, and who owns
   the workspace? Write the four decisions in notes.md — that's the
   checklist item.
