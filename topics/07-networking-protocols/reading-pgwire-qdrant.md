# Reading pgwire (Rust) + qdrant's tonic setup (1.5 h)

Two contrasts with RESP: a protocol with *stateful sessions and streaming*
(postgres wire), and a protocol you don't write at all (gRPC).

## 1. pgwire — [~/repos/pgwire](https://github.com/sunng87/pgwire)

The crate structure IS the protocol lesson:

- `src/messages/` — every frontend/backend message as a typed struct with
  encode/decode. Postgres framing: 1 type byte + i32 length + payload —
  like RESP's type-first byte but with *binary* length (RESP: ASCII digits).
- `src/api/query.rs` — the two query protocols:
  - `SimpleQueryHandler` — :48: one `Query` message in, a stream of
    `RowDescription` + `DataRow`* + `CommandComplete` out. RESP-like.
  - `ExtendedQueryHandler` — :174: Parse → Bind → Execute → Sync, five
    round-trips of state. Prepared statements, parameter binding, binary
    result formats, and **portals** — a suspended query you pull N rows
    from. This is protocol-level backpressure and cursoring; RESP has
    neither (a module either buffers the whole reply or blocks the loop).
- `src/api/auth.rs` — `StartupHandler` (see api/mod.rs:555): the connection
  is a state machine from byte 0 — startup params, auth exchange, then
  ready-for-query. RESP connections have no handshake at all (HELLO is
  optional) — count what that costs postgres in connection setup and what
  it buys (per-session GUCs, tx state, cancel keys).

Read it asking: *where does session state live?* — pgwire forces a
`ClientInfo` through every call; your RESP server keeps per-connection state
implicitly in the task. Both are answers to "protocol = state machine".

## 2. qdrant — [~/repos/qdrant](https://github.com/qdrant/qdrant)/src/tonic/

- `src/tonic/mod.rs:138` and `:277` — `Server::builder()` twice: separate
  internal (peer-to-peer raft) and public gRPC servers. Protocol surface
  split by trust domain — compare redis exposing admin + data on one port.
- The services are generated from `.proto` (see `api/` crate: qdrant's
  protos) — the parser, framing, streaming, and backpressure (HTTP/2 flow
  control windows) are *inherited*, not written. The cost: every message is
  protobuf — field tags, varints, no zero-copy into your value types; and
  HTTP/2 framing means you can't debug with `nc`.
- Note the middleware layers in mod.rs (auth :138 area, logging, telemetry)
  — tower's onion model vs redis's "check ACL inside processCommand".

## 3. The design-space table (fill the last row yourself)

| | RESP | pgwire | gRPC |
|---|---|---|---|
| framing | ASCII len prefixes | type byte + i32 len | HTTP/2 frames |
| parse cost | memchr + atoi | fixed header read | protobuf decode |
| streaming | no (buffer all) | portals, row-at-a-time | HTTP/2 streams |
| backpressure | output-buffer kill | portal suspend | flow-control windows |
| debuggability | `nc` works | needs a tool | needs grpcurl |
| your GRAPH.QUERY | ? | ? | ? |

## Questions to answer in notes.md

1. FalkorDB result sets ride RESP arrays — huge ones buffer entirely in the
   module. What would a portal-style GRAPH.QUERY cursor look like as RESP
   commands? (FalkorDB actually has one — recall GRAPH.QUERY's timeout +
   result-set config; design the missing GRAPH.CURSOR anyway.)
2. Why does qdrant run TWO tonic servers instead of one with authz? What
   attack/ops story does the split simplify?
3. Extended query's 5 messages cost a round-trip each unless pipelined —
   how does pgwire's async design let Parse/Bind/Execute/Sync coalesce, and
   what's the RESP equivalent? (MULTI? No — pipelining itself.)

## Done when

You can fill the table's last row with committed answers for M7 and defend
"RESP + explicit cursor commands" against "just use gRPC" for a graph DB.
