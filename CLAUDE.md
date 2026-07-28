# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A self-paced database-internals learning path, rendered as an mdBook (`book.toml`, content in `topics/`, exercises in `capstone/`). `PLAN.md` is the curriculum plan — 44 topics, the source of truth. `PROGRESS.md` tracks status and capstone milestones; `SESSION-LOG.md` is the detailed build log, one entry per topic, newest first. `CONTRIBUTING.md` documents the topic package format and the conventions below.

## Working rules

- After editing content, build the book and verify it renders: diagrams (mermaid) must render without syntax errors and internal links must resolve. Do not report content work done without this check.
- For changes that apply across many topics, do 1-2 topics first and ask for review before applying to all (then parallelize).
- The user cares primarily about performance-oriented depth; keep material practical and tied to real database implementations.

## Content rules

- **Never assert a number you did not verify.** Download and read the actual paper; cite the section or table it came from. If a figure cannot be checked against the source, it does not go in.
- **Every performance claim must come from code in this repo.** Write the experiment, run it, and record the measurement. `./verify.sh` runs every measured lane — a new topic's lane belongs in its `BENCHES` list, and the headline belongs in `FINDINGS.md`. Two topics (4, 10) deliberately have no lane because their benches measure only the reader's code; say so in the README rather than inventing a number.
- **A benchmark that prints an implausible number is a bug in the benchmark.** Topic 12 once reported 19,047,619 GB/s from a hoisted timing loop. `black_box` the inputs, and sanity-check any figure against the hardware's actual limits before recording it.
- **Report the negative result.** If a published technique does not reproduce on the local generator, that is the finding: say so, explain which premise is absent, and add an exercise that constructs the case where it holds. (Topic 42's multi-hit booster is the worked example; topic 3's non-step-function height ladder is another.)
- **Generators are seeded**, so every figure reproduces exactly apart from timings. Lockfiles are committed for the same reason.
- **Exercise lanes must degrade, not crash.** A bench binary on a fresh clone prints its provided lanes and a `[stub — ...]` note for the rest, and exits 0. Never let a `todo!()` panic hide a measurement above it.

## Topic package shape

Each `topics/NN-name/` contains: `README.md` (study guide, opening with *the problem, measured* — the provided benchmark lane's real output), four to seven `reading-*.md` guides in the concept-first format (framing lead → "the problem in one sentence" → numbered `### Step N` sections → how to read the source → questions → `## Done when` checklist → references), `notes.md` (a `## Baseline (provided lane, <machine>, measured <date>)` section recording the real output, *then* the reader's prediction worksheet — leave those cells empty, they are the exercise), and `experiments/` — a Rust crate with **lane 1 implemented and two lanes stubbed**, where the stub tests are the specification and the reference numbers live in `notes.md`.

When adding a topic: PLAN.md section, the package above, a `FINDINGS.md` row, a `verify.sh` lane, a capstone `M`NN milestone row in PROGRESS.md, SUMMARY.md entries whose link titles match the on-disk H1s exactly, a SESSION-LOG.md entry (with a `## date — topic NN — title` heading) carrying every measured number, and one commit.

Reference clones live in `~/repos`; the commit each was read at is recorded once in the pin table at the end of `resources/codebases.md` — regenerate with `python3 tools/pin-table.py`, never hand-edit it.
