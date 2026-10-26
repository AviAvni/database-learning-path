# Text-to-SQL benchmarks are broken: when half the labels are wrong

The other half of "LLMs in the query processor" is the front door: a model
that turns a question into SQL (or Cypher), so a user never writes a query
at all. Progress there is measured on leaderboards — BIRD and Spider 2.0 —
and Jin, Choi, Zhu & Kang (CIDR'26) audited the labels those leaderboards
score against. They found errors in **52.8%** of BIRD Mini-Dev and **66.1%**
of the Spider 2.0-Snow problems they could check, and showed that
correcting a 100-problem sample moves one agent from fourth place to first.
For this topic the paper is the text-to-query version of a lesson the
semantic-operator chapters already met: a metric defined against a
reference — LOTUS's gold algorithm, a benchmark's gold SQL — is only as good
as the reference. This chapter builds the vocabulary, recomputes every rate
from its counts, and shows that the paper's own per-example accounting does
not close against its headline accuracies.

## The problem in one sentence

If a text-to-SQL benchmark scores a system by whether its query returns the
same rows as an annotated "gold" query, how much of the score measures the
system and how much measures mistakes in the gold queries?

## The concepts, step by step

### Step 1 — a text-to-SQL problem and execution accuracy

> **In:** nothing yet — a natural-language question over a database.
> **Out:** the triple every benchmark problem consists of, and the metric
> leaderboards rank by; Steps 2–5 attack both.

**Text-to-SQL** is translating a natural-language question into a SQL query
that answers it. Each benchmark problem is a triple (§1): the **input T** — a
user question plus, optionally, **external knowledge** (a hint such as "State
Special Schools refers to DOC = 31"); an example **database D**; and a
ground-truth **SQL query Q**. A system's output is scored by **execution
accuracy** (EX): the fraction of problems on which the system's query returns
the same result as Q when both run on D (§5.1, following BIRD).

Worked: an agent whose queries match the gold results on 62 of 100 problems
has EX = 62%.

Why it matters: EX trusts Q completely. If Q is wrong, a correct answer
scores as wrong and a matching wrong answer scores as right — which Step 5
shows happening.

### Step 2 — four ways an annotation goes wrong

> **In:** the triple (T, D, Q) from Step 1.
> **Out:** the error taxonomy E1–E4 that Step 3 counts.

The paper's patterns (§1), each with an example from §4:

- **E1** — Q's semantics do not match T's intended logic. Spider 2.0-Snow
  `sf_bq291` calls Snowflake's `ST_POINT(26.75, 51.5)` with latitude first;
  the function takes longitude first (Figure 1a). BIRD uses `BETWEEN … AND …`
  where the question asks for strict inequality.
- **E2** — Q does not match D, from limited understanding of the data. BIRD
  questions about schools omit `rtype = 'S'`, the column that distinguishes
  schools from districts — a column the annotators had labelled "unuseful".
  Spider joins and `FLATTEN`s that inflate row counts without `DISTINCT`.
- **E3** — Q does not match domain knowledge. BIRD's problem 46 treats
  "Enrollment (K-12)" as grades 1–12, but K-12 includes kindergarten (Figure
  1b); Formula 1 problems ignore that "+n Lap" drivers also finished.
- **E4** — T is ambiguous: a column name present in several tables, or an
  output format the question never specifies.

Why it matters: E2 and E3 are errors a stronger annotator — or a model with
the domain knowledge — would *not* make. A system that is right where the
annotator was wrong is penalized.

### Step 3 — measuring the error rate, and recomputing it

> **In:** the taxonomy of Step 2, applied to every audited problem.
> **Out:** the two headline error rates and their breakdown, each checked
> against its counts.

The audit (§3.2): a problem is erroneous if it shows at least one of E1–E4,
checked by reading the schema, executing the SQL and its subqueries, and
cross-checking domain facts. Scope (§3.1): BIRD **Mini-Dev**, 498 problems;
Spider 2.0-Snow's **121** problems with public gold SQL (of 547), in the
version before the 2025-07-13 update.

Recomputed from Table 1 and §4:

| benchmark | erroneous / audited | rate | E1 | E2 | E3 | E4 |
|---|---|---|---|---|---|---|
| BIRD Mini-Dev | 263 / 498 | 52.8% | 77 (29.3%) | 152 (57.8%) | 28 (10.7%) | 78 (29.7%) |
| Spider 2.0-Snow | 80 / 121 | 66.1% | 39 (48.8%) | 44 (55%) | 9 (11.3%) | 20 (25%) |

The 263 is not printed; it is what 52.8% of 498 must be, and Table 1's
percentages confirm it (77 / 263 = 29.3%, 152 / 263 = 57.8%). Pattern
percentages are of erroneous problems and sum past 100% because a problem
can have several (Table 1 caption).

One inconsistency: the introduction says prior work reported errors in
"32% of problems in the mini development set" [2], and §1 later says the new
rate "is 16.7% higher than the previously reported rate of 36.1% [2]" — same
reference, two rates. §4.1's "102 additional erroneous examples compared to
prior work" gives 263 − 102 = 161 prior errors = 161 / 498 = **32.3%**,
consistent with the 32%; 36.1% would be 180 problems. The "16.7% higher" is
52.8 − 36.1 percentage points.

Why it matters: more than half of the problems a leaderboard scores on have
a wrong or ambiguous label, and E2 — not understanding the data — is the
most common pattern in both benchmarks.

### Step 4 — re-scoring five agents on a corrected sample

> **In:** the error patterns of Step 2, applied to a random sample of BIRD
> Dev, and five open-source agents from the BIRD leaderboard.
> **Out:** how far scores and ranks move when the labels are fixed.

The authors sampled 100 BIRD Dev problems (62 simple, 28 moderate, 10
challenging, §5.1), found errors in **48%** of them, and corrected T, Q and,
where an erroneous query happened to return the right rows, D itself — adding
rows that distinguish the wrong query from the right one (Figure 2, §5.2).
Five agents (CSC-SQL, OpenSearch-SQL, OmniSQL, GSR-SQL, CHESS) ran on both
versions (§5.3):

- CHESS: EX **62% → 81%**, +19 points absolute, 81 / 62 − 1 = **+30.6%**
  relative (the "31%"); rank 4th → 1st.
- OpenSearch-SQL: EX **71% → 69%**, −2 points, 69 / 71 − 1 = **−2.8%**
  relative (the "−3%"); rank 1st → 3rd.

The abstract's "−3% to 31% in relative terms" and §1's "−2% to 19% in
absolute terms" are the same two agents, measured two ways.

Why it matters: the gap between first and fourth on the original sample is
smaller than the label noise. A leaderboard position on BIRD is, at this
error rate, a statement about the annotators as much as about the agents.

### Step 5 — the per-example accounting does not close

> **In:** Table 4's lists of problems whose correctness changed after
> correction, for CHESS and OpenSearch-SQL, and the EX values of Step 4.
> **Out:** a reconciliation the paper does not do — and its failure.

Table 4 lists, per agent, the problems that flipped False→True (F→T) or
True→False (T→F), by which component was corrected. Counting its entries:

| agent | F→T | T→F | net | EX change in §5.3 |
|---|---|---|---|---|
| CHESS | 0 + 12 + 14 = 26 | 1 + 1 + 2 = 4 | **+22** | **+19** (62 → 81) |
| OpenSearch-SQL | 0 + 4 + 6 = 10 | 0 + 6 + 8 = 14 | **−4** | **−2** (71 → 69) |

(Rows: corrected T only, corrected Q, corrected T & Q. CHESS's 26 and 4 match
§5.3's text, "26 examples were updated from False to True … whereas four
examples changed from True to False".) On 100 problems each flip is one
point, so the nets should equal the EX changes. They differ by 3 points for
CHESS and 2 for OpenSearch-SQL. Problems whose labels were *not* corrected
can also flip if an agent is nondeterministic between runs, and Figure 4
plots changes "alongside information on whether annotation corrections were
applied" — but its bars are not readable as numbers, and the paper does not
reconcile them.

Why it matters: this is the paper's own lesson applied to the paper. The
headline (labels are wrong often enough to reorder a leaderboard) survives a
3-point discrepancy; a precise claim such as "CHESS gains 19 points from
corrections alone" does not.

### Step 6 — the same lesson in a query plan

> **In:** Steps 1–5, and the gold-relative targets of
> [reading-lotus-semantic-operators.md](reading-lotus-semantic-operators.md).
> **Out:** why an engine that uses an LLM to *write* or *evaluate* queries
> needs an independent check on its reference.

Text-to-SQL's gold Q plays the role LOTUS's gold algorithm plays for a
semantic filter: the thing correctness is defined against. This topic's lane
1a shows the semantic-operator version numerically: the oracle-on-every-row
"gold" set has 364 rows where the truth has 297 (simulated, 97%-accurate
oracle, two predicates), so a plan that returned the truth exactly would
score F1 0.835 against gold. Lane 3 shows the join version: 27 of the 417
gold join pairs are oracle inventions on non-matching pairs, which no
similarity-based blocking can — or should — reproduce. In both cases the
honest metric reports agreement with the reference *and* says how good the
reference is.

Why it matters: for a graph database adding a text-to-Cypher front door
(topic 38), the evaluation set is part of the system. An audit like this
paper's — schema-aware, domain-aware, executing subqueries — is the cost of
believing the number.

## How to read the paper with these concepts in hand

| section | what to take from it | step |
|---|---|---|
| Abstract, §1, Figure 1 | E1–E4; 52.8% / 66.1%; −3% to 31%; 32% vs 36.1% | 2, 3, 4 |
| §3 | scope (498, 121 of 547) and the audit procedure | 3 |
| §4, Tables 1–3 | counts per pattern; representative examples | 2, 3 |
| §5.1–5.2, Figure 2 | the 100-problem sample; corrections to T, Q and D | 4 |
| §5.3, Figures 3–5 | EX and rank changes | 4 |
| §5.4, Table 4 | per-example flips for CHESS and OpenSearch-SQL | 5 |

## Questions to answer

1. EX compares result sets on one database D. Why did the authors have to
   *modify D* to make some corrections visible, and what does that say about
   EX as a metric?
2. E2 errors come from not understanding the data. Which of them would an
   agent that runs exploratory queries first (see
   [reading-agent-first.md](reading-agent-first.md)) be more likely to avoid?
3. If a benchmark's labels are wrong on 50% of problems, what is the largest
   EX a perfect system can score on the uncorrected benchmark? (It depends on
   how many wrong labels a correct query still matches.)
4. Design the reconciliation Table 4 is missing: what per-example data would
   you need to explain CHESS's +19 from its +22?

## Done when

Answer each before unfolding it.

- [ ] You can define the text-to-SQL triple and execution accuracy, and say what EX assumes.

  <details><summary>Answer</summary>

  A problem is (T, D, Q): question plus optional external knowledge, an
  example database, and a gold SQL query. EX is the fraction of problems on
  which the system's SQL returns the same result as Q on D. It assumes Q is
  right; when Q is wrong, a correct system is marked wrong and a system that
  repeats Q's mistake is marked right.

  </details>

- [ ] You can recompute both headline error rates and resolve the 32% vs 36.1% discrepancy.

  <details><summary>Answer</summary>

  Spider 2.0-Snow: 80 / 121 = 66.1%. BIRD Mini-Dev: 52.8% of 498 = 263
  erroneous problems (Table 1's 77 / 263 = 29.3% confirms the count). §4.1's
  "102 additional" errors over prior work leaves 161 prior errors = 32.3% of
  498, matching the "32%" in §1; the "36.1%" quoted later in §1 for the same
  reference would be 180 problems and does not match.

  </details>

- [ ] You can state the CHESS and OpenSearch-SQL changes in both absolute and relative terms.

  <details><summary>Answer</summary>

  CHESS 62% → 81%: +19 points, +30.6% relative, 4th → 1st. OpenSearch-SQL
  71% → 69%: −2 points, −2.8% relative, 1st → 3rd. The paper's "−2% to 19%
  absolute" and "−3% to 31% relative" are these two.

  </details>

- [ ] You can show that Table 4 does not reconcile with the EX changes.

  <details><summary>Answer</summary>

  CHESS: 26 F→T and 4 T→F in Table 4, net +22, but EX rose 19 points.
  OpenSearch-SQL: 10 F→T and 14 T→F, net −4, but EX fell 2 points. On a
  100-problem sample one flip is one point, so the tables differ by 3 and 2
  points; the paper does not account for flips outside Table 4 (Figure 4 is
  not numeric).

  </details>

## References

**Paper** — Tengjun Jin, Yoojin Choi, Yuxuan Zhu, Daniel Kang. "Text-to-SQL
Benchmarks are Broken: An In-Depth Analysis of Annotation Errors." CIDR 2026,
[vldb.org/cidrdb/papers/2026/p5-jin.pdf](https://vldb.org/cidrdb/papers/2026/p5-jin.pdf).
An extended version appears as arXiv:2601.08778 ("Pervasive Annotation
Errors Break Text-to-SQL Benchmarks and Leaderboards"); this chapter read
only the CIDR paper, and every number above is from it.

| where | what this chapter takes from it |
|---|---|
| Abstract, §1 | E1–E4; 52.8%, 66.1%; −3% to 31%; −2% to 19%; 32% and 36.1% |
| §3.1–3.2 | 498 Mini-Dev problems; 121 of 547 Spider 2.0-Snow; audit procedure |
| §4, Table 1 | per-pattern counts; 102 additional errors; 80 of 121 |
| §5.1–5.3, Figures 2–5 | 100-problem sample (62/28/10), 48% erroneous; 62 → 81, 71 → 69; ranks |
| §5.4, Table 4 | per-example flips |

**This topic** — [README.md](README.md) Step 6,
[reading-lotus-semantic-operators.md](reading-lotus-semantic-operators.md)
(the gold algorithm as a reference that can be wrong).
