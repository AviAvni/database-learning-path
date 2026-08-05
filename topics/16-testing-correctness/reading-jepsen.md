# Jepsen & elle: isolation anomalies are cycles

Jepsen believes nothing you tell it: it drives real concurrent
clients against a real cluster while breaking the network, records
the history, and only afterwards decides whether that history was
even possible under the claimed consistency model. Before the
analyses, this chapter builds the machinery step by step — the
black-box method, why checking a history is NP-complete, and elle's
workload-design trick that makes anomalies show up as graph cycles —
then routes you through two reports worth reading in full:
Redis-Raft (the catalog of consensus-plumbing bugs) and Dgraph (the
graph-DB cautionary tale).

Every number below is quoted from a primary source: the Elle paper
(Kingsbury & Alvaro, VLDB 2020, `arXiv:2003.10554`) with its section
number, or the Jepsen report itself with the issue number Redis Labs
or Dgraph filed. There is no `elle` clone in this repo's pin table,
so the code anchors are turso's own Elle integration at commit
`dd775bc` — which emits Elle's EDN and hands it to `elle-cli`.

## The problem in one sentence

Databases routinely claim "serializable" or "linearizable" and lose
acked writes the first time a network partition lands mid-failover —
and Jepsen's Redis-Raft analysis found 21 issues, five of them
losing committed updates, in a system built directly on the Raft
paper's math.

## The concepts, step by step

### Step 1 — the method: real cluster, real faults, recorded history

> **In:** an unmodified binary running on a real cluster, plus
> client access. No source, no instrumentation.
> **Out:** a **history** — a timestamped log of every operation's
> invocation, completion, and result.

Jepsen is black-box testing. It spawns concurrent clients issuing
operations while a **nemesis** process injects real environmental
faults, and records everything:

```
 generators → concurrent client ops (read/write/cas/txn)
            → against a REAL cluster
            → while nemesis injects: partitions, clock skew,
              process kills/pauses (SIGSTOP = the GC-pause stand-in)
            → record HISTORY: [{op, start, end, result}, ...]
            → checker: is this history linearizable / serializable?
```

Note the fault menu is topic 15's failure catalog made physical:
iptables rules for partitions, SIGSTOP for the process that's alive
but not responding (the GC-pause / VM-migration stand-in a crash
doesn't model). The Redis-Raft test design section names the exact
menu — "process pauses, crashes, network partitions, clock skew, and
membership changes" — on "five-node Debian 9 clusters, on both LXC
and EC2".

The three-part record per operation is the load-bearing detail. An
operation has an **invocation** time, a **completion** time, and an
outcome that may be `ok`, `fail` (definitely did not happen), or
`info` (**indeterminate** — the client never learned). Indeterminate
is not a nuisance; it is the normal outcome of a partition, and any
checker that cannot represent it will either miss bugs or invent
them.

Why it matters: everything Jepsen finds, it finds because it refused
to trust the system's own account of what happened. The history is
the only evidence.

### Step 2 — the checker problem: verifying a history is the hard part

> **In:** a recorded history of `n` operations with `c` of them
> concurrent at any moment.
> **Out:** a verdict — reachable only by searching orderings, and
> the search is exponential.

**Linearizability** (every operation appears to take effect
atomically at some instant between its invocation and completion)
sounds checkable — but given a history of concurrent operations,
deciding whether *any* legal ordering explains it is NP-complete in
general. The Elle paper §1 states the same for the isolation side:
"Serializability checking is also (in general) NP-complete."

Jepsen's first checker, Knossos, did exactly this search. Work the
cost, using the paper's own framing — "given c concurrent
transactions, the number of permutations to evaluate is c!":

```
 c = 10 concurrent txns    10! = 3,628,800                feasible
 c = 15                    15! ≈ 1.3 × 10^12             hours
 c = 20                    20! ≈ 2.4 × 10^18             no

 measured (§7.5), 24-core Xeon / 128 GB, 100 s runtime cap:
   Knossos "often timed out or ran out of memory after a few
   hundred transactions"
   "many Knossos runs involved search spaces on the order of 10^24"
   "With 40+ concurrent processes, even histories of 5000
   transactions were (generally) uncheckable"
```

An earlier attempt using the Gecode constraint solver fared no
better: "Histories of more than a hundred-odd transactions quickly
become intractable" (§1).

Histories therefore had to stay short — which is the opposite of
what fault-finding wants, because a partition takes seconds to land
and the interesting interleavings are rare. elle is the escape, and
the measured gap is stark: Elle "checked hundreds of thousands of
transactions in tens of seconds" under the same 100-second cap, and
is "primarily linear in the length of a history" (§7.5). The
Redis-Raft report describes it the same way: "a new type of
consistency checker, which operates in linear (rather than
exponential) time".

Why it matters: the checker's complexity is what caps how long you
can run a test, and how long you can run a test is what caps which
bugs you can find. This is a *performance* constraint on a
*correctness* tool.

### Step 3 — elle's trick: design the workload so dependencies are visible

> **In:** freedom to choose what operations the clients issue.
> **Out:** a workload whose *results* directly reveal the
> dependency edges, so nothing has to be searched for.

Don't check arbitrary histories — DESIGN the operations so the
outcome itself reveals what ordered what. elle's workload is
**list-append**: every write is `append(k, v)` with a globally
unique v, and every read returns the *entire list* for k. Now a
single read of `[1,3]` on k is loaded with facts: 1 preceded 3
(a write-write dependency, **ww**), this read saw 3's write (a
write-read dependency, **wr**), and any transaction appending 4
must come after this read (a read-write anti-dependency, **rw**,
inferred). Plain registers (get/set of a single value) hide all of
this — each write destroys the evidence of the previous one; lists
keep the whole lineage.

Count what one read buys, for a key whose list has grown to length
`n`:

```
 read of k = [v1, v2, ..., vn]

 ww edges recovered   n − 1     (v1→v2, v2→v3, … : the list IS the write order)
 wr edges recovered   n         (this txn read every one of those writes)
 rw edges implied     ≥ 1       (any later appender to k comes after this read)

 n = 20  →  19 + 20 = 39 dependency facts from ONE read

 same read against a register:
 ww edges recovered   0         (the previous value is gone)
 wr edges recovered   1         (you saw *someone's* write; which one is ambiguous
                                 unless values are unique)
```

That ratio — 39 to 1 — is the whole technique. The paper's
recoverability property is what makes it sound: because appends are
unique and lists are never overwritten, the *version order* of each
key is directly readable off the data, rather than being something
the checker must guess.

The concrete data format is short enough to read in full. turso's
simulator implements the Jepsen side of this to feed `elle-cli`:

```rust
// simulator/testing/concurrent-simulator/elle.rs — ElleOp and to_edn, 18-67 (elided)
    18  pub enum ElleOp {
    19      /// Append a value to a list identified by key (list-append model)
    20      Append { key: String, value: i64 },
    21      /// Read a list by key, result is None before execution and Some after (list-append model)
    22      Read {
    23          key: String,
    24          result: Option<Vec<i64>>,
    25      },
// ... 26-29: Write / RwRead — the weaker rw-register model, kept for comparison ...
    30  }
// ... 32-35: doc comment giving the two target forms ...
    36      pub fn to_edn(&self) -> String {
    37          match self {
    38              ElleOp::Append { key, value } => {
    39                  format!("[:append \"{}\" {}]", escape_edn_string(key), value)
    40              }
    41              ElleOp::Read { key, result } => {
    42                  let result_str = match result {
    43                      None => "nil".to_string(),
// ... 44-53: Some(vals) → "[1 2 3]", empty → "[]" ...
    54                  format!("[:r \"{}\" {}]", escape_edn_string(key), result_str)
    55              }
```

Line 24 is the one to stare at: `result: Option<Vec<i64>>` — the
`Option` is Step 1's "not yet completed", and the `Vec` is Step 3's
whole point. The `nil` at line 43 is how an *invoked but
uncompleted* read is written down; the checker needs to see the
invocation even when the result never arrived.

`ElleEventType` at `:72-79` carries the other half — `Invoke`, `Ok`,
`Fail`, `Info` — which is exactly Step 1's three outcomes plus the
invocation record.

Question: why do unique values + list semantics make wr/ww edges
*directly observable* where plain registers hide them?

Why it matters: this is a *test design* insight, not an algorithm.
The exponential search in Step 2 didn't get a better algorithm; it
got deleted by choosing a different workload.

### Step 4 — the serialization graph: a cycle IS an anomaly

> **In:** the ww / wr / rw edges recovered in Step 3, plus real-time
> ordering from Step 1's timestamps.
> **Out:** a directed graph — and any cycle in it is a named
> anomaly, found in near-linear time.

Collect those ww/wr/rw facts into a directed graph over transactions
(the **serialization graph** — an edge T1 → T2 means T1 must come
before T2 in any serial order). If the graph has a cycle, no serial
order exists — and the cycle's *edge types* name the anomaly. The
paper's §6 gives the taxonomy precisely, and it is by *composition
of edge types around the cycle*:

| cycle | edges it contains | classic name |
|---|---|---|
| **G0** | **all** ww | dirty write / write cycle |
| **G1c** | ww or wr (at least one wr, no rw) | cyclic information flow |
| **G-single** | **exactly one** rw, rest ww/wr | read skew |
| **G2** | **one or more** rw | anti-dependency cycle (incl. write skew) |

Note G-single is not "a cycle with rw edges" — it is a cycle with
*exactly one*, which is what makes it a distinct and much more
common finding than general G2. Elle also reports non-cycle
anomalies §6.1 names directly: garbage reads, duplicate writes, and
internal inconsistency (a transaction not seeing its own earlier
writes), plus §4.3.1's aborted read, intermediate read, and dirty
update.

The paper's claim for the whole scheme, from the abstract: Elle "can
detect every anomaly in Adya et al's formalism (except for
predicates)".

Detection is **Tarjan's strongly-connected-components algorithm**
followed by a BFS within each SCC to extract a short, human-readable
cycle (§6). Both are near-linear:

```
 Knossos:  O(c!) in the concurrency c        — Step 2's 10^24
 Elle:     Tarjan SCC   O(V + E)
           + BFS per SCC O(V + E)
           ≈ linear in the number of transactions and edges

 measured (§7.5, same 100 s cap, 24-core Xeon):
   Knossos   a few hundred transactions before timeout/OOM
   Elle      hundreds of thousands of transactions in tens of seconds
   ratio     ~10^3 more history, checked in less time
```

And the counterexample is human-readable ("this txn read state that
implies it ran both before and after that one") rather than "no
linearization exists", which is what a search-based checker gives
you.

Why it matters: the cycle is both the *proof* and the *explanation*.
A checker that only says yes/no produces bug reports nobody can act
on; §7's results depended on being able to hand a vendor four
transactions.

### Step 5 — what the method finds: Redis-Raft, 2020

> **In:** Redis-Raft development builds `1b3fbf6` through `e0123a9`,
> five-node clusters, the Elle append workload over `RPUSH`/`LRANGE`.
> **Out:** 21 issues — and a lesson about where they were.

The Redis-Raft analysis is the catalog of consensus-*integration*
bugs. None were in the Raft paper's math; all were in the plumbing.
The report's own tally:

> "we found twenty-one issues, including long-lasting unavailability
> in healthy clusters, eight crashes, three cases of stale reads,
> one case of aborted reads, five bugs resulting in the loss of
> committed updates, one infinite loop, and two cases where
> logically corrupt responses could be sent to clients."

Four worth knowing by their mechanism, because the mechanism is the
teaching:

- **#14, total data loss on failover.** Not a stale-leader window —
  a **missing re-entrancy check**. Redis-Raft intercepts `SET k v`,
  rewrites it to `RAFT SET k v`, replicates it, then unwraps and
  applies it — whereupon the interception code saw `SET k v` again
  and re-wrapped it. With proxying off, followers rejected the
  re-wrapped op, so nothing ever reached a follower's state machine
  and *any* failover elected a leader with empty state. The same
  bug with proxying *on* was #13: an infinite loop that ballooned
  the log on every write. One missing check, two catastrophes.
- **#19, stale reads with no faults at all.** A leader is supposed
  to commit a no-op entry on election to learn what is committed;
  the bundled Raft library didn't. Report example: T1 appended 11 to
  key 1 and completed **3.25 seconds** before T2 began, and T2 read
  `[5 8 9]`. This is the ReadIndex-adjacent hole from topic 15 — but
  note the fault column in the report's table says **None**.
- **#17, split-brain via membership change.** `RAFT_LOGTYPE_REMOVE_NODE`
  was left out of the set of log entry types counted as voting
  configuration changes, so a leader could remove every other node
  unilaterally and declare itself a single-node cluster. "Given *n*
  nodes and a sufficiently pathological operator, Redis-Raft could
  split into *n* separate clusters."
- **#28, split-brain redux.** Reads of key 81 on `n1` returned lists
  beginning `[171 172 176 …]` while `n5` returned `[176 …]` — and
  appends of 178 and 208 landed on **both** divergent prefixes. The
  underlying library assumed nodes would be demoted *then* removed,
  rather than removed directly.

Now count the fault column of the report's summary table:

```
 21 issues, by the fault needed to trigger them:
   None                            5    (#13, #19, #21, #25, #42)
   Failover only                   1    (#14)
   crash / partition / pause / membership   15

 fraction needing NO fault injection = 5 / 21 = 23.8%
 fraction of the first tested build's two headline bugs
   that needed no fault              2 / 2   (#13 and #14: "essentially
                                     unusable" before a nemesis ran)
```

Nearly a quarter of the findings needed no nemesis at all — they
needed only *a workload with an oracle*. That is the cheapest
lesson in this topic: before you build fault injection, build the
checker.

Question: for each finding, which of our topic-15 `raft.rs` tests
(or which MISSING test) covers it?

The Dgraph analysis is the graph-DB cautionary tale, and its punch
line is even better. Dgraph shards by predicate into per-group Raft
clusters, with a separate Zero Raft cluster allocating timestamps
(an Omid Reloaded design) and claims **snapshot isolation**. The
bank test lost money — a $100 total reading as $102, then 70–80% of
balances vanishing — after a routine predicate migration, with "no
network or node failures". And the cause:

> "Losing all but the most recently inserted value is a suspicious
> bug to say the least, and its cause turned out **not to be a
> distributed systems problem at all**! … the temporary data
> structure for serialization received Go slices (i.e. pointers) to
> a mutable loop variable which identified the key for that triple.
> This meant that before serialization, *every* triple shared the
> most recently iterated key."

A Go loop-variable aliasing bug, surfaced as a distributed
consistency violation. Which is Step 5's actual thesis: the
consistency checker is an *end-to-end* oracle, and end-to-end
oracles catch bugs that have nothing to do with the layer you
suspected.

Why it matters: you will be tempted to test the consensus algorithm.
Both reports say the algorithm was fine and the wrapping was not.

### Step 6 — Jepsen vs DST: complements, not competitors

> **In:** two testing methods that both find concurrency bugs.
> **Out:** a division of labour — and a specific gap each leaves
> that the other fills.

The comparison that matters for M16:

| | Jepsen | DST (turso/FDB) |
|---|---|---|
| SUT | unmodified binary | instrumented / DI'd |
| faults | real (iptables, SIGSTOP) | simulated |
| reproducibility | statistical, flaky | perfect (seed) |
| finds | integration + env bugs | logic bugs, deep interleavings |
| checker | elle (history-based) | model/invariant (state-based) |

DST explores deeper (millions of seeded interleavings), Jepsen
believes nothing you told it (real kernel, real network, real
binary). A serious engine wants both: the bug classes barely
overlap.

The Redis-Raft report names its own gap, and it is exactly the gap
this topic's `crash_matrix` measures:

> "We have not explored single-node faults, such as filesystem
> corruption or the loss of un-fsynced data written to disk. Both
> might be of interest for Redis-Raft, whose correctness hinges
> (like most consensus systems) on single-node durability."

Put the two side by side with this topic's own numbers. Jepsen's
Redis-Raft campaign ran for months across a dozen builds and
produced 21 issues; `crash_matrix` sweeps 5,000 seeds × 40 ops in
about 0.02 s — roughly 200,000 simulated crash-recoveries per second
— and catches a planted `NoSyncOnCommit` bug in 4,980 of 5,000
seeds (99.6%). Those are not competing numbers; they are numbers
about different things. `NoSyncOnCommit` is precisely the
un-fsynced-write fault Jepsen said it had not explored, and no
amount of `iptables` would have found it.

And the discipline runs the other way too. The report's closing
caveat is the sentence to carry into M16:

> "Jepsen takes an experimental approach to safety verification: we
> can prove the presence of bugs, but not their absence."

Which is also true of `crash_matrix`, and is why topic 21's solver
exists.

Why it matters: choosing between them is a category error. Choosing
*which one to build first* is not — and the answer is whichever
covers the fault your durability story depends on.

## How to read the analyses (with the concepts in hand)

1. **Elle paper (VLDB 2020) §4 and §6** — the dependency-graph
   construction with Steps 3–4 in hand; §6's cycle taxonomy is a
   topic-8 isolation refresher with better names, and §6.1's
   non-cycle anomalies (garbage read, duplicate write, internal
   inconsistency) are the ones you would not have thought to check.
2. **Elle paper §7.5** — the performance section. This is the
   argument for the whole design, in measurements: Knossos's few
   hundred transactions against Elle's hundreds of thousands.
3. **"Redis-Raft 1b3fbf6" (2020)** — read in full. For every
   finding, identify which Step 4 edge types formed the cycle, and
   which plumbing layer (election, log, membership, *proxying*)
   produced it. Then check the fault column: five needed none.
4. **"Dgraph 1.0.2" (2018)** — read as the graph-DB case, then read
   the "Migration Read Skew & Write Loss" section twice: the
   distributed-looking symptom, the single-node cause.
5. **Elle paper §7's case studies** — TiDB 2.1.7–3.0.0-beta.1
   (G-single from two default-on automatic retry mechanisms, fixed
   in 3.0.0-rc2), YugaByte DB 1.3.1 (G2-item on master crash, from a
   fresh master briefly advertising an empty capabilities set),
   FaunaDB 2.6.0 (internal inconsistency with **no faults at all**),
   and Dgraph 1.1.1 (cyclic version orders from shard migration).
   Note the paper's own summary: "Elle revealed anomalies in every
   system we tested."

## Questions for notes.md

1. Why does Jepsen use SIGSTOP/SIGCONT instead of kill -9 for one
   nemesis class — which production failure does a *pause* model
   that a crash doesn't (fencing! DDIA ch. 8)?
2. elle needs append+read-full-list ops. What can it NOT check about
   a system that only exposes get/set registers?
3. An elle cycle of pure rw edges (write skew) — which isolation
   level permits it and which forbids it? (Topic 8 refresher.)
4. Redis-raft served stale reads from deposed leaders. Write the
   ReadIndex fix in one sentence and its cost per read.
5. For M15+M16: sketch a mini-elle for our sim: unique-value
   appends via propose(), reads of committed(), cycle check over
   the history. What does the deterministic sim make TRIVIAL that
   real Jepsen fights (total real-time order is known!)?

## Done when

Answer each before unfolding it.

- [ ] You can explain why checking a recorded history is the hard half of the method, not collecting one.

  <details><summary>Answer</summary>

  Collecting is `n` clients writing to a log. Checking asks whether
  *any* serial order explains the observations, and both
  linearizability and serializability checking are NP-complete in
  general (Elle §1). The search space is the permutations of
  concurrent operations: "given c concurrent transactions, the number
  of permutations to evaluate is c!" (§7.5).

  The measured consequence, from §7.5 on a 24-core Xeon with 128 GB
  and a 100-second cap: Knossos "often timed out or ran out of memory
  after a few hundred transactions", "many Knossos runs involved
  search spaces on the order of 10^24", and "with 40+ concurrent
  processes, even histories of 5000 transactions were (generally)
  uncheckable".

  That caps test *duration*, which caps which bugs you can reach —
  a correctness tool bounded by a performance problem.

  </details>

- [ ] You can describe elle's workload trick and say why append-and-read-full-list makes dependencies visible.

  <details><summary>Answer</summary>

  Every write is `append(k, v)` with a globally unique `v`; every
  read returns the entire list for `k`. The list *is* the version
  order, written down by the database itself.

  One read of an `n`-element list yields `n − 1` ww edges (adjacent
  pairs), `n` wr edges (this transaction saw each of those writes),
  and at least one rw anti-dependency (any later appender follows
  this read). For `n = 20` that is 39 dependency facts from a single
  operation. The same read of a register yields at most one wr edge
  and zero ww edges, because each write destroyed its predecessor's
  evidence.

  turso's `ElleOp::Read { key, result: Option<Vec<i64>> }`
  (`testing/concurrent-simulator/elle.rs:22-25`) is the type: the
  `Vec` is the recovered version order, the `Option` is "invoked but
  not completed".

  </details>

- [ ] You can explain why a cycle in the serialization graph *is* an anomaly, and identify which isolation level a pure-rw cycle violates.

  <details><summary>Answer</summary>

  An edge `T1 → T2` asserts "T1 must precede T2 in any serial order".
  A cycle asserts a transaction must precede itself — so no serial
  order exists, and the history is by definition not serializable.

  A cycle of rw anti-dependencies is **G2** (Elle §6: "one or more
  rw"), which in its two-edge form is write skew. Snapshot isolation
  *permits* it — that is SI's defining hole, and it is why Dgraph's
  upsert test needed index entries treated as conflictable objects.
  Serializable forbids it. G-single, one rw edge exactly, is read
  skew and is forbidden by SI.

  Detection is Tarjan's SCC plus a BFS inside each component (§6) —
  near-linear, versus the `c!` of Step 2, and it hands you the
  offending transactions rather than a bare "no".

  </details>

- [ ] You can say why Jepsen uses SIGSTOP/SIGCONT rather than kill -9 for certain faults.

  <details><summary>Answer</summary>

  A `kill -9` node is *gone*: it stops holding leases, stops
  responding, and its peers correctly conclude it is dead. A
  SIGSTOPped node is alive and will **resume**, still believing
  whatever it believed before — that it is the leader, that its lease
  is valid, that its in-flight write succeeded.

  That is the GC-pause / VM-migration / hypervisor-steal failure, and
  it is the one that breaks leases and produces two leaders. It is
  the reason for fencing tokens (DDIA ch. 8): a paused leader that
  wakes up must be *rejected by the storage layer*, because nothing
  it can check locally will tell it time has passed.

  Redis-Raft's nemesis list includes pauses explicitly, and issue #51
  (`EntryCacheAppend` assertion) has "Pause" alone in its fault
  column — a crash would not have found it.

  </details>

- [ ] You can state what elle cannot check, and where DST complements rather than competes.

  <details><summary>Answer</summary>

  Elle's own stated boundary (abstract): it detects "every anomaly in
  Adya et al's formalism (**except for predicates**)" — predicate
  anti-dependencies, the phantom-adjacent class, are out of scope
  because the workload observes keys, not predicates. It also cannot
  check a system that only exposes registers with the same power (see
  the previous answer), and being experimental it "can prove the
  presence of bugs, but not their absence" (Redis-Raft, Discussion).

  The complement is the fault axis, and the Redis-Raft report names
  it: "We have not explored single-node faults, such as filesystem
  corruption or the loss of un-fsynced data written to disk." That
  is precisely what this topic's `crash_matrix` sweeps — and the
  planted `NoSyncOnCommit` bug is caught in 4,980 of 5,000 seeds
  (99.6%) at roughly 200,000 simulated crash-recoveries per second.
  No `iptables` rule reaches it.

  Deterministic simulation also makes reproduction free (a seed) and
  the real-time order exact, where Jepsen must reason about clock
  uncertainty between machines.

  </details>

- [ ] You can name the actual root cause of Redis-Raft's total data loss and of Dgraph's write loss, and say what both have in common.

  <details><summary>Answer</summary>

  **Redis-Raft #14**: a missing re-entrancy check. Commands were
  intercepted and wrapped as `RAFT SET k v`; after commit they were
  unwrapped to `SET k v` and applied — and the interception code
  wrapped them *again*. With proxying off, followers rejected the
  re-wrapped op, so no follower ever applied anything and every
  failover produced an empty leader.

  **Dgraph 1.0.2**: a Go slice aliasing a mutable loop variable
  during predicate migration, so every triple in a batch ended up
  sharing the most recently iterated key. The report is explicit:
  "its cause turned out not to be a distributed systems problem at
  all!"

  What they share: neither is a flaw in Raft or in snapshot
  isolation. Both are ordinary programming bugs in the *integration*
  layer, and both were found by an end-to-end consistency oracle that
  did not know or care which layer it was testing. Test the claim,
  not the algorithm.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the redis-raft stale-read history.

  <details><summary>Answer</summary>

  No unfoldable answer — this one is the writing. For the stale-read
  history, the report hands you the shape: `T1: [:append 1 11]`
  completing 3.25 seconds before `T2: [:r 1 [5 8 9]]` begins. Write
  it as an Elle history with invoke/ok events and say which edge is
  missing (the real-time edge T1 → T2 that the wr edge contradicts),
  and note the fault column: **None**.

  For question 5, the thing the deterministic simulator makes trivial
  is exactly the thing Elle spends §7.5's budget on: in a simulator
  the total real-time order of every event is *known by
  construction*, so real-time edges are exact rather than inferred
  from wall-clock windows with uncertainty at both ends. What you
  give up is Step 1's whole premise — you are no longer testing an
  unmodified binary on a real kernel.

  </details>

## References

**Papers**
- Kingsbury & Alvaro — "Elle: Inferring Isolation Anomalies from
  Experimental Observations" (VLDB 2020,
  [arXiv:2003.10554](https://arxiv.org/abs/2003.10554)) — §1 for the
  NP-completeness and the Gecode history, §4 and §6 for the graph
  construction and the G0/G1c/G-single/G2 taxonomy, §6.1 for the
  non-cycle anomalies, §7 for the four case studies, §7.5 for the
  Knossos-vs-Elle measurements

**Reports** ([jepsen.io/analyses](https://jepsen.io/analyses)) — read
TWO in full:
- "Redis-Raft 1b3fbf6" (2020) — 21 issues; the Discussion section's
  tally and the per-issue fault column are the parts to reason over
- "Dgraph 1.0.2" (2018) — the bank test, the predicate-migration
  write loss, and the Go loop-variable cause

**Code**
- [elle](https://github.com/jepsen-io/elle) — the checker itself;
  not pinned in `resources/codebases.md`, so nothing here cites it
  by line
- turso @ `dd775bc` — the Jepsen-side integration, walked in
  [reading-turso-simulator.md](reading-turso-simulator.md)

| File | Lines | What |
|---|---|---|
| `testing/concurrent-simulator/elle.rs` | 1-7 | module doc: G0/G1/G2/G-Single, "export to EDN for analysis with elle-cli" |
| `testing/concurrent-simulator/elle.rs` | 18-30 | `ElleOp` — list-append and rw-register models side by side |
| `testing/concurrent-simulator/elle.rs` | 36-67 | `to_edn` — `[:append "k" v]` and `[:r "k" [1 2 3]]` |
| `testing/concurrent-simulator/elle.rs` | 72-79 | `ElleEventType` — Invoke / Ok / Fail / Info |
| `.github/workflows/elle.yml` | — | the check running in CI |
