# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A self-paced database-internals learning path, rendered as an mdBook (`book.toml`, content in `topics/`, exercises in `capstone/`). `PLAN.md` is the curriculum plan — 45 topics, the source of truth. `PROGRESS.md` tracks status and capstone milestones; `SESSION-LOG.md` is the detailed build log, one entry per topic, newest first. `CONTRIBUTING.md` documents the topic package format and the conventions below.

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

## Reading-guide depth

The `reading-*.md` chapters teach from zero: a reader who knows systems but not the chapter's theory must be able to finish without leaving the page. `topics/00-performance-toolbox/reading-criterion.md` is the reference implementation of these rules; match it.

- **Define every term at first use.** A term of art (t-test, p-value, quartile, IQR, MAD, standard error, null hypothesis, arithmetic intensity, coordinated omission) gets a **bold** name and a one-sentence plain-language definition at the point it first appears, *before* any argument leans on it. A step may use only terms defined in an earlier step or defined on the spot. Borrowed jargon — using a word the guide never defined because the source material used it — is the failure this rule exists to stop.
- **Every step declares its input and output.** Each `### Step N` opens with a `> **In:** … **Out:** …` blockquote naming the dataset it consumes, *which earlier step produced it*, and what it emits. When one stage forks into two datasets used by different downstream steps, the fork gets its own numbered step. "Is this the same data as the previous section?" must never be left to the reader to infer.
- **A formula gets its symbols named and one worked example.** Quote it as the source actually computes it, name every symbol, then run it once on 3–5 concrete numbers so a real answer comes out. Arithmetic printed in a guide is verified like any other number in this repo — compute it, don't estimate it.
- **Anchors are verified against the pinned clone, file *and* line.** Citing the right line of the wrong file is the same error class as inventing a number. Re-grep every anchor before committing; state the version the line numbers belong to.
- **A quoted snippet carries the line numbers it actually occupies, and names the one to look at.** Put the real number in the gutter of every line, mark elided ranges (`// ... 131–139: bookkeeping ...`) rather than silently closing a gap, and say in the prose which line carries the argument ("the line to focus on is 277, its only `return`"). A snippet anchored to the function signature while quoting code forty lines below it leaves the reader unable to find anything. Pseudocode gets a `// ILLUSTRATION — not quoted from the crate` header and a pointer to the real code.
- **Describe what the code does, not what the technique usually does.** criterion 0.5.1's `Slope::fit` is a one-field struct fitting through the origin, so the textbook "the intercept absorbs the overhead" account of least squares is simply false there. Read the implementation before writing the explanation, and prefer the honest, weaker claim over the tidy, wrong one.
- **Every `## Done when` box carries its answer in a collapsed `<details>` block**, introduced by "Answer each before unfolding it." The checklist is a self-test, so the answer must be reachable without leaving the page but never visible by accident. Answers restate the reasoning rather than pointing back at a step number, and are held to the same standard as the body: real anchors, real numbers, the honest claim.
- **Never trade a definition, a worked example or an answer for brevity.** These chapters have no length target — a guide that assumes vocabulary is not shorter, it is unfinished. Cut redundancy instead.

The mechanical half of these rules is enforced by `python3 tools/check-reading-depth.py` (step In/Out blockquotes, collapsed answers under every `Done when` item, line-number gutters on quoted snippets, the section spine); run it on a guide before committing it. `--check` is a ratchet — a guide that has started following the rules must follow all of them, and the guides the rollout has not reached yet are reported without failing. The other half — definitions, worked arithmetic, honest claims — is judgement and stays the writer's job.

Anchors are checked with `python3 tools/pinned-source.py`, which opens a file at the revision the pin table records (`show`, `grep`, `check`, `list`). It uses a real clone under `~/repos` when one is present and otherwise fetches that same commit into a gitignored `.cache/`, so an anchor can be verified on a machine that has not cloned 85 upstream repos. A repo that is not in the pin table — a crate read from the cargo registry — needs `--ref` and a version stated in the guide.

## Topic package shape

Each `topics/NN-name/` contains: `README.md` (study guide, opening with *the problem, measured* — the provided benchmark lane's real output), four to seven `reading-*.md` guides in the concept-first format (framing lead → "the problem in one sentence" → numbered `### Step N` sections → how to read the source → questions → `## Done when` checklist → references), `notes.md` (a `## Baseline (provided lane, <machine>, measured <date>)` section recording the real output, *then* the reader's prediction worksheet — leave those cells empty, they are the exercise), and `experiments/` — a Rust crate with **lane 1 implemented and two lanes stubbed**, where the stub tests are the specification and the reference numbers live in `notes.md`.

When adding a topic: PLAN.md section, the package above, a `FINDINGS.md` row, a `verify.sh` lane, a capstone `M`NN milestone row in PROGRESS.md, SUMMARY.md entries whose link titles match the on-disk H1s exactly, a SESSION-LOG.md entry (with a `## date — topic NN — title` heading) carrying every measured number, and one commit.

Reference clones live in `~/repos`; the commit each was read at is recorded once in the pin table at the end of `resources/codebases.md` — regenerate with `python3 tools/pin-table.py`, never hand-edit it.
