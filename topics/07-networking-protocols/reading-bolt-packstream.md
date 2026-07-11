# Reading guide — Bolt & PackStream (via FalkorDB's removed implementation)

Specs: Bolt Protocol + PackStream specifications (neo4j docs,
https://neo4j.com/docs/bolt/current/). Code: FalkorDB *had* a complete
Bolt 5.x server until #2170 removed it (2026-07-08) — read it frozen in
time with `cd [~/repos/FalkorDB](https://github.com/FalkorDB/FalkorDB) && git show 0b11a00b3^:src/bolt/<file>`.

## The shape of a Bolt session

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

Every message and value is one PackStream **structure**: a marker byte
`0xB0+size` (bolt.c:36), a tag byte (the `BST_*` enum, bolt.h:27), then
fields. The whole message vocabulary — HELLO/RUN/PULL/DISCARD, and the
*data* types Node 0x4E / Relationship 0x52 / Path 0x50 — lives in one
enum. RESP has no equivalent: a FalkorDB RESP reply encodes a node as
nested arrays the client library must re-interpret; Bolt puts the graph
in the type system.

## PackStream in one sitting (bolt.c)

- Markers: NULL 0xC0 (bolt.c:11), tiny-string base 0x80 (:21), structure
  base 0xB0 (:36) — high nibble = type, low nibble = size for "tiny"
  variants.
- `bolt_reply_int` (bolt.c:133) picks tiny-int/int8/16/32/64 by value —
  varint-by-cases, biased so -16..127 costs one byte.
- `bolt_reply_structure` (:250), lists (:198), maps (:225): everything
  nests; a Path is a structure of lists of Node/Relationship structures.
- Compare topic 7 §1: RESP optimizes the *parser* (scan for \r\n);
  PackStream optimizes the *type round-trip* (marker dispatch table).

## Server-side mechanics worth stealing

- `BoltRequestHandler` (bolt_api.c:670): one dispatch switch over
  `BST_*` — the protocol state machine is ~10 cases.
- RUN executes the query but replies only metadata (:467-482); records
  flow in the PULL handler (:504-521) — result *materialization* and
  result *transport* are decoupled server-side too.
- `ws_handshake` (bolt_api.c:831): the same port sniffs and upgrades
  WebSocket — that's how browser clients speak Bolt.
- It's all inside a Redis module: the Bolt socket bypasses RESP entirely
  and injects work into the same executor — two protocols, one engine.

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
