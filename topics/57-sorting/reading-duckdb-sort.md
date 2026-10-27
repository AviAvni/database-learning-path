# Normalized keys: how DuckDB turns a multi-column ORDER BY into one byte string

A relational sort is not a sort of integers. It sorts rows by a list of
columns with different types, `ASC`/`DESC` per column, `NULLS FIRST/LAST` per
column and, for strings, a collation — and the cost of a comparison sort is
dominated by the comparator that has to interpret all of that on every one of
its `n log n` calls. Kuiper and Mühleisen's ICDE 2023 paper makes one move that
removes the interpretation: encode each row's key once into a byte string whose
plain byte-wise order *is* the `ORDER BY` order. This chapter builds that idea
from the comparator up, works the paper's own example by hand, and then reads
how the pinned DuckDB (`d54cbbe3df`) actually encodes, sizes, and sorts those
keys — which differs from the paper in ways worth knowing.

Anchors into DuckDB are quoted at the commit recorded in the pin table of
`resources/codebases.md`, `d54cbbe3df`, with the line numbers they occupy
there. The paper is Kuiper & Mühleisen, **"These Rows Are Made for Sorting and
That's Just What We'll Do"**, ICDE 2023, read in full; its section numbers are
quoted as §.

## The problem in one sentence

`ORDER BY c_birth_country DESC NULLS LAST, c_birth_year ASC NULLS FIRST` is a
comparison with four independent rules that a comparison sort re-evaluates
about `n log₂ n` times — can the rules be applied *once per row* so that every
comparison afterwards is a `memcmp`?

## The concepts, step by step

### Step 1 — the comparator is the cost

> **In:** nothing — this step is the motivation.
> **Out:** the two costs a relational sort pays that an integer sort does not,
> and the vocabulary (**key columns**, **payload columns**, **NSM**, **DSM**)
> the later steps use.

A **key column** is a column that appears in `ORDER BY`; every other selected
column is a **payload column** (§II). A **comparison sort** (quicksort,
mergesort) decides order by calling a comparator on pairs of rows, about
n·log₂ n times: sorting 1,000,000 rows costs roughly 1,000,000 × 19.9 ≈ 20
million comparator calls (log₂ 1,000,000 = 19.93). Two things make that call
expensive for a relational row:

1. **Interpretation.** The comparator must look up, per key column, its type,
   its direction and its null rule. In a vectorized engine that is decided at
   run time, so each call carries branches and often a function-pointer call
   per column (§V-B).
2. **Memory access.** If rows are stored **columnar** (the **DSM**, "decomposition
   storage model": one array per column), comparing two rows reads one cache
   line per key column per row — random access. If they are stored as
   **rows** (the **NSM**, "N-ary storage model": all of a row's fields side by
   side), the key fields of a row share a cache line.

The paper's micro-benchmarks (§IV) sort `u32` columns with 2^10 to 2^24 rows
and report, for `std::sort` in all cases, that sorting row format is faster
than the best columnar approach on almost every input and "especially
noticeable when sorting a large number of tuples" (Fig. 3, §IV-B); row sorting
incurs "fewer cache misses than sorting columnar format data by an order of
magnitude" (§IV-B, from the counters in Tables II and III). The *subsort* trick — sort by column 1, find ties,
sort the ties by column 2 — removes the per-comparison branch for columnar
data but not the cache misses, and cannot be used when sorted runs are merged
or when a merge join reads two sorted runs, because those need a full-row
comparison per step (§V-B).

Why it matters: both costs vanish if a row's whole key is one flat byte
string compared with `memcmp`. That is Step 2.

### Step 2 — a normalized key is the ORDER BY, pre-evaluated

> **In:** Step 1's two costs. **Out:** the encoding rules, checked on Fig. 5 of
> the paper by hand.

A **normalized key** (§VI-A, "key normalization", which the paper dates to
System R) is "a single order-preserving string from a sequence of values": a
byte string such that comparing two keys byte by byte (`memcmp`, i.e.
**lexicographic** order — compare the first byte, then the second on a tie, a
shorter string being smaller when it is a prefix) gives the same answer as the
`ORDER BY` comparator on the original rows. The construction, per key column,
left to right:

- **Unsigned integers:** write the bytes **big-endian** (most significant byte
  first), because `memcmp` looks at byte 0 first and the high-order byte must
  decide.
- **Signed integers:** big-endian, then flip the top bit. Two's complement
  gives negatives a leading 1 bit, so unflipped they would sort after the
  positives; flipping maps `-2^31 … 2^31−1` monotonically onto `0 … 2^32−1`.
- **`DESC`:** invert every byte of the column (`b → 255 − b`), which reverses
  the order of any byte string.
- **`NULL`:** one extra byte in front of each column's value, `0` or `1`,
  chosen so nulls sort first or last.
- **Strings:** a fixed-length prefix, zero-padded, and the full string compared
  only when prefixes tie (the paper encodes the first 12 bytes, §VII; the
  pinned code differs, Step 4).

Work Fig. 5 (§VI-A) as a check. Rows `(NETHERLANDS, 1992)` and
`(GERMANY, 1924)`, ordered `c_birth_country DESC, c_birth_year ASC`; the
figure shows the keys (a NULL byte is omitted there):

```
country  "GERMANY" padded to 12 bytes:  71 69 82 77 65 78 89  0 0 0 0 0
DESC flip, 255 - b:                    184 186 173 178 190 177 166 255 255 255 255 255
year     1924 = 7·256 + 132:  big-endian  0 0 7 132
sign bit of the first byte: 0 XOR 128 = 128 :   128 0 7 132
key = 184 186 173 178 190 177 166 255 255 255 255 255 128 0 7 132
```

That is the second key printed in the figure. For `NETHERLANDS` the 11
letters flip to 177 186 171 183 186 173 179 190 177 187 172 and the twelfth
(terminator) byte 0 flips to 255; `1992 = 7·256 + 200` encodes as 128 0 7 200.
The first byte already decides the comparison: `NETHERLANDS` starts with
177 < 184, so it sorts before `GERMANY` — and since the sort is `DESC`, N
before G is correct (N is later in the alphabet).

Why it matters: after this encoding a comparator that handled four rules per
column becomes `memcmp(key_a, key_b, KEY_LEN)`. The interpretation cost moved
from `n log n` comparator calls to `n` encode calls — and the encoding is done a
vector at a time with tight loops specialised on the column type, so its own
interpretation cost is amortised over the vector (§VI-A, §VII).

### Step 3 — what fixed width buys, and what it costs

> **In:** Step 2's keys. **Out:** why the key has a fixed length, and the
> price of making it so.

A key of fixed length lets rows be moved by `memcpy` of a known size, so rows
can swap places in an array — which the paper credits with better cache
behaviour (§VI-A). Variable-length columns (strings) break fixed width: the
paper encodes only a prefix, and compares "the rest of the string only if the
prefixes are equal". Two prices follow, both measured later in this topic:

- **A prefix can tie while the strings differ.** Two strings sharing their
  first 12 bytes have equal key bytes, so the fast path cannot order them and
  a slower tie-break on the full strings runs. Lane 2's `b3` data set — every
  string starts with the same 20 bytes — is built to exercise exactly this.
- **The key repeats data.** A key column's bytes are stored in the key *and*,
  unless dropped, in the payload. DuckDB's `Sort` removes a column that appears
  in both, by mapping input column → key position (`src/common/sort/sort.cpp:64-71`) —
  except for FLOAT and DOUBLE, whose key encoding maps `-0.0` and `0.0` to the
  same bytes and so cannot be decoded back; those stay in the payload
  (`sort.cpp:79-84`).

The paper also times `memcpy`/`memcmp` with sizes known at compile time versus
run time (§VI-B, Fig. 6): on the ARM Graviton2 the static `memcmp` is about
25% faster on average below 16 bytes and the static `memcpy` about 55% faster
overall — a reason DuckDB templates its sort on key width, which Step 4
reads.

### Step 4 — the pinned code: the key is a *function*, and its width is derived

> **In:** Steps 2–3. **Out:** the three facts to take from the code — how a
> key is assembled, how its width and type are chosen, and which bytes are
> declared skippable.

At the pin, the key is built by a scalar function, `create_sort_key`, bound
once per `ORDER BY` (`src/function/scalar/create_sort_key.cpp`). Its byte
vocabulary:

```cpp
// create_sort_key.cpp, lines 97-102 at d54cbbe3df
  97  struct SortKeyVectorData {
  98  	static constexpr data_t NULL_FIRST_BYTE = 1;
  99  	static constexpr data_t NULL_LAST_BYTE = 2;
 100  	static constexpr data_t STRING_DELIMITER = 0;
 101  	static constexpr data_t LIST_DELIMITER = 0;
 102  	static constexpr data_t BLOB_ESCAPE_CHARACTER = 1;
```

NULL is a validity byte `1` or `2` — chosen so that, together with the
`NULLS FIRST/LAST` swap (lines 112–116 of the same file), one byte decides
where NULLs go. A fixed-width value is `Radix::EncodeData` (big-endian +
sign flip, exactly Step 2) and, for `DESC`, every byte is inverted:

```cpp
// create_sort_key.cpp, lines 191-213 at d54cbbe3df (the lines to read: 201, 202-211)
 191  template <class T>
 192  struct SortKeyConstantOperator {
 193  	using TYPE = T;
 ...
 199  	template <bool FLIP_BYTES>
 200  	static idx_t Encode(data_ptr_t result, TYPE input) {
 201  		Radix::EncodeData<T>(result, input);
 202  		if (FLIP_BYTES) {
 203  			// flip word-at-a-time - the encoded size is a compile-time constant, so this fully unrolls
 204  			idx_t b = 0;
 205  			for (; b + SwarWord::SIZE <= sizeof(T); b += SwarWord::SIZE) {
 206  				Store<uint64_t>(~Load<uint64_t>(result + b), result + b);
 207  			}
 208  			for (; b < sizeof(T); b++) {
 209  				result[b] = static_cast<data_t>(~result[b]);
 210  			}
 211  		}
 212  		return sizeof(T);
 ...
```

`FLIP_BYTES` is a template parameter, so the direction is resolved when the
code is instantiated, not per row. (`EncodeSigned`, the sign flip, is
`src/include/duckdb/common/radix.hpp`'s `Radix::EncodeSigned`.)

**Strings are not padded in the pinned code — they are delimited.** Every byte
is incremented by one and a `0` delimiter follows:

```cpp
// create_sort_key.cpp, lines 232-255 at d54cbbe3df (the lines to read: 251, 253)
 232  struct SortKeyVarcharOperator {
 233  	using TYPE = string_t;
 ...
 239  	//! Strings are encoded by incrementing every byte, so that no byte collides with the delimiter.
 ...
 250  		for (; pos < input_size; pos++) {
 251  			result[pos] = SortKeyWord::FlipByte<FLIP_BYTES>(static_cast<data_t>(input_data[pos] + 1));
 252  		}
 253  		result[input_size] = SortKeyWord::FlipByte<FLIP_BYTES>(SortKeyVectorData::STRING_DELIMITER);
 254  		return input_size + 1;
```

Worked on `"ab"`: `'a' = 97 → 98`, `'b' = 98 → 99`, delimiter 0, so the key bytes
are `98 99 0`. `"abc"` is `98 99 100 0`. At byte 2, `0 < 100`, so `"ab" <
"abc"` — a proper prefix sorts first, correctly, with no padding and no
length field. Under `DESC` every byte inverts (`255 − b`): `157 156 255` vs
`157 156 155 255`; now `255 > 155`, so `"ab"` sorts *after* `"abc"`, which is
what a descending order must do. The padded prefix of Step 2 has a weakness
the delimiter scheme avoids: `"ab"` padded with zeros is byte-for-byte the
key of the string `"ab\0"`, so padding is only sound if no string contains a
zero byte (lane 2's generator guarantees that; a real engine cannot). The
delimiter scheme keeps the order for any string without a `0xFF` byte, at the
price of the `+1`, which wraps for `0xFF`. (BLOBs, which may
contain any byte, take a different route: escape both `\x00` and `\x01`,
`SortKeyBlobOperator`, lines 284 onward. Whether VARCHAR input can ever
contain a `0xFF` byte is not something this chapter checked in the code.)

How wide is the key? That is decided at layout time, from the types and the
column statistics, not from the data:

```cpp
// tuple_data_layout.cpp, lines 169-196 at d54cbbe3df (the lines to read: 176-179, 184-195)
 169  	sort_width = 0;
 170  	bool all_valid_and_truly_constant = true;
 171  	for (idx_t order_idx = 0; order_idx < orders.size(); order_idx++) {
 ...
 176  		if (all_valid_and_truly_constant && order.stats && !order.stats->CanHaveNull() &&
 177  		    TypeIsConstantSize(physical_type) && sort_width < 7) {
 178  			// We don't have to sort by this byte, all values are valid
 179  			sort_skippable_bytes.emplace_back(sort_width);
 180  		} else {
 181  			all_valid_and_truly_constant = false;
 182  		}
 183  
 184  		if (TypeIsConstantSize(physical_type)) {
 185  			// NULL byte + fixed-width type
 186  			sort_width += 1 + GetTypeIdSize(physical_type);
 187  		} else if (logical_type == LogicalType::VARCHAR && order.stats &&
 188  		           StringStats::HasMaxStringLength(*order.stats)) {
 189  			// NULL byte + maximum string length + string delimiter
 190  			sort_width += 1 + StringStats::MaxStringLength(*order.stats) + 1;
 191  		} else {
 192  			// We don't know how long the key will be
 193  			sort_width = DConstants::INVALID_INDEX;
 194  			break;
 195  		}
```

Work two cases. `ORDER BY a INTEGER` with statistics proving no NULLs: the
first column's NULL byte is skippable (offset 0 recorded, line 179), and
`sort_width = 1 + 4 = 5`. `create_sort_key`'s bind function (lines 85–88 of
`create_sort_key.cpp`) returns a `BIGINT` when everything is constant and the
width is at most 8 bytes, and the layout picks `NO_PAYLOAD_FIXED_8` (lines
215–218 of `tuple_data_layout.cpp`): the whole key is one `uint64_t`. Second,
`ORDER BY a INTEGER, s VARCHAR` where `s` has a known maximum length of 10
and the query has payload columns: `sort_width = (1 + 4) + (1 + 10 + 1) = 17`;
with a payload pointer, `+ 8 = 25`; 25 > 24 and ≤ 32, so the key type is
`PAYLOAD_FIXED_32` (lines 225–227). A VARCHAR with no known maximum length
sets `sort_width` to invalid and the layout falls to
`PAYLOAD_VARIABLE_32`, a 32-byte inline part plus a heap pointer (lines
228–230).

Why it matters: DuckDB does not fix a prefix length for strings; it fixes a
*width class* (8, 16, 24 or 32 bytes) from the statistics, and falls back to a
variable-size key only when it cannot bound the string.

### Step 5 — comparing a key: integer compares, not `memcmp`

> **In:** Step 4's key types. **Out:** how a fixed key is actually compared,
> and why the paper's "memcmp" became `uint64_t` compares.

The paper compares keys with `memcmp` (§VI-B). The pinned fixed-width keys
store their bytes in 8-byte parts and compare part by part as integers:

```cpp
// sort_key.hpp, lines 148-156 and 223-225 at d54cbbe3df
 148  template <idx_t REMAINING>
 149  static bool SortKeyLessThan(const uint64_t *const &lhs, const uint64_t *const &rhs) {
 150  	return (*lhs < *rhs) || ((*lhs == *rhs) && SortKeyLessThan<REMAINING - 1>(lhs + 1, rhs + 1));
 151  }
 ...
 223  	friend bool operator<(const SORT_KEY &lhs, const SORT_KEY &rhs) {
 224  		return SortKeyLessThan<SORT_KEY::PARTS>(&lhs.part0, &rhs.part0);
 225  	}
```

A big-endian `uint64_t` compares exactly like its eight bytes under `memcmp`,
so the two are the same order — but the integer form compiles to one
compare-and-branch per 8 bytes with the part count known at compile time
(`PARTS` is a template parameter, so the recursion unrolls). To make that
true the key parts must be byte-swapped on a little-endian machine, and the
code says so (lines 189–192): "We don't actually store the data in
byte-comparable order! This allows us to do int64_t comparisons". Read the
line with `ByteSwap();` (192) and notice that it implies a second swap when
decoding.

Worked: the two 8-byte keys `01 02 03 04 05 06 07 08` and
`01 02 03 04 05 06 07 09`. As big-endian `uint64_t` values they are
`0x0102030405060708` and `0x0102030405060709`; the first is smaller because
its low byte is, 8 < 9. `memcmp` reaches the first differing byte, byte 7, and
also finds `08 < 09`. Now swap the roles: `02 00 00 00 00 00 00 00` against
`01 FF FF FF FF FF FF FF`. `memcmp` stops at byte 0 (2 > 1); the integers are
`0x0200000000000000` and `0x01FFFFFFFFFFFFFF`, and the first is larger. The
two views agree because the most significant byte of a big-endian integer is
the first byte.

### Step 6 — the sort itself: radix on the first 8 bytes, comparison for the rest

> **In:** Step 5's comparison and the paper's §VI-C. **Out:** the algorithm
> the pinned DuckDB runs, and the differences from the paper.

**The paper (§VI-C, §VII).** It implements LSD and MSD radix sort — **radix
sort** distributes rows into 256 buckets by one byte of the key, then repeats
on the next byte; **LSD** (least significant digit first) makes one stable
pass per byte from the last byte to the first and needs `bytes × n` work,
**MSD** (most significant first) recurses into buckets and can stop early.
Their rule: LSD when the key is ≤ 4 bytes, MSD otherwise, with insertion sort
for buckets of ≤ 24 tuples; pdqsort (pattern-defeating quicksort, a
branch-reduced introsort) on normalized keys when the key contains strings.
It cites Graefe's four objections to radix sort for databases (long keys with
duplicates make most passes unproductive; it wants uniform values; nearly
sorted input with a common prefix wastes the first passes; worse cache
behaviour than comparison sorts) and its Fig. 7 shows the result: radix wins
clearly on `Random`, pdqsort wins on 128-distinct-value data with one key
column, and the more key columns, the more radix recovers. The paper's
end-to-end claim (§VII-B): DuckDB sorts 100 million random integers in
1.55 s on a 16-core instance, with HyPer and Umbra at a similar level and
ClickHouse's columnar sort degrading faster as `n` grows. (This chapter did
not read Graefe's survey itself — the ACM copy is paywalled — so the four
objections are quoted as the paper relays them, §VI-C.)

**The pinned code.** `TemplatedSort` instantiates one sort per key type
(`SortSwitch`, `sorted_run.cpp:265-276`) and does this:

```cpp
// sorted_run.cpp, lines 244-260 at d54cbbe3df (the lines to read: 245, 246, 257-260)
 244  	const auto requires_next_sort =
 245  	    is_index_sort ? false : !SORT_KEY::CONSTANT_SIZE || SORT_KEY::INLINE_LENGTH != sizeof(uint64_t);
 246  	const auto ska_sort_width = MinValue<idx_t>(layout.GetSortWidth(), sizeof(uint64_t));
 247  	const auto &sort_skippable_bytes = layout.GetSortSkippableBytes();
 248  	auto ska_extract_key =
 249  	    SkaExtractKey<SORT_KEY>(requires_next_sort, ska_sort_width, sort_skippable_bytes, context.interrupt_state);
 ...
 257  	const auto fallback = [&ska_extract_key, &merge](const BLOCK_ITERATOR &fb_begin, const BLOCK_ITERATOR &fb_end) {
 258  		duckdb_ska_sort::ska_sort(fb_begin, fb_end, ska_extract_key, merge);
 259  	};
 260  	duckdb_vergesort::vergesort(begin, end, std::less<SORT_KEY>(), fallback, merge);
```

Three layers, outside in. **vergesort** "detects big runs in ascending or descending order"
and merges them at the end (`vergesort.h:210-212`); whatever it cannot place as
a run goes to the `fallback`. The fallback is **ska_sort** — Malte Skarupke's in-place MSD
radix sort ("American flag sort" for small partitions, a second in-place variant, `ska_byte_sort`, for large ones) — but ska_sort looks only at `ska_sort_width ≤ 8` bytes
(the first `uint64_t` of the key, line 246), skipping the bytes in
`sort_skippable_bytes` (the NULL bytes proven unnecessary, `ska_sort.hpp:1126`).
If the key is longer than 8 bytes (`requires_next_sort`, line 245), buckets that
tie on those 8 bytes are sorted by the *full* comparison with vergesort again
(`ska_sort.hpp:1454-1459`). Partition sizes decide the algorithm inside ska_sort:
`ska_sort<128, 1024>` (`ska_sort.hpp:1487`) means fewer than 128 elements go to
`pdqsort_branchless` with a full-key comparison (lines 1097–1107), fewer than
1,024 to the American flag sort, more to `ska_byte_sort`.

The differences from the paper, since a reader will meet both: the paper's
radix sort has no vergesort front end and no 8-byte limit — it recurses MSD
through every key byte; the pin radix-sorts one 8-byte window and compares the
rest. The paper falls back to pdqsort only when the key has strings; the pin
falls back by partition size and by whether the key fits in 8 bytes. After
the thread-local sorts, runs are merged (`sorted_run_merger.cpp`, where
`pdqsort_branchless` appears at line 657 and vergesort at 663 for the final
merged partition).

Why it matters: the *encoding* is stable across both versions, and it is what
makes any of these algorithms possible; the algorithm that sits on top of it
has been replaced since the paper. A reader should not read the paper's Fig. 7
as a description of today's DuckDB.

### Step 7 — what to take into lane 2

> **In:** Steps 2–6. **Out:** the checklist the lane-2 stubs in
> `experiments/src/normalize.rs` and `radix.rs` implement.

Lane 2 asks for a key `(a: i32 ASC, s: string ASC)`, 16 bytes: 4 bytes of
sign-flipped big-endian `a`, then a 12-byte zero-padded string prefix (the
paper's choice, §VII). It asks for an MSD radix sort over those 16 bytes and a
tie-break on the full strings for keys that are equal but whose strings are
longer than the prefix. The README records the measured counterpart: where
that radix sort beats `sort_unstable_by` over the row struct and where it does
not — in particular the `b3` case, where the prefix carries no information.

## How to read the source

1. `src/function/scalar/create_sort_key.cpp:97-102`, then `191-213` (fixed
   types) and `232-255` (strings). Read `FLIP_BYTES` as a template parameter.
2. `src/common/types/row/tuple_data_layout.cpp:150-235` — how a width class is
   chosen from types and statistics. Predict the class of three `ORDER BY`
   lists before reading the `if` ladder at 215–230.
3. `src/include/duckdb/common/sorting/sort_key.hpp:148-156`, `186-193`,
   `223-225` — the comparison and the deliberate byte swap.
4. `src/common/sort/sorted_run.cpp:204-262` — the vergesort/ska_sort pairing.
5. `third_party/ska_sort/ska_sort.hpp:1097-1132`, `1196`, `1425-1487` — the size
   thresholds and the `next_sort` fallback.
6. The paper: §VI-A (Fig. 5), §VI-C (Fig. 7, 8), §VII (Figs. 10-12).

## Questions (answer in notes.md)

1. A table has `INTEGER a` (no NULLs, statistics available) and `BIGINT b`
   (may be NULL). `ORDER BY a, b` with payload columns: how many bytes is
   `sort_width`, which `sort_skippable_bytes` are recorded, and which key
   type is chosen?
2. Why can `NULLS LAST` be implemented by choosing the validity bytes `1` and
   `2` differently rather than by a second pass over the data?
3. With strings encoded as `(byte + 1)… 0`, what is the key of the empty
   string, and where does it sort under `ASC` and under `DESC`?
4. The paper's radix sort recurses through every byte of the key; the pin
   radix-sorts 8 bytes. Construct an `ORDER BY` for which the pinned approach
   does asymptotically more comparison work than the paper's.
5. The 12-byte padded prefix of lane 2 cannot represent a string containing a
   `0x00` byte. What would you change in `encode_str_prefix`, and in the key
   width, to lift the restriction?

## Done when

Answer each before unfolding it.

- [ ] You can encode `(NETHERLANDS, 1992)` and `(GERMANY, 1924)` under `c_birth_country DESC, c_birth_year ASC` and say which sorts first.

  <details><summary>Answer</summary>

  The country is 12 bytes (letters, then padding zeros), inverted for `DESC`:
  `GERMANY` → 184 186 173 178 190 177 166 255 255 255 255 255. The year is
  big-endian with the first byte's top bit flipped: 1924 = 7·256 + 132 →
  `0 0 7 132` → `128 0 7 132`; 1992 = 7·256 + 200 → `128 0 7 200`.
  `NETHERLANDS` starts with 177 and `GERMANY` with 184, and 177 < 184, so
  `NETHERLANDS` sorts first — correct for `DESC`, since N is later in the
  alphabet than G (§VI-A, Fig. 5).

  </details>

- [ ] You can explain why a signed integer needs its sign bit flipped and `DESC` needs every byte inverted.

  <details><summary>Answer</summary>

  `memcmp` compares unsigned bytes. A negative two's-complement integer has a
  leading 1 bit, so as bytes it would sort after every positive one; flipping
  the top bit maps the signed range monotonically onto the unsigned range.
  `DESC` needs the reverse of the byte order, and `b → 255 − b` reverses the
  order of every byte value, hence of every byte string, including the
  case where one string is a prefix of another (`"ab"` becomes `157 156 255`,
  which is larger than `"abc"`'s `157 156 155 255`).

  </details>

- [ ] You can state how the pinned DuckDB encodes a VARCHAR and why it needs no padding.

  <details><summary>Answer</summary>

  Every byte is incremented by one and a `0` delimiter is appended
  (`create_sort_key.cpp:251,253`), flipped as a whole under `DESC`. The
  delimiter is smaller than any encoded byte, so a proper prefix is smaller
  than its extensions without a length field or a pad. The scheme requires
  that no input byte be `0xFF`, since `+1` would wrap onto the delimiter.

  </details>

- [ ] You can derive the sort-key type for `ORDER BY INTEGER, VARCHAR(max length 10)` with payload columns.

  <details><summary>Answer</summary>

  `sort_width = (1 + 4) + (1 + 10 + 1) = 17` (`tuple_data_layout.cpp:186,190`);
  a payload pointer adds 8 → 25 (`:199-202`); 25 > 24 and ≤ 32, so the key is
  `PAYLOAD_FIXED_32` with a 32-byte row (`:225-227`). Without a known maximum
  string length the width is invalid and the key is `PAYLOAD_VARIABLE_32`
  (`:228-230`).

  </details>

- [ ] You can describe the pinned sort's three layers and where it departs from the paper.

  <details><summary>Answer</summary>

  vergesort detects and merges existing runs; its fallback is ska_sort, an
  in-place MSD radix sort over only the first `min(sort_width, 8)` bytes,
  skipping skippable bytes; buckets that still tie, when the key is longer
  than 8 bytes, go to a comparison sort on the full key; partitions below 128
  elements go to `pdqsort_branchless` (`sorted_run.cpp:244-260`,
  `ska_sort.hpp:1097-1107,1487`). The paper's radix sort has no run
  detection and recurses through every key byte; it uses pdqsort instead of
  radix only when the key contains strings (§VI-C, §VII).

  </details>

- [ ] You can name Graefe's four objections to radix sort as the paper relays them.

  <details><summary>Answer</summary>

  (1) long keys with duplicates make many passes unproductive; (2) radix sort
  is most effective on uniformly distributed values; (3) nearly sorted input
  with a common key prefix makes the initial passes ineffective; (4) it has
  worse cache efficiency than comparison sorts (§VI-C, quoting Graefe's
  survey). This chapter did not read Graefe's paper directly.

  </details>

## References

- Laurens Kuiper, Hannes Mühleisen, **"These Rows Are Made for Sorting and
  That's Just What We'll Do"**, ICDE 2023 — §II (the query), §IV (DSM vs NSM,
  Tables II-III, Figs. 2-3), §V-B, §VI-A (key normalization, Fig. 5), §VI-B
  (mem* functions, Fig. 6), §VI-C (radix sort, Figs. 7-8), §VII (DuckDB's
  pipeline, Figs. 10-12).
- Goetz Graefe, **"Implementing Sorting in Database Systems"**, ACM Computing
  Surveys 38(3), 2006 — cited as relayed by Kuiper & Mühleisen §VI-C.
- DuckDB at `d54cbbe3df`: `src/function/scalar/create_sort_key.cpp`,
  `src/common/types/row/tuple_data_layout.cpp`,
  `src/include/duckdb/common/sorting/sort_key.hpp`,
  `src/common/sort/sorted_run.cpp`, `third_party/ska_sort/ska_sort.hpp`.
- Next: [reading-radix-sort.md](reading-radix-sort.md).
  Related: [reading-unicode-collation.md](reading-unicode-collation.md) (strings
  under a collation), topic 11's [README](../11-execution-models/README.md)
  (vectorized execution, whose row-format hash table DuckDB's sort shares).
