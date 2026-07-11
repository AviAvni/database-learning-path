# mmap is not a buffer pool

mmap looks like a free buffer pool, and a famous position paper says that for
a general-purpose write-heavy DBMS every apparent win reverses. It is short,
punchy, and deliberately provocative — so read it adversarially, then
construct the counter-evidence yourself (LMDB exists and is excellent). The
payoff is knowing precisely *which* property of a workload makes mmap wrong.

## The temptation (§1–2)

mmap looks like a free buffer pool: no copies, no eviction code, pointer
access, the kernel's page cache does the work. Systems that tried: MongoDB
(MMAPv1 — abandoned), LMDB (kept it, happily), SQLite (optional), RavenDB…
The paper's claim: for a *general-purpose write-heavy DBMS*, every apparent
win reverses.

## The four problems (§3) — memorize these

```
 1. Transactional safety   kernel may flush a dirty page ANY time
    ────────────────────   ⇒ can't order page-write after log-write
                           ⇒ WAL rule unenforceable without COW tricks
 2. I/O stalls             page fault = your thread stops; no async,
    ────────────────────   no prefetch you control, no admission control
 3. Error handling         disk error = SIGBUS in the middle of a memcpy,
    ────────────────────   not an error code at a syscall boundary
 4. Performance (§4)       the surprise: even READ-ONLY mmap loses at scale
```

## §4 — why read-only mmap still loses (the part worth re-reading)

Three kernel bottlenecks, measured:
- **page table contention** — single-threaded page-fault handling paths.
- **TLB shootdowns** — evicting a mapping ⇒ IPI every core that may have the
  TLB entry: eviction cost scales with core count.
- **4KB granularity + page-table walk overhead** vs one big explicit read.

Result (their fio experiment): explicit `pread`/O_DIRECT sustains device
bandwidth; mmap plateaus far below on NVMe arrays and *degrades over time*
once eviction starts.

## The rebuttal you must construct (LMDB, topic 3)

LMDB is mmap-based and wins its niche. Why it dodges each bullet: COW pages
never overwrite (1: ordering is a non-problem — the meta-page flip IS the
commit); read-mostly workloads fault once, then it's just memory (2); a
read-only mmap can't SIGBUS on writes (3); and its scale target is
"fits mostly in RAM" (4). The paper's own Table 1 concedes designs like this.
The honest conclusion: **mmap is wrong when the DB must control WRITE-BACK.**
Read-only/COW designs escape most of it.

## Map to what you know

| System | Uses | Escapes the trap because |
|---|---|---|
| LMDB | mmap everything | COW + read-mostly + single writer |
| SQLite | optional mmap for reads | WAL still explicit; mmap read-only |
| postgres | no mmap; shared_buffers | needs write ordering (FPIs, ckpts) |
| LeanStore/vmcache | anonymous mem / virt mapping | explicit residency control |

## Questions to answer in notes.md

1. Your topic-3 B+tree used explicit I/O. If you'd mmap'd it, which of your
   topic-5 WAL guarantees break, concretely? (Which test in
   `crash_test.rs` would start failing and why.)
2. TLB shootdowns: why does *eviction* trigger them but *faulting-in* not?
3. The paper measures read-only workloads losing. Reconcile with LMDB's
   read benchmarks winning — what's different in the setups (working set vs
   RAM, single NVMe vs array, pointer-chase vs scan)?
4. vmcache's answer: keep virtual-memory addressing, add explicit state.
   Which of the four problems does it solve, which does it merely soften?

## Done when

You can argue both sides for five minutes each — "never mmap" and "LMDB is
right" — and state precisely which property of your workload picks the side.

## References

**Papers**
- Crotty, Leis, Pavlo — "Are You Sure You Want to Use MMAP in Your DBMS?"
  (CIDR 2022) — short position paper; memorize the four problems of §3,
  re-read §4 (why even read-only mmap loses at scale)
