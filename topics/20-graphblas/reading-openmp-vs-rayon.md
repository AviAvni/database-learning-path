# Plan the work or steal it: SuiteSparse's OpenMP vs rayon

Two philosophies of parallelizing the same sparse multiply.
SuiteSparse costs the work up front (a flopcount pre-pass) and
slices it statically into OpenMP tasks; rayon skips the cost model
and lets idle threads steal halves at runtime. M20's kernels must
pick a side per kernel, so this chapter builds both schedulers as
concepts first — the skew problem, static slicing, task teams,
work-stealing deques — then reads saxpy3's slicing code and rayon's
`join` as two answers to the same question.

This is the chapter most at risk of unsourced folklore, so a
standing rule applies below: **every performance number carries its
source**, either a line of pinned code, a table in a paper, or a
measurement in this repo's own `notes.md`. Anything that is a
teaching sketch rather than a measurement is fenced and labelled
`ILLUSTRATION`. Three claims in the previous version of this
chapter were folklore; each is corrected where it lands.

Anchors are **SuiteSparse:GraphBLAS at `1fd5475`** (version 10.3.1)
and **rayon at `6d9e94b`**, the pins in `resources/codebases.md`.

## The problem in one sentence

You cannot know a sparse row's cost without computing something —
so you either measure first and freeze a plan, or split blind and
rebalance by theft, and the two choices have different failure
modes.

## The concepts, step by step

### Step 1 — the skew problem, and how big it actually is

> **In:** a power-law graph and a row-parallel kernel.
> **Out:** the measured degree skew, and the granularity at which
> it stops mattering.

Row-parallel sparse kernels look embarrassingly parallel — every
output row is independent. But a row's cost is its **flops** (here:
the number of semiring multiply-add pairs it performs, not
floating-point operations — the semiring may be integer or
boolean), and on power-law graphs flops concentrate in **hub**
rows.

This repo measured the skew rather than assuming it:

```
 source: topics/24-graph-algorithms/notes.md:5-7
         RMAT scale 16 — n = 65,536, m = 1,819,338

   max degree   9,751
   mean degree     27.8
   ratio          351×          ← the skew, measured
   uniform graph max degree 59  ← the control: no skew at all
```

Now the part the folklore gets wrong. A 351× skew does *not* mean
"7 cores idle while 1 grinds" — that only follows if the slices are
about one row wide. Cost it at a realistic granularity:

```
 inputs: SpMV over that RMAT-16 matrix; work per row = its degree
         16 slices (Step 5 shows why 16)
         65,536 / 16 = 4,096 rows per slice

 mean slice work   = 4,096 × 27.8              = 113,869 flops
 hub-bearing slice = 9,751 + 4,095 × 27.8      = 123,592 flops
 imbalance         = 123,592 / 113,869         = 1.085

 ⇒ 8.5% straggler, not 8×. One hub row drowns in 4,095 ordinary
   ones.
```

So when *does* the skew bite? Two cases, both real:

1. **Fine granularity.** Slice to 4,096 tasks of 16 rows each and
   the hub slice costs 9,751 + 15×27.8 = 10,168 against a mean of
   445 — a **23× straggler**. Skew severity is a function of slice
   width, not of the graph alone.
2. **Superlinear per-row work.** In SpGEMM, row i of A·A costs
   Σ over i's neighbours of their degree, so the hub's cost is
   quadratic-ish in degree. Even so, at 16 slices over the
   ~1.28e8 flops extrapolated from `notes.md:22-26`, a hub row
   contributing ≥ 9,751 × 27.8 = 271,078 flops is 3.4% of an
   8.0e6-flop slice. Still not fatal.

Report the negative result honestly: **at the granularities both
schedulers actually choose, degree skew alone is a single-digit-%
effect.** What genuinely destroys parallelism is Step 4's case —
too few columns to slice at all — and that is a *shape* problem,
not a skew problem.

Why it matters: every parallel scheduler is an answer to "who does
which slice?", and you now know the size of the question at each
granularity instead of repeating a slogan.

### Step 2 — the static answer: cost the work, then freeze the plan

> **In:** the pattern of A, B, and the mask.
> **Out:** an exact per-column flop vector, a thread count, and a
> frozen task list.

SuiteSparse measures first. The pre-pass walks only *patterns* —
never values — so it is much cheaper than the multiply it plans:

```c
// GB_AxB_saxpy3_flopcount.c — the complexity claim, 44-48
    44  // The algorithm scans all nonzeros in B.  It only scans at most the min and
    45  // max (first and last) row indices in A and M (if M is present).  If A and M
    46  // are not hypersparse, the time taken is O(nnz(B)+n).  If all matrices are
    47  // hypersparse, the time is O(nnz(B)*log(h)) where h = max # of vectors present
    48  // in A and M.  Assuming B is in standard (not hypersparse) form:
```

The pseudocode that follows (`:50-69`) is the whole pre-pass, and
two lines of it carry the design:

```c
// GB_AxB_saxpy3_flopcount.c — the pre-pass, 54-67 (elided)
    54      for each column j in B:
    55          if (B (:,j) is empty) continue ;
    56          mjnz = nnz (M (:,j))
    57          if (M is present, not complemented, and M (:,j) is empty) continue ;
    ...
    60          for each k where B (k,j) is nonzero:
    61              aknz = nnz (A (:,k))
    62              if (aknz == 0) continue ;
    ...
    66              Bflops (j) += aknz
    67          end
```

`:57` is the masked shortcut — an empty mask column means the whole
column of C is skipped, so the mask prunes the *plan*, not just the
output. `:66` is the flop definition: column j's cost is the sum of
`nnz(A(:,k))` over its nonzeros k. Exactly Gustavson's f from
[reading-gustavson-spgemm.md](reading-gustavson-spgemm.md).

The pre-pass is itself parallel, and note *which* OpenMP schedule:

```c
// GB_AxB_saxpy3_flopcount.c — the pre-pass parallelises itself, 219-221
   219      #pragma omp parallel for num_threads(B_nthreads) schedule(dynamic,1) \
   220          reduction(+:total_Mwork)
   221      for (taskid = 0 ; taskid < B_ntasks ; taskid++)
```

`schedule(dynamic,1)` over **pre-sliced tasks** — one task handed
out at a time, on demand. That is a mild form of dynamic balancing,
and it is here rather than in the multiply because at pre-pass time
the costs are precisely what is not yet known. Worth remembering
when someone claims SuiteSparse "has no dynamic scheduling".

Then the plan is derived from the measurement:

```c
// GB_AxB_saxpy3_slice_balanced.c — the pre-pass and its outputs, 308-311
   308      GB_OK (GB_AxB_saxpy3_flopcount (&Mwork, Bflops, M, Mask_comp, A, B, Werk)) ;
   309      double total_flops = (double) Bflops [bnvec] ;
   310      double axbflops = total_flops - Mwork ;
   311      GBURBLE ("axbwork %g ", axbflops) ;
```

(The previous version of this chapter labelled `:309` "entry".
`:309` reads `total_flops` out of the last cell of `Bflops`; the
call is `:308`, and the flopcount function's own entry point is
`GB_AxB_saxpy3_flopcount.c:80`.)

```c
// GB_AxB_saxpy3_slice_balanced.c — thread count and task count, 418-420
   418      (*nthreads) = GB_nthreads (total_flops, chunk, nthreads_max) ;
   419      int ntasks_initial = ((*nthreads) == 1) ? 1 :
   420          (GB_NTASKS_PER_THREAD * (*nthreads)) ;
```

```c
// GB_AxB_saxpy3_slice_balanced.c — target task size, 456-459
   456      double target_task_size = total_flops / ((double) ntasks_initial) ;
   457      target_task_size = GB_IMAX (target_task_size, chunk) ;
   458      double target_fine_size = target_task_size / GB_FINE_WORK ;
   459      target_fine_size = GB_IMAX (target_fine_size, chunk) ;
```

Read `:456` carefully: the target is **flops per task**, not
columns per task. Tasks are sized by cost. That is the whole static
philosophy in one line.

One more line, because the previous version of this chapter cited
it wrongly. `:434` is *not* where B is sliced:

```c
// GB_AxB_saxpy3_slice_balanced.c — the intensity heuristic, 432-438
   432          double abnz = GB_nnz (A) + GB_nnz (B) + 1 ;
   433          double workspace = (double) ntasks_initial * (double) cvlen ;
   434          double intensity = total_flops / abnz ;
   ...
   437          if (((*nthreads) <= 8 && intensity >= 8  && workspace < abnz)
   438          ||  (                    intensity >= 16 && workspace < abnz))
```

`:434` computes *arithmetic intensity* — flops per input nonzero —
and `:437-438` uses it to force Gustavson for every task when the
multiply is dense enough that dense-accumulator workspace is repaid.
Note `nthreads` appears in the condition at `:437`: **the thread
count feeds back into the algorithm choice**, because
`ntasks_initial` copies of an m-length accumulator is the workspace
bill. Fewer threads, cheaper Gustavson.

Why it matters: parallelism here is *costed like a query plan*,
using the same flopcount that sizes the hash tables in
[reading-suitesparse-internals.md](reading-suitesparse-internals.md).
It buys a deterministic schedule and zero runtime scheduling
overhead; it costs an O(nnz(B)+n) pass before every multiply.

### Step 3 — coarse and fine tasks: ownership vs teams

> **In:** a flop-balanced target task size.
> **Out:** the two task kinds, what each owns, and the memory bill
> for each.

Cost-balanced slicing hits a wall when ONE column's flops exceed a
whole fair share — you cannot hand half a column to another thread
by slicing columns. saxpy3's escape hatch is a second task kind,
and its header comment is the primary source:

```c
// GB_AxB_saxpy3.c — the task taxonomy, 22-35 (elided)
    22  // The matrix B is split into two kinds of tasks: coarse and fine.  A coarse
    23  // task computes C(:,j1:j2) = A*B(:,j1:j2), for a unique set of vectors j1:j2.
    24  // Those vectors are not shared with any other tasks.  A fine task works with a
    25  // team of other fine tasks to compute C(:,j) for a single vector j.  Each fine
    26  // task computes A*B(k1:k2,j) for a unique range k1:k2, and sums its results
    27  // into C(:,j) via atomic operations.
    ...
    32  //      fine Gustavson task
    33  //      fine hash task
    34  //      coarse Gustason task
    35  //      coarse hash task
```

Four kinds, "then subdivided into 3 variants, for C=A*B, C<M>=A*B,
and C<!M>=A*B, giving a total of 12 different types of tasks"
(`:37-38`). And the preference order, stated by the source itself:

```c
// GB_AxB_saxpy3.c — when fine tasks are the ONLY option, 40-48
    40  // Fine tasks are used when there would otherwise be too much work for a single
    41  // task to compute the single vector C(:,j).  Fine tasks share all of their
    42  // workspace with the team of fine tasks computing C(:,j).  Coarse tasks are
    43  // prefered since they require less synchronization, but fine tasks allow for
    44  // better parallelization when B has only a few vectors.  If B consists of a
    45  // single vector (for GrB_mxv if A is in CSC format and not transposed, or
    46  // for GrB_vxm if A is in CSR format and not transpose), then the only way to
    47  // get parallelism is via fine tasks.  If a single thread is used for this
    48  // case, a single-vector coarse task is used.
```

`:44-47` is the load-bearing sentence of this whole chapter, and
Step 6 cashes it out into a measured speedup ceiling.

The memory bill is tabulated in the same comment:

```c
// GB_AxB_saxpy3.c — the workspace table, 66-70
    66  //      fine Gustavson task (shared):   int8_t   Hf [m] ; ctype Hx [m] ;
    67  //      fine hash task (shared):        uint64_t Hf [s] ; ctype Hx [s] ;
    68  //      coarse Gustavson task:          uint64_t Hf [m] ; ctype Hx [m] ;
    69  //      coarse hash task:               uint64_t Hf [s] ; ctype Hx [s] ;
    70  //                                      uint64_t Hi [s] ;
```

Price it, because this is what `:433`'s `workspace` variable is
counting:

```
 inputs: C is m × n with m = 1,048,576 (scale 20); ctype = f64 (8 B)
         ntasks_initial = GB_NTASKS_PER_THREAD × nthreads   (:419-420)
         GB_NTASKS_PER_THREAD = 2                           (:18)

 coarse Gustavson, per task:  8 B (uint64 Hf) + 8 B (Hx) = 16 B/row
   one task   : 1,048,576 × 16 B                  = 16.8 MB
   8 threads × 2 tasks/thread = 16 tasks          = 268 MB

 fine Gustavson, SHARED by the team:
   1 B (int8 Hf) + 8 B (Hx)                       =  9 B/row
   one shared copy                                = 9.4 MB

 ⇒ fine tasks are 28× cheaper in memory here, because the team
   shares one accumulator instead of each thread owning one —
   and pay for it with the atomics at :27.
```

That inversion is the point. Coarse = private workspace, no
coordination, memory × ntasks. Fine = one shared workspace, atomic
updates, memory × 1. Skew and shape decide which you can afford.

Why it matters: "who owns the workspace?" is the question M20 has
to answer for every kernel, and it is a memory question as much as
a synchronization one.

### Step 4 — the dynamic answer: work stealing

> **In:** no cost model at all.
> **Out:** rayon's `join`, the deque, the steal loop, and what
> "potential parallelism" actually means.

rayon inverts the philosophy: measure nothing, split lazily,
rebalance by theft. Its own doc states the contract:

```rust
// rayon-core/src/join/mod.rs — the contract, 17-32 (elided)
    17  /// implementation is quite different and incurs very low
    18  /// overhead. The underlying technique is called "work stealing": the
    19  /// Rayon runtime uses a fixed pool of worker threads and attempts to
    20  /// only execute code in parallel when there are idle CPUs to handle
    21  /// it.
    ...
    26  /// participates in the thread pool. It will begin by executing closure
    27  /// A (on the current thread). While it is doing that, it will advertise
    28  /// closure B as being available for other threads to execute. Once closure A
    29  /// has completed, the current thread will try to execute closure B;
    30  /// if however closure B has been stolen, then it will look for other work
    31  /// while waiting for the thief to fully execute closure B. (This is the
    32  /// typical work-stealing strategy).
```

"Attempts to only execute code in parallel when there are idle
CPUs" (`:19-21`) is the precise meaning of *potential* parallelism:
`join` is not a spawn. `join` itself is a two-line forwarder:

```rust
// rayon-core/src/join/mod.rs — join, 93-106 (elided)
    93  pub fn join<A, B, RA, RB>(oper_a: A, oper_b: B) -> (RA, RB)
    ...
   105      join_context(call(oper_a), call(oper_b))
   106  }
```

The scheduler is `join_context`, `:115-173`. Its core:

```rust
// rayon-core/src/join/mod.rs — the real mechanism, 136-169 (elided)
   136          let job_b = StackJob::new(call_b(oper_b), SpinLatch::new(worker_thread));
   137          let job_b_ref = job_b.as_job_ref();
   138          let job_b_id = job_b_ref.id();
   139          worker_thread.push(job_b_ref);
   140
   141          // Execute task a; hopefully b gets stolen in the meantime.
   142          let status_a = unwind::halt_unwinding(call_a(oper_a, injected));
   ...
   153          while !job_b.latch.probe() {
   154              let Some(job) = worker_thread.take_local_job() else {
   ...
   157                  worker_thread.wait_until(&job_b.latch);
   159                  break;
   160              };
   161              if job_b_id == job.id() {
   ...
   165                  let result_b = job_b.run_inline(injected);
   166                  return (result_a, result_b);
   167              }
   168              worker_thread.execute(job);
   169          }
```

Four behaviours, each on a line: **push** B onto the local deque
(`:139` — the previous version of this chapter cited `:115` for the
push; `:115` is the function signature), **run** A inline (`:142`),
**pop and run B inline** if nobody took it (`:154`, `:165`), and —
the part people forget — **do other people's work** while waiting
(`:168`), rather than blocking. A `join` where nothing is stolen
costs one deque push and one pop.

The deque and the thief:

```rust
// rayon-core/src/registry.rs — one deque + one Stealer per worker, 248-257
   248          let (workers, stealers): (Vec<_>, Vec<_>) = (0..n_threads)
   249              .map(|_| {
   250                  let worker = if breadth_first {
   251                      Worker::new_fifo()
   252                  } else {
   253                      Worker::new_lifo()
   254                  };
   255
   256                  let stealer = worker.stealer();
   257                  (worker, stealer)
```

LIFO by default (`:253`) — the owner pops the most recent push,
which is the depth-first order that keeps the working set hot;
thieves take from the other end, which is the *oldest* and
therefore biggest job. And the theft itself:

```rust
// rayon-core/src/registry.rs — the steal loop, 886-895 (elided)
   886          loop {
   887              let mut retry = false;
   888              let start = self.rng.next_usize(num_threads);
   889              let job = (start..num_threads)
   890                  .chain(0..start)
   891                  .filter(move |&i| i != self.index)
   892                  .find_map(|victim_index| {
   893                      let victim = &thread_infos[victim_index];
   894                      match victim.stealer.steal() {
   895                          Steal::Success(job) => Some(job),
```

A **random** starting victim (`:888`) then a round-robin sweep
(`:889-891`) — randomization is what stops all idle threads
hammering the same victim.

Two hazards the docs state outright and which matter for a database
kernel: blocking I/O inside a `join` closure "may be poor" and can
**deadlock** (`join/mod.rs:76-84`), and panics propagate but both
closures always run (`:86-92`).

Why it matters: no cost model, no pre-pass, and skew handled by
whoever is idle. But now the question is how far it splits — which
is where the third piece of folklore dies.

### Step 5 — the small-job guard: thief-splitting, not split-to-one

> **In:** a `par_iter` over rows.
> **Out:** the actual number of leaves rayon creates, and
> SuiteSparse's equivalent floor.

**The correction.** The previous version of this chapter claimed
that without `with_min_len`, a small multiply "shatters into
thousands of deque pushes". That is false at this pin. rayon does
not split to one element; it uses **thief-splitting**, and the
source says so:

```rust
// src/iter/plumbing/mod.rs — the Splitter, 247-283 (elided)
   247  /// Thief-splitting is an adaptive policy that starts by splitting into
   248  /// enough jobs for every worker thread, and then resets itself whenever a
   249  /// job is actually stolen into a different thread.
   ...
   252      /// The `splits` tell us approximately how many remaining times we'd
   253      /// like to split this job.  We always just divide it by two though, so
   254      /// the effective number of pieces will be `next_power_of_two()`.
   ...
   262              splits: crate::current_num_threads(),
   ...
   267      fn try_split(&mut self, stolen: bool) -> bool {
   ...
   270          if stolen {
   273              self.splits = Ord::max(crate::current_num_threads(), self.splits / 2);
   274              true
   275          } else if splits > 0 {
   277              self.splits /= 2;
   278              true
   279          } else {
   281              false
   282          }
```

Trace it, which is the arithmetic this step exists for:

```
 inputs: current_num_threads() = 8; nothing gets stolen
         Splitter::new() ⇒ splits = 8   (:262)

 depth 0: splits 8 > 0  → splits = 4, SPLIT      (:277)
 depth 1: splits 4 > 0  → splits = 2, SPLIT
 depth 2: splits 2 > 0  → splits = 1, SPLIT
 depth 3: splits 1 > 0  → splits = 0, SPLIT
 depth 4: splits 0      → STOP                   (:281)

 4 successful splits along every path  ⇒  2^4 = 16 leaves

 over n = 65,536 rows: 65,536 / 16 = 4,096 rows per leaf
 (which is exactly the granularity Step 1 costed)

 versus "split all the way down":  65,536 leaves — 4,096× more
 deque traffic than actually happens.
```

Theft *resets* the budget (`:273`), which is the adaptive part: a
job that was stolen is evidently in demand, so it is worth
splitting again. Sixteen leaves when nothing is stolen; more only
when the machine proves it needs them.

`min_len` is a separate, harder floor, and its own doc rebuts the
folklore too:

```rust
// src/iter/plumbing/mod.rs — min_len defaults to 1, 68-79
    68      /// The minimum number of items that we will process
    69      /// sequentially. Defaults to 1, which means that we will split
    70      /// all the way down to a single item. This can be raised higher
    71      /// using the [`with_min_len`] method, which will force us to
    72      /// create sequential tasks at a larger granularity. Note that
    73      /// Rayon automatically normally attempts to adjust the size of
    74      /// parallel splits to reduce overhead, so this should not be
    75      /// needed.
    ...
    78      fn min_len(&self) -> usize {
    79          1
    80      }
```

`:72-75` — "this should not be needed" — because the Splitter
already caps the leaf count. `LengthSplitter` combines the two:

```rust
// src/iter/plumbing/mod.rs — LengthSplitter, 308-331 (elided)
   308      fn new(min: usize, max: usize, len: usize) -> LengthSplitter {
   309          let mut splitter = LengthSplitter {
   310              inner: Splitter::new(),
   311              min: Ord::max(min, 1),
   312          };
   ...
   318          let min_splits = len / Ord::max(max, 1);
   ...
   329      fn try_split(&mut self, len: usize, stolen: bool) -> bool {
   330          // If splitting wouldn't make us too small, try the inner splitter.
   331          len / 2 >= self.min && self.inner.try_split(stolen)
   332      }
```

`:331` is a conjunction: `with_min_len` can only make rayon split
*less*, never more; `with_max_len` raises the floor on splits via
`:318`.

SuiteSparse's equivalent floor is a single function:

```c
// Source/omp/include/GB_nthreads.h — the small-job guard, 17-32 (elided)
    17  // If work < 2*chunk, then only one thread is used.
    18  // else if work < 3*chunk, then two threads are used, and so on.
    ...
    27      work  = GB_IMAX (work, 1) ;
    28      chunk = GB_IMAX (chunk, 1) ;
    29      int64_t nthreads = (int64_t) floor (work / chunk) ;
    30      nthreads = GB_IMIN (nthreads, nthreads_max) ;
    31      nthreads = GB_IMAX (nthreads, 1) ;
```

`chunk` defaults to `GB_CHUNK_DEFAULT (64*1024)` = 65,536
(`GB_defaults.h:24`). Run this repo's own measurements through it:

```
 inputs: notes.md:22-26 SpGEMM flop counts; chunk = 65,536
         assume nthreads_max = 8 (substitute your core count)

 scale 10:    298,000 flops / 65,536 =  4.5 → floor 4 → min(4,8) = 4 threads
 scale 12:  2,270,000 flops / 65,536 = 34.6 → floor 34 → min(34,8) = 8 threads
 scale 14: 17,100,000 flops / 65,536 =  261 → floor 261 → min(261,8) = 8 threads

 the single-thread frontier: work < 2 × 65,536 = 131,072 flops
   at notes.md:28-31's ~15 ns/flop that is ~1.97 ms of work
   before GraphBLAS will even use a SECOND thread.
```

Two readings. First, the topic's own smallest bench (scale 10) gets
**half the machine**, by design — the guard is deliberately
conservative. Second, `GB_nthreads` saturates at scale 12; from
there on the thread count is pinned at `nthreads_max` and all the
remaining tuning happens in task *sizing* (`:456`), not in thread
count.

Why it matters: both worlds refuse tiny jobs — one from a cost
estimate, one from an adaptive split budget — and both floors are
higher than intuition suggests.

### Step 6 — the trade, and the one measured ceiling

> **In:** both schedulers, understood.
> **Out:** the axis-by-axis comparison, plus the one published
> measurement that tells you which kernels will disappoint.

| axis | static (SuiteSparse) | stealing (rayon) |
|---|---|---|
| needs a cost model | yes — `flopcount`, O(nnz(B)+n) (`flopcount.c:44-48`) | no |
| skew response | pre-balanced by flops (`:456`), fine-task atomics for hubs (`saxpy3.c:22-27`) | theft resets the split budget (`plumbing/mod.rs:273`) |
| leaf count | `GB_NTASKS_PER_THREAD × nthreads` = 2 × nthreads (`:18`, `:419-420`) | ~`next_power_of_two(nthreads)` (`:254`, `:262`) |
| per-task overhead | ~zero at runtime; one pre-pass per multiply | one deque push + pop per `join` (`join/mod.rs:139`, `:154`) |
| small-job guard | `GB_nthreads`, work < 2·chunk ⇒ 1 thread | Splitter budget; `with_min_len` as a hard floor |
| determinism of schedule | high — same inputs, same tasks | none — random victim (`registry.rs:888`) |
| scheduler code you own | a lot | none (but tune `min_len`) |

Now the number that matters, and it is published rather than
folklore. Davis, "Parallel GraphBLAS with OpenMP" (CSC '20), §5,
Table 2 — Intel Xeon E5-2698 v4, 20 cores / 40 hardware threads,
**speedup at 40 threads relative to 1 thread**:

| kernel | datagen-8_9-fb | cit-Patents | g-1073643522 | graph500-scale25-ef16 | MAWI |
|---|---|---|---|---|---|
| Triangle Counting | 26.6 | 16.1 | 11.2 | 30.5 | 5.8 |
| 4-Truss | 27.7 | 19.7 | 16.6 | * | 13.4 |
| LCC | 25.8 | 11.7 | 8.4 | 30.2 | 5.7 |
| Bellman-Ford | 11.6 | 9.1 | 5.2 | 9.5 | 2.4 |
| **BFS** | **3.5** | **2.6** | **3.6** | **3.9** | **9.7** |

(`*` = not reported for that pair.) The paper's own explanation,
§5: "Breadth-first search and Bellman-Ford both show modest
parallelism; they both rely on a matrix-vector or vector-matrix
multiply, which is harder to parallelize."

Join that to Step 3's `saxpy3.c:44-47` and you have mechanism plus
measurement:

```
 a matrix-VECTOR multiply means B has ONE column.

 coarse tasks own "a unique set of vectors j1:j2"   (saxpy3.c:23)
   with n = 1 there is exactly one vector
   ⇒ at most ONE coarse task ⇒ speedup 1×

 "If B consists of a single vector ... then the only way to get
  parallelism is via fine tasks."                   (saxpy3.c:44-47)
   fine tasks sum into C(:,j) "via atomic operations" (:27)

 ⇒ the entire parallel speedup of an SpMV is bought with atomics
   on one shared accumulator, which is why CSC'20 Table 2 shows
   BFS at 2.6-3.9× where triangle counting reaches 11-30×.
```

That is a 4-10× gap between kernels *in the same library on the
same machine*, caused by operand shape rather than by scheduler
quality. M20's kernel list is dominated by SpMV and SpMSpV, so plan
for the low end of that range.

Two more wrinkles to carry forward.

**Determinism.** With a floating-point ⊕, addition is not
associative, so a schedule-dependent combination order gives
run-to-run output wobble. A frozen schedule sidesteps the question;
theft cannot. Note this is a property of the *monoid*, not of
rayon: `GxB_ANY_*` and `GrB_LOR` are indifferent to order, and the
`ANY_SECONDI` semiring of
[reading-lagraph.md](reading-lagraph.md) is nondeterministic *by
design*. Question 4 is about which of your semirings care.

**The two-pool trap.** If your Rust process links a GraphBLAS
through FFI, you get SuiteSparse's OpenMP pool *and* your rayon
pool, both sized to the core count, and a rayon task calling
`GrB_mxm` oversubscribes. This chapter did **not** verify the
current state of the Rust GraphBLAS crates — question 5 asks you to
check the crate you actually intend to use before relying on any of
this. A pure-Rust kernel core — M20 — dodges the trap entirely and
inherits saxpy3's questions instead: when is one thread right, and
who owns the workspace?

Why it matters: the scheduler is not the bottleneck you think it
is. Operand shape is.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| `GB_AxB_saxpy3_flopcount.c:44-48` | 2 | the pre-pass complexity: O(nnz(B)+n), or O(nnz(B)·log h) hypersparse |
| `GB_AxB_saxpy3_flopcount.c:50-69` | 2 | the pre-pass in pseudocode; `:57` mask pruning, `:66` the flop definition |
| `GB_AxB_saxpy3_flopcount.c:80` | 2 | the function's actual entry point |
| `GB_AxB_saxpy3_flopcount.c:219-221` | 2 | `schedule(dynamic,1)` — the one dynamic schedule in the path |
| `GB_AxB_saxpy3_slice_balanced.c:308-310` | 2 | the flopcount call; `:309` is `total_flops`, `:310` is `axbflops` |
| `GB_AxB_saxpy3_slice_balanced.c:18` | 3, 5 | `GB_NTASKS_PER_THREAD 2` — two tasks per thread, not 32 as in `GB_AxB_dot2.c:233` |
| `GB_AxB_saxpy3_slice_balanced.c:418-420` | 2, 5 | `GB_nthreads(total_flops, …)` and `ntasks_initial` |
| `GB_AxB_saxpy3_slice_balanced.c:432-438` | 2 | `intensity = total_flops/abnz` — **not** where B is sliced |
| `GB_AxB_saxpy3_slice_balanced.c:456-459` | 2 | `target_task_size` in **flops**, and `target_fine_size` |
| `GB_AxB_saxpy3.c:22-38` | 3 | coarse vs fine, 4 kinds × 3 variants = 12 task types |
| `GB_AxB_saxpy3.c:40-48` | 3, 6 | **why a single-vector B can only parallelize via fine tasks** |
| `GB_AxB_saxpy3.c:62-70` | 3 | the workspace table — 9 B/row shared vs 16 B/row per task |
| `Source/omp/include/GB_nthreads.h:17-32` | 5 | `clamp(floor(work/chunk), 1, nthreads_max)` |
| `Source/include/GB_defaults.h:24` | 5 | `GB_CHUNK_DEFAULT (64*1024)` = 65,536 |
| `rayon-core/src/join/mod.rs:17-32` | 4 | the work-stealing contract in rayon's own words |
| `rayon-core/src/join/mod.rs:76-92` | 4 | blocking-I/O deadlock warning; panic semantics |
| `rayon-core/src/join/mod.rs:93-106` | 4 | `join` — a forwarder to `join_context` at `:105` |
| `rayon-core/src/join/mod.rs:115-173` | 4 | `join_context`: **push at `:139`**, run A `:142`, steal-back `:153-169` |
| `rayon-core/src/registry.rs:248-257` | 4 | one LIFO `Worker` deque + one `Stealer` per thread |
| `rayon-core/src/registry.rs:875-905` | 4 | the steal loop: random victim `:888`, round-robin `:889-891` |
| `src/iter/plumbing/mod.rs:68-80` | 5 | `min_len` defaults to 1; `:72-75` says you normally should not need it |
| `src/iter/plumbing/mod.rs:246-284` | 5 | **thief-splitting** — the split budget that caps the leaf count |
| `src/iter/plumbing/mod.rs:286-333` | 5 | `LengthSplitter`; `:331` is a conjunction — `min_len` only reduces splitting |
| `src/iter/mod.rs:3152-3176` | 5 | `with_min_len` public doc |

Read in this order: `GB_AxB_saxpy3.c:20-86` first (the header
comment is a scheduling essay and it is the best-written thing in
the directory), then `GB_AxB_saxpy3_flopcount.c:40-69`, then
`GB_AxB_saxpy3_slice_balanced.c:300-470`. Then rayon's
`join/mod.rs` end to end (186 lines), then `plumbing/mod.rs:246-333`
— which is the file that tells you what rayon *actually* does, and
where the folklore usually goes wrong.

### What transfers to M20

Four kernels, four decisions, and Step 6 says which will
disappoint. SpMV and SpMSpV are the single-vector case
(`saxpy3.c:44-47`) — expect the CSC'20 BFS row, 2.6-3.9× at 40
threads, and design the accumulator with that in mind. Masked
dot-SpGEMM and the `delta_mxm` fold have many output columns and
should behave like the triangle-counting row. For each: what axis
does `par_iter` run over, is a flopcount-style pre-pass worth its
O(nnz(B)+n), and who owns the workspace — a private 16 B/row
accumulator per task, or one shared 9 B/row accumulator with
atomics?

## Questions to answer in notes.md

1. saxpy3's flopcount pass costs O(nnz(B) + n)
   (`GB_AxB_saxpy3_flopcount.c:44-48`) before any multiply happens.
   For which matrix shapes is that pre-pass a bad deal, and what
   does rayon do instead of paying it? Use Step 5's arithmetic: a
   multiply below 131,072 flops gets one thread anyway, so what is
   the pre-pass buying there?
2. Fine tasks share one Gustavson workspace with atomics
   (`GB_AxB_saxpy3.c:27`, `:66`). What is the rayon-idiomatic
   equivalent for one fat row — and why does "split the row, each
   half gets its own SPA, merge after" change the memory bill?
   Step 3 prices both: 9 B/row shared against 16 B/row × ntasks.
3. `GB_nthreads(work, chunk, nthreads_max)` returns 1 for small
   work. Write the rayon equivalent — where does `with_min_len` go,
   and what actually happens if you omit it on a 1000×1000 multiply
   with 5K nonzeros? Predict the leaf count from
   `plumbing/mod.rs:262` and `:277` *before* you measure it, then
   check whether `with_min_len` changes anything at all
   (`:331` is a conjunction).
4. Work-stealing is nondeterministic: two runs assign rows to
   threads differently (`registry.rs:888`). Which GraphBLAS
   semirings make that visible in the OUTPUT (hint: floating-point
   ⊕ is not associative; `GxB_ANY_*` and `GrB_LOR` are indifferent),
   and how does SuiteSparse's static schedule sidestep the question?
5. FFI bindings to SuiteSparse inherit its OpenMP pool; your Rust
   process also has a rayon pool. Check the crate you would
   actually use — is it FFI or pure Rust? — and work out what goes
   wrong when both pools are sized to `num_cpus` and a rayon task
   calls `GrB_mxm`. This chapter deliberately asserts nothing about
   the current crate ecosystem.
6. **M20 mapping**: pick the M20 kernel list (SpMV, SpMSpV, masked
   dot-SpGEMM, delta_mxm fold). For each, decide: par_iter over
   what axis, does it need a flopcount-style pre-pass, and who owns
   the workspace? Write the four decisions in notes.md, with the
   speedup you expect from CSC'20 Table 2's rows — that is the
   checklist item.

## Done when

Answer each before unfolding it.

- [ ] You can state the skew problem *and* say at what granularity it stops mattering, using the RMAT numbers measured in topic 24.

  <details><summary>Answer</summary>

  `topics/24-graph-algorithms/notes.md:5-7`: RMAT scale 16 has
  n = 65,536, m = 1,819,338, max degree **9,751**, mean degree
  **27.8** — a 351× ratio. The uniform control graph's max degree is
  59, i.e. no skew.

  But at the granularity schedulers actually use, that skew is
  small. Sixteen slices of 4,096 rows: the hub-bearing slice costs
  9,751 + 4,095×27.8 = 123,592 against a mean of 113,869 — an
  **8.5%** straggler, not 8×. Cut to 4,096 slices of 16 rows and the
  same hub gives 10,168 against 445, a **23×** straggler.

  So skew severity is a function of slice width. The thing that
  really kills SpMV parallelism is operand shape (Step 6), not
  degree skew.

  </details>

- [ ] You can explain the static answer and say what the flopcount pre-pass costs and what it returns.

  <details><summary>Answer</summary>

  `GB_AxB_saxpy3_flopcount.c:44-48`: O(nnz(B)+n) when A and M are
  not hypersparse, O(nnz(B)·log h) when they are. It walks patterns
  only, never values, and it prunes on the mask (`:57`) so an empty
  mask column skips the whole output column.

  It returns `Bflops`, the exact per-column flop vector, whose last
  cell is `total_flops` (`slice_balanced.c:308-309`). Everything
  downstream is derived: thread count via `GB_nthreads` at `:418`,
  `ntasks_initial` at `:419-420`, the Gustavson-vs-hash intensity
  test at `:432-438`, and `target_task_size = total_flops /
  ntasks_initial` at `:456` — **flops per task, not columns per
  task**.

  The pre-pass is itself parallel, with `schedule(dynamic,1)` at
  `:219-221` — the one dynamic OpenMP schedule in the whole path,
  and it is there precisely because at that moment the costs are
  what is not yet known.

  </details>

- [ ] You can explain coarse vs fine tasks and price their workspace.

  <details><summary>Answer</summary>

  `GB_AxB_saxpy3.c:22-27`: a coarse task owns a unique set of
  columns of B outright; a fine task joins a **team** computing one
  column, each member taking a range k1:k2 and summing into C(:,j)
  "via atomic operations". Four kinds (coarse/fine × Gustavson/hash)
  × 3 mask variants = 12 task types (`:30-38`). Coarse is preferred
  "since they require less synchronization" (`:42-43`).

  Workspace, from the table at `:66-70`: coarse Gustavson is
  `uint64_t Hf[m] + ctype Hx[m]` = 16 B/row **per task**; fine
  Gustavson is `int8_t Hf[m] + ctype Hx[m]` = 9 B/row **shared by
  the team**. At m = 1,048,576 with f64 values and 16 tasks, that is
  268 MB against 9.4 MB — a 28× memory inversion, paid for with
  atomics.

  That workspace total is what `slice_balanced.c:433` computes and
  `:437-438` tests against `nnz(A)+nnz(B)`.

  </details>

- [ ] You can explain work stealing from rayon's code, naming the four things `join_context` does and what it gives up.

  <details><summary>Answer</summary>

  From `rayon-core/src/join/mod.rs:115-173`: **push** job B onto the
  local deque (`:139`), **run** A inline on the calling thread
  (`:142`), then either **pop and run B inline** if nobody took it
  (`:154`, `:165`) or, if it was stolen, **execute other people's
  jobs** while waiting (`:168`) rather than blocking. `join` itself
  (`:93-106`) is a forwarder to `join_context` at `:105`.

  Backing it: one LIFO `Worker` deque plus one `Stealer` per thread
  (`registry.rs:248-257`), and a steal loop that picks a **random**
  starting victim (`:888`) then sweeps round-robin (`:889-891`).

  What it gives up: schedule determinism. What it demands: no
  blocking I/O inside a closure — rayon's own doc says that can
  deadlock (`join/mod.rs:76-84`).

  </details>

- [ ] You can say how many leaves rayon actually creates, and why the "splits all the way down" story is wrong.

  <details><summary>Answer</summary>

  It is **thief-splitting** (`src/iter/plumbing/mod.rs:246-284`).
  `Splitter::new()` sets `splits = current_num_threads()` (`:262`);
  each unstolen split halves the budget (`:277`) and stops at zero
  (`:281`); a **stolen** job resets the budget to the thread count
  (`:273`), which is the adaptive part.

  With 8 threads and no theft: 8 → 4 → 2 → 1 → 0, four successful
  splits per path, **2⁴ = 16 leaves** — 4,096 rows each over a
  65,536-row matrix. Not thousands of deque pushes; sixteen. The
  comment at `:252-254` says it directly: "the effective number of
  pieces will be `next_power_of_two()`".

  `min_len` defaults to 1 (`:78-80`) but its own doc says raising it
  "should not be needed" because rayon already adjusts split size
  (`:72-75`), and `LengthSplitter::try_split` is a conjunction
  (`:331`) — `with_min_len` can only make rayon split *less*.

  </details>

- [ ] You can say what `GB_nthreads` does with each of this topic's three measured SpGEMM workloads.

  <details><summary>Answer</summary>

  `Source/omp/include/GB_nthreads.h:27-31` is
  `clamp(floor(work/chunk), 1, nthreads_max)` with chunk = 65,536
  (`GB_defaults.h:24`). Against `notes.md:22-26`, assuming 8 cores:

  - scale 10, 298K flops → floor(4.5) = **4 threads** (half the
    machine, by design)
  - scale 12, 2.27M flops → floor(34.6) = 34, clamped to **8**
  - scale 14, 17.1M flops → floor(261) = 261, clamped to **8**

  The one-thread frontier is work < 2·chunk = 131,072 flops
  (`:17-18`), which at `notes.md:28-31`'s ~15 ns/flop is about
  **1.97 ms** of work before a second thread is allowed. From scale
  12 on, the thread count is pinned and all remaining tuning is task
  *sizing* at `slice_balanced.c:456`.

  </details>

- [ ] You can fill in the trade table, and name the one *measured* reason an SpMV-heavy kernel list will disappoint.

  <details><summary>Answer</summary>

  The table is in Step 6; the key rows are cost model (yes/no), leaf
  count (`GB_NTASKS_PER_THREAD × nthreads` vs
  `next_power_of_two(nthreads)`), and determinism (high vs none).

  The measured reason: Davis, CSC '20, §5 Table 2 (Xeon E5-2698 v4,
  40 threads vs 1) gives **BFS 2.6-3.9×** across four of five
  datasets while triangle counting reaches **11.2-30.5×** and
  4-Truss **13.4-27.7×**. §5: "Breadth-first search and Bellman-Ford
  both show modest parallelism; they both rely on a matrix-vector or
  vector-matrix multiply, which is harder to parallelize."

  The mechanism is `GB_AxB_saxpy3.c:44-47`: a matrix-vector multiply
  means B has one column, coarse tasks own whole columns (`:23`), so
  there is at most one coarse task — "the only way to get
  parallelism is via fine tasks", which sum into a shared
  accumulator with atomics (`:27`). Shape, not scheduler quality.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including your M20 kernel list with an expected speedup per kernel.

  <details><summary>Answer</summary>

  Four kernels, four decisions — axis, pre-pass, workspace owner —
  and one expected speedup each, taken from the CSC'20 row that
  matches the kernel's shape: SpMV and SpMSpV are single-vector, so
  the BFS row (2.6-3.9× at 40 threads); masked dot-SpGEMM and the
  `delta_mxm` fold have many output columns, so the
  triangle-counting row (11-30×).

  Be explicit about which numbers are yours and which are Davis's:
  the CSC'20 figures are 40-thread speedups on a 20-core Xeon and do
  **not** transfer to the M3 Pro in `notes.md`. Use them as a
  ranking of kernels, not as a prediction of your own wall-clock.

  </details>

## References

**Papers**

- Davis, T. A. — "Parallel GraphBLAS with OpenMP", CSC '20
  (SIAM Workshop on Combinatorial Scientific Computing). The
  citable source for every parallel-speedup claim in this chapter:
  §3 dates the parallel version ("Version 3.0.1 has been released
  (July 31, 2019), with exploitation of multi-threaded parallelism
  expressed through OpenMP"), §3.1 explains the
  one-task-per-thread slicing, §5 and Table 2 give the per-kernel
  40-thread speedups and the sentence explaining why BFS lags.
- Davis, T. A. — "Algorithm 1000: SuiteSparse:GraphBLAS", ACM TOMS
  45(4), Article 44, 2019, doi:10.1145/3322125. **Do not cite this
  one for parallelism**: it describes version 2.3.3, which §4.2.1
  and §7 state is "not yet multi-threaded" and "an efficient and
  highly optimized single-threaded implementation". Read in
  [reading-davis-toms19.md](reading-davis-toms19.md).

**Code**

- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  at `1fd5475` — `Source/mxm/GB_AxB_saxpy3.c:20-86` (the header
  comment is a scheduling essay and the best entry point),
  `Source/mxm/GB_AxB_saxpy3_slice_balanced.c:300-470`,
  `Source/mxm/GB_AxB_saxpy3_flopcount.c:40-69` and `:219-221`,
  `Source/omp/include/GB_nthreads.h`,
  `Source/include/GB_defaults.h:24`. Full anchor table above.
- [rayon](https://github.com/rayon-rs/rayon) at `6d9e94b` —
  `rayon-core/src/join/mod.rs` (186 lines; `join` `:93`,
  `join_context` `:115-173`, the push at `:139`),
  `rayon-core/src/registry.rs:248-257` and `:875-905`,
  `src/iter/plumbing/mod.rs:246-333` (thief-splitting — read this
  before believing anything about how far rayon splits),
  `src/iter/mod.rs:3152-3176`.

**Measured, in this repo**

- `topics/20-graphblas/notes.md:22-31` — SpGEMM flop counts at
  scales 10/12/14 and the ~15 ns/flop rate. Step 5's `GB_nthreads`
  arithmetic runs on them.
- `topics/24-graph-algorithms/notes.md:5-7` — RMAT scale 16 degree
  distribution. Step 1's skew arithmetic runs on it.
