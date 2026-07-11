# Capstone — falkordb-rs-next-gen, from scratch

The capstone is a **clean-room rebuild of [falkordb-rs-next-gen](https://github.com/FalkorDB/falkordb-rs-next-gen)**:
a Cypher property-graph database in Rust, built one milestone per curriculum topic.
Working name: `falkordb-scratch` (rename at will).

Why rebuild something you already work on? Because on the real project you inherit
decisions; here you *make* every one — and benchmark it against the real thing. The
reference implementation lives at [`~/repos/falkordb-rs-next-gen`](https://github.com/FalkorDB/falkordb-rs-next-gen); every milestone ends
by comparing your design and numbers against the corresponding module there.

## Target architecture (mirrors the reference)

```mermaid
flowchart TD
    CLIENT["FalkorDB clients<br/>(falkordb-py, redis-cli, ...)"]
    CLIENT --> RESP["RESP server — GRAPH.QUERY / GRAPH.RO_QUERY<br/>wire-compatible · M7"]
    RESP --> PARSE["Cypher parser → binder<br/>M10"]
    PARSE --> PLAN["planner / optimizer<br/>M10 · egg rewrites M21"]
    PLAN --> RT["vectorized runtime — batches, operators, expression eval<br/>M11 · SIMD M17 · JIT M19 · GPU M18"]
    RT --> CORE["graph core: sparse/delta matrices — own GraphBLAS-subset kernels<br/>M13 naive → M20 sparse · + attribute store, string pool, datablocks M2"]
    CORE --> TXN["MVCC copy-on-write graph M8 · constraints ·<br/>indexes: range M3/M26, vector M14, full-text M23"]
    TXN --> PERSIST["persistence: WAL + recovery M5 · B+tree M3 / LSM M4 backends ·<br/>buffer pool M6 · tiered object storage M28"]
    PERSIST --> DIST["replication: WAL-shipping → Raft M15 ·<br/>cross-shard txns M29 · active-active CRDT M31"]
    QA["correctness & perf spine:<br/>DST + fuzzing + openCypher TCK M16 ·<br/>TLA+/Lean M21 · LDBC benches M22"]
    QA -.->|guards| RT
    QA -.-> CORE
    QA -.-> PERSIST
```

## Ground rules

- Cargo workspace; crates added as milestones demand, not upfront.
- **No peeking first**: design and build from the topic's concepts, *then* read the
  reference module and compare — the diff is where the learning is.
- Every milestone lands with: tests + a criterion benchmark + a `notes.md` entry
  comparing your approach vs the reference (design and numbers).
- Correctness bar grows over time: openCypher TCK subset (M16 onward) is the oracle.
- Unsafe allowed where the lesson requires it — with Miri runs.

## Milestone map

Milestones M0–M31 map 1:1 to curriculum topics 0–31 in `PLAN.md`; each topic's
"Capstone milestone" line defines the scope. Status lives in `PROGRESS.md`.

Rough dependency spine: M0 → M2 → M13 (naive adjacency graph) → M10/M11 (query engine)
→ M20 (sparse-matrix core replaces M13). Everything else attaches to that spine —
persistence (M3–M6), server (M7), MVCC/concurrency (M8/M9), indexes (M12/M14/M23),
distribution (M15), correctness (M16/M21), performance (M17/M18/M19/M22).

```mermaid
flowchart LR
    M0["M0<br/>workspace +<br/>bench harness"] --> M2["M2<br/>attribute store +<br/>datablocks"]
    M2 --> M13["M13<br/>naive adjacency<br/>graph core"]
    M13 --> M10["M10<br/>parser +<br/>planner"]
    M10 --> M11["M11<br/>vectorized<br/>runtime"]
    M11 --> M20["M20<br/>sparse-matrix core<br/>(the heart)"]
    P["persistence<br/>M3–M6"] -.-> M13
    S["server<br/>M7"] -.-> M10
    C["MVCC + concurrency<br/>M8 / M9"] -.-> M13
    I["indexes<br/>M12 / M14 / M23 / M26"] -.-> M20
    D["distribution<br/>M15 / M28 / M29 / M31"] -.-> M20
    Q["correctness<br/>M16 / M21"] -.-> M11
    PF["performance<br/>M17 / M18 / M19 / M22"] -.-> M20
```

Workspace is created at M0 (topic 0). Nothing lives here until then.
