# Collation: why `memcmp` does not sort text, and what a sort key is

Everything so far in this topic assumed that comparing two byte strings with
`memcmp` gives the order the user wants. For integers that is a construction
(the normalized key); for text it is simply false, and the gap between "bytes
order" and "the order people expect" is what a **collation** fills. The Unicode
Collation Algorithm (UCA, Unicode Technical Standard #10) defines how: map each
string to a *sort key* — an array of integers that compares correctly under a
plain memory comparison — and then every sort, index and merge in the system
can treat the string as bytes again. This chapter reads the UCA's sort-key
construction, builds a three-level toy of it that lane 3 implements, and then
reads how DuckDB (`d54cbbe3df`) wires ICU's sort keys into `ORDER BY`.

Sources: **UTS #10, Unicode Collation Algorithm**, version 18.0.0, revision 55
(2026-08-31, a draft at the time of reading), fetched from
`unicode.org/reports/tr10/` and read for §1, §3.6, §6–7, §9.1 (section numbers
below are that document's); ICU was not read, only DuckDB's calls into it.

## The problem in one sentence

`memcmp("zebra", "école")` says `"école" > "zebra"` (the UTF-8 byte `0xC3` of
`é` is larger than `0x7A` of `z`), a French reader expects the opposite, and
the correct comparison depends on accents, case and language — so how do you
get a byte string whose `memcmp` order *is* the language's order?

## The concepts, step by step

### Step 1 — byte order is not alphabetical order

> **In:** nothing. **Out:** the four ways `memcmp` and a collation disagree.

UTS #10 §1.1.1 states it flatly: "The position of characters in the Unicode
code charts does not specify their sort order", and because a collation must
be multi-level (Step 2), "it is not even possible to arrange the encoding for
characters so that simple binary string comparison produces the desired
collation order for any particular language." In practice the disagreements
are:

1. **Case:** `'Z'` is `0x5A` and `'a'` is `0x61`, so `"Zebra" < "apple"` by
   bytes; an alphabetical order has `apple` first.
2. **Accents:** `'é'` encodes in UTF-8 as `C3 A9`, larger than every ASCII
   letter, so every accented word sorts after `z`.
3. **Equivalence:** the same text can have two spellings (§1.2, "canonically
   equivalent" sequences: `é` as one code point or as `e` plus a combining
   accent); they must collate equal but differ as bytes. A collation must
   normalise before it compares.
4. **Language:** the order of letters differs by locale (§1.4, Customization; the
   per-language tailoring data lives in CLDR, which this chapter did not read).

Why it matters: a normalized key for `ORDER BY name` is only correct under a
binary collation. Under any other, the string has to go through a collation
first, and *that* is the extra per-row work this lane measures.

### Step 2 — multi-level comparison

> **In:** Step 1's disagreements. **Out:** the level structure, worked on the
> UTS's own Table 2 and on lane 3's toy.

Compare strings in levels (§1.1, Table 2). **L1, the primary level:** base
letters only — `role < roles < rule`. Differences at L1 decide the order and
nothing else matters. **L2, secondary:** accents — `role < rôle < roles`: the
table's note says that in the second comparison (`rôle` vs `roles`) the accent
difference is "swamped" by an L1 difference, the extra `s`, so `rôle` sorts
before `roles` even though it carries an accent; the accent only decides
between strings whose base letters tie. **L3, tertiary:** case and variants —
`role < Role < rôle`: `Role` sorts before `rôle` because the L2 accent
difference outranks the L3 case difference. Further levels cover punctuation (L4) and an identical level.

Lane 3's toy collation is exactly three levels. Work it on four spellings
that tie at L1:

```
cafe  : L1 = c a f e   L2 = 20 20 20 20   L3 = 02 02 02 02
Cafe  : L1 = c a f e   L2 = 20 20 20 20   L3 = 08 02 02 02
café  : L1 = c a f e   L2 = 20 20 20 21   L3 = 02 02 02 02
Café  : L1 = c a f e   L2 = 20 20 20 21   L3 = 08 02 02 02
```

All four have the same primary weights, so L1 ties. L2 separates them into
{cafe, Cafe} (all `20`) before {café, Café} (last position `21`, the acute
accent). Within each pair, L3 puts the lowercase (`02`) before the uppercase
(`08`). The order is `cafe < Cafe < café < Café`, accent outranking case, as in
the UTS example. This is the first test in `collation.rs`.

Why it matters: a **case- and accent-insensitive** collation is not a
different algorithm; it is the same one with the *maximum level* set to 1 —
§7.3: "if the maximum level is set to 2, then level 3 and higher weights are
not appended to the sort key". With the maximum level at 1 only primary
weights are compared, so `cafe`, `Café` and `CAFÉ` all collate equal.

### Step 3 — collation elements

> **In:** Step 2's levels. **Out:** the data structure that carries the
> weights, and what the toy leaves out.

The UCA maps each character (or sequence) to one or more **collation
elements**, each a tuple of weights, one per level, written
`[.primary.secondary.tertiary]` (§3, §6). A **weight** is just a 16-bit
integer; a smaller weight sorts first. A base letter gets a distinct primary
and the *neutral* secondary and tertiary `0020` and `0002`; an uppercase form
differs only in its tertiary weight; an accent is an element with primary
`0000` — **ignorable at L1** — and its own secondary weight. Lane 3's table
(`collation::ce`) follows the shape: primary `0x0400 + 16·(letter index)`,
secondary `0x0020` plus an accent id, tertiary `0x0002` lowercase and `0x0008`
uppercase.

What the toy leaves out, because each would hide the measured effect behind
code: **contractions** (a sequence of characters mapping to one element; the
UTS says digraphs such as "ch" in Spanish or Czech belong in per-language
tailorings, not the default table, §6.1.2), **expansions** (one character,
several elements), **variable weighting** (§4, how punctuation and spaces are
treated), **backward levels** (a French secondary-level option, §7.3 S3.6–S3.9)
and **canonical normalisation**: "UCA specifies that an input string shall
first be normalized into NFD" (§3.5), which splits a precomposed `à` into `a`
plus a combining grave, the latter a primary-ignorable element — exactly the
third element of Figure 2 below. The sort key of a real collation is therefore
not 3 weights per character; it is 3 weights per *collation element*, and a
decomposed accented letter is two elements.

### Step 4 — the sort key

> **In:** Steps 2–3. **Out:** the construction (§7.3), worked on the UTS's
> own example and on the toy's.

A **sort key** is "an array of non-negative integers, associated with an input
string, ... suitable for binary comparison" (§3.6, D36). The construction
(§7.3, steps S3.1–S3.5): for each level from 1 up to the maximum, append that
level's weight from every collation element — skipping zero weights — with a
**level separator** `0000` between levels. The separator is smaller than any
real weight, which is what makes a shorter string that is a prefix of a longer
one sort first. The UTS's Figure 2:

```
collation elements:  [.0706.0020.0002] [.06D9.0020.0002] [.0000.0021.0002] [.06EE.0020.0002]
sort key:            0706 06D9 06EE | 0000 | 0020 0020 0021 0020 | 0000 | 0002 0002 0002 0002
```

Level 1 contributes the three non-zero primaries (the third element's primary
is `0000` and is skipped); then a separator; then all four secondaries; then a
separator; then all four tertiaries: 3 + 1 + 4 + 1 + 4 = 13 weights.

The toy's version, with 16-bit big-endian weights (the lane-3 spec):
`sort_key("ab")` is
`04 00 04 10 | 00 00 | 00 20 00 20 | 00 00 | 00 02 00 02` — 14 bytes for a
2-character string. Compare `"ab"` with `"abc"`: the first 4 primary bytes
tie; then `"ab"` has the separator `00 00` where `"abc"` has `04 20`, and
`00 < 04`, so `"ab" < "abc"`. Compare `"ab"` with `"aB"`: primaries tie, the
separator ties, secondaries tie, and the tertiaries differ at the second
weight, `00 02` vs `00 08`: lowercase first.

The cost: the key is about **6 bytes per character plus 4**, versus 1–2 bytes
of UTF-8 per Latin character. §9.1 lists the standard reductions — eliminate
the level separators when weights are chosen so that all L3 weights are below
all L2 weights (the `càb` example shrinks from 13 weights to 11), and store
L2/L3 weights in 8 bits when they are small — which together give "significantly
shorter and more efficient sort keys while retaining the same ordering".

Why it matters: after this construction `memcmp(sort_key(a), sort_key(b))`
equals the collation's comparison — §1.7 states it as `A ≤ B according to C if
and only if sortkey(C, A) ≤ sortkey(C, B)`. The sort no longer needs to know
what a collation is.

### Step 5 — when a sort key pays for itself

> **In:** Step 4. **Out:** the break-even the UTS states, as a formula to
> test against lane 3's measurement.

§1.7 gives both sides of the trade: "simple string comparison is faster for
any individual comparison, because the generation of a sort key requires
processing an entire string, while differences in most string comparisons are
found before all the characters are processed. Typically ... simple string
comparison being about 5 to 10 times faster than generating sort keys and then
using a binary comparison." And the other side: "it is faster to use sort keys
whenever there will be more than about 10 comparisons per string, if the system
can afford the storage."

Turn it into arithmetic. Let *g* be the cost of building one key, *c* the
cost of one on-the-fly comparison, *b* the cost of one binary key comparison,
and *m* the number of comparisons each string takes part in. A comparison sort
makes about n·log₂ n comparisons and each involves two strings, so
*m* ≈ 2·log₂ n (for n = 2,000,000, 2 · 20.9 = 41.8). On-the-fly costs
`n · (m/2) · c`; keys cost `n · g + n · (m/2) · b`. Keys win when
`g < (m/2)(c − b)`, i.e. beyond the break-even `m* = 2g / (c − b)`.

Worked with *hypothetical* constants, the UTS's "5 to 10 times": `g = 7c`
(the middle of the range) and `b ≈ 0` give `m* = 14`, i.e. `2·log₂ n > 14`,
`n > 128` rows. That is the shape of the claim: the break-even is a count of
comparisons per string, and a comparison sort reaches any fixed count at a
modest `n`. Whether *this* toy collation has `g ≈ 7c` is a measurement, not an
assumption: lane 3's reference sweep (n from 16 to 2,000,000, `notes.md`)
records `g`, `c` and `b` per input size, the comparisons per string
(counted with a counter inside the comparator, not computed from `n`), the
observed speed-up, and the `m*` the formula gives. Rust's `sort_by_cached_key`
is the same trade in the standard library
([reading-rust-sort.md](reading-rust-sort.md) Step 5).

### Step 6 — how DuckDB wires a collation into ORDER BY

> **In:** Steps 4–5. **Out:** the three lines of the pinned code that matter:
> the sort key is produced once per row, as a BLOB, by a scalar function.

At the pin a collation is a *scalar function*, not a comparator. The binder
treats `ORDER BY` expressions through `PushCollation`
(`src/planner/binder/query_node/bind_select_node.cpp:402`), which for a VARCHAR
with a non-binary collation wraps the expression in the collation's
function(s) (`src/planner/collation_binding.cpp:229-238`); a binary, `C` or
`POSIX` collation adds nothing (`:35-38`), so a binary `ORDER BY` pays no
collation cost at all. The ICU extension's function returns a BLOB:

```cpp
// icu_extension.cpp, duckdb/duckdb at d54cbbe3df — lines 142-158 (the lines to read: 154-158).
 142  //! Writes the sort key of every string, either as a blob or in hexadecimal. The hexadecimal
 143  //! form is only used by the functions that are kept for backwards compatibility.
 144  template <bool HEX>
 145  static void ICUCollateFunction(DataChunk &args, ExpressionState &state, Vector &result) {
 ...
 152  	UnaryExecutor::Execute<string_t, string_t>(args.data[0], result, [&](string_t input) {
 153  		// create a sort key from the string, the trailing null byte is not part of the result
 154  		collator.GetSortKey(input.GetData(), input.GetSize(), buffer);
 155  		auto &key = buffer.key;
 156  		auto key_size = key.size() - 1;
 157  		if (!HEX) {
 158  			return StringVector::AddStringOrBlob(result, const_char_ptr_cast(key.data()), key_size);
```

The function runs a vector at a time (`UnaryExecutor`), reuses a per-thread
buffer so that "generating sort keys does not allocate" (the
`CollatorLocalState` comment, lines 132–135), and its signature says what it is
for: "The function a collation pushes into a query, it writes the sort key as a
blob" (line 210, `GetCollateFunction`). An `ORDER BY name COLLATE de` therefore
becomes an `ORDER BY` on `collate_de(name)`: the expression is an ordinary BLOB
column, which `create_sort_key` ([reading-duckdb-sort.md](reading-duckdb-sort.md)
Step 4) encodes like any other (BLOBs take the escape route,
`create_sort_key.cpp:284` onward). That is the whole of lane 3 in production
form: compute the key once, then sort bytes — and note it computes the key *in
the projection before the sort*, exactly as the lane's reference does.

The case- and accent-insensitive collation of lane 1c exists in DuckDB under a
name: `icu_noaccent` is registered with the tag `und-u-ks-level1-kc-true`
(`icu_extension.cpp:437`). The `ks-level1` part requests collation strength 1,
which is Step 2's "maximum level = 1"; the tag syntax itself was not checked
against the LDML specification in this chapter.

One detail is visible in the code: the function drops the key's trailing null
byte (lines 153 and 156), and the hexadecimal variant asserts that every other
key byte is non-zero (line 163). That is the property a zero-delimited
encoding such as the string operator of `create_sort_key` needs; it is read
here from DuckDB's code, not from ICU's documentation.

Why it matters: a collation turns a string column into a *longer* BLOB column.
Everything about the key being wide — fewer rows per cache line, 16-byte
prefixes that no longer contain whole words, ties on long prefixes — applies
to it (README, lane 2's `b3` case).

## How to read the source

1. UTS #10 §1.1 (levels, Table 2), §3.6 (D36–D37), §7.3 (Figure 2, S3.1–S3.10),
   §1.7 (the performance paragraph quoted in Step 5), §9.1 (shortening).
2. `collation.rs` in this crate: `ce` and the table first, then
   `collate_cmp`'s three loops, then the stub `sort_key`.
3. `extension/icu/icu_extension.cpp:132-170` and `:210-215` at the pin;
   `:437-444` for `icu_noaccent` and the `icu_sort_key` function.

## Questions (answer in notes.md)

1. Write out `sort_key("Ab")` byte by byte from the toy table, and compare it
   with `sort_key("ab")` and `sort_key("áb")`. Which is smallest?
2. The UTS's Figure 2 key has 13 weights for a 3-letter, one-accent string.
   What would the toy's key length be for `"càb"` precomposed (3 characters)?
   Why does the UCA's differ?
3. `gen_collation_strings(n)` draws `n / 4` base words and each row picks
   one uniformly. Predict the number of distinct *primary-level* classes
   (the number of distinct strings under a maximum level of 1, case and
   accents ignored) for `n = 2,000,000`, then check it against
   `notes.md`. (Hint: a base is picked at least once with probability
   `1 − e^(−4)`.)
4. Using §1.7's "about 10 comparisons per string", for which `n` is a
   comparison sort past the break-even? Compare with lane 3's measured sweep.
5. What does each approach cost in memory — a collation evaluated inside the
   comparator, and sort keys built once by a projection — and at what string
   length does the key stop fitting in DuckDB's widest fixed key (32 bytes
   with a payload pointer, `tuple_data_layout.cpp:225-230`)?

## Done when

Answer each before unfolding it.

- [ ] You can order `cafe`, `Café`, `Cafe`, `café` under a three-level collation and say which level decides each pair.

  <details><summary>Answer</summary>

  `cafe < Cafe < café < Café`. All four tie at L1 (same base letters).
  L2 (accents) separates {`cafe`, `Cafe`} from {`café`, `Café`} — the acute
  accent gives the last position secondary `0021` against `0020`. Inside each
  pair L3 (case) decides: lowercase `0002` before uppercase `0008`. Accent
  outranks case because L2 is compared before L3 (§1.1, Table 2).

  </details>

- [ ] You can build the sort key of the UTS's Figure 2 collation-element array.

  <details><summary>Answer</summary>

  Elements `[.0706.0020.0002] [.06D9.0020.0002] [.0000.0021.0002]
  [.06EE.0020.0002]`: append the non-zero primaries `0706 06D9 06EE`, the
  separator `0000`, all secondaries `0020 0020 0021 0020`, `0000`, all
  tertiaries `0002 0002 0002 0002` — 13 weights (§7.3, Figure 2; the third
  element is a primary-ignorable accent whose `0000` primary is skipped).

  </details>

- [ ] You can say why a shorter string that is a prefix of a longer one sorts first, and what makes it work.

  <details><summary>Answer</summary>

  After the shared primaries, the shorter key has the level separator `0000`
  where the longer key has a real primary weight, and the separator is smaller
  than every weight (§7.3 note under S3.2, D37) — so `"abc" < "abcX"` for any
  `X`, "in the absence of special features like contractions".

  </details>

- [ ] You can state the UTS's guidance on when to precompute sort keys, and turn it into a break-even.

  <details><summary>Answer</summary>

  Direct comparison is "about 5 to 10 times faster" than generating a key and
  comparing it, but keys are cheaper "whenever there will be more than about 10
  comparisons per string, if the system can afford the storage" (§1.7). With
  `g` the key-building cost, `c` an on-the-fly comparison and `b ≈ 0` a binary
  comparison, keys win when `g < (m/2)·(c − b)`; with `g = 7c` and `m ≈ 2·log₂ n`
  comparisons per string that is `log₂ n > 7`, n > 128.

  </details>

- [ ] You can explain how DuckDB's ICU extension integrates a collation into a sort.

  <details><summary>Answer</summary>

  A collation registers a scalar function `collate_<locale>(VARCHAR) → BLOB`
  that writes ICU's sort key for every string, a vector at a time with a
  reused buffer (`icu_extension.cpp:132-158,210-213`). The `ORDER BY` key
  becomes that BLOB column, which the normal sort-key encoder turns into
  bytes; the sort never sees a collation. A case- and accent-insensitive
  variant, `icu_noaccent`, is the same function with strength level 1
  (`:437`).

  </details>

## References

- Unicode Technical Standard #10, **Unicode Collation Algorithm**, version
  18.0.0 revision 55 (2026-08-31) — §1.1 (Table 2), §1.2, §1.7, §3.6 (D36–D37),
  §7.3 (Figure 2), §9.1.
- DuckDB at `d54cbbe3df`: `extension/icu/icu_extension.cpp`,
  `src/function/scalar/create_sort_key.cpp`.
- Previous: [reading-ips4o.md](reading-ips4o.md). Next:
  [reading-radix-sort.md](reading-radix-sort.md) — or back to
  [reading-duckdb-sort.md](reading-duckdb-sort.md).
