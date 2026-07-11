# Database Learning Path

A self-paced, hands-on curriculum for mastering database internals — with a focus on
**performance, data structures, and algorithms** — built around reading world-class
codebases, implementing things from scratch in Rust, and benchmarking everything.

Background: author is a core developer of [FalkorDB](https://github.com/FalkorDB/FalkorDB)
and [falkordb-rs-next-gen](https://github.com/FalkorDB/falkordb-rs-next-gen), so graph
internals are familiar ground; the goal is breadth + depth across all database domains.

## Repo layout

```
README.md          ← you are here: how this repo works
PLAN.md            ← the full curriculum (17 topics) — the source of truth
PROGRESS.md        ← status tracker: what's done, in progress, next
capstone/          ← "minidb": a multi-model DB built incrementally, one milestone per topic
topics/            ← created lazily — one dir per topic when the deep dive starts
  NN-topic-name/
    README.md      ← expanded study guide for that topic
    notes.md       ← learnings, insights, surprising things
    experiments/   ← standalone Rust experiments + criterion benchmarks
resources/
  papers.md        ← papers & articles (arXiv, VLDB, SIGMOD, classics)
  codebases.md     ← reference codebases and what each is best for studying
  tools.md         ← profiling, benchmarking, fuzzing, testing tools
```

## Workflow (for me and for Claude next session)

1. Open `PROGRESS.md` to see where we are.
2. Pick the next topic (or any topic — order is a suggestion, not a rule) from `PLAN.md`.
3. Create `topics/NN-topic-name/` and expand the PLAN.md section into a full study
   guide: concept explanations with examples, guided code-reading of the reference
   repos, exercises, and benchmarks.
4. Study, implement experiments, benchmark, take notes in `notes.md`.
5. Implement the topic's capstone milestone in `capstone/`.
6. Update `PROGRESS.md` (status + one-line takeaway) and commit.

## Conventions

- Language: **Rust** for all implementations and benchmarks (criterion + flamegraph).
- Every topic ends with a benchmark — numbers over intuition.
- Code reading is done against pinned clones under `~/repos/` (not vendored here).
- Notes capture *why* designs win, trade-offs, and measured results — not summaries.
