# Why AWS writes TLA+: exhaustively testable pseudo-code

The CACM 2015 experience report that moved TLA+ from academia to
industrial default for distributed protocols. Read it for the
*economics*, not the math: what class of bug justifies days of
spec-writing — and what a spec still can't do for you. Before the
paper, this chapter builds the concepts its argument rests on —
what a spec is, what model checking actually does, why testing
can't reach the bugs it finds — one step at a time. It frames every
other chapter in this topic.

## The problem in one sentence

S3's replication protocol had a data-loss bug that required a
specific 35-step interleaving of events to trigger — design review,
code review, and testing all missed it, because no human or test
generator reliably explores 35 steps deep.

## The concepts, step by step

### Step 1 — a specification is the design, written so a machine can explore it

A **specification** (spec) is a description of a system as a state
machine: the variables that make up a **state**, the initial
states, and the allowed transitions between states. Nothing about
threads, packets, or code — just "from this state, these next
states are legal." **TLA+** is a language for writing exactly
that, and it deliberately reads like pseudo-code with math
instead of control flow. The point of the formality is not rigor
for its own sake: a design written this way can be *executed
exhaustively* by a tool, while a design written in prose can only
be reviewed by tired humans. (The companion chapter,
[reading-tlaplus-raft.md](reading-tlaplus-raft.md), teaches the
language itself.)

### Step 2 — model checking: enumerate every reachable state

A **model checker** (TLC, for TLA+) takes a spec plus fixed small
parameters — 3 replicas, 3 log entries — and does breadth-first
search over the *entire* reachable state graph, checking a stated
**invariant** (a property that must hold in every reachable state,
e.g. "committed data survives failover") at each state. Contrast
the testing spectrum (topic 16): a test — even a
property-based-test generator — *samples* behaviors; TLC
*enumerates* them. Our WalReplication model is ~1080 distinct
states, checked in under a second; when the invariant fails, TLC
prints the exact step-by-step trace that breaks it. The
limitation is equally crisp: it checked 3 replicas × 3 entries,
nothing more — that gap is step 6.

### Step 3 — the core claim: human intuition fails at ~35 steps

S3's replication bug needed a 35-step interleaving to trigger.
Design reviews, code review, and testing all missed it. TLC found
it, because exhaustive breadth-first search doesn't get bored:
depth 35 is just another BFS frontier. The paper's engineers
report the same experience repeatedly — humans reason reliably
about interleavings a handful of steps deep, and distributed
protocol bugs live well past that horizon. This is the paper's
answer to "we already review our designs carefully": review
quality is not the bottleneck; the state space is.

### Step 4 — the economics: spec size vs payoff

The trade the paper is actually selling: 2-3 weeks to a first
useful spec, against design bugs found *before implementation*:

```
  spec size vs payoff (paper's table, paraphrased)
  S3 repl.      ~800 lines   2 design bugs, one 35-step
  DynamoDB      ~1000 lines  3 design bugs pre-impl
  EBS           ~450 lines   design confirmed (also a win)
```

DynamoDB's ~1000-line spec found 3 design bugs, one requiring a
fundamental change — the cheapest possible time to find it. Note
the EBS row: finding *no* bugs is also a payoff (confidence in the
design), which matters when deciding whether specs are worth it
for protocols that turn out fine.

### Step 5 — the pitch that worked: "exhaustively testable pseudo-code"

AWS did not sell "formal verification" internally — that phrase
promises proofs and demands mathematicians. The pitch that worked:
engineers write the spec *as the design doc* (it reads like
pseudo-code), and model checking comes free. This reframing is
load-bearing: the spec has a reason to exist even before checking
(it forces precision about the design), and checking is then a
button, not a research project. Steal the framing for any tool
adoption argument: attach the new cost to an artifact people
already need.

### Step 6 — model small, learn big: the small-scope hypothesis

Checking 3 replicas × 3 entries (like our WalReplication) is not
a proof — the bug could in principle appear only at N=7. The
**small-scope hypothesis** is the empirical observation that
protocol design bugs almost never work that way: a broken quorum
or ordering argument breaks at the smallest size where the
concepts exist (usually 2-3 processes). So a model TLC can finish
in seconds still finds the real bugs. Know when the hypothesis
*fails*, though: bugs triggered by resource-boundary edge cases
(a B+tree page becoming exactly full — topic 3) are about
magnitudes, not protocol logic, and small models never reach them
— question 4.

### Step 7 — what TLA+ did NOT do for them

The honest half of the report, and the boundary of the tool:

- No liveness in practice (they check safety; liveness is expensive
  and fairness assumptions are subtle).
- No code conformance — the spec and the C++ can drift. (MongoDB
  later attacked this with spec-driven test generation.)
- No performance modeling.

The drift point deserves the most respect: TLC verified the
*design*, and nothing keeps the implementation honest against it
afterwards. Question 5 asks what our capstone CI could do about
that.

## How to read the paper (with the concepts in hand)

It's a short CACM piece — read all of it, in order. The sidebar
tables carry the economics (step 4); compare each project row
against the two-to-three-week spec cost as you go. Watch for the
S3 35-step story (step 3), the "exhaustively testable pseudo-code"
framing (step 5 — note *where* in the adoption story it appears),
and the closing candor about what the method doesn't cover
(step 7). Read it with our `specs/WalReplication.tla` in mind:
every claim the paper makes at S3 scale has a miniature
counterpart in that 94-line model.

## Questions (answer in notes.md)

1. Which capstone protocol clears the paper's cost/benefit bar for a
   spec — MVCC visibility, delta-matrix `wait` concurrency, or WAL
   replication — and which is fine with proptest alone (topic 16)?
2. The 35-step bug: what makes an interleaving reachable-but-rare?
   Relate to why our SyncCommit=FALSE trace is only 5 steps (the
   model has no noise to wade through).
3. "Exhaustively testable pseudo-code": how is a TLA+ `Next` action
   different from a proptest state-machine transition (topic 16)?
   What does TLC explore that proptest samples?
4. Why does the small-scope hypothesis hold for protocols but NOT
   for, say, B+tree split bugs (topic 3) that need page-full edge
   cases?
5. Spec-code drift: sketch how the capstone's CI could keep
   WalReplication.tla honest against the real replication code.

## References

**Papers**
- Newcombe, Rath, Zhang, Munteanu, Brooker, Deroche — "How Amazon
  Web Services Uses Formal Methods" (CACM 2015) — short; read all
  of it, the sidebar tables carry the economics
