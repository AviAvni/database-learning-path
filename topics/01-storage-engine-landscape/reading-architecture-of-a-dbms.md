# Reading guide — "Architecture of a Database System" (Hellerstein, Stonebraker, Hamilton, 2007)

~120 pages, but you are NOT reading it all now. This is the curriculum's *atlas*:
read the map chapters this week, then return per-topic. Budget now: 2 h.

## Read NOW (topic 1)

- **§1 (main components)** — the five-box diagram of a DBMS. Memorize it; it's the
  table of contents for topics 3–16:

```mermaid
flowchart TB
    CM["Client communications manager<br/>(topic 7: protocol, RESP)"] --> PC["Process manager<br/>(topic 7/9: threads, admission)"]
    PC --> RP["Relational query processor<br/>parse → rewrite → optimize → execute<br/>(topics 10-11)"]
    RP --> TS["Transactional storage manager<br/>access methods + buffer + locks + log<br/>(topics 1-6, 8-9)"]
    TS --> SC["Shared components<br/>catalog, memory allocator, replication<br/>(topics 15, 22)"]
```

- **§2 (process models)** — process-per-worker vs thread-per-worker vs event/async;
  where admission control lives. Directly informs the capstone server (M7/M9).
- **§6 (storage management)** — spatial control (why DBs fight the filesystem),
  buffer pools vs OS page cache, the double-buffering problem. This is the section
  that justifies this topic's existence.

## Skim NOW, return LATER

| Section | Return at |
|---------|-----------|
| §3 parser/rewriter | topic 10 |
| §4 query processor internals | topics 10–11 |
| §5 transactions, ACID, locking | topics 8–9 |
| §7 shared components (catalog, replication) | topics 15–16 |

## Questions to answer in notes.md

1. §6 argues the DBMS should bypass OS caching (O_DIRECT). What are the *two*
   distinct problems with letting the OS cache pages? (Double buffering; the OS
   evicts/flushes with zero knowledge of WAL ordering.)
2. Which of the five §1 boxes does fjall implement? redb? (Neither has a query
   processor or client manager — "storage engine" ≠ "database". The capstone builds
   the other boxes on top, milestone by milestone.)
3. 2007 blind spots: name three things the paper couldn't see coming. (Candidates:
   NVMe erasing the seek-time mental model, cloud disaggregation — topic 28, columnar
   dominance for analytics — topic 12, LSM taking over write paths.)

## The one-line takeaway

A database is five cooperating managers, and a storage engine is just one of them —
this paper is the org chart for everything the capstone will build.
