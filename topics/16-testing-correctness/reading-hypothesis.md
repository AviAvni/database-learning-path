# Hypothesis: shrinking a choice sequence, not a value

This topic's table says property testing is "random ops" plus "an
in-memory model". That is the *idea*. The engineering that makes it
usable on a database is almost entirely about what happens **after** a
failure: a random 200-operation history that breaks your KV store is
not a bug report, it is a haystack.

Hypothesis — the Python property-testing library, and the one whose
internals are documented rather than folklore — answers this with a
single structural decision: **it does not shrink your values, it shrinks
the sequence of choices your generators made.** Every consequence
follows from that, including the ones that look like unrelated features
(a failure database, a prefix trie over past executions, swarm testing,
targeted search).

This chapter builds the vocabulary from zero — choice sequence, shortlex
order, complexity index, shrink pass, novel prefix — and works the
orderings on real numbers. Rust readers: `proptest`, which this topic's
exercises use, is a direct descendant of the same design (its integrated
shrinking comes from the same insight), so everything here transfers
except the file paths.

Anchors are `HypothesisWorks/hypothesis` at the commit
`resources/codebases.md` pins, quoted with the line numbers they occupy
there. Paths are relative to `hypothesis/src/hypothesis/`.

## The problem in one sentence

If shrinking is a method on the *type* — a `shrink()` for lists, another
for integers, another for your `Op` enum — then it cannot see the
constraints that made a value valid, it composes badly under `flat_map`,
and every new generator needs a new shrinker; whereas if the thing being
shrunk is the *choice sequence the generators consumed*, one shrinker
works for every generator that will ever be written, and re-running the
test is what checks validity.

## The concepts, step by step

### Step 1 — what a property test is, and where the pain is

> **In:** nothing. **Out:** the two halves of a property test, and the
> reason the second half is where the engineering went.

A **property-based test** has two parts: a **generator**, which produces
inputs, and a **property**, which must hold for all of them. Running it
is a search for a counterexample.

The classical (QuickCheck) design makes both type-directed: a
`Gen<T>` produces a `T`, and a paired `Shrink<T>` produces "smaller"
`T`s to try when one fails. Three things go wrong at database scale:

1. **Composition.** Generate a list of length `n`, then generate `n`
   indices into it. Shrinking the list to a shorter one invalidates
   the indices, and the shrinker for the pair has no way to know.
2. **Constraints.** "A `put` whose key was previously written" is a
   validity condition the type does not carry, so a type-directed
   shrinker produces mostly invalid candidates.
3. **Cost.** Every generator you write needs a shrinker written and
   maintained beside it, and the ones people skip are exactly the
   domain types where shrinking would help most.

**Integrated shrinking** is the fix: shrink the *input to generation*
rather than its output, and re-run generation to get a value that is
valid by construction. Hypothesis's realisation of it is Steps 2–5.

### Step 2 — the choice sequence

> **In:** Step 1's integrated shrinking. **Out:** the object Hypothesis
> actually manipulates, and the reason it is typed rather than a byte
> string.

Every draw a strategy makes goes through one of five primitives, and
the record of those draws is the **choice sequence**:

```python
# internal/conjecture/choice.py, lines 60-68 — the whole vocabulary of a test case
    60  ChoiceT: TypeAlias = int | str | bool | float | bytes
    61  ChoiceConstraintsT: TypeAlias = (
    // ... 62–67: the union of the five per-type constraint TypedDicts ...
    68  ChoiceTypeT: TypeAlias = Literal["integer", "string", "boolean", "float", "bytes"]
```

Five types, and each drawn value carries the **constraints** it was
drawn under — for an integer, `min_value`, `max_value`, `weights` and
`shrink_towards` (`choice.py:31-35`). A test case *is* a list of
`(type, value, constraints)` triples, and running the test is replaying
that list into the strategies.

Two properties matter later:

- **It is generic.** Nothing in the sequence knows about your `Op` enum;
  your enum's generator turned into integer and boolean draws.
- **It is typed.** Earlier versions of Hypothesis shrank an underlying
  *byte* stream, so a "small" change at the byte level could rewrite
  every subsequent draw. Recording the choices themselves means a change
  to one choice is a change to one decision.

### Step 3 — "simpler" is shortlex, and it is exactly defined

> **In:** the choice sequence of Step 2. **Out:** the total order the
> shrinker is minimising, with the arithmetic done on a real pair of
> candidates.

**Shortlex order** compares two sequences by length first and, among
equal lengths, lexicographically. Hypothesis's key:

```python
# internal/conjecture/shrinker.py, lines 73-94 — sort_key. The definition is
# lines 91-94; the docstring above it gives the three reasons.
    73  def sort_key(nodes: Sequence[ChoiceNode]) -> tuple[int, tuple[int, ...]]:
    74      """Returns a sort key such that "simpler" choice sequences are smaller than
    75      "more complicated" ones.
    76
    77      We define sort_key so that x is simpler than y if x is shorter than y or if
    78      they have the same length and map(choice_to_index, x) < map(choice_to_index, y).
    // ... 79–90: the three justifications, quoted in the prose below ...
    91      return (
    92          len(nodes),
    93          tuple(choice_to_index(node.value, node.constraints) for node in nodes),
    94      )
```

The docstring's three reasons are worth having in your head: a shorter
sequence means "we had to make fewer decisions"; a lower index at the
same position means a simpler value there; and earlier choices are
prioritised because they "potentially get used in more places" — a
choice made early can change how many later choices exist at all.

`choice_to_index` is Step 4. Taking it on trust for one moment, work
three candidates with `shrink_towards = 0` and no bounds:

```
   sequence        length   indices        sort_key
   [10]            1        (19,)          (1, (19,))
   [0, 3]          2        (0, 5)         (2, (0, 5))
   [3, 0]          2        (5, 0)         (2, (5, 0))

   shortlex:  [10]  <  [0, 3]  <  [3, 0]
```

`[10]` wins despite containing the largest number in the table, because
it is one decision instead of two. And `[0, 3]` beats `[3, 0]` because
the *first* position is simpler — the third justification, made
concrete. A failing test that shrinks to `[10]` really is a simpler
story than one that shrinks to `[3, 0]`, even though "10" looks bigger
than "3".

### Step 4 — the complexity index, and the zigzag

> **In:** Step 3's use of `choice_to_index`. **Out:** what "simpler"
> means *within* one type, worked, and why it depends on the
> constraints.

`choice_to_index` maps a value to its position in a per-type ordering,
0 being simplest, and **the ordering depends on the constraints the
value was drawn under**:

```python
# internal/conjecture/choice.py, lines 325-337 — choice_to_index's contract.
# Line 330 is the one to hold on to: the index is relative to constraints.
   325  def choice_to_index(choice: ChoiceT, constraints: ChoiceConstraintsT) -> int:
   326      # This function takes a choice in the choice sequence and returns the
   327      # complexity index of that choice from among its possible values, where 0
   328      # is the simplest.
   329      #
   330      # Note that the index of a choice depends on its constraints. The simplest value
   331      # (at index 0) for {"min_value": None, "max_value": None} is 0, while for
   332      # {"min_value": 1, "max_value": None} the simplest value is 1.
   333      #
   334      # choice_from_index inverts this function. An invariant on both functions is
   335      # that they must be injective. Unfortunately, floats do not currently respect
   336      // ... 336–337: floats do not satisfy the invariant; "nothing has blown up - yet" ...
```

Note the admission on 335–337 rather than glossing it: floats are not
injective under this mapping, and the comment says so. That is the
honest state of the code, and it is the kind of thing a guide that
described "what the technique usually does" would never surface.

For unbounded integers the ordering is a **zigzag** outward from
`shrink_towards`:

```python
# internal/conjecture/choice.py, lines 306-312 — the whole ordering, five lines
   306  def zigzag_index(value: int, *, shrink_towards: int) -> int:
   307      # value | 0  1 -1  2 -2  3 -3  4
   308      # index | 0  1  2  3  4  5  6  7
   309      index = 2 * abs(shrink_towards - value)
   310      if value > shrink_towards:
   311          index -= 1
   312      return index
```

Work it, with `shrink_towards = 0`:

```
   value      10        3        0       -3
   2|a − v|   20        6        0        6
   v > a?     yes       yes      no       no
   index      19        5        0        6
```

which is where Step 3's table came from — 19 for `10`, 5 for `3`. And
positive-before-negative is not an accident of the formula, it is the
formula's *purpose*: `-3` and `3` are equally far away, and a reader
staring at a minimal failing example would rather see `3`.

The constraint-dependence in the comment is the payoff of Step 2's
typed sequence. Drawn under `min_value=1`, the simplest integer is `1`,
not `0` — so shrinking never proposes a value the generator could not
have produced. Constraint satisfaction, for free, forever.

### Step 5 — shrink passes, and the invariant that makes them terminate

> **In:** the order of Steps 3–4. **Out:** the shrinker's loop, and the
> one rule every pass must obey.

```python
# internal/conjecture/shrinker.py, lines 162-171 — the loop, from the class docstring
   162      The shrinker keeps track of a value shrink_target which represents the
   163      current best known ConjectureData object satisfying the predicate.
   164      It refines this value by repeatedly running *shrink passes*, which are
   165      methods that perform a series of transformations to the current shrink_target
   166      and evaluate the underlying test function to find new ConjectureData
   167      objects. If any of these satisfy the predicate, the shrink_target
   168      is updated automatically. Shrinking runs until no shrink pass can
   169      improve the shrink_target, at which point it stops.
```

A **shrink pass** is any function that proposes new choice sequences and
tests them; the target moves whenever a proposal is shortlex-smaller and
still fails. The end state is a **local minimum for every pass** — not a
global minimum, which nobody can promise.

Then the rule that makes "run every pass until none makes progress"
terminate:

```python
# internal/conjecture/shrinker.py, lines 187-199 — the determinism invariant
   187      In aid of this goal, the main invariant that a shrink pass much
   188      satisfy is that whether it makes progress must be deterministic.
   189      It is fine (encouraged even) for the specific progress it makes
   190      to be non-deterministic, but if you run a shrink pass, it makes
   191      no progress, and then you immediately run it again, it should
   192      never succeed on the second time. This allows us to stop as soon
   193      as we have run each shrink pass and seen no progress on any of
   194      them.
   195
   196      This means that e.g. it's fine to try each of N deletions
   197      or replacements in a random order, but it's not OK to try N random
   198      deletions (unless you have already shrunk at least once, though we
   199      don't currently take advantage of this loophole).
```

Read the distinction on 196–199 twice, because it is subtle and it is
the reason the loop has a stopping condition at all: *which* of the N
deletions you try first may be random; *whether you tried all N* may
not. Randomising the order is a heuristic; randomising the coverage
turns "no pass made progress" into "no pass happened to make progress
this time", and the fixpoint disappears.

### Step 6 — `find_integer`, the primitive underneath

> **In:** Step 5's passes, which need to answer "how far can I go?".
> **Out:** the search Hypothesis uses for every such question, and its
> cost, worked.

Most passes reduce to: find the largest `n` such that some predicate
holds — delete `n` choices, lower a value by `n`, and so on.

```python
# internal/conjecture/junkdrawer.py, lines 435-470 — find_integer. The linear
# prefix on 445-447 is the part that is not textbook.
   435  def find_integer(f: Callable[[int], bool]) -> int:
   436      """Finds a (hopefully large) integer such that f(n) is True and f(n + 1) is
   437      False.
   438
   439      f(0) is assumed to be True and will not be checked.
   440      """
   // ... 441–444: comment explaining the linear prefix ...
   445      for i in range(1, 5):
   446          if not f(i):
   447              return i - 1
   // ... 448–457: exponential probe upward, doubling hi ...
   458      while f(hi):
   459          lo = hi
   460          hi *= 2
   // ... 462–463: binary search between lo and hi ...
   464      while lo + 1 < hi:
   465          mid = (lo + hi) // 2
   466          if f(mid):
   467              lo = mid
   468          else:
   469              hi = mid
   470      return lo
```

Every call to `f` is a **full test execution**, so the call count is the
cost. Work two cases:

```
   answer n = 2:   f(1) ✓  f(2) ✓  f(3) ✗                        →  3 calls
   answer n = 100: linear   f(1) f(2) f(3) f(4) all ✓          →  4
                   probe    f(5) f(10) f(20) f(40) f(80) ✓,
                            f(160) ✗                           →  6   (lo=80, hi=160)
                   bisect   120 ✗  100 ✓  110 ✗  105 ✗
                            102 ✗  101 ✗                       →  6   (lo=100)
                   ─────────────────────────────────────────────────
                                                                  16 calls
```

The doubling gets logarithmic behaviour for large answers; the linear
prefix on 445–447 exists because, as the comment says, "it's very hard
to win big when the result is small. If the result is 0 and we try 2
first then we've done twice as much work as we needed to!" A pure
binary search would be asymptotically identical and measurably worse,
because small answers are the common case.

### Step 7 — the phases, and why a failure is sticky

> **In:** generation and shrinking as separate activities. **Out:** the
> engine's actual state machine, and the feature that matters most in CI.

```python
# _settings.py, lines 145-172 — the six phases, in the order they run
   145      explicit = "explicit"        # run @example-decorated cases
   150      reuse = "reuse"              # "previous test cases will be reused"
   155      generate = "generate"        # generate new test cases
   160      target = "target"            # "test cases will be mutated for targeting"
   165      shrink = "shrink"            # shrink failing cases
   170      explain = "explain"          # attempt to explain the failure
```

`reuse` is the one to notice. Hypothesis keeps a **database** of the
choice sequences that previously failed (by default `.hypothesis/`), and
replays them first. So a bug found once at 3am by seed luck is
re-checked on every subsequent run, and the fix is verified against the
exact case that broke — without anyone having to copy a counterexample
into the test file by hand.

That is the same property this topic's `crash_matrix` lane gets by
printing `first seed`, and the same one FoundationDB-style DST gets from
a replayable seed: **a failure must survive the process that found it.**
Three different mechanisms, one requirement.

`target` is Hypothesis's version of *targeted property-based testing*: a
test can call `target(value)` to say "bigger is more interesting", and
the engine mutates toward it (the optimiser is
`internal/conjecture/optimiser.py`, with a Pareto front in `pareto.py`
for the multi-objective case). Keep that in view for
[reading-antithesis.md](reading-antithesis.md), where the same idea
appears as *guidance* and drives a fleet instead of a loop.

### Step 8 — the DataTree: don't re-run a prefix you have already seen

> **In:** the generate phase of Step 7. **Out:** the structure that
> stops generation from repeating itself, which is a database structure.

```python
# internal/conjecture/datatree.py, lines 546-556 — what it is for
   546  class DataTree:
   547      """
   548      A DataTree tracks the structured history of draws in some test function,
   549      across multiple ConjectureData objects.
   550
   551      This information is used by ConjectureRunner to generate novel prefixes of
   552      this tree (see generate_novel_prefix). A novel prefix is a sequence of draws
   553      which the tree has not seen before, and therefore the ConjectureRunner has
   554      not generated as an input to the test function before.
```

A trie over executions: each node a drawn choice, each leaf a conclusion
(`Status.VALID`, etc.) or a `Killed` marker meaning "there is more below
here but it is not worth exploring" (`datatree.py:567-571`).
`generate_novel_prefix` walks it to produce a prefix that has never been
run.

Two things a database person should notice. First, this is a
**prefix index with pruning** — the same shape as a trie index in topic
2, used for deduplication instead of lookup. Second, it makes the search
*stateful across test cases*: a plain QuickCheck loop is memoryless and
will happily draw the same small case fifty times, which is why "10,000
examples" in one system is not comparable to "10,000 examples" in
another.

### Step 9 — swarm testing, and the deviation from the paper

> **In:** the generator of Step 1. **Out:** a bias that finds bugs
> uniform generation cannot, and an honest reading of how Hypothesis
> actually implements it.

```python
# strategies/_internal/featureflags.py, lines 21-35 — the technique, and the twist
    21  class FeatureFlags:
    22      """Object that can be used to control a number of feature flags for a
    23      given test run.
    24
    25      This enables an approach to data generation called swarm testing (
    26      see Groce, Alex, et al. "Swarm testing." Proceedings of the 2012
    27      International Symposium on Software Testing and Analysis. ACM, 2012), in
    28      which generation is biased by selectively turning some features off for
    29      each test case generated. When there are many interacting features this can
    30      find bugs that a pure generation strategy would otherwise have missed.
    31
    32      FeatureFlags are designed to "shrink open", so that during shrinking they
    33      become less restrictive. This allows us to potentially shrink to smaller
    34      test cases that were forbidden during the generation phase because they
    35      required disabled features.
```

**Swarm testing** (Groce et al., ISSTA 2012): instead of drawing every
operation from the same distribution every time, disable a random subset
of features for the whole test case. The reason it works is a fact about
distributions, not about bugs — if a bug needs 30 consecutive `delete`s
and `delete` has probability 1/5 per op, uniform generation will
effectively never produce it, whereas a test case in which `put` is
disabled entirely produces it immediately.

Now the deviation, which the code states and the paper does not:

```python
# strategies/_internal/featureflags.py, lines 54-58 — not the paper's model
    54          # In the original swarm testing paper they turn features on or off
    55          # uniformly at random. Instead we decide the probability with which to
    56          # enable features up front. This can allow for scenarios where all or
    57          # no features are enabled, which are vanishingly unlikely in the
    58          # original model.
```

With n features and independent fair coins, "all features on" has
probability 2^-n — at n = 10, about 1 in 1024, and at n = 20 about 1 in
a million. Hypothesis draws an enable-probability first, so the
all-on and all-off corners have real mass. That is a deliberate
distributional change, documented in a comment, and it is the sort of
thing to imitate when you write your own generator: say what
distribution you chose and why.

"Shrink open" (lines 32–35) is the other half. During shrinking the
flags become *less* restrictive, so a minimal example may use features
that were disabled when the bug was found. Without it, swarm testing
would trade a better search for worse counterexamples.

### Step 10 — stateful testing: where this meets a database

> **In:** everything above, which is about generating *values*.
> **Out:** the generator shape you actually want for a storage engine,
> and its relationship to this topic's DST harness.

```python
# stateful.py, lines 300-309 — the model-based interface
   300  class RuleBasedStateMachine(metaclass=StateMachineMeta):
   301      """A RuleBasedStateMachine gives you a structured way to define state machines.
   302
   303      The idea is that a state machine carries the system under test and some supporting
   304      data. This data can be stored in instance variables or
   305      divided into Bundles. The state machine has a set of rules which may read data
   306      from bundles (or just from normal strategies), push data onto
   307      bundles, change the state of the machine, or verify properties.
   308      At any given point a random applicable rule will be executed.
   309      """
```

A **rule** is a permitted operation; a **bundle** is a named pool of
values produced by earlier rules, so a rule can consume a key that some
earlier `put` created rather than a key drawn from nowhere. That single
mechanism is what makes generated histories *interesting* against a
storage engine: without it, almost every `get` misses.

And now the important observation for this topic. A rule-based state
machine is the same object as the DST harness in
[reading-fdb-simulation.md](reading-fdb-simulation.md) and
[reading-turso-simulator.md](reading-turso-simulator.md), with two
differences:

```
                        rule-based state machine     deterministic simulation
   what is generated    a sequence of operations     ops + faults + schedules
   what is controlled   the operations only          clock, disk, network, threads
   the oracle           a model in the same process  model + invariants
   minimisation         shrink passes to a fixpoint  replay the seed, then hand-cut
```

The column on the right controls more, and it is the reason a simulator
finds bugs a property test cannot. The column on the left *minimises*,
and it is the reason a property test's failures are cheap to act on.
Neither subsumes the other, which is why turso's simulator has a shrink
step and why this topic's exercise list asks you to build one.

## How to read the source (with the concepts in hand)

An afternoon, bottom-up, in this order:

1. `internal/conjecture/choice.py` whole (637 lines) — Steps 2 and 4.
   The `choice_to_index` / `choice_from_index` pair is the file.
2. `internal/conjecture/shrinker.py:73-94` (`sort_key`), then the
   `Shrinker` class docstring `:150-282` — read it as documentation, it
   is the best available explanation of the design.
3. `internal/conjecture/junkdrawer.py:435-470` (`find_integer`), then
   pick any one pass in `shrinker.py` and follow it into
   `internal/conjecture/shrinking/`.
4. `internal/conjecture/datatree.py:546-620` — the docstring includes
   a drawn example of the tree growing.
5. `internal/conjecture/engine.py`, the phase driver, for how Step 7's
   phases are sequenced.
6. `strategies/_internal/featureflags.py` (about 90 lines) and
   `stateful.py:300-450`.

If you write Rust: read `proptest`'s `strategy/traits.rs` and
`test_runner/` afterwards and match up the vocabulary. The design is the
same; the choice sequence is called something else.

## Questions (answer in notes.md)

1. Compute `sort_key` for the choice sequences `[0, 0, 5]` and `[7, 2]`
   with `shrink_towards = 0`, and say which the shrinker prefers.
   Then construct a pair where your intuition and shortlex disagree, and
   decide which of you is wrong.
2. `choice_to_index` depends on the constraints the value was drawn
   under. Give a concrete example — a bounded integer draw — where
   ignoring the constraints would make the shrinker propose an invalid
   test case, and say what the engine would do with it.
3. `find_integer` costs one test execution per call to `f`. For a
   200-operation failing history where the answer is "delete 150 of
   them", how many executions does one pass cost? What does that imply
   about shrinking a test whose single run takes 50 ms?
4. The determinism invariant (Step 5) forbids "try N random deletions".
   Write a shrink pass that violates it, and describe the symptom a user
   would see — not the theory, the symptom.
5. Take this topic's `crash_matrix` bug table. `TornWriteAccepted` is
   caught by 48.8% of seeds. Design a swarm-testing bias over the
   operation mix that should raise that number, predict the new rate,
   then implement and measure it.
6. A `RuleBasedStateMachine` over your KV store versus the DST harness
   of `dst_run`: name one bug class each finds that the other cannot,
   and say what it would cost to close the gap in either direction.

## Done when

Answer each before unfolding it.

- [ ] You can explain integrated shrinking without using the word
      "integrated".
  <details><summary>Answer</summary>

  The shrinker manipulates the **choice sequence** — the record of every
  primitive draw a generator made, with the constraints it was drawn
  under — and then *re-runs generation* on the modified sequence. So a
  shrunk value is valid by construction, no per-type shrinker is
  written, and constraints such as `min_value=1` are respected
  automatically because they are part of the recorded draw
  (`choice.py:325-332`). The alternative — shrinking the output value —
  needs one shrinker per type and cannot see the constraints that made
  the value legal.
  </details>

- [ ] You can compute `sort_key` and explain each of its two components.
  <details><summary>Answer</summary>

  `sort_key = (len(nodes), tuple(choice_to_index(v, constraints)))`
  (`shrinker.py:91-94`): shortlex — length first, then per-choice
  complexity indices lexicographically. With `shrink_towards = 0`,
  `[10]` has key `(1, (19,))`, `[0, 3]` has `(2, (0, 5))` and `[3, 0]`
  has `(2, (5, 0))`, so `[10] < [0,3] < [3,0]`. Length dominates because
  a shorter sequence means fewer decisions were made; earlier positions
  dominate within a length because early choices influence how many
  later choices exist.
  </details>

- [ ] You can compute the zigzag index of a value and say why the
      ordering is not simply by magnitude.
  <details><summary>Answer</summary>

  `index = 2·|shrink_towards − value|`, minus one if `value >
  shrink_towards` (`choice.py:306-312`). With `shrink_towards = 0`:
  `0 → 0`, `3 → 5`, `−3 → 6`, `10 → 19`. It is not magnitude order
  because `3` and `−3` are equidistant and the tie has to be broken —
  and it is broken toward the positive value, because a minimal
  counterexample reading `3` is easier to think about than one reading
  `−3`. The centre is `shrink_towards`, not zero, which is how
  "year 2000 is simpler than year 0" is expressed for datetimes.
  </details>

- [ ] You can state the invariant every shrink pass must satisfy and why
      it exists.
  <details><summary>Answer</summary>

  Whether a pass *makes progress* must be deterministic
  (`shrinker.py:187-194`): if it runs, makes no progress, and is
  immediately run again, it must not then succeed. Which progress it
  makes may be random. Without this, "run every pass; stop when none
  made progress" is not a termination condition, because a pass that
  samples randomly might have succeeded had it sampled differently. The
  code spells out the legal version: try N deletions *in a random
  order*, never N *random* deletions (`:196-199`).
  </details>

- [ ] You can say what the DataTree buys and name the data structure it
      is.
  <details><summary>Answer</summary>

  It is a **trie over test executions** (`datatree.py:546-556`): nodes
  are drawn choices, leaves are conclusions or `Killed` markers meaning
  "do not explore below here". `generate_novel_prefix` uses it to emit a
  prefix the runner has never executed, so generation does not
  rediscover the same small cases, and exhausted subtrees are pruned. It
  makes the search stateful across examples, which is why example counts
  are not comparable between property-testing libraries.
  </details>

- [ ] You can say how Hypothesis's swarm testing differs from the paper
      it cites, and why.
  <details><summary>Answer</summary>

  Groce et al. (ISSTA 2012) turn each feature on or off by an
  independent uniform coin. Hypothesis instead draws the *enable
  probability* up front (`featureflags.py:54-58`), so runs where all or
  no features are enabled — probability 2^-n each in the original model,
  about one in a million at 20 features — have real mass. The flags also
  "shrink open" (`:32-35`): during shrinking they become less
  restrictive, so the minimal example may use features that were
  disabled when the bug was found, which keeps the biased search from
  degrading the counterexample.
  </details>

- [ ] You can place property testing next to deterministic simulation
      rather than choosing between them.
  <details><summary>Answer</summary>

  A `RuleBasedStateMachine` generates *operations* against a model
  oracle and minimises the failure to a fixpoint. A DST harness
  additionally controls the clock, disk, network and scheduler, so it
  can generate *faults and interleavings* a property test cannot reach —
  and it replays from a seed rather than shrinking. So DST finds a
  strictly larger bug class, and property testing hands you a smaller
  report. This is why turso's simulator grew a shrink step, and why this
  topic asks for both.
  </details>

## References

- `HypothesisWorks/hypothesis` at the pinned commit (see the pin table
  at the end of [resources/codebases.md](../../resources/codebases.md)).
  Files read here, under `hypothesis/src/hypothesis/`:
  `internal/conjecture/choice.py`, `shrinker.py`, `junkdrawer.py`,
  `datatree.py`, `_settings.py`, `strategies/_internal/featureflags.py`,
  `stateful.py`.
- David R. MacIver, Zac Hatfield-Dodds et al., **"Hypothesis: A new
  approach to property-based testing"**, Journal of Open Source
  Software 4(43), 2019 — the citable overview; the design rationale
  lives in the source and on hypothesis.works.
- Alex Groce, Chaoqiang Zhang, Eric Eide, Yang Chen, John Regehr,
  **"Swarm Testing"**, ISSTA 2012 — Step 9's technique, and the model
  Hypothesis deliberately does not use.
- Andrea Löscher, Konstantinos Sagonas, **"Targeted Property-Based
  Testing"**, ISSTA 2017 — the `target` phase of Step 7.
- Koen Claessen, John Hughes, **"QuickCheck: A Lightweight Tool for
  Random Testing of Haskell Programs"**, ICFP 2000 — the type-directed
  design Step 1 is arguing with.
- In this topic: [reading-antithesis.md](reading-antithesis.md) (the
  same ideas at fleet scale),
  [reading-turso-simulator.md](reading-turso-simulator.md) and
  [reading-fdb-simulation.md](reading-fdb-simulation.md) (Step 10's
  right-hand column).
