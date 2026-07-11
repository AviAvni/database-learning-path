# cr-sqlite: a real database goes multi-master

The other guides in this topic are about *documents*. cr-sqlite is the
one that answers the database question: what does it take to bolt CRDT
semantics onto a *relational* engine as a loadable extension — no fork,
no new storage engine. This is the closest published prior art to M31's
"active-active FalkorDB."

## The one picture

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

- Rows are LWW **maps** (one register per column — your `lww.rs::LwwMap`
  is exactly this): concurrent writes to *different* columns of a row
  both survive; same column → one loses. Bench lane 1 priced that.
- Deletes are tombstoned via a sentinel clock row; delete wins over
  concurrent column updates (a *remove-wins* choice — opposite of your
  OR-Set! worth pausing on).

The per-cell merge rule, entire:

```rust
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

## Code walk (core/rs/core/src/)

| anchor | what to see |
|---|---|
| `local_writes/mod.rs:83-133` | `after_update` bookkeeping: bump db_version, write one clock row per changed column — the Lamport-clock spine of the whole design |
| `db_version.rs` | db_version = per-database Lamport clock; `next_db_version` peeks/bumps. Compare topic 29's HLC: no wall-clock component at all here |
| `compare_values.rs` | the tiebreak when col_versions are equal: compare the VALUES by SQLite type ordering. Deterministic convergence with zero clock trust |
| `changes_vtab.rs` | the genius move: replication endpoint as a *virtual table* — sync = SQL |
| `create_crr.rs` | what `crsql_as_crr()` actually creates (clock table, triggers) |

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

## References

**Papers**
- None — the design lives in the cr-sqlite README and the vlcn.io docs;
  James Long's "CRDTs for Mortals" talk is the closest lineage write-up
  (same per-cell LWW with hybrid logical clocks instead of db_version)

**Code**
- [cr-sqlite](https://github.com/vlcn-io/cr-sqlite) `core/rs/core/src/`
  — start at `local_writes/mod.rs` and `changes_vtab.rs`; the merge
  rules are deceptively short
