# Pivot Tracing: a join operator over causality

This is the database paper in an operations topic, and it should be read as one. Pivot Tracing's
contribution is a **relational operator** — the happened-before join — plus an evaluation strategy
and a set of query rewrite rules. Swap the vocabulary and you are reading about a distributed join
whose predicate is Lamport's `→`, evaluated by pushing state along the request instead of shipping
tuples to a coordinator, with projection, selection and aggregation pushed down to the sources. The
measured effect of the pushdown is a hundredfold reduction in tuple traffic. Topic 10 would
recognise every move.

This is a paper, not a codebase, so every claim below is anchored to the section, table or figure of
*Pivot Tracing: Dynamic Causal Monitoring for Distributed Systems* (Mace, Roelke, Fonseca, SOSP 2015,
Best Paper) that states it; each was re-checked against the PDF while writing this chapter.

## The problem in one sentence

**The metric you need was not the one anybody thought to record, and the fields you want to group it
by are measured in a different process on a different machine.**

## The concepts, step by step

### Step 1 — Two failures of ordinary monitoring

> **In:** nothing yet — this step is the motivation.
> **Out:** the two problems (§2.3) that every later step attacks: *what* gets recorded is fixed too
> early, and the *cause* lives across a boundary the record cannot cross.

**"One size does not fit all"** (§2 heading). What gets logged is decided a priori, by developers,
and "there is a mismatch between the expectations and incentives of the developer and the needs of
operators and users." The paper's evidence is a wall of Apache issue-tracker citations: users asking
for new metrics, new aggregations, new breakdowns of existing metrics — and being refused. And when
metrics *are* added, everybody pays: "HBase SchemaMetrics were introduced to aid developers, but all
users of HBase pay the 10% performance overhead they incur" (§2.3).

**Crossing boundaries.** The root cause and the symptom live in different processes, different tiers,
and are visible to different people. The paper quotes a Mesos issue (§2.3): "The actually
interesting / useful information is hidden in one of four or five different places, potentially
spread across as many different machines. This leads to unpleasant and repetitive searching through
logs looking for a clue to what went wrong. (…) There's a lot of information that is hidden in log
files and is very hard to correlate."

Dynamic instrumentation (DTrace, Fay, SystemTap) fixes the first problem and not the second. The
paper's own framing (§1): the limitation "is fundamental" — those probes are side-effect-free by
design, so "neither Fay nor DTrace can affect the monitored system to propagate the monitoring
context" across an address-space or OS boundary. Which is exactly what the second problem needs.

Why it matters: the two problems are orthogonal, and Pivot Tracing is the first system that answers
both — dynamic queries (problem one) whose operator spans boundaries (problem two).

### Step 2 — Tracepoints and a query language

> **In:** the "record it dynamically" half of Step 1.
> **Out:** the data model (**tracepoint invocations are streaming datasets**) and the relational
> query language over them (§3, Table 1) — so that Step 3's join has operands.

A **tracepoint** is a location in the code where instrumentation can be installed; when execution
reaches it, it emits a tuple of exported variables plus host, timestamp, process id and name.
Tracepoint invocations are therefore *streaming datasets*, and Pivot Tracing queries are relational
queries over them (§3, Table 1):

```
   From    use tuples from a set of tracepoints
   Union   combine events from several
   σ       Where e.Size < 10
   Π       Select e.User, e.Host
   A       Select SUM(e.Cost)
   G / GA  GroupBy e.User
   ⋈       Join d In Disk On d -> e          the happened-before join
```

plus temporal filters `MostRecent`, `MostRecentN`, `First`, `FirstN` (Table 1).

Why it matters: framing instrumentation output as a *relation* is what lets a query optimizer touch
it at all — every rewrite in Step 6 is legal only because these are relational operators.

### Step 3 — The happened-before join

> **In:** two tracepoint queries `Q1` and `Q2` from Step 2.
> **Out:** the paper's one novel operator, defined exactly (§3), plus the scoping note that stops
> you mistaking it for a general join.

```
   Q1 ⋈ Q2  produces  t1t2  for all t1 ∈ Q1, t2 ∈ Q2  such that  t1 → t2
```

where `a → b` means "the occurrence of `a` causally preceded the occurrence of `b`, and they
occurred as part of the execution of the same request" — Lamport's happened-before, restricted to a
single request. If `a` and `b` are in different requests, or in parallel threads that never
communicate, there is no join.

Figure 3 is the one to internalise: one execution triggering tracepoints A, B and C several times,
and the tuples produced by `A`, `A ⋈ B`, `B ⋈ C`, and `(A ⋈ B) ⋈ C`. Work it by hand once and the
operator stops being mysterious.

The claim for it is precise (§3): "Happened-before join substantially improves our ability to perform
root cause analysis by giving us visibility into the relationships *between* events in the system."
And the honest scoping note, which is the reason the whole system can be efficient: "Pivot Tracing is
designed to efficiently support happened-before joins, but does not optimize more general joins such
as equijoins." The operator is narrow on purpose.

Why it matters: this operator — not "dynamic instrumentation", which predates the paper — is the
contribution. It joins on *causal reachability within one request*, and everything downstream exists
to evaluate it cheaply.

### Step 4 — Advice, and how a query becomes instrumentation

> **In:** the query and its `⋈` from Step 3.
> **Out:** the five-primitive intermediate form (**advice**, §3, Table 2) that a query compiles to
> and that gets woven into tracepoints at runtime — the executable form the join takes.

Queries compile to **advice**, woven into tracepoints at runtime. Five primitives (§3, Table 2):

```
   OBSERVE   construct a tuple from the tracepoint's exported variables
   UNPACK    retrieve tuples packed by earlier advice in this execution
   FILTER    evaluate a predicate
   PACK      make tuples available to later advice in this execution
   EMIT      output a tuple for global aggregation
```

The compilation is mechanical: a `From` clause becomes `OBSERVE`; each `Join` becomes an `UNPACK` in
the downstream advice and a `PACK` in the upstream one; `Where` becomes `FILTER`; `Select` becomes
`EMIT`. `PACK` has the special cases `FIRST` and `RECENT` (and their `N` variants) that implement the
temporal filters from Table 1.

The advice API is deliberately restricted (§3): "advice code has no jumps or recursion, and is
guaranteed to terminate." A safety property you would want from anything you weave into a production
system at runtime.

Why it matters: `PACK`/`UNPACK` are where the join is realised — the upstream side stashes its tuples
and the downstream side retrieves them, which is only possible because of the channel in Step 5.

### Step 5 — Baggage, and why the join is evaluated in-band

> **In:** the `PACK`/`UNPACK` pair from Step 4, which need a channel between them.
> **Out:** **baggage** (§4) — the per-request container that carries packed tuples along the
> execution path — and the reason the join runs in-situ instead of at a coordinator (Figure 6).

The naive way to evaluate `⋈` is the way Magpie did: ship all tuples to a coordinator and join them
there (Figure 6a). It works and it is expensive.

Pivot Tracing instead uses **baggage** (§4): "a per-request container for tuples that is propagated
alongside a request as it traverses thread, application and machine boundaries. `PACK` and `UNPACK`
store and retrieve tuples from the current request's baggage. Tuples follow the request's execution
path and therefore explicitly capture the happened-before relationship."

So the join happens *in situ*, during execution, at the downstream tracepoint (Figure 6b). No
coordinator, no cross-cluster tuple shuffle for the join itself — only the final aggregates are
emitted.

Baggage is a generalisation of X-Trace's and Dapper's metadata propagation (§4). If you have met the
W3C `baggage` header in OpenTelemetry, this is where it comes from.

The risk is named rather than hidden (§4): "Pivot Tracing does not inherently bound the number of
packed tuples and potentially accumulates a new tuple for every tracepoint invocation. However, we
liken this to database queries that inherently risk a full table scan — our optimizations mean that
in practice, this is an unlikely event."

Why it matters: evaluating the join *in-band* is what makes the whole system cheap enough to leave on
— but it requires a propagation channel through every boundary, which is the price and the deployment
constraint.

### Step 6 — Pushdown, and the hundredfold

> **In:** the in-band join of Step 5, whose two costs are *tuples emitted for aggregation* and
> *tuples packed into baggage*.
> **Out:** the §4 optimizations that cut each cost separately — process-level aggregation for the
> first (the 600 → 6 result), Table 3 rewrites for the second — and why keeping them distinct
> matters.

Table 3 is a set of query rewrite rules, and if you have read topic 10 you already know them:

```
   Π_{p,q}(P ⋈ Q)   →   Π_p(P) ⋈ Π_q(Q)
   σ_p(P ⋈ Q)       →   σ_p(P) ⋈ Q
   σ_q(P ⋈ Q)       →   P ⋈ σ_q(Q)
   A_p(P ⋈ Q)       →   Combine_p(A_p(P)) ⋈ Q
   GA_p(P ⋈ Q)      →   G_p Combine_p(GA_p(P)) ⋈ Q
```

"Pivot Tracing rewrites queries to minimize the number of tuples packed... push projection,
selection, and aggregation terms as close as possible to source tracepoints" (§4). `Combine` is the
aggregator's combiner function — `Sum` for `Count` — the same partial-aggregation trick as a map-side
combiner.

There are **two distinct costs, cut by two distinct mechanisms**, and the paper is careful to keep
them apart — so keep them apart too:

1. **Tuples emitted for global aggregation.** Reduced by *process-level (intermediate) aggregation*,
   not by the Table 3 rewrites: Pivot Tracing "aggregates the emitted tuples within each process and
   reports results globally at a regular interval, e.g., once per second. Process-level aggregation
   substantially reduces traffic for emitted tuples; Q2 from §2 is reduced from approximately **600
   tuples per second to 6 tuples per second** from each DataNode" (§4). *That* is the hundredfold.
2. **Tuples packed into the baggage.** Reduced by the Table 3 rewrites, which push projection,
   selection and aggregation toward the sources so fewer tuples ride along the request.

Conflating the two — attributing the 600 → 6 to the join rewrites — is a common misreading; the paper
credits it to intermediate aggregation. Both are pushdown in spirit; they act on different cost
metrics.

The topic-10 lesson arriving from an unexpected direction: **the gap between what a user writes and
what should actually run is worth closing automatically, wherever the query happens to live** — even
when "where it lives" is woven into a running production system.

### Step 7 — What this means for a graph engine

> **In:** the join (Step 3), its in-band evaluation (Step 5) and its rewrites (Step 6).
> **Out:** two things to carry into capstone M43 — the join is a graph reachability operator, and the
> real design axis is *where* it runs, not how.

The happened-before join is a *graph* operator wearing relational clothes: `t1 → t2` is reachability
in the causal DAG of a request. Implemented over a trace store, it is a variable-length pattern match
with a temporal constraint — precisely what a Cypher engine already knows how to plan. The interesting
question is not how to evaluate it but **where**: in-band during execution (Pivot Tracing's answer,
cheap but requires instrumentation everywhere) or post-hoc over stored traces (the answer available
to a database, expensive but requires nothing of the application).

And the pushdown rules apply to the post-hoc version unchanged. Exercise 7 asks you to implement both
the join and the rewrites over this topic's trace set and measure the reduction.

Why it matters: Pivot Tracing and a trace database are the same query evaluated at two ends of a
spectrum; knowing that is what lets you choose the point on it that your deployment can actually pay
for.

## How to read the paper (with the concepts in hand)

- **§1–2.** The motivating HDFS scenario — six workloads, one of them causing the problem, and no
  existing metric that can tell you which. Read Q2, the query that solves it, before reading how.
- **§2.3.** The two challenges. The Apache issue-tracker citations are worth a minute; they are the
  empirical case that this is a real problem.
- **§3 + Table 1.** The query language. Then §3's happened-before join definition and **Figure 3** —
  work the example by hand.
- **§3 Advice + Table 2 + Figures 4–5.** The five primitives and how a query compiles to them.
- **§4 Baggage + Figure 6.** In-situ versus centralised evaluation. Figure 6 is the whole argument in
  one picture.
- **§4 + Table 3.** The rewrite rules, and the two cost metrics: emitted tuples (600 → 6 via
  intermediate aggregation) versus packed tuples (Table 3 rewrites). Keep them straight.
- **§5.** Implementation: runtime weaving, the agent in every process, one-second publish interval.
- **After the paper.** Do exercise 7 — implement `Q1 ⋈ Q2` over this topic's traces, then the Table 3
  rewrites, and measure the tuple reduction.

## Questions to answer in notes.md

1. State the happened-before join as a graph query rather than a relational one. What is the graph,
   what is the path predicate, and which topic-11 execution model would you use for it?
2. Baggage evaluates the join in-band, during execution. List what that buys and what it costs, and
   name the situation in which post-hoc evaluation over stored traces is strictly better.
3. Table 3's rewrites are textbook pushdown. For each of the five rules, say what would go wrong if
   you applied it without checking a precondition.
4. The paper likens unbounded baggage growth to "database queries that inherently risk a full table
   scan" (§4). Extend the analogy: what is the equivalent of a query planner's cost estimate here,
   and what would an admission-control policy (topic 35) look like?
5. Pivot Tracing needs tracepoints everywhere and a baggage channel through every boundary. Given
   OpenTelemetry's `baggage` header exists, what is stopping you from running Q2 on your own stack
   tomorrow? Be specific.

## Done when

Answer each before unfolding it.

- [ ] You can write the happened-before join's definition and work Figure 3's example.

  <details><summary>Answer</summary>

  `Q1 ⋈ Q2` produces the concatenated tuple `t1t2` for every `t1 ∈ Q1` and `t2 ∈ Q2` with `t1 → t2`
  (§3), where `→` is Lamport happened-before *restricted to a single request*: `a → b` iff `a`
  causally precedes `b` and both are part of the same request's execution. Tuples in different
  requests, or in parallel non-communicating threads, do not join.

  Figure 3 shows one execution hitting A, B, C repeatedly; working it by hand you should be able to
  produce the tuple sets for `A`, `A ⋈ B`, `B ⋈ C`, and `(A ⋈ B) ⋈ C`, and see that each downstream
  tuple pairs only with the upstream tuples that causally preceded it on that request. The scoping
  note matters: the system optimizes *this* join and "does not optimize more general joins such as
  equijoins" (§3).

  </details>

- [ ] You can name the five advice primitives and say how a query compiles to them.

  <details><summary>Answer</summary>

  OBSERVE, UNPACK, FILTER, PACK, EMIT (§3, Table 2). Compilation is mechanical: `From` → OBSERVE;
  each `Join` → an UNPACK in the downstream advice paired with a PACK in the upstream advice; `Where`
  → FILTER; `Select` → EMIT. PACK's `FIRST`/`RECENT` special cases (and their `N` variants) implement
  Table 1's temporal filters.

  The API is restricted on purpose: "advice code has no jumps or recursion, and is guaranteed to
  terminate" (§3) — the safety property you need before weaving code into a live system. The PACK on
  one side and UNPACK on the other are the two halves of the happened-before join, connected by the
  baggage channel.

  </details>

- [ ] You can explain baggage and draw Figure 6's two evaluation strategies.

  <details><summary>Answer</summary>

  Baggage is "a per-request container for tuples that is propagated alongside a request as it
  traverses thread, application and machine boundaries" (§4); PACK/UNPACK write and read it, so tuples
  follow the execution path and "explicitly capture the happened-before relationship." It generalises
  X-Trace/Dapper metadata propagation and is the ancestor of OpenTelemetry's `baggage` header.

  Figure 6a is the Magpie-style strategy: ship every tuple to a coordinator and join there — correct
  but a cross-cluster shuffle. Figure 6b is Pivot Tracing's: the join runs in-situ at the downstream
  tracepoint using baggage, so only final aggregates leave the process. The trade is a propagation
  channel through every boundary in exchange for eliminating the shuffle.

  </details>

- [ ] You can state three of Table 3's rewrite rules and the 600 → 6 result.

  <details><summary>Answer</summary>

  Three rules (§4, Table 3): projection distributes over the join, `Π_{p,q}(P ⋈ Q) → Π_p(P) ⋈
  Π_q(Q)`; a selection on `P`'s columns pushes to `P`, `σ_p(P ⋈ Q) → σ_p(P) ⋈ Q`; aggregation pushes
  through with a combiner, `A_p(P ⋈ Q) → Combine_p(A_p(P)) ⋈ Q`. They push projection, selection and
  aggregation toward the source tracepoints, cutting the number of tuples *packed* into baggage.

  The 600 → 6 result is a *different* cost metric and a *different* mechanism: it is the number of
  tuples *emitted for global aggregation*, cut by process-level (intermediate) aggregation — "Q2 from
  §2 is reduced from approximately 600 tuples per second to 6 tuples per second from each DataNode"
  (§4). Do not attribute the hundredfold to the Table 3 rewrites; the paper credits intermediate
  aggregation.

  </details>

- [ ] You can argue in-band versus post-hoc evaluation for a graph engine.

  <details><summary>Answer</summary>

  The happened-before join is reachability in a request's causal DAG, so it can run two ways.
  *In-band* (Pivot Tracing) evaluates it during execution via baggage: cheap at query time, only
  aggregates leave each process, but it requires tracepoints and a propagation channel everywhere and
  can only answer queries installed before the request ran. *Post-hoc* over a stored trace set asks
  nothing of the application and can answer questions you thought of after the fact, but pays to
  store and scan the traces and re-derive causality.

  Crucially the Table 3 pushdown rules apply to the post-hoc version unchanged, so the two are the
  same query at two ends of a spectrum — you pick the point your deployment can pay for. Exercise 7
  builds the post-hoc version and measures the reduction.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five questions restate the join as a graph query (topic 11 execution models), weigh in-band vs
  post-hoc evaluation, probe the precondition behind each Table 3 rewrite, extend the "full table
  scan" analogy toward admission control (topic 35), and ask what actually blocks running Q2 on your
  own stack given OpenTelemetry baggage exists.

  Answer them against the anchors above — §3 for the operator and advice, §4 for baggage and the two
  cost metrics — not from memory. The recurring lesson is topic 10's: automatically close the gap
  between the written query and what runs, wherever the query lives.

  </details>

## References

- Mace, Roelke, Fonseca. *Pivot Tracing: Dynamic Causal Monitoring for Distributed Systems.* SOSP
  2015 (Best Paper) — [PDF](https://cs.brown.edu/~rfonseca/pubs/mace15pivot.pdf). Section, table and
  figure citations in this chapter refer to this paper.
- Lamport. *Time, Clocks, and the Ordering of Events in a Distributed System.* CACM 1978 — the `→`
  the join is built on.
- Erlingsson, Peinado, Peter, Budiu. *Fay: Extensible Distributed Tracing from Kernels to Clusters.*
  SOSP 2011 — the dynamic-instrumentation ancestor, and the source of the pushdown optimizations.
- Barham, Donnelly, Isaacs, Mortier. *Using Magpie for Request Extraction and Workload Modelling.*
  OSDI 2004 — the centralised join strategy Figure 6a describes.
- Topic 10 (query planning) — the rewrite rules; topic 11 (execution models) — how you would actually
  run the join; topic 35 (overload control) — what an admission policy for a monitoring query would
  need.
