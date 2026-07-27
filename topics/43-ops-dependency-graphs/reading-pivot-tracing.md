# Pivot Tracing: a join operator over causality

This is the database paper in an operations topic, and it should be read as one. Pivot Tracing's
contribution is a **relational operator** — the happened-before join — plus an evaluation strategy
and a set of query rewrite rules. Swap the vocabulary and you are reading about a distributed join
whose predicate is Lamport's `→`, evaluated by pushing state along the request instead of shipping
tuples to a coordinator, with projection, selection and aggregation pushed down to the sources.
The measured effect of the pushdown is a hundredfold reduction in tuple traffic. Topic 10 would
recognise every move.

## The problem in one sentence

**The metric you need was not the one anybody thought to record, and the fields you want to group
it by are measured in a different process on a different machine.**

## The concepts, step by step

### Step 1 — Two failures of ordinary monitoring

**"One size does not fit all."** What gets logged is decided a priori, by developers, and "there is
a mismatch between the expectations and incentives of the developer and the needs of operators and
users." The paper's evidence is a wall of Apache issue-tracker citations: users asking for new
metrics, new aggregations, new breakdowns of existing metrics — and being refused. And when
metrics *are* added, everybody pays: "HBase SchemaMetrics were introduced to aid developers, but
all users of HBase pay the 10% performance overhead they incur."

**Crossing boundaries.** The root cause and the symptom live in different processes, different
tiers, and are visible to different people. The paper quotes a Mesos issue: "The actually
interesting / useful information is hidden in one of four or five different places, potentially
spread across as many different machines. This leads to unpleasant and repetitive searching
through logs looking for a clue to what went wrong. (…) There's a lot of information that is
hidden in log files and is very hard to correlate."

Dynamic instrumentation (DTrace, Fay, SystemTap) fixes the first problem and not the second: those
probes are side-effect-free by design, so they cannot share information across boundaries.

### Step 2 — Tracepoints and a query language

A **tracepoint** is a location in the code where instrumentation can be installed; when execution
reaches it, it emits a tuple of exported variables plus host, timestamp, process id and name.
Tracepoint invocations are therefore *streaming datasets*, and Pivot Tracing queries are relational
queries over them:

```
   From    use tuples from a set of tracepoints
   Union   combine events from several
   σ       Where e.Size < 10
   Π       Select e.User, e.Host
   A       Select SUM(e.Cost)
   G / GA  GroupBy e.User
   ⋈       Join d In Disk On d -> e          the happened-before join
```

plus temporal filters `MostRecent`, `MostRecentN`, `First`, `FirstN`.

### Step 3 — The happened-before join

```
   Q1 ⋈ Q2  produces  t1t2  for all t1 ∈ Q1, t2 ∈ Q2  such that  t1 → t2
```

where `a → b` means "the occurrence of `a` causally preceded the occurrence of `b`, and they
occurred as part of the execution of the same request". If `a` and `b` are in different requests,
or in parallel threads that never communicate, there is no join.

Figure 3 of the paper is the one to internalise: one execution triggering tracepoints A, B and C
several times, and the tuples produced by `A`, `A ⋈ B`, `B ⋈ C`, and `(A ⋈ B) ⋈ C`. Work it by
hand once and the operator stops being mysterious.

The claim for it is precise: "Happened-before join substantially improves our ability to perform
root cause analysis by giving us visibility into the relationships *between* events in the
system." And the honest scoping note: "Pivot Tracing is designed to efficiently support
happened-before joins, but does not optimize more general joins such as equijoins."

### Step 4 — Advice, and how a query becomes instrumentation

Queries compile to **advice**, woven into tracepoints at runtime. Five primitives:

```
   OBSERVE   construct a tuple from the tracepoint's exported variables
   UNPACK    retrieve tuples packed by earlier advice in this execution
   FILTER    evaluate a predicate
   PACK      make tuples available to later advice in this execution
   EMIT      output a tuple for global aggregation
```

The compilation is mechanical: a `From` clause becomes `OBSERVE`; each `Join` becomes an `UNPACK`
in the downstream advice and a `PACK` in the upstream one; `Where` becomes `FILTER`; `Select`
becomes `EMIT`. `PACK` has the special cases `FIRST` and `RECENT` (and their `N` variants) that
implement the temporal filters.

The advice API is deliberately restricted: "advice code has no jumps or recursion, and is
guaranteed to terminate." A safety property you would want from anything you weave into a
production system at runtime.

### Step 5 — Baggage, and why the join is evaluated in-band

The naive way to evaluate `⋈` is the way Magpie did: ship all tuples to a coordinator and join
them there. Figure 6a. It works and it is expensive.

Pivot Tracing instead uses **baggage**: "a per-request container for tuples that is propagated
alongside a request as it traverses thread, application and machine boundaries. `PACK` and `UNPACK`
store and retrieve tuples from the current request's baggage. Tuples follow the request's execution
path and therefore explicitly capture the happened-before relationship."

So the join happens *in situ*, during execution, at the downstream tracepoint. No coordinator, no
cross-cluster tuple shuffle for the join itself — only the final aggregates are emitted. Figure 6b.

(Baggage is a generalisation of X-Trace's and Dapper's metadata propagation. If you have met the
W3C `baggage` header in OpenTelemetry, this is where it comes from.)

The risk is named rather than hidden: "Pivot Tracing does not inherently bound the number of packed
tuples and potentially accumulates a new tuple for every tracepoint invocation. However, we liken
this to database queries that inherently risk a full table scan — our optimizations mean that in
practice, this is an unlikely event."

### Step 6 — Pushdown, and the hundredfold

Table 3 is a set of query rewrite rules, and if you have read topic 10 you already know them:

```
   Π_{p,q}(P ⋈ Q)   →   Π_p(P) ⋈ Π_q(Q)
   σ_p(P ⋈ Q)       →   σ_p(P) ⋈ Q
   σ_q(P ⋈ Q)       →   P ⋈ σ_q(Q)
   A_p(P ⋈ Q)       →   Combine_p(A_p(P)) ⋈ Q
   GA_p(P ⋈ Q)      →   G_p Combine_p(GA_p(P)) ⋈ Q
```

"Pivot Tracing rewrites queries to minimize the number of tuples packed... push projection,
selection, and aggregation terms as close as possible to source tracepoints." `Combine` is the
aggregator's combiner function — `Sum` for `Count` — which is the same partial-aggregation trick as
a map-side combiner.

Two measured effects. Intermediate aggregation within each process: "Q2 from §2 is reduced from
approximately **600 tuples per second to 6 tuples per second** from each DataNode." And a reduction
in tuples carried in the baggage, from the join rewrites.

A hundredfold reduction in data movement, from predicate and aggregate pushdown, in a monitoring
system. That is the topic-10 lesson arriving from an unexpected direction: **the gap between what
a user writes and what should actually run is worth closing automatically, wherever the query
happens to live.**

### Step 7 — What this means for a graph engine

Two things worth carrying into capstone M43.

The happened-before join is a *graph* operator wearing relational clothes: `t1 → t2` is
reachability in the causal DAG of a request. Implemented over a trace store, it is a
variable-length pattern match with a temporal constraint — precisely what a Cypher engine already
knows how to plan. The interesting question is not how to evaluate it but **where**: in-band
during execution (Pivot Tracing's answer, cheap but requires instrumentation everywhere) or
post-hoc over stored traces (the answer available to a database, expensive but requires nothing of
the application).

And the pushdown rules apply to the post-hoc version unchanged. Exercise 7 asks you to implement
both the join and the rewrites over this topic's trace set and measure the reduction.

## How to read the paper (with the concepts in hand)

- **§1–2.** The motivating HDFS scenario — six workloads, one of them causing the problem, and no
  existing metric that can tell you which. Read Q2, the query that solves it, before reading how.
- **§2.3.** The two challenges. The Apache issue-tracker citations are worth a minute; they are the
  empirical case that this is a real problem.
- **§3 + Table 1.** The query language. Then §3's happened-before join definition and **Figure 3** —
  work the example by hand.
- **§3 Advice + Table 2 + Figures 4–5.** The five primitives and how a query compiles to them.
- **§4 Baggage + Figure 6.** In-situ versus centralised evaluation. Figure 6 is the whole argument
  in one picture.
- **§4 + Table 3.** The rewrite rules and the 600 → 6 tuples/s result.
- **§5.** Implementation: runtime weaving, the agent in every process, one-second publish interval.
- **After the paper.** Do exercise 7 — implement `Q1 ⋈ Q2` over this topic's traces, then the
  Table 3 rewrites, and measure the tuple reduction.

## Questions to answer in notes.md

1. State the happened-before join as a graph query rather than a relational one. What is the graph,
   what is the path predicate, and which topic-11 execution model would you use for it?
2. Baggage evaluates the join in-band, during execution. List what that buys and what it costs,
   and name the situation in which post-hoc evaluation over stored traces is strictly better.
3. Table 3's rewrites are textbook pushdown. For each of the five rules, say what would go wrong
   if you applied it without checking a precondition.
4. The paper likens unbounded baggage growth to "database queries that inherently risk a full table
   scan". Extend the analogy: what is the equivalent of a query planner's cost estimate here, and
   what would an admission-control policy (topic 35) look like?
5. Pivot Tracing needs tracepoints everywhere and a baggage channel through every boundary. Given
   OpenTelemetry's `baggage` header exists, what is stopping you from running Q2 on your own stack
   tomorrow? Be specific.

## Done when

- [ ] You can write the happened-before join's definition and work Figure 3's example.
- [ ] You can name the five advice primitives and say how a query compiles to them.
- [ ] You can explain baggage and draw Figure 6's two evaluation strategies.
- [ ] You can state three of Table 3's rewrite rules and the 600 → 6 result.
- [ ] You can argue in-band versus post-hoc evaluation for a graph engine.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Mace, Roelke, Fonseca. *Pivot Tracing: Dynamic Causal Monitoring for Distributed Systems.*
  SOSP 2015 (Best Paper) — [PDF](https://cs.brown.edu/~rfonseca/pubs/mace15pivot.pdf).
- Lamport. *Time, Clocks, and the Ordering of Events in a Distributed System.* CACM 1978 — the `→`
  the join is built on.
- Erlingsson, Peinado, Peter, Budiu. *Fay: Extensible Distributed Tracing from Kernels to Clusters.*
  SOSP 2011 — the dynamic-instrumentation ancestor, and the source of the pushdown optimizations.
- Barham, Donnelly, Isaacs, Mortier. *Using Magpie for Request Extraction and Workload Modelling.*
  OSDI 2004 — the centralised join strategy Figure 6a describes.
- Topic 10 (query planning) — the rewrite rules; topic 11 (execution models) — how you would
  actually run the join; topic 35 (overload control) — what an admission policy for a monitoring
  query would need.
