# cr-sqlite: a real database goes multi-master

The other guides in this topic are about *documents*. cr-sqlite is the
one that answers the database question: what does it take to bolt CRDT
semantics onto a *relational* engine as a loadable extension — no fork,
no new storage engine. This is the closest published prior art to M31's
"active-active FalkorDB." Before the code, this chapter builds the
design step by step — what a row becomes, where the clocks come from,
how a cell merges, and why sync is just a SELECT — then hands you the
five files that carry it.

## The problem in one sentence

Let two ordinary SQLite databases — say, an app on two phones — both
accept writes offline and later sync to the same state, with no server,
no consensus, and no changes to SQLite itself: everything must fit in a
loadable extension driven by triggers and virtual tables.

## The concepts, step by step

### Step 1 — the constraint: an extension, not a fork

> **In:** two ordinary SQLite databases and the rule that neither the
> engine nor the file format may change.
> **Out:** the mechanism cr-sqlite is confined to — triggers and virtual
> tables — and the one call, `crsql_as_crr()`, that grows CRDT state.

cr-sqlite is a SQLite **extension** (a shared library SQLite loads at
runtime), so it can only add tables, triggers (SQL hooks that fire on
insert/update/delete), and virtual tables (tables whose rows are
computed by code) — it cannot touch the pager, the B-tree, or the WAL.
Calling `crsql_as_crr('post')` ("conflict-free replicated relation")
takes an existing ordinary table and grows CRDT bookkeeping around it:

```
  CREATE TABLE post(id PRIMARY KEY, title, likes);
  SELECT crsql_as_crr('post');            -- "conflict-free replicated relation"

  ┌────────────┐   every column write bumps          ┌─────────────────┐
  │ post (real │──► post__crsql_clock:                │ crsql_changes   │
  │ table)     │    (pk, col_name, col_version,       │ (virtual table) │
  └────────────┘     db_version, site_id, seq)        └─────────────────┘
                     one clock ROW per CELL                  │
        replication = SELECT * FROM crsql_changes WHERE db_version > ?
                      on peer: INSERT INTO crsql_changes ...  (that's it)
  merge rule per cell: larger col_version wins;
  tie → value comparison (deterministic, not wall clock!)
```

Why this matters: the whole multi-master capability costs the
application one function call per table, and the OLTP path keeps
SQLite's untouched performance minus trigger overhead. Steps 2–6 unpack
each box of that picture.

### Step 2 — merge identity: the primary key names the row, the cell is the unit

> **In:** the CRR bookkeeping from Step 1.
> **Out:** the two-level identity that makes merge possible — primary key
> names the row, and each cell is an independent LWW register.

Replicas can only merge what they can *match up*, and cr-sqlite matches
rows by **primary key** — the PK is the row's identity across all
replicas (the relational stand-in for this topic's `Dot`; hence
auto-increment integer PKs are poison across two masters — both mint
`id=42` for different rows and the merge silently fuses them; question 5).
Below the row, the unit of conflict is the **cell** (one column of one
row): a row is treated as an LWW **map** — one last-writer-wins register
per column, exactly your `lww.rs::LwwMap`.

The payoff of cell granularity: replica A sets `post.title` while
replica B concurrently sets `post.likes` on the *same row* — both
survive, because they're different registers. Row-granularity LWW would
throw one whole row away. Same column → one write still loses; bench
lane 1 (FINDINGS row 31) priced that at **94.98%** — 37,991 of 40,000
acknowledged writes to 10 hot keys lost under per-write sync.

### Step 3 — versions without wall clocks: db_version and col_version

> **In:** the cell-as-register model from Step 2, which needs a comparable
> version per write.
> **Out:** the two Lamport clocks — `db_version` (per database, the sync
> cursor) and `col_version` (per cell, the merge version) — plus `site_id`.

Every write needs a version to compare in merges, and cr-sqlite mints
them with **Lamport clocks** (a counter that only moves forward: bump on
every local write, fast-forward to any larger value seen from a peer) —
no wall-clock time anywhere:

- **db_version** — one Lamport clock per database; every transaction
  bumps it. It orders *everything this replica has seen* and is the sync
  cursor ("give me changes where `db_version > 1234`").
- **col_version** — one counter per cell; bumps each time that cell is
  written. This is the per-register merge version.
- **site_id** — the replica's unique id, recorded per change for
  provenance.

Contrast topic 29's HLC (hybrid logical clock — Lamport clock plus a
physical-time component): with pure Lamport versions, "last writer" means
*most-written*, not *most recent* — a site offline for a week can still
win a merge if its col_version ran higher (question 3). The clock rows
live in `post__crsql_clock`, one row per cell — the metadata cost is
O(cells written), the relational cousin of OR-Set tombstones.

### Step 4 — the merge rule: version first, then compare the values

> **In:** the per-cell `col_version` from Step 3.
> **Out:** the two-step deterministic winner rule — larger `col_version`,
> then value comparison — and why it needs no clock and no site_id.

When a change arrives for a cell, the winner is decided in two steps:
larger `col_version` wins; on a tie, compare the *values themselves*
using SQLite's type ordering — not site_id, not wall clock. The pinned
tree does this in `changes_vtab_write.rs:54-94` (the `col_version >
local_version` / `col_version < local_version` branches at :59-62, then
the "versions are equal / need to compare values" fall-through at :79-94
calling `crsql_compare_sqlite_values`). The block below is the shape of
that rule, not a quote:

```rust
// ILLUSTRATION — not quoted from the crate. The real decision is
// changes_vtab_write.rs:54-94 (version compare, then value compare via
// crsql_compare_sqlite_values); the value primitive is compare_values.rs:9.
// One clock row per CELL: (pk, col) -> (col_version, db_version, site_id)
fn merge_cell(local: &mut Cell, remote: &Cell) {
    if remote.col_version > local.col_version {
        *local = remote.clone();            // larger Lamport version wins
    } else if remote.col_version == local.col_version
        && sqlite_cmp(&remote.value, &local.value) == Ordering::Greater
    {
        *local = remote.clone();            // tie → compare the VALUES, not
    }                                       // clocks or site ids: deterministic
}                                           // convergence with zero clock trust
```

Why value comparison for the tiebreak? It's deterministic (every replica
computes the same winner from the same two cells — convergence needs
nothing else), and it's *symmetric in the machines*: no site wins ties
just for having a bigger id. It's still arbitrary in semantics — "the
alphabetically larger title wins" — but arbitrary-and-deterministic is
the whole LWW deal, and your `lww.rs` makes the other classic choice,
`(ts, replica)` (question 2). One precise caveat: `site_id` *is*
consulted, but only as a last resort when the two values are byte-for-byte
equal **and** the `mergeEqualValues` option is on (`changes_vtab_write.rs`
:96 onward) — and when the values are already equal, which "wins" changes
nothing the user can see. So the load-bearing tiebreak is the value
comparison; `site_id` never decides between *different* values.

### Step 5 — deletes: a tombstone row, and remove wins

> **In:** the per-cell LWW merge from Step 4.
> **Out:** how a whole-row delete is recorded (a sentinel clock row) and
> why cr-sqlite makes delete win over concurrent updates — remove-wins.

Deleting a row writes a **sentinel clock row** (a tombstone — the
delete recorded as data, since merge state can only grow), and the
delete **wins over concurrent column updates** to that row. Pause on
that: it's *remove-wins* — the opposite of your OR-Set's add-wins and of
the JSON chapter's revive-on-edit. The relational rationale: a row is
often referenced by foreign keys and uniqueness constraints, and a
half-resurrected row (some cells revived, others gone, references
dangling) is worse for a relational schema than a lost update. Graph
nodes pushed the other way in `graph.rs` (question 4). One consequence
either way: deleted rows still cost a clock row forever — the causal
stability problem again.

### Step 6 — sync is a virtual table: replication as SQL

> **In:** the merge and delete rules from Steps 4–5, each keyed by
> `db_version`.
> **Out:** the whole replication protocol — read `crsql_changes` where
> `db_version > cursor`, insert it on the peer — and why it is crash-safe.

The genius move: the replication endpoint is the **`crsql_changes`
virtual table**. Reading it yields every change (pk, column, value,
col_version, db_version, site_id) — so pulling a peer's delta is
`SELECT * FROM crsql_changes WHERE db_version > ?` with your last-seen
cursor, and applying it is `INSERT INTO crsql_changes ...`, which routes
each row through Step 4's merge. That's the whole protocol. No custom
wire format, no sync daemon: any transport that can move query results —
HTTP, a file, a message queue — is a replication link, and the merge is
idempotent (re-applying a batch is harmless), so retries after a
mid-batch crash are safe. This is the pattern M31 must copy for graphs
(question 6).

## Where each step lives in the code

All under `core/rs/core/src/` in the cr-sqlite repo:

| anchor | step | what to see |
|---|---|---|
| `create_crr.rs` | 1 | what `crsql_as_crr()` actually creates (clock table, triggers) |
| `local_writes/after_update.rs:65-123` | 2, 3 | the `after_update` trigger body: bump db_version (`next_db_version`, :74), then for each column whose new≠old value (`crsql_compare_sqlite_values`, :107) call `mark_locally_updated` — the Lamport-clock spine of the whole design |
| `local_writes/mod.rs:110-137` | 2, 3 | `mark_locally_updated` — binds `(key, col_name, db_version, seq)` to write **one clock row per changed column** (the shared helper `after_update.rs` calls) |
| `db_version.rs:55` | 3 | `next_db_version` = per-database Lamport clock; it peeks/bumps. Compare topic 29's HLC: no wall-clock component at all here |
| `changes_vtab_write.rs:54-94` | 4 | the merge rule: larger `col_version` wins (:59-62), tie → compare the VALUES by SQLite type ordering (:79-94). Deterministic convergence with zero clock trust |
| `compare_values.rs:9` | 4 | `crsql_compare_sqlite_values` — the value-comparison primitive the merge rule calls |
| `changes_vtab.rs` | 6 | the genius move: replication endpoint as a *virtual table* — sync = SQL (the merge on write is `crsql_merge_insert`, `changes_vtab_write.rs:390`) |

Start at `local_writes/after_update.rs` and `changes_vtab.rs`; the merge
rules are deceptively short.

## Reading + background

- cr-sqlite README + docs (vlcn.io) — the deceptively short merge rules.
- James Long's "CRDTs for Mortals" talk (actual-budget lineage) — same
  per-cell LWW idea with hybrid logical clocks instead of db_version.

## Questions

1. Why one clock row per *cell* instead of per row? What anomaly appears
   with row-granularity LWW that lane 1's per-key numbers understate?
2. `compare_values.rs` breaks version ties by comparing values, not
   site_id. Your `lww.rs` uses (ts, replica). Both converge — which
   gives saner semantics when two sites write the same value, and which
   when they write different values?
3. db_version is a pure Lamport clock (no physical component). What
   user-visible LWW behavior does this change vs an HLC (topic 29) when
   one site is offline for a week, then syncs?
4. cr-sqlite chose delete-wins for rows; your orset.rs/graph.rs chose
   add-wins. Reconstruct why *relational* rows push toward remove-wins
   (hint: foreign keys, uniqueness) while graph nodes push add-wins.
5. Primary keys are the merge identity. What goes wrong if an app uses
   auto-increment integer PKs across two masters, and what does cr-sqlite
   tell you to use instead? (Same question M31 must answer for node ids —
   compare your `Dot`-based identity.)
6. **M31 mapping**: design FalkorDB's `crsql_changes` equivalent: what's
   the minimal change-row schema for (node adds/removes, edge
   adds/removes, property LWW sets), what plays the role of db_version,
   and how does a peer apply a batch idempotently mid-crash? Sketch it
   against your `graph.rs` merge.

## Done when

Answer each before unfolding it.

- [ ] You can walk one UPDATE from trigger to clock row to `crsql_changes` to a peer's merge.

  <details><summary>Answer</summary>

  An `UPDATE post SET title=? WHERE id=?` fires the after-update trigger,
  whose body (`after_update.rs:65-123`) bumps the database's Lamport clock
  once (`next_db_version`, :74), then for each column whose value actually
  changed (`crsql_compare_sqlite_values != 0`, :107) calls
  `mark_locally_updated` (`mod.rs:110-137`) to write one row into
  `post__crsql_clock` — `(pk, col_name, col_version, db_version, seq,
  site_id)`. A peer pulls `SELECT * FROM crsql_changes WHERE db_version >
  cursor`, and applying each row (`INSERT INTO crsql_changes`) routes it
  through the merge decision (`changes_vtab_write.rs:54-94`) against its
  local cell (Steps 3, 4, 6).

  </details>

- [ ] You can defend per-cell granularity against row-granularity LWW.

  <details><summary>Answer</summary>

  A row is treated as an LWW *map*, one register per column, so replica A
  writing `post.title` and replica B concurrently writing `post.likes` on
  the same row both survive — they are different registers with their own
  `col_version`. Row-granularity LWW would keep only one replica's whole
  row and silently drop the other's column write. Lane 1's 94.98% loss is
  the *same-cell* collision rate; row-granularity would add cross-column
  losses on top of it, so per-cell is strictly less lossy (Step 2).

  </details>

- [ ] You can defend remove-wins deletes against the document-CRDT default of add-wins/revive.

  <details><summary>Answer</summary>

  A deleted row writes a sentinel clock row, and that delete wins over
  concurrent column updates to the row. The document CRDTs in this topic do
  the opposite (OR-Set add-wins, the JSON CRDT revives an edited-but-deleted
  subtree). The relational reason is integrity: a half-resurrected row —
  some cells revived, others gone, foreign keys and uniqueness constraints
  pointing at a partial record — is worse for a schema than a lost update.
  `graph.rs` chose the other way (hide-not-delete) because a graph node has
  no such constraint surface (Step 5, question 4).

  </details>

- [ ] You can say what pure-Lamport versioning changes about "last writer" versus an HLC.

  <details><summary>Answer</summary>

  `db_version`/`col_version` are pure Lamport counters with no physical-time
  component, so "last writer" means *highest col_version*, i.e.
  *most-written*, not *most recent by the clock*. A replica offline for a
  week that had written a cell many times can still carry a higher
  `col_version` and win the merge over a peer's newer-in-wall-time write.
  An HLC (topic 29) folds physical time in, so its "last writer" tracks
  real recency. cr-sqlite trades that for needing no clock trust at all
  (Step 3, question 3).

  </details>

## References

**Papers**
- None — the design lives in the cr-sqlite README and the vlcn.io docs;
  James Long's "CRDTs for Mortals" talk is the closest lineage write-up
  (same per-cell LWW with hybrid logical clocks instead of db_version)

**Code**
- [cr-sqlite](https://github.com/vlcn-io/cr-sqlite) `core/rs/core/src/`
  — start at `local_writes/after_update.rs` and `changes_vtab.rs`; the
  merge rules (`changes_vtab_write.rs:54-94`) are deceptively short
