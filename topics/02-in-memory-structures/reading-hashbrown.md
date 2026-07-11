# hashbrown: the probe loop the flamegraph couldn't show

This IS `std::collections::HashMap` — you profiled it in topic 0 (21% SipHash,
rest inlined probe loop), and now you read the probe loop the flamegraph
flattened into "everything else". One idea carries the whole design: keep a
dense array of 1-byte tags beside the slots, so one SIMD load filters 8–16
candidates before a single key byte is touched.

## 1. The control-byte array — the whole idea

Every slot has a 1-byte **tag** in a separate dense array (`src/control/tag.rs:9–49`):

```
tag values:  EMPTY = 0xff   DELETED = 0x80   FULL = 0b0xxxxxxx (h2: top 7 hash bits)

hash (64 bits): ┌──────── h1: index bits ────────┬─ h2: top 7 ─┐
                └── which group to probe first ──┴─ tag value ─┘

control array:  [23|EMPTY|91|07|DELETED|55|23|EMPTY| ... ]
                 └────────── one 8/16-byte SIMD load ─────────┘
slot array:     [ kv | ___ | kv | kv | ___ | kv | kv | ___ ]  touched only on tag hit
```

Probing = compare h2 against 16 tags in one SIMD op; only matching slots get a real
key comparison. False-positive rate per group ≈ 16/128 — cheap. This is the
"dense filter + fat payload" pattern (README §4).

The lookup, de-macro'd:

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

## 2. Where things live

| What | Where |
|------|-------|
| `RawTable` | `src/raw.rs:557` |
| Tag constants + h2 extraction | `src/control/tag.rs:9–49` |
| Group dispatch (SSE2/NEON/generic) | `src/control/group/mod.rs:8–46` |
| **NEON match (your machine)** | `src/control/group/neon.rs:78–90` |
| Probe sequence (triangular) | `src/raw.rs:76–93` |
| Insert / tombstone reuse | `src/raw.rs:1952–1984, 1033–1043` |
| Load factor 7/8 | `src/raw.rs:152–156` |

## 3. Read in this order

1. **`tag.rs`** — EMPTY/DELETED encoding. Why is EMPTY `0xff` and full tags
   `0b0xxxxxxx`? (So `match_empty_or_deleted` = "high bit set" — one SIMD sign test.)
2. **`group/neon.rs:78–90`** — the 8-byte NEON group ops (Apple Silicon path). Note
   x86 SSE2 gets 16-wide groups; ARM gets 8. Measurable? (Experiment idea.)
3. **`raw.rs:76–93`** — `ProbeSeq`: stride grows by one group per step (triangular
   numbers). The comment links the proof that mod-power-of-two triangular probing
   visits every group exactly once — no cycling, no missed slots.
4. **Insert path `raw.rs:1952`** — find first EMPTY *or* DELETED. Tombstone subtlety
   (`raw.rs:1033–1043`): inserting over DELETED doesn't consume `growth_left`, and a
   table full of tombstones triggers **rehash-in-place** instead of growth.
5. **Aha: the trailing mirror** — `raw.rs:223`: the control array allocates
   `buckets + Group::WIDTH` bytes; the tail replicates the head so a group load
   starting near the end never wraps. Branchless boundary handling paid in 16 bytes.

## 4. Connect to your topic 0 numbers

Your flamegraph showed the probe loop fully inlined and memory-stall-bound at 10M
keys. Now you can name what's stalling: the **control-byte load** is the one
guaranteed miss per probe (dense array, ~1 cache line per group); the slot touch is
the second. h2 filtering exists precisely so there's rarely a *third*.

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
