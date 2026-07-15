# pgwire & tonic: sessions, portals, and protocols you don't write

Two contrasts with RESP: a protocol with *stateful sessions and streaming*
(postgres wire, via the pgwire Rust crate), and a protocol you don't write at
all (gRPC, via qdrant's tonic setup). Together they bracket RESP's design
point — no handshake, no cursors, buffer-or-die — and fill in the
design-space table M7 has to take a position on. This chapter builds the
concepts step by step — protocol as state machine, framing, the two postgres
query modes, portals, sessions, and generated protocols — then maps them to
the two codebases.

## The problem in one sentence

A query returning 10M rows must not require the server to hold 10M rows in
a reply buffer (RESP's answer) — postgres's protocol lets the client pull
1,000 rows at a time from a suspended query, and gRPC inherits flow control
from HTTP/2; both bake into the *protocol* the backpressure RESP doesn't
have.

## The concepts, step by step

### Step 1 — a wire protocol is a state machine plus a framing rule

A wire protocol answers two questions: how does the receiver know where one
message ends (**framing**), and what messages are legal *now* (the **state
machine**). RESP's state machine is trivial — any command, any time, one
reply each. Postgres's is not: a connection moves through startup → auth →
ready → (parse → bind → execute)* → sync, and several messages are only
legal in certain states. Reading pgwire, keep asking: *where does session
state live?* — the crate forces a `ClientInfo` parameter through every
handler call; your RESP server keeps per-connection state implicitly in the
tokio task. Both are answers to "protocol = state machine".

### Step 2 — postgres framing: one type byte + a binary length

Every postgres message is framed as 1 ASCII type byte + a 4-byte
big-endian i32 length + payload. Like RESP it dispatches on a leading type
byte; unlike RESP the length is *binary* (RESP spells lengths in ASCII
digits terminated by CRLF). Fixed 5-byte headers mean the reader never
scans: read 5 bytes, learn the size, read exactly that many.

In pgwire this is `src/messages/` — every frontend/backend message as a
typed struct with encode/decode. The crate structure IS the protocol
lesson: one module per message family, one type per message.

### Step 3 — the simple query protocol: RESP with row framing

`SimpleQueryHandler` (src/api/query.rs:48) is the RESP-like mode: one
`Query` message carrying a SQL string in; a stream of messages out —
`RowDescription` (column names/types), then zero or more `DataRow`s, then
`CommandComplete`. One request, one complete response, no state left
behind. Note what's already better than RESP for a database: rows are
individually framed messages, so the server can *write them as it produces
them* instead of materializing the whole result first.

### Step 4 — the extended query protocol: portals are protocol-level backpressure

`ExtendedQueryHandler` (src/api/query.rs:174) splits "run this SQL" into
five messages — Parse → Bind → Execute → Sync — and that decomposition is
where the power lives:

- **Parse**: compile SQL into a named **prepared statement** (a compiled
  query kept server-side, reusable with different parameters).
- **Bind**: attach concrete parameter values to a statement, producing a
  **portal** — a suspended, partially-executed query the server holds.
- **Execute {max_rows}**: pull up to max_rows rows *from* the portal. Not
  done? The server replies `PortalSuspended` and *stops producing* — the
  client decides when (whether) to pull more. This is backpressure and
  cursoring in the protocol itself; RESP has neither (a module either
  buffers the whole reply or blocks the loop).
- **Sync**: close out the sequence, recover from errors, get
  `ReadyForQuery`.

```rust
// The protocol IS a session state machine; portals are protocol-level
// backpressure — a suspended query the client pulls N rows at a time.
match msg {
    Parse { name, sql }          => { self.stmts.insert(name, prepare(sql)?); }
    Bind { portal, stmt, args }  => { self.portals.insert(portal, cursor(stmt, args)?); }
    Execute { portal, max_rows } => {
        let cur = self.portals.get_mut(&portal)?;
        for row in cur.take(max_rows) { send(DataRow(row))?; }
        if cur.done() { send(CommandComplete)?; }
        else          { send(PortalSuspended)?; }   // client decides when to pull more
    }
    Sync => { self.close_txn_if_failed(); send(ReadyForQuery)?; }
    _ => { /* Describe, Close, Flush … */ }
}
```

Cost: five messages instead of one — a round-trip each unless pipelined
(question 3). Buy: prepared statements, parameter binding, binary result
formats, and results in client-sized bites.

### Step 5 — sessions from byte 0: the startup handshake

A postgres connection is a state machine *before the first query*:
`StartupHandler` (src/api/auth.rs; see api/mod.rs:555) processes startup
parameters (user, database, options), runs an auth exchange (possibly
multi-round-trip: cleartext, MD5, SCRAM), and only then reports
ready-for-query. RESP connections have no handshake at all (HELLO is
optional). Count what the handshake costs postgres — connection setup
latency, hence everyone runs connection pools — and what it buys:
per-session GUCs, transaction state, and cancel keys (a token another
connection can use to cancel your running query).

### Step 6 — tonic/gRPC: the protocol you don't write

gRPC inverts the whole exercise: you write a `.proto` interface definition,
and the framing, parser, streaming, and backpressure (HTTP/2 flow-control
windows — receiver-advertised byte budgets per stream) are *generated and
inherited*, not written. In qdrant, the services are generated from the
protos in the `api/` crate. The costs of not writing it: every message is
protobuf (field tags, varints — no zero-copy into your value types), and
HTTP/2 framing means you can't debug with `nc`.

Two qdrant deployment details worth noting:

- `src/tonic/mod.rs:138` and `:277` — `Server::builder()` **twice**:
  separate internal (peer-to-peer raft) and public gRPC servers. Protocol
  surface split by trust domain — compare redis exposing admin + data on
  one port.
- The middleware layers in mod.rs (auth around :138, logging, telemetry) —
  tower's onion model, where each concern wraps the next, vs redis's
  "check ACL inside processCommand".

### Step 7 — the design space, assembled

The three protocols answer the same questions differently — fill the last
row yourself:

| | RESP | pgwire | gRPC |
|---|---|---|---|
| framing | ASCII len prefixes | type byte + i32 len | HTTP/2 frames |
| parse cost | memchr + atoi | fixed header read | protobuf decode |
| streaming | no (buffer all) | portals, row-at-a-time | HTTP/2 streams |
| backpressure | output-buffer kill | portal suspend | flow-control windows |
| debuggability | `nc` works | needs a tool | needs grpcurl |
| your GRAPH.QUERY | ? | ? | ? |

## Where each step lives in the code

Local clones at `~/repos/pgwire` and `~/repos/qdrant`:

| Anchor | What | Step |
|--------|------|------|
| pgwire `src/messages/` | typed message structs, encode/decode | 2 |
| pgwire `src/api/query.rs:48` — `SimpleQueryHandler` | simple query | 3 |
| pgwire `src/api/query.rs:174` — `ExtendedQueryHandler` | Parse/Bind/Execute/Sync, portals | 4 |
| pgwire `src/api/auth.rs` + `api/mod.rs:555` — `StartupHandler` | startup + auth state machine | 5 |
| qdrant `src/tonic/mod.rs:138`, `:277` | two servers, tower middleware | 6 |
| qdrant `api/` crate | generated services from `.proto` | 6 |

Read pgwire asking Step 1's question (*where does session state live?*),
and qdrant asking Step 6's (*what did I not have to write, and what did
that cost me?*).

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

## References

**Code**
- [sunng87/pgwire](https://github.com/sunng87/pgwire) — `src/messages/`,
  `src/api/query.rs`, `src/api/auth.rs`; the crate structure IS the
  protocol lesson. Local clone at `~/repos/pgwire`.
- [qdrant/qdrant](https://github.com/qdrant/qdrant) — `src/tonic/mod.rs`
  (two servers, tower middleware) plus the generated services in the
  `api/` crate. Local clone at `~/repos/qdrant`.
