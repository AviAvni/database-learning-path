# Database Learning Path

**A self-paced curriculum in database internals, where every claim is measured.**

44 topics, from B-trees to GPU query execution to attack graphs. Each one walks you
through the papers and the production code, then hands you a Rust benchmark that
demonstrates the thing being claimed — so you finish with a number you produced
yourself, not a fact you read.

📖 **[Read it online](https://aviavni.github.io/database-learning-path/)** ·
📄 **[Download as PDF](https://aviavni.github.io/database-learning-path/database-learning-path.pdf)** ·
🔬 **`./verify.sh`** to re-derive the headline numbers on your own machine

---

## Why another one of these

There are many good reading lists for database internals. This is not a reading
list. The difference is that the interesting claims here are *demonstrated*, with
a seeded benchmark you can run in seconds, and the surprising results are the
point of the topic rather than a footnote.

Four examples, each the opening measurement of its topic:

| finding | topic |
|---|---|
| A closed-loop benchmark reported **p99 = 1.0 µs** where an open-loop one reported **90 ms** on identical work — coordinated omission, a 90,000× lie | [34 — Debugging & Production Diagnosis](topics/34-debugging/README.md) |
| Growing a hash-sharded cluster from 16 to 17 nodes moves **94.1% of all keys**, and gets *worse* the larger you are | [36 — Sharding & Rebalancing](topics/36-sharding/README.md) |
| A directory whose console reports **8 privileged accounts, forever** has **1969 of 2000 users** holding a path to Domain Admin | [40 — Security & Attack Graphs](topics/40-security-attack-graphs/README.md) |
| The industry-default Bitcoin taint rule marks **93% of all addresses** as tainted from one theft; an 1816 English court case brings it to **1.35%** | [41 — On-Chain & Crypto Analytics](topics/41-onchain-analytics/README.md) |

Each of those runs today:

```bash
git clone https://github.com/AviAvni/database-learning-path
cd database-learning-path
./verify.sh 34 36 40 41        # or ./verify.sh for all of them
```

## Start here

**If you want to learn the material** — open the
[online book](https://aviavni.github.io/database-learning-path/) and pick any topic
that interests you; the order is a suggestion, not a prerequisite chain. Each topic
has a study guide, four to seven reading guides for the papers and codebases behind
it, and an experiments crate. Start with
[Topic 0 — The Performance Toolbox](topics/00-performance-toolbox/README.md) if you
want the measurement discipline first, or jump straight to whatever you actually
work on.

**If you want to do the exercises** — every experiments crate ships one implemented
"lane" and two stubbed ones. The stubs' tests *are* the specification: run
`cargo test` in any `topics/*/experiments/` and the failures tell you precisely what
to build, with the reference numbers recorded in that topic's `notes.md` so you can
check your work.

**If you want the reading list** — [PLAN.md](PLAN.md) is the whole curriculum in one
file, with the papers, the codebases and the concepts for each topic.
[resources/papers.md](resources/papers.md) and
[resources/codebases.md](resources/codebases.md) are the flat indexes.

**If you want the graph-engine build** — [capstone/](capstone/README.md) rebuilds a
FalkorDB-flavored graph database from scratch in Rust, one milestone per topic, with
baselines measured against the production C implementation.

## How this was built, and how to check it

Written with heavy AI assistance (Claude Code), quickly — which is worth saying
plainly, because 60,000 lines of prose in a few weeks is not a human writing pace and
you should know what you are reading.

What that assistance was *not* allowed to do is assert numbers. Every paper cited was
downloaded and read; every figure quoted is attributed to a specific section or table;
and every performance claim in the repo comes from code in this repo that you can
run. `./verify.sh` exists so you never have to trust any of it — it builds and runs
each measured benchmark and prints the output, and the generators are seeded, so the
figures reproduce exactly apart from timings.

That is the real argument for this format over a reading list: a link collection
cannot be wrong in a way you can detect, and this can.

If you find a claim that does not reproduce, or a paper summarised incorrectly,
please [open an issue](https://github.com/AviAvni/database-learning-path/issues) —
that is the most useful contribution possible here.

## Layout

```
PLAN.md            the full 44-topic curriculum — the source of truth
PROGRESS.md        status tracker + capstone milestones
SESSION-LOG.md     detailed build log, one entry per topic
verify.sh          re-run every measured benchmark
topics/NN-name/
  README.md        study guide: concepts, measured results, exercises
  reading-*.md     one guide per paper or codebase, concept-first
  notes.md         predictions vs measurements, cross-topic threads
  experiments/     Rust crate: one implemented lane, two as exercises
capstone/          the graph engine, one milestone per topic
resources/         flat indexes: papers, codebases, tools
```

## Background

Written by [Avi Avni](https://github.com/AviAvni), a core developer of
[FalkorDB](https://github.com/FalkorDB/FalkorDB) and
[falkordb-rs-next-gen](https://github.com/FalkorDB/falkordb-rs-next-gen) — so graph
internals are home turf and get the deepest treatment, but the goal is breadth across
every database domain, which is why roughly two-thirds of the topics are not about
graphs at all.

Contributions, corrections and new topics welcome — see
[CONTRIBUTING.md](CONTRIBUTING.md) for how a topic is put together.

## License

Rust code and scripts: [MIT](LICENSE). Written material (guides, curriculum, notes):
[CC BY 4.0](LICENSE-CONTENT). Quoted papers and code excerpts remain the property of
their authors and are attributed in each guide's References section.
