# Plan the work or steal it: SuiteSparse's OpenMP vs rayon

Two philosophies of parallelizing the same sparse multiply.
SuiteSparse costs the work up front (a flopcount pre-pass) and
slices it statically into OpenMP tasks; rayon skips the cost model
and lets idle threads steal halves at runtime. M20's kernels must
pick a side per kernel, so this chapter builds both schedulers as
concepts first — the skew problem, static slicing, task teams,
work-stealing deques — then reads saxpy3's slicing code and rayon's
`join` as two answers to the same question.

## The problem in one sentence

Split one sparse multiply across 8 cores when a power-law graph
puts 1000× more work in some rows than others — divide the rows
into 8 equal-count slices and 7 cores finish early while 1 grinds a
hub row, so "parallel" delivers ~1× speedup.

## The concepts, step by step

### Step 1 — the skew problem: equal slices aren't equal work

Row-parallel sparse kernels look embarrassingly parallel — every
output row is independent. But a row's cost is its flops (the
count of multiply-adds it actually performs), and on power-law
graphs flops concentrate in **hub** rows: a few rows cost 1000×
the median. Slicing by row *count* therefore produces wildly
unequal slices, and the multiply finishes when the unluckiest
thread does. Every parallel scheduler is an answer to "who does
which slice?" under that skew — and there are exactly two families
of answer: measure the work and slice by cost, or slice lazily and
let idle threads take from busy ones.

### Step 2 — the static answer: cost the work, then freeze the plan

SuiteSparse measures first. The flopcount pre-pass walks the
patterns and produces total and per-column flops; the thread count
and the task list are then *derived* from those numbers and frozen
before any multiply happens:

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
        ▼
   #pragma omp parallel — every thread grabs its task list
```

Note `GB_nthreads`: a tiny multiply gets ONE thread — the
parallelism is *costed* like a query plan, using the same
flopcount that sizes the hash tables. What this buys: zero
scheduling overhead at execution time and a deterministic
schedule. What it costs: the O(nnz)-ish pre-pass runs before every
multiply, profitable only when the multiply is big enough to
repay it.

### Step 3 — coarse and fine tasks: ownership vs teams

Cost-balanced slicing still hits a wall when ONE column's flops
exceed a whole fair share — you can't give half a column to
another thread by slicing columns. saxpy3's escape hatch is a
second task kind:

```
 B's vectors (columns) → tasks:
   coarse task: one thread OWNS whole columns of B
                (private workspace, no coordination)
   fine task:   a TEAM splits one fat column (a hub);
                Gustavson workspace shared, atomics coordinate
```

Coarse tasks are the cheap common case: private workspace, no
atomics. Fine tasks buy load balance on hubs at the price of
atomic operations on the shared accumulator — coordination cost
paid only where skew forces it. This is the static world's
version of "help the overloaded thread": decided up front, from
the flopcount.

### Step 4 — the dynamic answer: work stealing

rayon inverts the philosophy: measure nothing, split lazily, and
rebalance by theft. Each worker thread owns a **deque**
(double-ended queue) of pending work; an idle thread **steals**
from the other end of a busy thread's deque. The primitive is
`join(a, b)` — "these two closures *may* run in parallel":

```
 rayon (split lazily, steal dynamically):

   par_iter over rows ──► join(left, right)   (join/mod.rs:93)
     caller runs left inline, pushes right onto ITS deque (:115)
     idle worker steals right          (registry.rs:248, Stealer)
     each stolen half splits again — recursion IS the scheduler
```

rayon's entire scheduler contract fits in one function:

```rust
// join: run `left` inline, PUBLISH `right` for theft — recursion is the scheduler
fn join<A, B>(left: A, right: B) {
    let pending = my_deque.push(right);   // ~free if no thread is idle
    left();                               // the caller does real work NOW
    match my_deque.pop(pending) {
        Some(right) => right(),           // nobody stole it — run it inline
        None => {
            // an idle worker took `right`; don't block — steal OTHER
            // work until it finishes (skewed halves rebalance themselves)
            steal_until_done(pending);
        }
    }
}
// vs saxpy3: nthreads = f(total_flops, chunk); tasks pre-sliced by flops —
// the schedule is COSTED like a query plan, then frozen
```

Skew handles itself: a hub row's half gets split again and stolen
again until the work spreads. The flopcount pre-pass becomes
*optional* — RMAT's heavy tail rebalances dynamically. The price:
every potential split pays a deque push, and theft is
nondeterministic (two runs assign rows to threads differently —
question 4 asks when that shows in the *output*).

### Step 5 — the small-job guard exists in both worlds

Parallelism has a floor cost, and both schedulers refuse tiny
jobs — they just spell it differently. SuiteSparse:
`GB_nthreads(work, chunk, nthreads_max)` returns 1 when
total_flops is below a chunk — one thread, zero overhead. rayon:
`with_min_len(k)` stops the recursive splitting below k elements —
without it, a 1000×1000 multiply with 5K nonzeros shatters into
thousands of deque pushes that each cost more than the work they
carry (question 3 makes you write it). Same decision — "is this
worth parallelizing?" — made from a cost estimate in one world and
from a per-split granularity floor in the other.

### Step 6 — the trade in one table

| axis | static (SuiteSparse) | stealing (rayon) |
|---|---|---|
| needs a cost model | yes (flopcount) | no |
| skew response | pre-balanced or fine-task atomics | automatic |
| per-task overhead | ~zero at runtime | deque push + potential steal |
| determinism of schedule | high | none |
| lines of scheduler code you own | many | zero (but tune min_len) |

Two operational wrinkles to carry into M20. Determinism: with
floating-point ⊕ (addition isn't associative in floats),
schedule-dependent combination order means run-to-run output
wobble — static schedules sidestep the question, stealing must
answer it (question 4). And the FFI trap: no native-Rust GraphBLAS
exists — `rustgraphblas` and `graphblas_sparse_linear_algebra` are
FFI bindings over SuiteSparse, so your process ends up with TWO
thread pools (SuiteSparse's OpenMP + your rayon), both sized to
num_cpus — question 5. A pure-Rust kernel core — M20 —
parallelizes with rayon and must answer saxpy3's questions itself:
when is one thread right, and who owns the workspace?

## Where each step lives in the code

What to read, in order:

1. `GB_AxB_saxpy3.c:22-48` (steps 2-3) — the header comment is a
   scheduling essay: coarse/fine taxonomy, Gustavson-vs-hash per
   task.
2. `GB_AxB_saxpy3_slice_balanced.c:309` (entry), :418 (nthreads
   from flops — steps 2, 5), :456 (target task size). Note what is
   *not* here: no dynamic load balancing at execution time.
3. `GB_AxB_saxpy3_flopcount.c:80` (step 2) — exact flops per column
   of B, cheap because it only walks pattern, not values (and
   itself parallel: `omp schedule(dynamic,1)` at :219).
4. rayon `join/mod.rs:93-140` (step 4) — `join_context`: inline +
   push + steal-back. The "potential parallelism" framing: `join`
   costs ~nothing when no thread is idle.
5. `registry.rs:10-60, :248` (step 4) — one `Worker` deque per
   thread, `Stealer` handles crossed between them; the sleep/wake
   protocol is why idle rayon threads don't spin.

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

## References

**Code**
- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  `Source/mxm/GB_AxB_saxpy3.c` (the header comment is a scheduling
  essay), `GB_AxB_saxpy3_slice_balanced.c`,
  `GB_AxB_saxpy3_flopcount.c`
- [rayon](https://github.com/rayon-rs/rayon)
  `rayon-core/src/join/mod.rs` (:93 `join_context`),
  `rayon-core/src/registry.rs` (:248 — one deque + `Stealer` per
  worker)
