# Vectorized in Rust: polars-stream morsels and DataFusion streams

Two Rust codebases answering the same design questions DuckDB answered
in C++ — and the closest templates for M11's runtime. polars-stream
makes morsels a first-class type driven by an async graph; DataFusion
keeps Volcano's shape with a vector payload and async clothes. Before
the code, this chapter builds the five design decisions the two systems
embody — the batch contract, async scheduling, SIMD kernel shape, static
vs dynamic parallelism, and the group-by-as-arrays pattern — then maps
each to its file:line.

Anchors are polars py-2.1.0 at `7f902bd3ff` and datafusion `main` at
`350ca7650` (`resources/codebases.md`; confirm with
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
// crates/polars-config/src/lib.rs — polars' default, 36-37.
    36  const IDEAL_MORSEL_SIZE: &str = "POLARS_IDEAL_MORSEL_SIZE";
    37  const DEFAULT_IDEAL_MORSEL_SIZE: u64 = 100_000;
```

```rust
// datafusion/common/src/config.rs — DataFusion's default, 913.
   913          pub batch_size: ConfigNonZeroUsize, default = non_zero_usize_default(8192)
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
> implicit things explicit — the data (which may be on disk), its order,
> who produced it, and when it was consumed — plus the graph, the memory
> states and the executor that move them.

polars-stream is the streaming executor behind `.lazy().collect()`, and
it promotes the work unit to a type:

```rust
// crates/polars-stream/src/morsel.rs — the type, 82-96.
    82  #[derive(Debug)]
    83  pub struct Morsel {
    84      /// The data contained in this morsel.
    85      sf: SpillFrame,
    86  
    87      /// The sequence number of this morsel. May only stay equal or increase
    88      /// within a pipeline.
    89      seq: MorselSeq,
    90  
    91      /// A token that indicates which source this morsel originates from.
    92      source_token: SourceToken,
    93  
    94      /// Used to notify someone when this morsel is consumed, to provide backpressure.
    95      consume_token: Option<WaitToken>,
    96  }
```

Four fields. Take them in turn.

**`SpillFrame` — data that may not be in memory.** Line 85 does not hold a
`DataFrame`; it holds a `SpillFrame` (`crates/polars-ooc/src/spill_frame.rs:94-98`),
a handle the out-of-core memory manager can write to disk and read back.
Getting the rows out is therefore `async` — `into_df` (`morsel.rs:137-139`)
awaits `self.sf.into_df()`, which may mean an IPC read of a spill file
(`spill_frame.rs:75-91`, on a blocking thread). Out-of-core execution is on
by default: the effective budget is the smaller of
`POLARS_OOC_MEMORY_BUDGET_MB` (default `u64::MAX`, i.e. no limit) and
`POLARS_OOC_MEMORY_BUDGET_FRACTION` × total memory (default 0.8)
(`crates/polars-config/src/lib.rs:78-82` for the defaults, `:402-406` for
the `min`). On a 64 GiB machine that is 0.8 × 64 = 51.2 GiB before morsels
start going to disk.

**`MorselSeq` — order, written down.** Parallel workers pull morsels and
finish them out of order; an ORDER BY or a LIMIT has to put them back.
DuckDB reconstructs this from batch indices; polars carries it:

```rust
// crates/polars-stream/src/morsel.rs — the sequence number, 16-22.
    16  /// A token indicating the order of morsels in a stream.
    17  ///
    18  /// The sequence tokens going through a pipe are monotonely non-decreasing and are allowed to be
    19  /// discontinuous. Consequently, `1 -> 1 -> 2` and `1 -> 3 -> 5` are valid streams of sequence
    20  /// tokens.
    21  #[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Default)]
    22  pub struct MorselSeq(u64);
```

Read 18-20 carefully: the guarantee is *monotonely non-decreasing and
allowed to be discontinuous*, so `1 → 3 → 5` is legal. That is weaker
than "consecutive", which is what makes it cheap — a source that splits or
drops morsels does not have to renumber anything. `MorselSeq::new`
(`:27-29`) multiplies by two, reserving the low bit for a future
"last morsel with this sequence number" flag (`:25-26`, `:33-34`) —
topic 2's bit-smuggling again, pre-allocated.

**`SourceToken` — a stop request, not backpressure.**

```rust
// crates/polars-stream/src/morsel.rs — the token, 52-58, and what it does, 73-79.
    52  /// A token indicating which source this morsel originated from, and a way to
    53  /// pass information/signals to it. Currently it's only used to request a source
    54  /// to stop with passing new morsels this execution phase.
    55  #[derive(Clone, Debug)]
    56  pub struct SourceToken {
    57      stop: Arc<RelaxedCell<bool>>,
    58  }
// ... 59-72: Default and new() ...
    73      pub fn stop(&self) {
    74          self.stop.store(true);
    75      }
    76  
    77      pub fn stop_requested(&self) -> bool {
    78          self.stop.load()
    79      }
```

The doc comment (52-54) is explicit that this is "only used to request a
source to stop", which is the LIMIT case: a sink that has enough rows sets
the flag and the scan notices. **Backpressure** — slowing a fast producer
so a slow consumer's queue stays bounded — is the *other* token, the
`consume_token: Option<WaitToken>` on line 95: a producer that wants to
throttle waits on it and is woken when the morsel is actually consumed. It
is easy to conflate the two, and the fields are separate because the two
mechanisms are: one ends production, the other paces it.

**The size is a config knob, not a constant** (`:12-14`, resolving to the
`100_000` of Step 1) — contrast DuckDB's `#define`.

The plan is an explicit graph:

```rust
// crates/polars-stream/src/graph.rs — the graph, 16-24, and a node, 184-190.
    16  /// Represents the compute graph.
    17  ///
    18  /// The `nodes` perform computation and the `pipes` form the connections between nodes
    19  /// that data is sent through.
    20  #[derive(Default)]
    21  pub struct Graph {
    22      pub nodes: SlotMap<GraphNodeKey, GraphNode>,
    23      pub pipes: SlotMap<LogicalPipeKey, LogicalPipe>,
    24  }
// ... 25-183: add_node, port wiring, state update ...
   184  /// A node in the graph represents a computation performed on the stream of morsels
   185  /// that flow through it.
   186  pub struct GraphNode {
   187      pub compute: Box<dyn ComputeNode>,
   188      pub inputs: Vec<LogicalPipeKey>,
   189      pub outputs: Vec<LogicalPipeKey>,
   190  }
```

Nodes implement `ComputeNode`, and the trait is where polars' answer to
DuckDB's `OperatorResultType` lives — not a return code, but a state
negotiation, a memory declaration and a spawn:

```rust
// crates/polars-stream/src/nodes/mod.rs — the operator contract, 118-139.
   118      fn update_state(
   119          &mut self,
   120          recv: &mut [PortState],
   121          send: &mut [PortState],
   122          state: &StreamingExecutionState,
   123      ) -> PolarsResult<()>;
   124  
   125      /// The memory usage of this node in its current state.
   126      fn memory_usage(&self) -> NodeMemoryUsage {
   127          NodeMemoryUsage::Bounded
   128      }
   129  
   130      /// Spawn the tasks that this compute node needs to receive input(s),
   131      /// process it and send to its output(s). Called once per execution phase.
   132      fn spawn<'env, 's>(
   133          &'env mut self,
   134          scope: &'s TaskScope<'s, 'env>,
   135          recv_ports: &mut [Option<RecvPort<'_>>],
   136          send_ports: &mut [Option<SendPort<'_>>],
   137          state: &'s StreamingExecutionState,
   138          join_handles: &mut Vec<JoinHandle<PolarsResult<()>>>,
   139      );
```

`update_state` (118-123) negotiates port readiness before a phase;
`spawn` (130-139) then creates **async tasks** — cooperatively-scheduled
functions that yield at `.await` points rather than blocking a thread —
one set per execution phase. `memory_usage` (125-128) is where polars
encodes what DuckDB calls a pipeline breaker, and it says more than yes or
no. The answer is one of six states:

```rust
// crates/polars-stream/src/nodes/mod.rs — the memory states, 83-101.
    83  /// How a node's memory use relates to its input size, given its current state.
    84  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    85  pub enum NodeMemoryUsage {
    86      /// Memory does not meaningfully grow with the input size, now or in a later
    87      /// state.
    88      Bounded,
    89      /// Memory may grow with the input size while data streams through the node.
    90      Unbounded,
    91  
    92      /// Stores nothing meaningful yet, but will be `Accumulating` in a later state.
    93      WillAccumulate,
    94      /// Stores (substantial parts of) its input while running. Each phase runs
    95      /// at most one such node, unless all such nodes remaining are sinks.
    96      Accumulating,
    97      /// Holds significant stored data that may be freed once this node runs to completion.
    98      HoldingUntilDone,
    99      /// Holds significant stored data that may be freed immediately while this node runs.
   100      Draining,
   101  }
```

`Accumulating` (96) is the closest match to DuckDB's breaker — a node that
"stores (substantial parts of) its input while running", like a hash-join
build or a sort — and the other states describe the rest of its life: about
to accumulate (93), holding until done (98), freeing as it drains (100).
The scheduling rule rides along in the comment at 94-95, and the phase
loop enforces it.

And the execution is *phased*: `execute_graph` (`execute.rs:285`) loops
(`:323-377`) — update all port states (`:328`), find a runnable subgraph
(`find_runnable_subgraph`, `:343`), stop when there is none (`:353-355`),
run it (`:358`), repeat. The graph is not run once; it is run in waves,
which is how a breaker's downstream gets to start only after it finishes.
`find_runnable_subgraph` (`:116-137`) is where the memory states earn their
keep: it partitions the runnable phase sinks into `Accumulating` nodes and
the rest (`:120-122`), and unless every accumulating node left is a final
sink (`:124-131`), it runs only *one* of them (`:133`). Two accumulating
nodes — two hash-join builds, say — that could both run now are run one per
phase, so peak memory is one
build's worth, not two — and anything that does not fit goes to disk
through the `SpillFrame` handles of Step 2's first field.

It is tempting to say pipeline parallelism here "falls out of the async
runtime rather than a hand-rolled scheduler". It does not. polars wrote its
own work-stealing executor:

```rust
// crates/polars-async/src/executor/mod.rs — the scheduler polars wrote, 256-292.
   256      fn try_steal_task<R: Rng>(&self, thread: usize, rng: &mut R) -> Option<ReadyTask> {
   257          // Try to get a global task.
   258          loop {
   259              match self.global_high_prio_task_queue.steal() {
   260                  Steal::Empty => break,
// ... 261-273: the rest of the high-priority loop, then the same for low priority ...
   274          // Try to steal tasks.
   275          let ttl = &self.thread_task_lists[thread];
   276          let steal_iters = 4;
   277          for steal_idx in 0..steal_iters {
   278              // For the first few steal attempts try to limit ourselves to our
   279              // own NUMA region, then on the last attempt try globally.
   280              let limit_to_numa_region = steal_idx != steal_iters - 1;
// ... 281-285: retry loop ...
   286                  for idx in random_permutation(self.thread_task_lists.len() as u32, rng) {
   287                      if limit_to_numa_region
   288                          && self.thread_numa_regions[thread]
   289                              != self.thread_numa_regions[idx as usize]
   290                      {
   291                          continue;
   292                      }
```

`try_steal_task` (256) drains a global high-priority queue, then a
low-priority one (`:266-272`), then steals from randomly ordered sibling
threads — and line 280 is the one to focus on: the first three of the four
steal rounds (`steal_iters = 4`, 276) skip any victim in a different
**NUMA region** (a socket's worth of cores and the memory attached to it;
reading another region's memory is slower than reading your own), and only
the last round steals globally. Each runner pins itself to its region
(`pin_thread_to_numa_region`, `:318`), parks in a per-region park group
(`:321`, `:181-182`), and makes one last steal attempt before sleeping
(`:354-360`). That is the morsel paper's NUMA-local work stealing,
implemented — but compiled in only with the `numa` Cargo feature
(`crates/polars-async/Cargo.toml:29`, `numa = ["dep:hwlocality-sys"]`);
without it the executor uses `numa/dummy.rs`, where `num_numa_regions()`
is 1 and the region filter never rejects anyone. tokio appears in
`polars-stream` only with the `sync` and `macros` features
(`Cargo.toml:34`) and as the runtime for blocking I/O
(`ASYNC.block_in_place_on`, `execute.rs:334`). So `async` here buys the
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
// crates/polars-compute/src/float_sum.rs — the pair, 66-70, and the masked
// implementation, 90-103.
    66  // As a trait to not proliferate SIMD bounds.
    67  pub trait SumBlock<F> {
    68      fn sum_block_vectorized(&self) -> F;
    69      fn sum_block_vectorized_with_mask(&self, mask: BitMask<'_>) -> F;
    70  }
// ... 71-89: the impl header and the unmasked variant ...
    90      fn sum_block_vectorized_with_mask(&self, mask: BitMask<'_>) -> F {
    91          let zero = Simd::default();
    92          let vsum = self
    93              .as_chunks::<STRIPE>()
    94              .0
    95              .iter()
    96              .enumerate()
    97              .map(|(i, a)| {
    98                  let m: Mask<T::Mask, STRIPE> = mask.get_simd(i * STRIPE);
    99                  m.select(Simd::from_slice(a).cast_generic::<F>(), zero)
   100              })
   101              .sum::<Simd<F, STRIPE>>();
   102          vector_horizontal_sum(vsum)
   103      }
```

`as_chunks::<STRIPE>()` (93) splits the 128-element block into
compile-time-sized `[T; 16]` arrays, so the length check is gone from the
loop. Line 99 is the whole trick: `m.select(values, zero)` blends per lane —
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
 those 8 adds are a *dependent* chain into one accumulator (86, 101)
 at an assumed 3-4 cycle FP-add latency: 24-32 cycles per 128 elements
                                       = 4.0-5.3 elements/cycle
 a scalar loop, same latency, chain of 128:            0.25-0.33 el/cycle
```

So the 16 lanes are not worth 16× on their own — a single accumulator
turns them into roughly 4-5 elements per cycle, because each add waits for
the previous one. The independence has to come from somewhere else, and it
comes from the recursion:

```rust
// crates/polars-compute/src/float_sum.rs — the recursion, 210-215.
   210      unsafe {
   211          let blocks = f.len() / PAIRWISE_RECURSION_LIMIT;
   212          let left_len = (blocks / 2) * PAIRWISE_RECURSION_LIMIT;
   213          let (left, right) = (f.get_unchecked(..left_len), f.get_unchecked(left_len..));
   214          pairwise_sum(left) + pairwise_sum(right)
   215      }
```

`pairwise_sum` (`:194`) splits the slice in half down to 128-element
blocks and adds the halves (214). The two subtree sums are independent, so
the machine can have several accumulator chains in flight at once — this
is topic 0's memory-level-parallelism lesson applied to arithmetic ports.

But note *why* the code is shaped that way, because it is easy to read
backwards: pairwise summation is a **numerical** technique first. Its
error bound grows as O(log n) where a naive running sum grows as O(n); the
instruction-level parallelism is a side effect the author got for free.
The same honesty applies to the reduce:

```rust
// crates/polars-compute/src/float_sum.rs — the final reduce, 45-64.
    45  fn vector_horizontal_sum<V, T>(mut v: V) -> T
    46  where
    47      V: IndexMut<usize, Output = T>,
    48      T: Add<T, Output = T> + Sized + Copy,
    49  {
    50      // We have to be careful about this reduction, floating
    51      // point math is NOT associative so we have to write this
    52      // in a form that maps to good shuffle instructions.
    53      // We fold the vector onto itself, halved, until we are down to
    54      // four elements which we add in a shuffle-friendly way.
    55      let mut width = STRIPE;
    56      while width > 4 {
    57          for j in 0..width / 2 {
    58              v[j] = v[j] + v[width / 2 + j];
    59          }
    60          width /= 2;
    61      }
    62  
    63      (v[0] + v[2]) + (v[1] + v[3])
    64  }
```

The comment at 50-54 says it outright: float addition is not associative,
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
// datafusion/physical-plan/src/execution_plan.rs — the trait, 102, and the
// one method that is the whole model, 696-700.
   102  pub trait ExecutionPlan: Any + Debug + DisplayAs + Send + Sync {
// ... 103-695: name, properties, children, doc examples ...
   696      fn execute(
   697          &self,
   698          partition: usize,
   699          context: Arc<TaskContext>,
   700      ) -> Result<SendableRecordBatchStream>;
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
(`target_partitions`, `datafusion/common/src/config.rs:948`), and skew is
patched *inside* the plan by `RepartitionExec`
(`datafusion/physical-plan/src/repartition/mod.rs:1526`), inserted by the
optimizer under the `repartition_joins` / `repartition_aggregations` flags
(`config.rs:1721`, `:1709`, both default `true`).

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
// 83-98.
    83  /// # Group Ids
    84  ///
    85  /// Each distinct group in a hash aggregation is identified by a unique group id
    86  /// (usize) which is assigned by instances of this trait. Group ids are
    87  /// continuous without gaps, starting from 0.
    88  pub trait GroupValues: Send {
    89      /// Calculates the group id for each input row of `cols`, assigning new
    90      /// group ids as necessary.
    91      ///
    92      /// When the function returns, `groups`  must contain the group id for each
    93      /// row in `cols`.
    94      ///
    95      /// If a row has the same value as a previous row, the same group id is
    96      /// assigned. If a row has a new value, the next available group id is
    97      /// assigned.
    98      fn intern(&mut self, cols: &[ArrayRef], groups: &mut Vec<usize>) -> Result<()>;
```

"Continuous without gaps, starting from 0" (86-87) is the load-bearing
sentence: it is what lets a group id be an *array index* rather than a map
key. The per-batch loop then interns once and hands the same
`group_indices` to every accumulator:

```rust
// datafusion/physical-plan/src/aggregates/grouped_hash_stream.rs — inside
// group_aggregate_batch (869): intern once, 921-924 …
   921                      let starting_num_groups = self.group_values.len();
   922                      self.group_values
   923                          .intern(group_values, &mut self.current_group_indices)?;
   924                      let group_indices = &self.current_group_indices;
// ... 925-943: new-group bookkeeping, the aggregation timer, zip accumulators with their inputs ...
   944                  for (idx, ((acc, values), opt_filter)) in t.enumerate() {
   945                      if self.mode.input_mode() == AggregateInputMode::Raw && !is_stream_merging
   946                      {
   947                          self.aggregate_accumulator_metrics.time(
   948                              idx,
   949                              AccumulatorPhase::Update,
   950                              || {
   951                                  acc.update_batch(
   952                                      values,
   953                                      group_indices,
   954                                      *opt_filter,
   955                                      total_num_groups,
   956                                  )
```

Line 923 is the one probe per row; line 953 hands the same `group_indices`
to every accumulator the loop at 944 visits.

And the accumulator's state really is a flat array:

```rust
// datafusion/functions-aggregate-common/src/aggregate/groups_accumulator/prim_op.rs
// — the state, 48-49, and the update, 101-115.
    48      /// values per group, stored as the native type
    49      values: Vec<T::Native>,
// ... 50-100: the other fields, constructors, and the update_batch header ...
   101          // update values
   102          self.values.resize(total_num_groups, self.starting_value);
   103  
   104          // NullState dispatches / handles tracking nulls and groups that saw no values
   105          self.null_state.accumulate(
   106              group_indices,
   107              values,
   108              opt_filter,
   109              total_num_groups,
   110              |group_index, new_value| {
   111                  // SAFETY: group_index is guaranteed to be in bounds
   112                  let value = unsafe { self.values.get_unchecked_mut(group_index) };
   113                  (self.prim_fn)(value, new_value);
   114              },
   115          );
```

`values: Vec<T::Native>` (49), grown to `total_num_groups` (102), written
through a raw index (112-113). No per-group heap object exists to chase,
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
// grouped_hash_stream.rs:921-956 (intern, then one update_batch per
// accumulator) and prim_op.rs:101-115 (the flat-array update). This
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
| parallelism | morsel pull | morsel pull, async graph, one accumulating node per phase | static partitions + `RepartitionExec` |
| scheduling | own task scheduler | own work-stealing executor over async tasks, NUMA-region-first with the `numa` feature | tokio |
| where a batch's data lives | memory (`Vector` arrays) | a `SpillFrame` handle — memory or disk, out-of-core on by default | memory (Arrow arrays) |
| ordering | implicit (batch index) | explicit `MorselSeq` | stream contract |
| flow control | `OperatorResultType` return codes | `SourceToken` (stop) + `WaitToken` (backpressure) | async backpressure via `poll_next` |
| operator state | executor-owned chunks | node-owned, per phase; declared via `memory_usage` | stream-owned |

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
Anchors are polars `7f902bd3ff` (py-2.1.0), datafusion `350ca7650`.

| File | Lines | What is there | Step |
|---|---|---|---|
| `crates/polars-config/src/lib.rs` | 36-37 | `DEFAULT_IDEAL_MORSEL_SIZE = 100_000`, and its env var | 1 |
| `datafusion/common/src/config.rs` | 913 | `batch_size` default 8192 | 1 |
| `crates/polars-stream/src/morsel.rs` | 12-14 | `get_ideal_morsel_size` — a config read, not a constant | 1, 2 |
| `crates/polars-stream/src/morsel.rs` | 16-22, 25-36 | `MorselSeq`, non-decreasing and discontinuous; the reserved low bit | 2 |
| `crates/polars-stream/src/morsel.rs` | 52-58, 73-79 | `SourceToken` — a *stop* request, not backpressure | 2 |
| `crates/polars-stream/src/morsel.rs` | 82-96, 137-139 | `Morsel` — four fields; `sf: SpillFrame` (85); `consume_token` (95) is the backpressure one; `into_df` is async | 2 |
| `crates/polars-ooc/src/spill_frame.rs` | 75-91, 94-98 | `SpillFrame` — a handle the memory manager can spill; the unspill read | 2 |
| `crates/polars-config/src/lib.rs` | 78-82, 402-406 | out-of-core budget defaults (0.8 of memory, no MB cap) and the `min` | 2 |
| `crates/polars-stream/src/graph.rs` | 16-24, 184-190, 192-209 | `Graph`, `GraphNode`, `LogicalPipe` | 2 |
| `crates/polars-stream/src/nodes/mod.rs` | 83-101, 118-139 | `NodeMemoryUsage`'s six states; `update_state`, `memory_usage`, `spawn` — the operator contract | 2 |
| `crates/polars-stream/src/execute.rs` | 116-137, 285, 323-377 | `find_runnable_subgraph` (one accumulating node per phase); `execute_graph`; the phase loop | 2 |
| `crates/polars-async/src/executor/mod.rs` | 256-310, 312-360 | the work-stealing scheduler polars wrote itself, NUMA region first (280) | 2 |
| `crates/polars-async/Cargo.toml` | 29 | the opt-in `numa` feature | 2 |
| `crates/polars-compute/src/float_sum.rs` | 13-14 | `STRIPE = 16`, `PAIRWISE_RECURSION_LIMIT = 128` | 3 |
| `crates/polars-compute/src/float_sum.rs` | 45-64 | `vector_horizontal_sum` — and why the fold order is chosen | 3 |
| `crates/polars-compute/src/float_sum.rs` | 66-70, 90-103 | `SumBlock`; the masked variant selects into lanes (99) | 3 |
| `crates/polars-compute/src/float_sum.rs` | 194-216 | `pairwise_sum` — accuracy first, ILP as a bonus | 3 |
| `datafusion/physical-plan/src/execution_plan.rs` | 102, 696-700 | `trait ExecutionPlan`; `execute(partition, ctx)` | 4 |
| `datafusion/common/src/config.rs` | 948, 1709, 1721 | `target_partitions`; the repartition flags | 4 |
| `datafusion/physical-plan/src/repartition/mod.rs` | 1526 | `RepartitionExec` — the skew patch | 4 |
| `.../aggregates/group_values/mod.rs` | 83-98 | `GroupValues::intern`; ids dense from 0 | 5 |
| `.../aggregates/grouped_hash_stream.rs` | 285, 665, 869, 921-956 | the stream; `poll_next`; `group_aggregate_batch`; intern once, then every accumulator | 5 |
| `.../groups_accumulator/prim_op.rs` | 48-49, 101-115 | `values: Vec<T::Native>` — flat state indexed by group id | 5 |

## Takeaway

The three engines agree completely on the decision that matters — batch,
don't tuple — and the arithmetic shows why they can afford to disagree
about everything else: at 2048, 8192 or 100,000 rows per call the dispatch
term is 0.01 ns/row and no longer participates in the argument. What is
left to design is what the batch *carries* (order, stop signals,
backpressure), who hands batches to whom (own scheduler vs static
partitions), and what the innermost loop looks like (lanes, selects, and
a recursion that is really about float error).

Two plausible readings do not survive the code: polars-stream does *not*
get its parallelism from an off-the-shelf async runtime — it ships its own
work-stealing executor — and `float_sum`'s independent accumulators come
from a *pairwise* recursion whose first purpose is numerical accuracy. Both
are the kind of thing that reads plausibly and is wrong, which is the
argument for the file:line habit.

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
  (`polars-config/src/lib.rs:37`), morsel-pull over an async graph on its
  own work-stealing executor. DataFusion: Arrow `RecordBatch`, default
  8192 (`config.rs:913`), static partition-per-stream with
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

  From `morsel.rs:83-96`: `sf: SpillFrame` is the data, held through a
  handle the out-of-core manager can spill to disk, so reading it is
  `async` (`into_df`, 137-139); `seq: MorselSeq` is the ordering token,
  guaranteed monotonely non-decreasing but explicitly allowed to be
  discontinuous (16-20), so order-sensitive sinks can reassemble and
  order-insensitive ones can ignore it; `source_token: SourceToken`
  identifies the producing source and carries a *stop* request (52-54,
  73-79) — the LIMIT case; and `consume_token: Option<WaitToken>` (94-95)
  is the backpressure mechanism, notifying a waiting producer only once the
  morsel has actually been consumed.

  Stop and backpressure are different fields because they are different
  mechanisms: one ends production, the other paces it.

  </details>

- [ ] You can explain why a masked SIMD kernel selects rather than branches, and why 16 lanes do not give 16×.

  <details><summary>Answer</summary>

  It selects because a branch on a scattered null mask is unpredictable:
  `m.select(values, zero)` (`float_sum.rs:99`) makes a null contribute
  `0.0` with no control flow at all. FINDINGS row 17 measures the
  alternative at 0.95 GB/s branchy against ~10 GB/s branchless.

  16 lanes do not give 16× because a 128-element block
  (`PAIRWISE_RECURSION_LIMIT`) is 8 chunks of `STRIPE = 16` summed into
  *one* accumulator (86, 101) — a dependent chain of 8 FP adds. At 3-4
  cycles of add latency that is 24-32 cycles per 128 elements, about 4-5
  elements per cycle. The independent chains come from `pairwise_sum`
  (194-216) splitting the input recursively, and that recursion exists
  primarily to bound floating-point error at O(log n) rather than O(n) —
  the instruction-level parallelism is a bonus.

  </details>

- [ ] You can describe the intern-then-flat-arrays group-by in two sentences, and count the probes it saves.

  <details><summary>Answer</summary>

  Per input batch, DataFusion interns the group keys once — one hash probe
  per row mapping each key to a dense group id, contiguous from 0
  (`group_values/mod.rs:86-98`) — and then hands the same
  `group_indices` slice to every accumulator
  (`grouped_hash_stream.rs:921-956`). Each accumulator's state is a flat
  `Vec` indexed by that id (`prim_op.rs:49`, written at 112-113), so there
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
  (`execution_plan.rs:696-700`) gives a task one partition for the whole
  query, with N defaulting to the machine's parallelism
  (`config.rs:948`). If the work splits evenly across 8 partitions, 50 M
  rows is 6.25 M each. If one partition holds 3× the mean it holds
  18.75 M, and since the query ends when the slowest partition does, the
  wall clock is 3.0× the balanced case.

  Morsel pulling bounds the same imbalance by one morsel. At DuckDB's
  122,880-row grain the 50 M-row lane is 408 units, 51 per thread, so a
  straggler costs at most one unfinished morsel — 0.25% of a thread's
  share. DataFusion patches the gap inside the plan instead, with
  `RepartitionExec` (`repartition/mod.rs:1526`) inserted under the
  `repartition_joins` / `repartition_aggregations` flags (`config.rs:1721`,
  `:1709`).

  </details>

## References

**Code**
- [polars](https://github.com/pola-rs/polars) py-2.1.0 at `7f902bd3ff` —
  `crates/polars-stream/src/` (`morsel.rs`, `graph.rs`, `nodes/mod.rs`,
  `execute.rs`), `crates/polars-ooc/src/spill_frame.rs` for out-of-core,
  `crates/polars-async/src/executor/mod.rs` for the scheduler, and `crates/polars-compute/src/float_sum.rs` for what a SIMD
  kernel actually looks like
- [datafusion](https://github.com/apache/datafusion) at `350ca7650` —
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
