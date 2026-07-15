# hashbrown: the probe loop the flamegraph couldn't show

This IS `std::collections::HashMap` — you profiled it in topic 0 (21% SipHash,
rest inlined probe loop), and now you read the probe loop the flamegraph
flattened into "everything else". One idea carries the whole design: keep a
dense array of 1-byte tags beside the slots, so one SIMD load filters 8–16
candidates before a single key byte is touched. This chapter builds that idea
step by step — open addressing, the control byte, group probing, the probe
sequence, tombstones — then maps each step onto the source.

## The problem in one sentence

A chained hash table pays 2+ dependent cache misses per lookup (bucket array,
then each malloc'd node) — ~200 ns at 10M keys — when the theoretical minimum
is one miss: the line the entry actually lives on.

## The concepts, step by step

### Step 1 — open addressing: store entries in the array itself

Instead of buckets pointing at malloc'd chain nodes (chaining — the redis
dict chapter), **open addressing** stores the key-value pairs directly in one
flat array of **slots**. On collision (the slot your hash points at is
taken), you don't follow a pointer — you **probe**: try other slots in a
deterministic sequence until you find the key or an empty slot. Wins: no
per-entry malloc, no pointer chase, and probing walks memory the prefetcher
can follow. Costs: deletion gets tricky (Step 5), and performance collapses
as the table fills — near 100% full, probe sequences get long, which is why
every open-addressing table enforces a maximum **load factor** (fraction of
slots occupied; hashbrown: 7/8).

### Step 2 — the control byte: a dense 1-byte summary of every slot

The naive probe compares full keys slot by slot — touching a cache line of
slot data per step. hashbrown's move: keep a *separate, dense* array with
**one byte per slot** (the **control byte** or tag, `src/control/tag.rs:9–49`)
that answers "is this slot worth touching?" without touching it:

```
tag values:  EMPTY = 0xff   DELETED = 0x80   FULL = 0b0xxxxxxx (h2: top 7 hash bits)

hash (64 bits): ┌──────── h1: index bits ────────┬─ h2: top 7 ─┐
                └── which group to probe first ──┴─ tag value ─┘

control array:  [23|EMPTY|91|07|DELETED|55|23|EMPTY| ... ]
                 └────────── one 8/16-byte SIMD load ─────────┘
slot array:     [ kv | ___ | kv | kv | ___ | kv | kv | ___ ]  touched only on tag hit
```

The hash is split once: the low bits (**h1**) choose where to start probing;
the top 7 bits (**h2**) become the tag of a FULL slot. A probe compares h2
against tags first, and only a tag match earns a real key comparison. This is
the "dense filter + fat payload" pattern (README §4): the filter array is 1
byte per slot, so 64 slots of metadata fit in one cache line.

### Step 3 — group probing: 16 tags in one SIMD instruction

Because tags are dense bytes, SIMD (single instruction, multiple data — CPU
instructions that operate on 16 bytes at once) can compare h2 against a whole
**group** of 16 tags (8 on ARM NEON) in one instruction, yielding a bitmask
of candidates. The lookup, de-macro'd:

```rust
fn find(table: &RawTable, hash: u64, key: &K) -> Option<usize> {
    let h2 = (hash >> 57) as u8;                        // top 7 bits = the tag
    let mut probe = ProbeSeq::new(h1(hash), table.mask); // triangular stride
    loop {
        let group = Group::load(&table.ctrl[probe.pos]); // ONE dense cache line
        for bit in group.match_tag(h2) {                 // SIMD: 8–16 tags at once
            let slot = (probe.pos + bit) & table.mask;
            if table.key(slot) == key { return Some(slot); } // 2nd line: the slot
        }
        if group.match_empty().any_bit_set() {
            return None;    // EMPTY stops the probe; DELETED does NOT —
        }                   //   the key may have been pushed past a tombstone
        probe.move_next(table.mask);
    }
}
```

False-positive rate: 16 slots × 2⁻⁷ ≈ 16/128 per group — a wasted key
comparison ~12% of the time, cheap. Net cache-line budget per lookup: one
line of control bytes + one line of slot data — the theoretical minimum plus
one dense byte.

### Step 4 — the probe sequence: triangular stride, guaranteed coverage

When a group has neither a match nor an EMPTY, probing moves to another
group. Linear probing (always +1) suffers **clustering** — runs of full slots
grow and merge, lengthening everyone's probes. hashbrown's `ProbeSeq`
(`src/raw.rs:76–93`) grows its stride by one group per step (positions follow
triangular numbers: +1, +2, +3, … groups). The comment links the proof that
triangular probing mod a power of two visits every group exactly once — no
cycling, no missed slots — while spreading clusters out.

### Step 5 — deletion and tombstones: why DELETED ≠ EMPTY

Open-addressing deletion cannot just mark a slot EMPTY: an EMPTY stops every
probe (Step 3's early exit), so erasing a slot mid-probe-chain would make
keys *beyond* it unfindable. The fix is a **tombstone**: the DELETED tag,
which probes skip over but inserts may reuse. The subtleties
(`src/raw.rs:1952–1984, 1033–1043`): inserting over DELETED doesn't consume
`growth_left` (the tombstone already "spent" its capacity), and a table full
of tombstones triggers **rehash-in-place** — rewriting the control array to
reclaim tombstones without growing. Churn-heavy tables otherwise degrade:
same disease as LSM tombstones, same cure (rewrite/compact).

### Step 6 — two closing tricks: load factor 7/8 and the mirrored tail

Load factor: hashbrown allows 7/8 = 87.5% occupancy (`src/raw.rs:152–156`) —
versus 50% for classic open addressing — because group probing checks 16
slots per step, so even near-full tables resolve in ~1 group. That's 1.14
bytes of overhead per slot where chaining pays a 16+ byte malloc'd node.

The trailing mirror (`src/raw.rs:223`): the control array allocates
`buckets + Group::WIDTH` bytes, the tail replicating the head, so a 16-byte
group load starting near the end never wraps around. Branchless boundary
handling, paid in 16 bytes.

### Step 7 — naming what stalled in your topic 0 flamegraph

Your flamegraph showed the probe loop fully inlined and memory-stall-bound at
10M keys. Now you can name the stalls: the **control-byte load** is the one
guaranteed miss per probe (dense array, ~1 cache line per group); the slot
touch is the second. h2 filtering exists precisely so there's rarely a
*third*. And the 21% SipHash slice is the price of computing h1/h2 at all —
the hash-policy question your capstone must answer.

## Where each step lives in the code

| What | Where | Step |
|------|-------|------|
| `RawTable` | `src/raw.rs:557` | 1 |
| Tag constants + h2 extraction | `src/control/tag.rs:9–49` | 2 |
| Group dispatch (SSE2/NEON/generic) | `src/control/group/mod.rs:8–46` | 3 |
| **NEON match (your machine)** | `src/control/group/neon.rs:78–90` | 3 |
| Probe sequence (triangular) | `src/raw.rs:76–93` | 4 |
| Insert / tombstone reuse | `src/raw.rs:1952–1984, 1033–1043` | 5 |
| Load factor 7/8 | `src/raw.rs:152–156` | 6 |
| Trailing mirror | `src/raw.rs:223` | 6 |

Read in this order:

1. **`tag.rs`** — EMPTY/DELETED encoding (Step 2). Why is EMPTY `0xff` and
   full tags `0b0xxxxxxx`? (So `match_empty_or_deleted` = "high bit set" —
   one SIMD sign test.)
2. **`group/neon.rs:78–90`** — the 8-byte NEON group ops (Apple Silicon path,
   Step 3). Note x86 SSE2 gets 16-wide groups; ARM gets 8. Measurable?
   (Experiment idea.)
3. **`raw.rs:76–93`** — `ProbeSeq` (Step 4): stride grows by one group per
   step (triangular numbers); the comment links the coverage proof.
4. **Insert path `raw.rs:1952`** — find first EMPTY *or* DELETED; tombstone
   subtlety at `raw.rs:1033–1043` (Step 5).
5. **Aha: the trailing mirror** — `raw.rs:223` (Step 6).

## Questions to answer in notes.md

1. Why 7/8 load factor rather than redis's 1.0? (Open addressing degrades near full —
   probe lengths explode; chaining just grows chains linearly.)
2. Rust 2018 chose SipHash default for HashMap (DoS resistance) — after this reading
   plus the 21% flamegraph number, write the one-paragraph policy for the capstone:
   where FxHash/ahash, where SipHash stays.
3. What does DELETED do to a long-lived table with churn? Relate to LSM tombstones —
   same problem, same fix (rewrite/compact).

## Done when

You can draw the control-byte array and narrate one lookup from hash to slot,
including both cache lines it touches.

## References

**Code**
- [hashbrown](https://github.com/rust-lang/hashbrown) (shallow clone at
  `~/repos/hashbrown`) — `src/raw.rs` (RawTable, ProbeSeq, insert path),
  `src/control/tag.rs`, `src/control/group/neon.rs` (the Apple Silicon
  path; SSE2 sibling for x86)
