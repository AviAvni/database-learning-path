# Vectorized in Rust: polars-stream morsels and DataFusion streams

Two Rust codebases answering the same design questions DuckDB answered
in C++ — and the closest templates for M11's runtime. polars-stream
makes morsels a first-class type driven by an async graph; DataFusion
keeps Volcano's shape with a vector payload and async clothes. Before
the code, this chapter builds the five design decisions the two systems
embody — the batch contract, async scheduling, SIMD kernel shape, static
vs dynamic parallelism, and the group-by-as-arrays pattern — then maps
each to its file:line.

Anchors are polars at `f8bcc3d` and datafusion at `1e77af8`, the commits
this repo pins (`resources/codebases.md`; confirm with
`tools/pinned-source.py ref polars`). Quoted Rust carries its real line
numbers; elisions are marked. Where a batch size or a default could be
misremembered, the constant is quoted rather than asserted.

## The problem in one sentence

A vectorized engine in Rust must decide four things — what the batch
type carries, who schedules operators (async runtime or hand-rolled
state machine), how work splits across cores, and what a kernel looks
like under nulls and SIMD — and these two codebases picked differently
on almost every axis, which is exactly what makes reading both worth it.

## The concepts, step by step

### Step 1 — the batch contract: what travels between operators

> **In:** three engines that agree on the big decision — move data in
> batches, not tuples — and therefore look interchangeable from a
> distance.
> **Out:** the three *batch types*, read side by side, which turn out to
> disagree about size by a factor of fifty and about what a batch carries
> at all. What the batch carries is what the scheduler does not have to
> reconstruct later.

A **batch** is a fixed-capacity set of column arrays plus a row count —
DuckDB's `DataChunk`, seen in
[reading-duckdb-execution.md](reading-duckdb-execution.md). All three
systems move batches. They differ on two axes, and both are worth reading
out of the source rather than remembering:

```
 system        batch type      default size   where the size lives
 DuckDB        DataChunk       2048           compile-time #define
 polars        Morsel          100,000        runtime config, env-overridable
 DataFusion    RecordBatch     8192           runtime config, session-settable
```

```rust
// crates/polars-config/src/lib.rs — polars' default, 33-35.
    33  const IDEAL_MORSEL_SIZE: &str = "POLARS_IDEAL_MORSEL_SIZE";
    34  const STREAMING_CHUNK_SIZE: &str = "POLARS_STREAMING_CHUNK_SIZE"; // Backwards compatibility.
    35  const DEFAULT_IDEAL_MORSEL_SIZE: u64 = 100_000;
```

```rust
// datafusion/common/src/config.rs — DataFusion's default, 733.
   733          pub batch_size: ConfigNonZeroUsize, default = non_zero_usize_default(8192)
```

Both are *runtime* knobs where DuckDB's is a `#define`
(`vector_size.hpp:16`) — the first real design difference, and it follows
from the second: a compile-time size lets DuckDB size stack buffers and
unroll to it; a runtime one lets polars pick a morsel size from the data.

Now put the sizes on the same ruler. At eight 8-byte columns, a batch
costs 64 B/row:

```
 DuckDB      2048 rows × 64 B =   128 KB
 DataFusion  8192 rows × 64 B =   512 KB
 polars    100000 rows × 64 B = 6.10 MB
```

Against this machine's measured ladder
([topics/00-performance-toolbox/notes.md](../00-performance-toolbox/notes.md)):
128 KB is exactly where the L1d plateau ends (1.02 ns), 512 KB reads
5.3-5.8 ns, 4-8 MB reads 7.6-9.0 ns. Only DuckDB's batch is an L1
residency bet. DataFusion's and polars' are L2 bets — which is coherent,
because they are not the same *kind* of unit. DuckDB's 2048 is a **kernel**
grain: the array a `for` loop runs over. polars' 100,000 is a
**scheduling** grain: the quantum of work a thread claims before going
back for more (see [reading-morsel-parallelism.md](reading-morsel-parallelism.md),
where the paper's own recommendation is checked). Comparing them as if
they were the same number is the classic mistake with this table.

### Step 2 — polars-stream: the morsel as a first-class type

> **In:** the morsel-driven idea from the SIGMOD'14 paper, where "a
> morsel" is a size and an informal convention, and everything the
> scheduler needs to know about a unit of work is implicit.
> **Out:** a `Morsel` *struct* whose four fields each make one of those
> implicit things explicit — the data, its order, who produced it, and
> when it was consumed — plus the graph and executor that move them.

polars-stream is the streaming executor behind `.lazy().collect()`, and
it promotes the work unit to a type:

```rust
// crates/polars-stream/src/morsel.rs — the type, 81-95.
    81  #[derive(Debug, Clone)]
    82  pub struct Morsel {
    83      /// The data contained in this morsel.
    84      df: DataFrame,
    85  
    86      /// The sequence number of this morsel. May only stay equal or increase
    87      /// within a pipeline.
    88      seq: MorselSeq,
    89  
    90      /// A token that indicates which source this morsel originates from.
    91      source_token: SourceToken,
    92  
    93      /// Used to notify someone when this morsel is consumed, to provide backpressure.
    94      consume_token: Option<WaitToken>,
    95  }
```

Four fields, not the three this guide used to list, and the fourth is the
one that matters most for flow control. Take them in turn.

**`MorselSeq` — order, written down.** Parallel workers pull morsels and
finish them out of order; an ORDER BY or a LIMIT has to put them back.
DuckDB reconstructs this from batch indices; polars carries it:

```rust
// crates/polars-stream/src/morsel.rs — the sequence number, 15-21.
    15  /// A token indicating the order of morsels in a stream.
    16  ///
    17  /// The sequence tokens going through a pipe are monotonely non-decreasing and are allowed to be
    18  /// discontinuous. Consequently, `1 -> 1 -> 2` and `1 -> 3 -> 5` are valid streams of sequence
    19  /// tokens.
    20  #[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Default)]
    21  pub struct MorselSeq(u64);
```

Read 17-19 carefully: the guarantee is *monotonely non-decreasing and
allowed to be discontinuous*, so `1 → 3 → 5` is legal. That is weaker
than "consecutive", which is what makes it cheap — a source that splits or
drops morsels does not have to renumber anything. `MorselSeq::new`
(`:26-28`) multiplies by two, reserving the low bit for a future
"last morsel with this sequence number" flag (`:24-25`, `:32-33`) —
topic 2's bit-smuggling again, pre-allocated.

**`SourceToken` — a stop request, not backpressure.**

```rust
// crates/polars-stream/src/morsel.rs — the token, 51-57, and what it does, 72-78.
    51  /// A token indicating which source this morsel originated from, and a way to
    52  /// pass information/signals to it. Currently it's only used to request a source
    53  /// to stop with passing new morsels this execution phase.
    54  #[derive(Clone, Debug)]
    55  pub struct SourceToken {
    56      stop: Arc<RelaxedCell<bool>>,
    57  }
// ... 58-71: Default and new() ...
    72      pub fn stop(&self) {
    73          self.stop.store(true);
    74      }
    75  
    76      pub fn stop_requested(&self) -> bool {
    77          self.stop.load()
    78      }
```

The doc comment (51-53) is explicit that this is "only used to request a
source to stop", which is the LIMIT case: a sink that has enough rows sets
the flag and the scan notices. **Backpressure** — slowing a fast producer
so a slow consumer's queue stays bounded — is the *other* token, the
`consume_token: Option<WaitToken>` on line 94: a producer that wants to
throttle waits on it and is woken when the morsel is actually consumed.
This guide previously attributed backpressure to `SourceToken`; the fields
are separate because the two mechanisms are.

**The size is a config knob, not a constant** (`:11-13`, resolving to the
`100_000` of Step 1) — contrast DuckDB's `#define`.

The plan is an explicit graph:

```rust
// crates/polars-stream/src/graph.rs — the graph, 16-24, and a node, 163-169.
    16  /// Represents the compute graph.
    17  ///
    18  /// The `nodes` perform computation and the `pipes` form the connections between nodes
    19  /// that data is sent through.
    20  #[derive(Default)]
    21  pub struct Graph {
    22      pub nodes: SlotMap<GraphNodeKey, GraphNode>,
    23      pub pipes: SlotMap<LogicalPipeKey, LogicalPipe>,
    24  }
// ... 25-162: add_node, port wiring, state update ...
   163  /// A node in the graph represents a computation performed on the stream of morsels
   164  /// that flow through it.
   165  pub struct GraphNode {
   166      pub compute: Box<dyn ComputeNode>,
   167      pub inputs: Vec<LogicalPipeKey>,
   168      pub outputs: Vec<LogicalPipeKey>,
   169  }
```

Nodes implement `ComputeNode`, and the trait is where polars' answer to
DuckDB's `OperatorResultType` lives — not a return code, but two methods:

```rust
// crates/polars-stream/src/nodes/mod.rs — the operator contract, 99-114.
    99      /// If this node (in its current state) is a pipeline blocker, and whether
   100      /// this is memory intensive or not.
   101      fn is_memory_intensive_pipeline_blocker(&self) -> bool {
   102          false
   103      }
   104  
   105      /// Spawn the tasks that this compute node needs to receive input(s),
   106      /// process it and send to its output(s). Called once per execution phase.
   107      fn spawn<'env, 's>(
   108          &'env mut self,
   109          scope: &'s TaskScope<'s, 'env>,
   110          recv_ports: &mut [Option<RecvPort<'_>>],
   111          send_ports: &mut [Option<SendPort<'_>>],
   112          state: &'s StreamingExecutionState,
   113          join_handles: &mut Vec<JoinHandle<PolarsResult<()>>>,
   114      );
```

`update_state` (`:92-97`) negotiates port readiness before a phase;
`spawn` (105-114) then creates **async tasks** — cooperatively-scheduled
functions that yield at `.await` points rather than blocking a thread —
one set per execution phase. `is_memory_intensive_pipeline_blocker`
(99-103) is polars' name for DuckDB's pipeline breaker.

And the execution is *phased*: `execute_graph` (`execute.rs:301`) loops —
update all port states, find a runnable subgraph
(`find_runnable_subgraph`, `:106`), run it, repeat until nothing is
runnable (`:328-360`). The graph is not run once; it is run in waves,
which is how a breaker's downstream gets to start only after it finishes.

**One correction worth making loudly.** It is tempting to say pipeline
parallelism here "falls out of the async runtime rather than a hand-rolled
scheduler". It does not. polars wrote its own work-stealing executor:

```rust
// crates/polars-async/src/executor/mod.rs — the scheduler polars wrote, 236-240.
   236      fn try_steal_task<R: Rng>(&self, thread: usize, rng: &mut R) -> Option<ReadyTask> {
   237          // Try to get a global task.
   238          loop {
   239              match self.global_high_prio_task_queue.steal() {
   240                  Steal::Empty => break,
```

`try_steal_task` (236) drains a global high-priority queue, then a
low-priority one, then steals from a randomly chosen sibling thread
(`:254-265`), then parks with one last steal attempt before sleeping
(`:319-321`). tokio appears in `polars-stream` only with the `sync`
feature (`Cargo.toml:32`) and as the runtime for blocking I/O
(`ASYNC.block_in_place_on`, `execute.rs:339`). So `async` here buys the
*task representation* — a suspended operator is a state machine the
compiler wrote — while the scheduling is as hand-rolled as DuckDB's.
What async genuinely buys is that a blocking source integrates for free;
what it costs is poll overhead and fuzzier buffer ownership (question 1).

### Step 3 — what a SIMD kernel actually looks like

> **In:** a column of floats with a null mask, and an ambition to add
> them up at memory speed.
> **Out:** the three things a production kernel does about it — SIMD
> lanes instead of a scalar loop, a select instead of a branch for nulls,
> and a recursion that both breaks the dependency chain and bounds the
> floating-point error.

polars-compute's float sum is small enough to read whole and shows all
three. Two constants set the shape:

```rust
// crates/polars-compute/src/float_sum.rs — the two shape constants, 13-14.
    13  const STRIPE: usize = 16;
    14  const PAIRWISE_RECURSION_LIMIT: usize = 128;
```

**Masked variants, not branches.** Every kernel comes in a pair, because
a column carries a **null mask** — a bitmask marking which values are
missing:

```rust
// crates/polars-compute/src/float_sum.rs — the pair, 65-69, and the masked
// implementation, 87-98.
    65  // As a trait to not proliferate SIMD bounds.
    66  pub trait SumBlock<F> {
    67      fn sum_block_vectorized(&self) -> F;
    68      fn sum_block_vectorized_with_mask(&self, mask: BitMask<'_>) -> F;
    69  }
// ... 70-86: the impl header and the unmasked variant ...
    87      fn sum_block_vectorized_with_mask(&self, mask: BitMask<'_>) -> F {
    88          let zero = Simd::default();
    89          let vsum = self
    90              .chunks_exact(STRIPE)
    91              .enumerate()
    92              .map(|(i, a)| {
    93                  let m: Mask<T::Mask, STRIPE> = mask.get_simd(i * STRIPE);
    94                  m.select(Simd::from_slice(a).cast_generic::<F>(), zero)
    95              })
    96              .sum::<Simd<F, STRIPE>>();
    97          vector_horizontal_sum(vsum)
    98      }
```

Line 94 is the whole trick: `m.select(values, zero)` blends per lane —
a null contributes `0.0` and the sum is unchanged — so there is no branch
for the predictor to miss when nulls are scattered.
[FINDINGS.md](../../FINDINGS.md) row 17 puts the scale on what that
avoids: 0.95 GB/s branchy against ~10 GB/s branchless on the same data.
This masked/unmasked pairing is the columnar counterpart of DuckDB's
selection vectors — same problem, different representation.

**Lanes, and the dependency chain they leave behind.** Work the block out:

```
 block                = PAIRWISE_RECURSION_LIMIT      = 128 elements
 lanes                = STRIPE                        =  16
 chunks per block     = 128 / 16                      =   8 SIMD adds
 those 8 adds are a *dependent* chain into one accumulator (83, 96)
 at an assumed 3-4 cycle FP-add latency: 24-32 cycles per 128 elements
                                       = 4.0-5.3 elements/cycle
 a scalar loop, same latency, chain of 128:            0.25-0.33 el/cycle
```

So the 16 lanes are not worth 16× on their own — a single accumulator
turns them into roughly 4-5 elements per cycle, because each add waits for
the previous one. The independence has to come from somewhere else, and it
comes from the recursion:

```rust
// crates/polars-compute/src/float_sum.rs — the recursion, 205-210.
   205      unsafe {
   206          let blocks = f.len() / PAIRWISE_RECURSION_LIMIT;
   207          let left_len = (blocks / 2) * PAIRWISE_RECURSION_LIMIT;
   208          let (left, right) = (f.get_unchecked(..left_len), f.get_unchecked(left_len..));
   209          pairwise_sum(left) + pairwise_sum(right)
   210      }
```

`pairwise_sum` (`:189`) splits the slice in half down to 128-element
blocks and adds the halves (209). The two subtree sums are independent, so
the machine can have several accumulator chains in flight at once — this
is topic 0's memory-level-parallelism lesson applied to arithmetic ports.

But note *why* the code is shaped that way, because the guide used to get
this backwards: pairwise summation is a **numerical** technique first. Its
error bound grows as O(log n) where a naive running sum grows as O(n); the
instruction-level parallelism is a side effect the author got for free.
The same honesty applies to the reduce:

```rust
// crates/polars-compute/src/float_sum.rs — the final reduce, 44-63.
    44  fn vector_horizontal_sum<V, T>(mut v: V) -> T
    45  where
    46      V: IndexMut<usize, Output = T>,
    47      T: Add<T, Output = T> + Sized + Copy,
    48  {
    49      // We have to be careful about this reduction, floating
    50      // point math is NOT associative so we have to write this
    51      // in a form that maps to good shuffle instructions.
    52      // We fold the vector onto itself, halved, until we are down to
    53      // four elements which we add in a shuffle-friendly way.
    54      let mut width = STRIPE;
    55      while width > 4 {
    56          for j in 0..width / 2 {
    57              v[j] = v[j] + v[width / 2 + j];
    58          }
    59          width /= 2;
    60      }
    61  
    62      (v[0] + v[2]) + (v[1] + v[3])
    63  }
```

The comment at 49-53 says it outright: float addition is not associative,
so the fold order is chosen to "map to good shuffle instructions" *and*
to be a defensible order. The consequence is one every engine documents
away — a vectorized sum does not equal the sequential sum bit for bit.

### Step 4 — DataFusion: Volcano's shape, async clothes, static partitions

> **In:** the iterator model, which DataFusion had no reason to abandon —
> its shape is a good fit for a composable plan.
> **Out:** the same shape with two substitutions: the payload becomes an
> Arrow `RecordBatch` (so `next()` is amortized), and `next()` becomes an
> async `poll_next` (so a blocking source does not block a thread). The
> parallelism, though, is decided statically, which is the axis on which
> DataFusion differs from both others.

```rust
// datafusion/physical-plan/src/execution_plan.rs — the trait, 97, and the
// one method that is the whole model, 478-482.
    97  pub trait ExecutionPlan: Any + Debug + DisplayAs + Send + Sync {
// ... 98-477: name, properties, children, doc examples ...
   478      fn execute(
   479          &self,
   480          partition: usize,
   481          context: Arc<TaskContext>,
   482      ) -> Result<SendableRecordBatchStream>;
```

Read the signature as Volcano with two words changed: `execute` is
`open()`, and the returned stream's `poll_next` is `next()`. What changed
is the payload — a `RecordBatch` of 8192 rows by default (Step 1) — and
so the per-call cost is amortized over 8192 rows exactly as DuckDB's is
over 2048:

```
 one poll per 8192 rows, at an assumed 100 ns per poll
   (async wake, stream plumbing, an Arrow batch handoff)
 per-row share = 100 ns / 8192                     = 0.012 ns/row
 compare tuple-at-a-time dispatch, from the postgres guide: 25 ns/row
```

Same conclusion as everywhere else in this topic: batching kills the
dispatch term, whatever the dispatch mechanism happens to be. This is why
"is it async?" is the wrong question to argue about.

The interesting difference is the first argument. `execute(partition, ctx)`
is **static partitioning**: the plan declares N output partitions, the
runtime calls `execute(0..N)` and spawns one task each, and a task owns its
partition to the end. That is the thing morsel-driven scheduling exists to
avoid. N defaults to the machine's parallelism
(`target_partitions`, `datafusion/common/src/config.rs:768`), and skew is
patched *inside* the plan by `RepartitionExec`
(`datafusion/physical-plan/src/repartition/mod.rs:1150`), inserted by the
optimizer under the `repartition_joins` / `repartition_aggregations` flags
(`config.rs:1443`, `:1455`, both default `true`).

Worked, so the cost of static is concrete. Take the 50 M-row lane and
eight workers:

```
 static, balanced:   8 partitions × 6.25 M rows → wall clock = 6.25 M rows
 static, one partition holds 3× the mean:
      that partition has 18.75 M rows → wall clock = 18.75 M = 3.0× worse
 morsel-driven, DuckDB's 122,880-row grain (see the DuckDB guide):
      408 units over 8 threads = 51 each; a straggler costs at most
      one unfinished morsel = 122,880 rows = 0.25% of a thread's share
```

Static partitioning is not a mistake — it removes a scheduler, and a
`RepartitionExec` fixes the common cases — but its worst case is a
multiple, and morsel-driven's is a rounding error.

### Step 5 — group-by as array arithmetic: intern, then index flat states

> **In:** a `GROUP BY` with several aggregates, and the obvious
> implementation — a hash map from key to a little struct of running
> totals, probed once per aggregate per row.
> **Out:** DataFusion's shape, which probes once per *row* no matter how
> many aggregates there are, and keeps every aggregate's state in a flat
> `Vec` indexed by a dense integer.

The move is **interning**: map each group key to a dense integer group id
— 0, 1, 2, … in first-seen order — and then never touch the key again.

```rust
// datafusion/physical-plan/src/aggregates/group_values/mod.rs — the contract,
// 85-100.
    85  /// # Group Ids
    86  ///
    87  /// Each distinct group in a hash aggregation is identified by a unique group id
    88  /// (usize) which is assigned by instances of this trait. Group ids are
    89  /// continuous without gaps, starting from 0.
    90  pub trait GroupValues: Send {
    91      /// Calculates the group id for each input row of `cols`, assigning new
    92      /// group ids as necessary.
    93      ///
    94      /// When the function returns, `groups`  must contain the group id for each
    95      /// row in `cols`.
    96      ///
    97      /// If a row has the same value as a previous row, the same group id is
    98      /// assigned. If a row has a new value, the next available group id is
    99      /// assigned.
   100      fn intern(&mut self, cols: &[ArrayRef], groups: &mut Vec<usize>) -> Result<()>;
```

"Continuous without gaps, starting from 0" (88-89) is the load-bearing
sentence: it is what lets a group id be an *array index* rather than a map
key. The per-batch loop then interns once and hands the same
`group_indices` to every accumulator:

```rust
// datafusion/physical-plan/src/aggregates/grouped_hash_stream.rs — inside
// group_aggregate_batch (845): intern once, 884-888 …
   884              // calculate the group indices for each input row
   885              let starting_num_groups = self.group_values.len();
   886              self.group_values
   887                  .intern(group_values, &mut self.current_group_indices)?;
   888              let group_indices = &self.current_group_indices;
// ... 889-912: ordering bookkeeping and metrics ...
   913              for ((acc, values), opt_filter) in t {
   914                  let opt_filter = opt_filter.as_ref().map(|filter| filter.as_boolean());
   915  
   916                  // Call the appropriate method on each aggregator with
   917                  // the entire input row and the relevant group indexes
   918                  if self.mode.input_mode() == AggregateInputMode::Raw
   919                      && !self.spill_state.is_stream_merging
   920                  {
   921                      acc.update_batch(
   922                          values,
   923                          group_indices,
   924                          opt_filter,
   925                          total_num_groups,
   926                      )?;
```

And the accumulator's state really is a flat array:

```rust
// datafusion/functions-aggregate-common/src/aggregate/groups_accumulator/prim_op.rs
// — the state, 46-47, and the update, 99-113.
    46      /// values per group, stored as the native type
    47      values: Vec<T::Native>,
// ... 48-98: the other fields, constructors, and the update_batch header ...
    99          // update values
   100          self.values.resize(total_num_groups, self.starting_value);
   101  
   102          // NullState dispatches / handles tracking nulls and groups that saw no values
   103          self.null_state.accumulate(
   104              group_indices,
   105              values,
   106              opt_filter,
   107              total_num_groups,
   108              |group_index, new_value| {
   109                  // SAFETY: group_index is guaranteed to be in bounds
   110                  let value = unsafe { self.values.get_unchecked_mut(group_index) };
   111                  (self.prim_fn)(value, new_value);
   112              },
   113          );
```

`values: Vec<T::Native>` (47), grown to `total_num_groups` (100), written
through a raw index (110-111). No per-group heap object exists to chase,
so the aggregate's state is as SIMD-friendly and prefetchable as the input.

Worked, for the second win — sharing the probe. Take 50 M rows, four
aggregates, and a group count large enough that the group table does not
fit in cache, so each probe costs about one miss at the ~25 ns this
machine measures for its DRAM plateau (topic 0 notes):

```
 probe per aggregate:  50e6 × 4 = 200,000,000 probes × 25 ns = 5.00 s
 intern once per row:  50e6 × 1 =  50,000,000 probes × 25 ns = 1.25 s
 saved                                                        = 3.75 s
```

That is an *upper* bound — real probes hit in cache some of the time, and
the interned path still pays four sequential array writes per row — but
the shape of the saving is right, and it grows linearly with the number of
aggregates, which is why the pattern is universal.

In the shape M11 will want it:

```rust
// ILLUSTRATION — not quoted from datafusion. The real loop is
// grouped_hash_stream.rs:884-926 (intern, then one update_batch per
// accumulator) and prim_op.rs:99-113 (the flat-array update). This
// collapses both into one function to show the data flow.
fn update_batch(&mut self, keys: &Column, vals: &[i64]) {
    let gids = self.group_values.intern(keys); // ONE HT probe per row,
    for (i, &g) in gids.iter().enumerate() {   // shared by all aggregates
        self.sums[g] += vals[i];               // states are flat arrays
        self.counts[g] += 1;                   // indexed by group id —
    }                                          // no per-group heap objects
}
```

### Step 6 — the comparison that matters

> **In:** five design decisions read one system at a time, which is how
> you learn them and not how you use them.
> **Out:** the three systems on one page, with the axis each one bet
> differently on — and, for each row, the cost that came with the win, so
> that M11's fourth column is chosen rather than copied.

| | DuckDB | polars-stream | DataFusion |
|---|---|---|---|
| batch type | `DataChunk` | `Morsel` | Arrow `RecordBatch` |
| default size | 2048, compile-time | 100,000, config | 8192, config |
| parallelism | morsel pull | morsel pull, async graph | static partitions + `RepartitionExec` |
| scheduling | own task scheduler | own work-stealing executor over async tasks | tokio |
| ordering | implicit (batch index) | explicit `MorselSeq` | stream contract |
| flow control | `OperatorResultType` return codes | `SourceToken` (stop) + `WaitToken` (backpressure) | async backpressure via `poll_next` |
| operator state | executor-owned chunks | node-owned, per phase | stream-owned |

No row has a free winner. Morsel pulling beats static partitions on skew
(Step 4's 3.0× against 0.25%) but you must write the scheduler — polars
and DuckDB both did. Async tasks integrate blocking sources for free and
cost poll overhead plus fuzzier buffer ownership. An explicit `MorselSeq`
costs eight bytes per batch and buys ordered sinks without a global sort.
M11 has to fill in a fourth column, which is the point of reading all
three.

## Where each step lives in the code

Read polars first (the morsel type is the topic's vocabulary made
concrete), then DataFusion's aggregate (the pattern you will copy).
Anchors are polars `f8bcc3d`, datafusion `1e77af8`.

| File | Lines | What is there | Step |
|---|---|---|---|
| `crates/polars-config/src/lib.rs` | 33-35 | `DEFAULT_IDEAL_MORSEL_SIZE = 100_000`, and its env var | 1 |
| `datafusion/common/src/config.rs` | 733 | `batch_size` default 8192 | 1 |
| `crates/polars-stream/src/morsel.rs` | 11-13 | `get_ideal_morsel_size` — a config read, not a constant | 1, 2 |
| `crates/polars-stream/src/morsel.rs` | 15-21, 24-35 | `MorselSeq`, non-decreasing and discontinuous; the reserved low bit | 2 |
| `crates/polars-stream/src/morsel.rs` | 51-57, 72-78 | `SourceToken` — a *stop* request, not backpressure | 2 |
| `crates/polars-stream/src/morsel.rs` | 81-95 | `Morsel` — four fields; `consume_token` (94) is the backpressure one | 2 |
| `crates/polars-stream/src/graph.rs` | 16-24, 163-169, 171-185 | `Graph`, `GraphNode`, `LogicalPipe` | 2 |
| `crates/polars-stream/src/nodes/mod.rs` | 92-97, 99-114 | `ComputeNode::update_state` and `spawn` — the operator contract | 2 |
| `crates/polars-stream/src/execute.rs` | 106, 301, 328-360 | `find_runnable_subgraph`; `execute_graph`; the phase loop | 2 |
| `crates/polars-async/src/executor/mod.rs` | 236-265, 313-321 | the work-stealing scheduler polars wrote itself | 2 |
| `crates/polars-compute/src/float_sum.rs` | 13-14 | `STRIPE = 16`, `PAIRWISE_RECURSION_LIMIT = 128` | 3 |
| `crates/polars-compute/src/float_sum.rs` | 44-63 | `vector_horizontal_sum` — and why the fold order is chosen | 3 |
| `crates/polars-compute/src/float_sum.rs` | 65-69, 87-98 | `SumBlock`; the masked variant selects into lanes (94) | 3 |
| `crates/polars-compute/src/float_sum.rs` | 189-211 | `pairwise_sum` — accuracy first, ILP as a bonus | 3 |
| `datafusion/physical-plan/src/execution_plan.rs` | 97, 478-482 | `trait ExecutionPlan`; `execute(partition, ctx)` | 4 |
| `datafusion/common/src/config.rs` | 768, 1443, 1455 | `target_partitions`; the repartition flags | 4 |
| `datafusion/physical-plan/src/repartition/mod.rs` | 1150 | `RepartitionExec` — the skew patch | 4 |
| `.../aggregates/group_values/mod.rs` | 85-100 | `GroupValues::intern`; ids dense from 0 | 5 |
| `.../aggregates/grouped_hash_stream.rs` | 275, 641, 845, 884-926 | the stream; `poll_next`; `group_aggregate_batch`; intern once, then every accumulator | 5 |
| `.../groups_accumulator/prim_op.rs` | 46-47, 99-113 | `values: Vec<T::Native>` — flat state indexed by group id | 5 |

## Takeaway

The three engines agree completely on the decision that matters — batch,
don't tuple — and the arithmetic shows why they can afford to disagree
about everything else: at 2048, 8192 or 100,000 rows per call the dispatch
term is 0.01 ns/row and no longer participates in the argument. What is
left to design is what the batch *carries* (order, stop signals,
backpressure), who hands batches to whom (own scheduler vs static
partitions), and what the innermost loop looks like (lanes, selects, and
a recursion that is really about float error).

Two of this guide's earlier claims did not survive reading the code:
polars-stream does *not* get its parallelism from an off-the-shelf async
runtime — it ships its own work-stealing executor — and `float_sum`'s
independent accumulators come from a *pairwise* recursion whose first
purpose is numerical accuracy. Both are the kind of thing that reads
plausibly and is wrong, which is the argument for the file:line habit.

## Questions for notes.md

1. Async operators (polars/DF) vs hand-rolled state machines (DuckDB's
   OperatorResultType): what does async buy (blocking sources) and cost
   (poll overhead, buffer ownership)? Which fits M11 — remember topic 7's
   one-threadpool decision. Note that polars pays for *both*: async tasks
   **and** its own scheduler.
2. MorselSeq: which graph query results are order-sensitive? (ORDER BY
   obviously — anything else in Cypher? LIMIT without ORDER BY?)
3. The masked-kernel pattern: your batches will have selection vectors
   instead of null masks. Same trick? When does select-in-lanes beat
   compact-then-compute? (Selectivity threshold — guess, then bench.)
4. DataFusion interning group keys per batch: why is
   hash-once-then-index cheaper than hashing per aggregate? Count the
   HT probes for 4 aggregates either way.
5. M11: FalkorDB's Expand does one GraphBLAS SpMV per batch — which of
   the three systems' operator contracts fits "one call produces a
   whole matrix of results" best?

## Done when

Answer each before unfolding it.

- [ ] You can name the batch unit and parallelism strategy of all three systems without the table, and say why comparing 2048 to 100,000 directly is a category error.

  <details><summary>Answer</summary>

  DuckDB: `DataChunk`, 2048 rows fixed at compile time
  (`vector_size.hpp:16`), morsel-pull parallelism with its own scheduler.
  polars-stream: `Morsel`, default 100,000 rows from config
  (`polars-config/src/lib.rs:35`), morsel-pull over an async graph on its
  own work-stealing executor. DataFusion: Arrow `RecordBatch`, default
  8192 (`config.rs:733`), static partition-per-stream with
  `RepartitionExec` for skew.

  The category error is that 2048 is a *kernel* grain — the array length a
  `for` loop runs over, chosen so eight 8-byte columns are 128 KB and stay
  in L1 between operators — while 100,000 is a *scheduling* grain, the
  quantum a thread claims before going back for more work. They are
  answers to different questions; only DataFusion's 8192 is directly
  comparable to 2048.

  </details>

- [ ] You can say what each of `Morsel`'s four fields is for, and which one actually implements backpressure.

  <details><summary>Answer</summary>

  From `morsel.rs:82-95`: `df` is the data; `seq: MorselSeq` is the
  ordering token, guaranteed monotonely non-decreasing but explicitly
  allowed to be discontinuous (15-19), so order-sensitive sinks can
  reassemble and order-insensitive ones can ignore it; `source_token:
  SourceToken` identifies the producing source and carries a *stop*
  request (51-53, 72-78) — the LIMIT case; and `consume_token:
  Option<WaitToken>` (93-94) is the backpressure mechanism, notifying a
  waiting producer only once the morsel has actually been consumed.

  Stop and backpressure are different fields because they are different
  mechanisms: one ends production, the other paces it.

  </details>

- [ ] You can explain why a masked SIMD kernel selects rather than branches, and why 16 lanes do not give 16×.

  <details><summary>Answer</summary>

  It selects because a branch on a scattered null mask is unpredictable:
  `m.select(values, zero)` (`float_sum.rs:94`) makes a null contribute
  `0.0` with no control flow at all. FINDINGS row 17 measures the
  alternative at 0.95 GB/s branchy against ~10 GB/s branchless.

  16 lanes do not give 16× because a 128-element block
  (`PAIRWISE_RECURSION_LIMIT`) is 8 chunks of `STRIPE = 16` summed into
  *one* accumulator (83, 96) — a dependent chain of 8 FP adds. At 3-4
  cycles of add latency that is 24-32 cycles per 128 elements, about 4-5
  elements per cycle. The independent chains come from `pairwise_sum`
  (189-211) splitting the input recursively, and that recursion exists
  primarily to bound floating-point error at O(log n) rather than O(n) —
  the instruction-level parallelism is a bonus.

  </details>

- [ ] You can describe the intern-then-flat-arrays group-by in two sentences, and count the probes it saves.

  <details><summary>Answer</summary>

  Per input batch, DataFusion interns the group keys once — one hash probe
  per row mapping each key to a dense group id, contiguous from 0
  (`group_values/mod.rs:88-100`) — and then hands the same
  `group_indices` slice to every accumulator
  (`grouped_hash_stream.rs:884-926`). Each accumulator's state is a flat
  `Vec` indexed by that id (`prim_op.rs:47`, written at 110-111), so there
  is no per-group heap object and the update is array arithmetic.

  With four aggregates over 50 M rows, probing per aggregate is 200 M
  probes against 50 M — and if the group table exceeds cache so each probe
  costs about one ~25 ns miss, that is 5.00 s against 1.25 s, an upper
  bound of 3.75 s saved. The saving scales with the number of aggregates,
  which is why the pattern is universal.

  </details>

- [ ] You can state static partitioning's worst case against morsel pulling's, on numbers.

  <details><summary>Answer</summary>

  DataFusion's `execute(partition, ctx)`
  (`execution_plan.rs:478-482`) gives a task one partition for the whole
  query, with N defaulting to the machine's parallelism
  (`config.rs:768`). If the work splits evenly across 8 partitions, 50 M
  rows is 6.25 M each. If one partition holds 3× the mean it holds
  18.75 M, and since the query ends when the slowest partition does, the
  wall clock is 3.0× the balanced case.

  Morsel pulling bounds the same imbalance by one morsel. At DuckDB's
  122,880-row grain the 50 M-row lane is 408 units, 51 per thread, so a
  straggler costs at most one unfinished morsel — 0.25% of a thread's
  share. DataFusion patches the gap inside the plan instead, with
  `RepartitionExec` (`repartition/mod.rs:1150`) inserted under the
  `repartition_joins` / `repartition_aggregations` flags (`config.rs:1443`,
  `:1455`).

  </details>

## References

**Code**
- [polars](https://github.com/pola-rs/polars) at `f8bcc3d` —
  `crates/polars-stream/src/` (`morsel.rs`, `graph.rs`, `nodes/mod.rs`,
  `execute.rs`), `crates/polars-async/src/executor/mod.rs` for the
  scheduler, and `crates/polars-compute/src/float_sum.rs` for what a SIMD
  kernel actually looks like
- [datafusion](https://github.com/apache/datafusion) at `1e77af8` —
  `datafusion/physical-plan/src/execution_plan.rs` (the trait) and
  `aggregates/` (`grouped_hash_stream.rs`, `group_values/`) — the
  engine's heart; ~1.5 h for both

**In this repo**
- [reading-duckdb-execution.md](reading-duckdb-execution.md) — the C++
  system these two are answering, and where 2048 comes from
- [reading-morsel-parallelism.md](reading-morsel-parallelism.md) — the
  paper polars' `Morsel` type is a transcription of
- [FINDINGS.md](../../FINDINGS.md) row 17 — branchy vs branchless
  throughput, the number behind Step 3's select-don't-branch
- [topics/00-performance-toolbox/notes.md](../00-performance-toolbox/notes.md)
  — the measured cache and latency ladder every size argument is checked
  against
