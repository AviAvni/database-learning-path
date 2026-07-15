# Bolt & PackStream: the graph in the type system

RESP encodes a node as nested arrays the client must re-interpret; Bolt puts
Node, Relationship, and Path on the wire as first-class types, and makes
result streaming client-driven — backpressure IS the protocol. The reference
implementation here is FalkorDB's own Bolt 5.x server, complete until #2170
removed it (2026-07-08): read it frozen in time with
`git show 0b11a00b3^:src/bolt/<file>` in `~/repos/FalkorDB`. This chapter
builds the protocol step by step — the serialization format, the graph
types, the message vocabulary, the pull-based streaming — before walking the
C files.

## The problem in one sentence

A graph query returns nodes, relationships, and paths — typed, structured
values — but RESP can only say "array of arrays of strings", so every
FalkorDB client library re-parses nested arrays into graph objects by
convention; and if the result is 10M rows, RESP's server must buffer all of
them, because the client has no way to say "give me 1,000 at a time."

## The concepts, step by step

### Step 1 — the two problems Bolt exists to solve

Bolt is Neo4j's binary protocol, and it differs from RESP in exactly the
two places the problem statement names. First, **typing**: the wire format
(PackStream, Steps 3–4) has markers for maps, lists, and *graph types* —
Node, Relationship, Path — so a driver hands you a graph object, not a
string table to re-interpret. Second, **streaming**: after a query runs,
records flow only when the client asks for them (`PULL {n}`, Step 5) —
backpressure designed in, not bolted on (topic 7 §4's problem, solved at
the protocol layer).

Why it matters: these are the two axes of the RESP/pgwire/Bolt table in
topic 7 §5 — Bolt is what a protocol looks like when the *data model* lives
in the protocol.

### Step 2 — the handshake: version negotiation in 20 bytes

A Bolt connection opens with the client sending 4 magic bytes
`0x60 0x60 0xB0 0x17` (so a server can tell Bolt from a stray HTTP request
on byte 1) plus four *proposed* protocol versions, 4 bytes each; the server
answers with the one version it picks, and every byte after that is spoken
in it. FalkorDB's implementation accepts 5.1..5.7 (bolt_api.c:803, version
pick :845–864, clamped to that range). Compare RESP, where versioning is an
optional in-band `HELLO 2|3` command — question 3 asks which design a proxy
can transparently downgrade.

### Step 3 — PackStream: type in the high nibble, size in the low

**PackStream** is Bolt's serialization format — think binary JSON with an
extension point. Every value starts with a **marker byte**: the high nibble
says the type, and for "tiny" variants the low nibble carries the size, so
small values cost one marker byte total. From the FalkorDB source
(bolt.c): NULL is 0xC0 (:11), tiny-string base 0x80 (:21) — so a 5-char
string is marker 0x85 + 5 bytes.

Integers are varint-by-cases: `bolt_reply_int` (bolt.c:133) picks
tiny-int/int8/16/32/64 by value, biased so the common range -16..127 costs
exactly one byte:

```rust
// High nibble = type, low nibble = size for "tiny" variants; ints are
// varint-by-cases, biased so -16..127 costs exactly one byte.
fn write_int(out: &mut Vec<u8>, v: i64) {
    match v {
        -16..=127 => out.push(v as u8),                                        // tiny
        _ if i8::try_from(v).is_ok()  => { out.push(0xC8); out.push(v as u8); }
        _ if i16::try_from(v).is_ok() => { out.push(0xC9); out.extend((v as i16).to_be_bytes()); }
        _ if i32::try_from(v).is_ok() => { out.push(0xCA); out.extend((v as i32).to_be_bytes()); }
        _ => { out.push(0xCB); out.extend(v.to_be_bytes()); }
    }
}
fn write_struct_header(out: &mut Vec<u8>, n_fields: u8, tag: u8) {
    out.push(0xB0 + n_fields);   // marker: tiny structure of n fields
    out.push(tag);               // 0x4E Node, 0x52 Relationship, 0x50 Path, 0x10 RUN…
}                                // then the fields follow, each PackStream-encoded
```

Compare topic 7 §1: RESP optimizes the *parser* (scan for \r\n);
PackStream optimizes the *type round-trip* (marker dispatch table).

### Step 4 — structures: one mechanism for messages AND graph types

PackStream's extension point is the **structure**: a marker byte
`0xB0 + n_fields` (bolt.c:36), then a **tag byte** naming what the
structure *is*, then that many fields, each PackStream-encoded and
arbitrarily nested (lists :198, maps :225, structures :250 in bolt.c).

The elegant part: one tag enum (`BST_*`, bolt.h:27) covers both the
protocol's *messages* — HELLO 0x01, RUN 0x10, PULL 0x3F, RECORD 0x71 — and
its *data types* — Node 0x4E, Relationship 0x52, Path 0x50. A Path is a
structure of lists of Node/Relationship structures; a RUN message is a
structure of (query string, params map, extra map). RESP has no
equivalent: a FalkorDB RESP reply encodes a node as nested arrays the
client library must re-interpret; Bolt puts the graph in the type system.

### Step 5 — RUN/PULL: the client drives the stream

Bolt splits "execute" from "fetch". `RUN` executes the query, and the
server replies only metadata (`SUCCESS {fields}` — column names, **no rows
sent!**). Rows flow only in response to `PULL {n: 1000}` — n RECORDs, then
`SUCCESS {has_more: true}` — and the client either PULLs again or sends
`DISCARD` (stop paying for rows it doesn't want). The whole session:

```
client                                server
  │ 0x60 0x60 0xB0 0x17 + 4 versions   │  handshake: bolt_api.c:803,
  ├────────────────────────────────────►│  version pick :845-864
  │◄──────────────── chosen version ────┤  (5.1..5.7 accepted)
  │ HELLO {auth...}          0x01       │
  │◄─────────────── SUCCESS  0x70 ──────┤
  │ RUN "MATCH..." {} {}     0x10       │  bolt_api.c:721
  │◄─────────────── SUCCESS {fields} ───┤  (query ran; no rows sent!)
  │ PULL {n: 1000}           0x3F       │  bolt_api.c:726
  │◄─────────────── RECORD × n  0x71 ───┤  client-driven streaming:
  │◄─────────────── SUCCESS {has_more}──┤  backpressure IS the protocol
  │ DISCARD                  0x2F       │  (or: stop paying for rows)
```

This is a pull-based cursor *in the protocol* — pgwire's portal
(Execute {max_rows} / PortalSuspended) rediscovered, with the client
explicitly naming its batch size. The cost: between RUN and the final PULL,
the server holds a suspended result — state per open cursor (question 1:
what does 10K idle cursors cost?).

One framing detail: PackStream values have no overall message-length
prefix, so messages are wrapped in **chunks** — 2-byte length headers, a
0x0000 chunk as terminator. Chunking lets the server start transmitting a
RECORD before knowing the full message size — it streams records as it
produces them (question 2).

### Step 6 — the server side: one switch, two decouplings

The FalkorDB implementation shows how little a Bolt server core is:

- `BoltRequestHandler` (bolt_api.c:670): one dispatch switch over the
  `BST_*` message tags — the protocol state machine is ~10 cases.
- RUN executes the query but replies only metadata (:467–482); records
  flow in the PULL handler (:504–521) — result *materialization* and
  result *transport* are decoupled server-side too, mirroring Step 5's
  wire-level split.
- `ws_handshake` (bolt_api.c:831): the same port sniffs and upgrades
  WebSocket — that's how browser clients speak Bolt.
- It's all inside a Redis module: the Bolt socket bypasses RESP entirely
  and injects work into the same executor — two protocols, one engine.

Why it matters for M7: the stretch goal is exactly this shape — a Bolt
listener beside your RESP one, sharing the executor and result set
(question 6).

## Where each step lives in the code

All in the removed `src/bolt/` tree — read frozen in time with
`git show 0b11a00b3^:src/bolt/<file>` in `~/repos/FalkorDB`:

| Anchor | What | Step |
|--------|------|------|
| bolt_api.c:803, :845–864 | handshake magic + version pick (5.1..5.7) | 2 |
| bolt.c:11, :21, :36 | markers: NULL 0xC0, tiny-string 0x80, structure 0xB0 | 3 |
| bolt.c:133 — `bolt_reply_int` | varint-by-cases integers | 3 |
| bolt.c:198 / :225 / :250 | lists, maps, `bolt_reply_structure` | 4 |
| bolt.h:27 — `BST_*` enum | messages + Node 0x4E / Relationship 0x52 / Path 0x50 | 4 |
| bolt_api.c:721 / :726 | RUN and PULL entry points | 5 |
| bolt_api.c:670 — `BoltRequestHandler` | the ~10-case dispatch switch | 6 |
| bolt_api.c:467–482 / :504–521 | RUN replies metadata; PULL streams records | 6 |
| bolt_api.c:831 — `ws_handshake` | WebSocket sniff-and-upgrade | 6 |

Suggested route: bolt.h (the enum, Step 4) → bolt.c top-down (markers →
ints → containers, Steps 3–4) → bolt_api.c following one session in the
Step 5 diagram's order.

## Questions

1. RUN/PULL splits "execute" from "fetch". What does the server have to
   *hold* between the two, and what does that cost under 10K idle
   cursors? (Compare pgwire portals, topic 7 §4.)
2. PackStream has no length prefix on messages — chunking (2-byte chunk
   headers, 0x0000 terminator) wraps it. Why chunk instead of
   length-prefixing the whole message, for a server that streams records
   as it produces them?
3. The handshake proposes four versions, server picks one
   (bolt_api.c:845-864 clamps to 5.1..5.7). Compare RESP's HELLO 2/3.
   Which design lets a proxy transparently downgrade, and why?
4. Node/Relationship on the wire carry element ids + property maps.
   What does this rule out that RESP's "everything is arrays" allows —
   and which side of the trade does a *new* graph database want?
5. Why might FalkorDB have removed Bolt (#2170)? List the real costs a
   second protocol imposes on an engine (state machines, result
   encoders, auth, tests, fuzz surface) — then what you'd need to keep
   it cheap.
6. **M7 mapping**: the stretch goal is a Bolt listener beside RESP. Which
   pieces of your M7 RESP server are protocol-neutral (executor,
   result set) and which need a Bolt twin? Sketch the
   `bolt_reply_*`-equivalent trait your result set must implement.

## References

**Papers**
- Neo4j — Bolt Protocol + PackStream specifications
  (https://neo4j.com/docs/bolt/current/) — the normative source for
  markers, messages, and the handshake

**Code**
- [FalkorDB/FalkorDB](https://github.com/FalkorDB/FalkorDB) `src/bolt/`
  (`bolt.c`, `bolt.h`, `bolt_api.c`) — removed by #2170; read it frozen
  in time with `git show 0b11a00b3^:src/bolt/<file>` in
  `~/repos/FalkorDB`
