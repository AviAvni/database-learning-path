# RocksDB: buy the map before walking the territory

RocksDB is everything fjall and tidesdb do, ~50x larger — too big to read,
too important to skip. This chapter is not a walkthrough but an orientation
map: 30 minutes of `ls` and header-skimming now, so that when topic 4
(compaction), topic 6 (block cache), and topic 22 (db_bench) ask "where does
X live?", you already know which directory holds the answer.

## Directory map

```mermaid
flowchart TB
    API["include/rocksdb/db.h<br/>public API"] --> DBI["db/db_impl/db_impl.h<br/>DBImpl — ~3.8K-line god class"]
    DBI --> MEM["memtable/<br/>skiplist & friends"]
    DBI --> TAB["table/<br/>SST formats<br/>block_based/*"]
    DBI --> VS["db/version_set.h<br/>manifest: which SSTs exist"]
    DBI --> CMP["db/compaction/<br/>compaction_job.h"]
    TAB --> CACHE["cache/<br/>lru_cache.h — block cache"]
    DBI --> FILE["file/ + env/<br/>IO + OS abstraction"]
    DBI --> MON["monitoring/<br/>statistics, histograms"]
```

| Dir | What lives there | Anchor |
|-----|------------------|--------|
| `db/` | engine core: DBImpl, column families, versions, compaction | `db/db_impl/db_impl.h`, `db/column_family.h` |
| `table/` | SST file formats | `table/block_based/`, `table/format.h` |
| `memtable/` | memtable representations | `memtable/skiplist.h` |
| `cache/` | block/row cache | `cache/lru_cache.h` |
| `file/` | IO helpers, prefetch, filenames | `file/filename.h` |
| `util/` | blooms, hashing, compression | `util/bloom_impl.h` |
| `options/` | the infamous config surface | `options/db_options.h` |
| `env/` | OS abstraction | `env/env_posix.cc` |
| `monitoring/` | stats/histograms/perf context | `monitoring/statistics.h` |
| `utilities/` | transactions, backup, checkpoints | `utilities/transactions/` |

## The two entry points

- `DBImpl::Write()` — `db/db_impl/db_impl.h:256` (write path entry)
- `DBImpl::Get()` — `db/db_impl/db_impl.h:271` (read path entry)

Everything you traced in fjall/tidesdb exists here too, ~50x larger: journal ↔
`db/log_writer.cc`, keyspace ↔ column family, manifest ↔ `version_set`.

## Why orient now

When topic 4 asks "how does leveled compaction pick files?", you should already know
the answer lives in `db/compaction/` and version metadata in `db/version_set.h` —
navigation cost paid once, here.

## References

**Code**
- [rocksdb](https://github.com/facebook/rocksdb) (shallow clone @
  `7c80a5a` at `~/repos/rocksdb`) — don't read it yet; orient with the
  directory map above. Anchors: `db/db_impl/db_impl.h`,
  `db/version_set.h`, `db/compaction/`, `table/block_based/`,
  `memtable/skiplist.h`, `cache/lru_cache.h`
