# Agent-first data systems: speculation is a workload with redundancy you can price

Liu, Ponnapalli, Shankar, Zeighami, Zhu, Agarwal, Chen, Suwito, Yuan,
Stoica, Zaharia, Cheung, Crooks, Gonzalez & Parameswaran ("Supporting Our AI
Overlords: Redesigning Data Systems to be Agent-First", CIDR'26; arXiv
2509.00997) turn the topic around. The previous chapters put an LLM *inside*
the query plan; this one puts it *in front of* the database, as the client —
and argues that agents will be the dominant workload. Their queries do not
look like a human's or an application's: high volume, mostly exploratory,
heavily redundant, and steerable if the database talks back. The paper is a
vision paper with two small measured case studies, and this chapter treats
it that way: it builds the vocabulary, checks the two measured tables
against themselves, and connects each proposal to an optimizer idea that
already exists — multi-query optimization, approximate query processing,
caching — and to the lane of this topic's bench that prices it.

## The problem in one sentence

When an army of LLM agents issues hundreds of mostly exploratory,
overlapping queries to solve one task, what should a database do
differently — and how much of that work is redundant?

## The concepts, step by step

### Step 1 — agentic speculation and its four traits

> **In:** nothing yet — an LLM agent that can issue SQL.
> **Out:** the workload the paper names, and the four properties Steps 2–6
> exploit.

An **agent** is an LLM that acts — issues tool calls (here, queries),
observes results, and decides its next step — on a user's behalf.
**Grounding** is knowledge of the data and the system it lives in; agents
lack it and make up for it by trying many things. **Agentic speculation** is
"this combination of discovery and solution formulation" (§1): metadata
exploration (schemas, column statistics), partial solutions, validation. Its
four traits (§1): **scale** (high request throughput, in sequence or in
parallel), **heterogeneity** (requests range from metadata browsing to full
solutions), **redundancy** (many requests access similar data or repeat
operations), and **steerability** (a database that talks back can redirect
the agent).

Why it matters: each trait maps to an optimizer technique a database already
has — scale to admission control, heterogeneity to approximate answers,
redundancy to sharing and caching, steerability to cost feedback.

### Step 2 — case study 1: more attempts help, and repeat each other

> **In:** the BIRD text-to-SQL benchmark on DuckDB, GPT-4o-mini and
> Qwen2.5-Coder-7B (§2).
> **Out:** two measured properties — speculation raises success, and its
> sub-plans are mostly duplicates.

Parallel speculation: K independent "field" agents attempt each task and an
agent "in charge" picks one answer (Figure 1a); sequential: one agent keeps
querying until satisfied (Figure 1b). The paper reports success "increases as
a function of requests, and by 14%–70% in our case study" (§2). For
redundancy it decomposes the 50 attempts' query plans into **sub-plans**
(sub-expressions of a plan) and counts total vs distinct across the full
BIRD dataset (Figure 2): "the number of distinct sub-plans of each size is
often a small fraction of less than 10-20% of the total".

Both numbers are read off plots; the paper prints no table for them, and
this chapter cannot verify them beyond what the text states. (BIRD's labels
are themselves wrong on about half of its problems by
[reading-text-to-sql-broken.md](reading-text-to-sql-broken.md)'s count — a
caveat on any success rate measured on it.)

Why it matters: "<10–20% distinct" means 80–90% of the work across attempts
is repeated. That is the premise of multi-query optimization, and of lane 1d
here: ten agents probing overlapping 2,000-row subsets of 10,000 rows request
20,000 row-evaluations, of which 8,878 rows are distinct; a shared
(document, predicate) cache cuts oracle calls from 26,150 to **11,615**
(simulated, exact by construction).

### Step 3 — case study 2: phases, and how much hints save

> **In:** 44 sequential traces of OpenAI's o3 solving 22 multi-database tasks
> twice each (§2), labelled by activity.
> **Out:** Table 1's per-activity counts with and without hints, checked.

Each action in a trace was labelled as exploring tables, exploring specific
columns, attempting part of the query, or attempting all of it (Figure 3:
exploration first, but phases overlap throughout). The same 22 tasks were
re-run with a human-written **hint** in the prompt (which column holds the
needed information). Table 1:

| activity | no hints | with hints | reduction (paper) | reduction (recomputed) |
|---|---|---|---|---|
| exploring tables | 3.44 | 2.95 | −14.2% | −14.2% |
| exploring specific columns | 3.56 | 2.57 | −27.7% | −27.8% |
| attempting part of the query | 4.28 | 2.71 | −36.6% | −36.7% |
| attempting entire query | 1.26 | 1.05 | −16.6% | −16.7% |
| all SQL queries | 12.67 | 10.38 | −18.1% | −18.1% |

The recomputed column is (with − without) / without; three rows differ in
the last digit, consistent with truncation instead of rounding. Two things
the table does not support as stated: the four activity rows sum to 12.54
and 9.28, not 12.67 and 10.38, so "all SQL queries" counts actions outside
the four labels (unexplained); and the headline "reducing queries by >20%,
depending on phase" holds for two of the four phases — the all-query
reduction is **18.1%**.

Why it matters: grounding is worth about a fifth of an agent's queries here.
A database that supplies grounding proactively (Step 4) is competing with a
human writing hints.

### Step 4 — the architecture: probes, briefs, sleeper agents

> **In:** the four traits (Step 1) and the measured savings (Steps 2–3).
> **Out:** the paper's proposed interface — the objects Step 5's optimizer
> operates on.

The paper replaces queries with **probes** (§3–4): one or more SQL queries
plus a **brief**, natural-language statements of the probe's goal, its phase
(exploration or solution formulation), the accuracy it needs and priorities
among its queries. Probes may also search for a phrase anywhere in data or
metadata — "semantic similarity operators, beyond LIKE" (§4.1). An in-database
**probe interpreter** reads the brief; **sleeper agents** inside the system
gather auxiliary information (related tables, why a result was empty, cost
estimates) and return it alongside answers (§4.2). The database aims to
**satisfice** — "produce reasonable results that address needs, without
evaluating the query completely" (§3).

Why it matters: a brief is a workload hint the optimizer can act on — the
same role LIMIT, `APPROX_COUNT_DISTINCT` or a query priority play today, but
expressed in natural language and attached to every request.

### Step 5 — probe optimization: what to run, not just how

> **In:** probes and briefs from Step 4.
> **Out:** the optimizer's new objective and its toolbox, each tied to an
> existing technique and to a lane of this topic.

The objective changes (§5.2): not throughput but "minimize the total time
spent on answering the field agents' probes given available computational
resources", which forces a trade — too coarse an answer now causes more
follow-up probes later. Within a batch (§5.2.1) the database decides *what*
to run — pruning queries irrelevant to the brief, or dropping probe P′ when
its extra rows over P are irrelevant — and executes with **multi-query
optimization** (sharing work across queries), **approximate query
processing** (answering from samples or sketches with an error bound) and
**caching of partial results**. Across turns (§5.2.2) it can drop probes
that add no information and materialize joins the agent's history predicts.

Why it matters: these are the classic techniques of topics 10 and 26 with a
new cost model in which the client's *next* request is part of the cost.
Lane 1d prices the sharing; lane 1e (LIMIT pushdown, 83× fewer calls) prices
the simplest form of "evaluate only enough".

### Step 6 — storage: an agentic memory store and branched updates

> **In:** the redundancy of Step 2, now across time.
> **Out:** the two storage proposals, and the one measured number behind
> the second.

An **agentic memory store** (§6.1) is a persistent, queryable "semantic
cache that provides grounding": results of prior probes, partial solutions,
and metadata such as column encodings and date ranges — with the open
problems of keeping it consistent with schema changes and of sharing it
across users without leaking. **Branched updates** (§6.2): agents explore
"what-if" transactions, so the database should support many forks that are
logically isolated but physically shared ("multi-world isolation"). The one
number: at Neon, "agents created 20× more branches, and performed 50× more
rollbacks, relative to humans" (§6.2) — an observation without a table or
method.

Why it matters: the memory store is the agent-facing version of topic 38's
agent memory, and branching is copy-on-write storage (topic 28's Neon) asked
to scale by 20×.

## How to read the paper with these concepts in hand

| section | what to take from it | step |
|---|---|---|
| Abstract, §1 | agentic speculation; scale, heterogeneity, redundancy, steerability | 1 |
| §2, Figures 1–3, Table 1 | the two case studies; 14–70%; <10–20% distinct; hints | 2, 3 |
| §3, Figure 4 | the architecture | 4 |
| §4.1–4.2 | probes, briefs, semantic search over metadata, sleeper agents | 4 |
| §5.1–5.2 | probe optimization within and across turns | 5 |
| §6.1–6.2 | agentic memory store; branched updates; 20× / 50× at Neon | 6 |

## Questions to answer

1. Satisficing needs a stopping rule. Which of this topic's lanes is the
   simplest one, and what would a brief add to it?
2. Redundancy across agents is "less than 10–20% distinct" sub-plans. What
   cache key captures a sub-plan, and what invalidates it?
3. Table 1's hints save 18.1% of queries. What would a database have to know
   to generate such a hint itself — and is that a semantic operator over the
   schema?
4. A probe optimizer minimizes total time across turns. Write the cost model:
   which terms come from the database and which from the agent's behaviour?

## Done when

Answer each before unfolding it.

- [ ] You can define agentic speculation and map each of its four traits to an existing database technique.

  <details><summary>Answer</summary>

  Agentic speculation is agents' high-throughput mix of discovery (metadata,
  statistics) and solution attempts (§1). Scale → admission control and
  prioritization; heterogeneity → approximate answers for exploratory
  requests; redundancy → multi-query optimization, shared caches; steerability
  → returning cost estimates and auxiliary hints with answers.

  </details>

- [ ] You can recompute Table 1's reductions and state the two things the table does not support.

  <details><summary>Answer</summary>

  (with − without) / without: −14.2%, −27.8%, −36.7%, −16.7%, −18.1%; the
  paper prints −27.7, −36.6, −16.6, consistent with truncation. The activity
  rows sum to 12.54 / 9.28, not the 12.67 / 10.38 of "all SQL queries", so
  that row counts other actions; and the overall reduction is 18.1%, under
  the ">20%" the paper's summary box states (only two phases exceed 20%).

  </details>

- [ ] You can say which of the paper's numbers are measured, which are read from plots, and which are observations.

  <details><summary>Answer</summary>

  Measured with a table: Table 1's per-activity counts (44 traces each way).
  Read from plots only: the 14–70% success gain (Figure 1) and the "<10–20%"
  distinct sub-plans (Figure 2). Observation without method: Neon's 20× more
  branches and 50× more rollbacks (§6.2).

  </details>

- [ ] You can price the redundancy claim with lane 1d.

  <details><summary>Answer</summary>

  Ten agents, 2,000 random rows each out of 10,000: 20,000 requested, 8,878
  distinct (expected 10,000 · (1 − 0.8¹⁰) = 8,926). Without sharing: 26,150
  oracle calls; with a shared (document, predicate) cache: 11,615 = distinct
  rows + distinct rows passing the first predicate (8,878 + 2,737), with
  14,535 cache hits and identical answers — a 2.25× saving from redundancy
  alone (simulated).

  </details>

## References

**Paper** — Shu Liu, Soujanya Ponnapalli, Shreya Shankar, Sepanta Zeighami,
Alan Zhu, Shubham Agarwal, Ruiqi Chen, Samion Suwito, Shuo Yuan, Ion Stoica,
Matei Zaharia, Alvin Cheung, Natacha Crooks, Joseph E. Gonzalez, Aditya G.
Parameswaran. "Supporting Our AI Overlords: Redesigning Data Systems to be
Agent-First." CIDR 2026,
[vldb.org/cidrdb/papers/2026/p32-liu.pdf](https://vldb.org/cidrdb/papers/2026/p32-liu.pdf);
[arXiv:2509.00997](https://arxiv.org/abs/2509.00997).

| where | what this chapter takes from it |
|---|---|
| §1 | definitions; the four traits |
| §2, Figures 1–3, Table 1 | 14–70%; <10–20%; 44 traces, 22 tasks; per-activity counts |
| §3–4, Figure 4 | probes, briefs, sleeper agents, satisficing |
| §5.2 | the new objective; MQO, AQP, caching; inter-probe decisions |
| §6 | agentic memory store; branched updates; Neon 20× / 50× |

**This topic** — [README.md](README.md) Step 6 and lanes 1d–1e;
[reading-text-to-sql-broken.md](reading-text-to-sql-broken.md) (the BIRD
labels case study 1 is scored against); topic 38's
[README.md](../38-graphrag-agent-memory/README.md) (agent memory as a graph).
