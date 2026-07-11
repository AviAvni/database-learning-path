# Why AWS writes TLA+: exhaustively testable pseudo-code

The CACM 2015 experience report that moved TLA+ from academia to
industrial default for distributed protocols. Read it for the
*economics*, not the math: what class of bug justifies days of
spec-writing — and what a spec still can't do for you. It frames
every other chapter in this topic.

## The core claims

1. **Human intuition fails at ~35 steps.** S3's replication bug
   needed a 35-step interleaving to trigger; design reviews, code
   review, and testing all missed it. TLC found it because
   exhaustive breadth-first search doesn't get bored.
2. **Specs are cheap relative to the bug.** 2-3 weeks to first
   useful spec; DynamoDB's spec was ~1000 lines and found 3 design
   bugs pre-implementation, one requiring a fundamental change.
3. **"Exhaustively testable pseudo-code"** — the internal pitch that
   worked. Not "formal verification": engineers write the spec as
   the design doc, then get model checking for free.
4. **Model small, learn big.** Checking 3 replicas × 3 entries
   (like our WalReplication) is not a proof — but protocol bugs are
   almost never "only at N=7"; small-scope hypothesis.

```
  spec size vs payoff (paper's table, paraphrased)
  S3 repl.      ~800 lines   2 design bugs, one 35-step
  DynamoDB      ~1000 lines  3 design bugs pre-impl
  EBS           ~450 lines   design confirmed (also a win)
```

## What TLA+ did NOT do for them

- No liveness in practice (they check safety; liveness is expensive
  and fairness assumptions are subtle).
- No code conformance — the spec and the C++ can drift. (MongoDB
  later attacked this with spec-driven test generation.)
- No performance modeling.

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
