# Bolt & PackStream: the graph in the type system

RESP encodes a node as nested arrays the client must re-interpret; Bolt puts
Node, Relationship, and Path on the wire as first-class types, and — on paper —
makes result streaming client-driven, so backpressure IS the protocol. The
reference implementation here is FalkorDB's own Bolt 5.x server, complete until
#2170 removed it on 2026-07-08. This chapter builds the protocol step by step —
the serialization format, the graph types, the message vocabulary, the
pull-based streaming — and then walks the C files to find out how much of that
design FalkorDB's server actually implemented. The answer is the most useful
thing in the chapter, and it is not the answer the design promises.

Every code anchor below is FalkorDB at commit **`40780e992`** — that is
`0b11a00b3^`, the tree one commit before PR #2170 deleted `src/bolt/`. The
repo's pin table records FalkorDB at `ccb449a9a` (`resources/codebases.md`),
where these files no longer exist, so read them at the older commit:

```sh
tools/pinned-source.py --ref 40780e992ecc11f598ce3f4f65e04367f9abae2f \
    show FalkorDB src/bolt/bolt.c -r 133:151
# or, in a clone:  git show 0b11a00b3^:src/bolt/bolt.c
```

Every protocol claim is checked against Neo4j's Bolt specification
(<https://neo4j.com/docs/bolt/current/>), cited by page and section.

## The problem in one sentence

A graph query returns nodes, relationships, and paths — typed, structured
values — but RESP can only say "array of arrays of strings", so every FalkorDB
client library re-parses nested arrays into graph objects by convention; and if
the result is 10M rows, RESP's server must buffer all of them, because the
client has no way to say "give me 1,000 at a time."

## The concepts, step by step

### Step 1 — the two problems Bolt exists to solve

> **In:** nothing yet — this step fixes the two axes every later step is
> measured against.
> **Out:** the two design goals (typing, client-driven streaming) that Steps
> 2–5 build, and that Step 6 audits the implementation against.

A **wire protocol** is the agreement between a client and a server about what
bytes mean: how a receiver finds where one message ends, what messages are
legal, and how values are spelled. Bolt is Neo4j's binary wire protocol, and it
differs from RESP in exactly the two places the problem statement names.

First, **typing** — whether the wire format carries what a value *is*, or only
what it *looks like*. RESP has five kinds of value (simple string, error,
integer, bulk string, array), so a node arrives as an array of arrays and the
client library rebuilds the object by convention. Bolt's serialization format,
**PackStream** (Steps 3–4), has markers for maps, lists, and *graph types* —
Node, Relationship, Path — so a driver hands you a graph object.

Second, **streaming**: whether the server may push a result as fast as it can
produce it, or must wait to be asked. **Backpressure** is a receiver's ability
to make a sender slow down; without it the only options are buffer (spend
memory) or die (drop the client). Bolt's answer is a protocol-level **cursor** —
a server-side position in a result the client advances explicitly — driven by
the `PULL {n}` message (Step 5).

Why it matters: these are the two axes of the RESP/pgwire/Bolt table in
[topic 7 §5](README.md#5-bolt-the-third-answer-resp-vs-pgwire-vs-bolt). Bolt is
what a protocol looks like when the *data model* lives in the protocol — and
Step 6 is what happens when an engine adopts the format but not the cursor.

### Step 2 — the handshake: version negotiation in 20 bytes

> **In:** a fresh TCP connection, no bytes exchanged.
> **Out:** an agreed protocol version, after exactly 20 bytes from the client
> and 4 from the server — and the first place FalkorDB's implementation is
> narrower than the spec.

A **handshake** is a fixed exchange that happens before any protocol message,
used to agree on which protocol will be spoken. Bolt's is deliberately tiny and
is itself unversioned (spec, *Handshake*).

The client sends 4 identification bytes `60 60 B0 17` (spec, *Handshake* —
"the identification consists of the following four bytes"), which lets a server
tell Bolt from a stray HTTP request on byte 1. FalkorDB checks them with one
comparison:

```c
// src/bolt/bolt_client.c — bolt_check_handshake, 672-680
   672  // validate bolt handshake
   673  bool bolt_check_handshake
   674  (
   675  	bolt_client_t *client  // the client
   676  ) {
   677  	ASSERT(client != NULL);
   678
   679  	return ntohl(buffer_read_uint32(&client->read_buf.read)) == 0x6060B017;
   680  }
```

Line 679 is the whole check: read four bytes big-endian (**big-endian** = most
significant byte first, which PackStream uses exclusively — spec, *PackStream*
§ *Endianness*) and compare against the magic constant.

Then the version proposals. The spec is precise: "the client submits exactly
four protocol versions, each encoded as a big-endian 32-bit unsigned integer for
a total of 128 bits" (*Handshake* § *Version negotiation*), and "a server should
assume that the versions … have been sent in order of preference. Therefore, if
a match occurs for more than one version, the first match should be selected."
The arithmetic of the chapter title:

```
client → server:   4 magic bytes  +  4 versions × 4 bytes  =  4 + 16 = 20 bytes
server → client:   1 chosen version                        =           4 bytes
```

FalkorDB reads all 16 bytes and then looks at two of them:

```c
// src/bolt/bolt_client.c — bolt_read_supported_version, 682-695
   682  // return the latest supported bolt version
   683  bolt_version_t bolt_read_supported_version
   684  (
   685  	bolt_client_t *client  // the client
   686  ) {
   687  	ASSERT(client != NULL);
   688
   689  	char data[16];
   690  	buffer_index_read(&client->read_buf.read, data, 16);
   691  	bolt_version_t version;
   692  	version.minor = data[2];
   693  	version.major = data[3];
   694  	return version;
   695  }
```

Lines 692–693 are the ones to look at: `data[2]` and `data[3]` are the low two
bytes of the *first* 4-byte proposal. The other three proposals are read into
the buffer and never examined. The spec's "first match" rule is implemented as
"first proposal or nothing".

The decision itself is in the handshake handler:

```c
// src/bolt/bolt_api.c — inside BoltHandshakeHandler, 845-866
   845  	bolt_version_t version = bolt_read_supported_version(client);
   846  	if(version.major == (uint)-1 || version.major == 255) {
   847  		version.major = 5;
   848  		version.minor = 7;
   849  	}
   850  	if(version.major != 5 || version.minor < 1) {
   851  		RedisModule_EventLoopDel(fd, REDISMODULE_EVENTLOOP_READABLE);
   852  		raxRemove(clients, (unsigned char *)&client->socket, sizeof(client->socket), NULL);
   853  		bolt_client_free(client);
   854  		return;
   855  	}
   // ... 857-859: point `write` at the start of the write buffer ...
   860  	if(client->ws) {
   861  		buffer_write_uint16(&write, htons(0x8204));
   862  	}
   863  	buffer_write_uint16(&write, 0x0000);
   864  	buffer_write_uint8(&write, MIN(version.minor, 7));
   865  	buffer_write_uint8(&write, version.major);
   866  	buffer_socket_write(&start, &write, client->socket);
```

The two lines that carry the argument are 850 and 864, and they say different
things. **Line 850 is the acceptance test**: major must be 5 and minor at least
1 — with *no upper bound*. **Line 864 is the answer**: `MIN(version.minor, 7)`.
So a client proposing 5.9 is not rejected; it is accepted and told "5.7", and
the connection then speaks 5.7. (An earlier version of this chapter said the
server "accepts 5.1..5.7, clamped to that range". The accepted range is 5.1 and
up; only the *reply* is clamped.)

Line 846 is a second surprise: a first-proposal high byte of 255 is rewritten to
5.7. `FF` in that position is how the spec spells a *manifest*-style handshake
request, introduced in Bolt 5.7 (spec, *Handshake* § *Bolt version 5.7*) — so
FalkorDB answers a manifest request with a plain 4-byte version reply.

Compare RESP, where versioning is an optional in-band `HELLO 2|3` command sent
after the connection is already usable — question 3 asks which of the two a
proxy can transparently downgrade.

### Step 3 — PackStream: type in the high nibble, size in the low

> **In:** an agreed 5.x version from Step 2; from here on both sides spell
> values in PackStream.
> **Out:** the byte-level encoding of scalars — the alphabet Step 4's
> structures and Step 5's messages are both written in.

**PackStream** is Bolt's serialization format: binary JSON with an extension
point. Every value begins with a **marker byte** — one byte that says what the
value is, and for small values how big it is (spec, *PackStream* § *General
representation*). A **nibble** is half a byte, four bits: the marker's high
nibble picks the type family, and for "tiny" variants the low nibble carries the
size, so a small value costs one marker byte total.

FalkorDB's markers are one `#define` block, and they match the spec exactly:

```c
// src/bolt/bolt.c — the marker table, 11-39
    11  #define NULL_MARKER 0xC0
   // ... 12-13: TRUE_MARKER 0xC3, FALSE_MARKER 0xC2 ...
    14  #define TINY_INT8_MIN 0xF0
    15  #define TINY_INT8_MAX 0x7F
   // ... 16-20: INT8/16/32/64 markers 0xC8-0xCB, FLOAT_MARKER 0xC1 ...
    21  #define TINY_STRING_BASE_MARKER 0x80
   // ... 22-31: STRING8/16/32, TINY_LIST 0x90, LIST8/16/32, BYTES8/16/32 ...
    32  #define TINY_MAP_BASE_MARKER 0xA0
   // ... 33-35: MAP8/16/32 markers 0xD8-0xDA ...
    36  #define STRUCTURE_BASE_MARKER 0xB0
    37
    38  #define TINY_SIZE 16
    39  #define TINY_MARKER_CHECK(base, marker) (marker >= base && marker <= base + 0x0F)
```

Line 38 is the one that explains the shape: `TINY_SIZE` is 16 because a nibble
holds 0–15. A string of 5 characters is marker `0x80 + 5 = 0x85` then 5 bytes; a
list of 3 items is `0x93` then the items; a map of 2 pairs is `0xA2` then four
values. Sixteen or more, and you pay a separate 8-, 16- or 32-bit size field
(spec, *PackStream* § *Sized values*).

Integers are a **varint-by-cases** encoding — a variable-length integer where
the width is chosen per value rather than fixed — biased so the common range
costs one byte:

```c
// src/bolt/bolt.c — bolt_reply_int, 130-151
   130  // write int value to client response buffer
   131  // using the minimal representation
   132  // if the minimal representation is known use it for better performance
   133  void bolt_reply_int
   134  (
   135  	bolt_client_t *client,  // client to write to
   136  	int64_t data            // int value to write
   137  ) {
   138  	ASSERT(client != NULL);
   139
   140  	if(data >= TINY_INT8_MIN && data <= TINY_INT8_MAX) {
   141  		bolt_reply_tiny_int(client, data);
   142  	} else if(INT8_MIN <= data && data <= INT8_MAX) {
   143  		bolt_reply_int8(client, data);
   144  	} else if(INT16_MIN <= data && data <= INT16_MAX) {
   145  		bolt_reply_int16(client, data);
   146  	} else if(INT32_MIN <= data && data <= INT32_MAX) {
   147  		bolt_reply_int32(client, data);
   148  	} else {
   149  		bolt_reply_int64(client, data);
   150  	}
   151  }
```

Line 140 is the one to focus on: `TINY_INT8_MIN` is `0xF0` and `TINY_INT8_MAX`
is `0x7F` (lines 14–15), which as signed bytes are **−16 and +127** — exactly
the range the spec calls TINY_INT and says is "encoded within a single byte".
`bolt_reply_tiny_int` (bolt.c:68–77) then writes the value itself as the marker,
with no separate type byte at all.

Worked example — three integers through this ladder, checked against the
spec's *optimal representation* table:

```
       42   →  140 true  (−16 ≤ 42 ≤ 127)   → tiny_int      2A                    1 byte
      300   →  144 true  (INT16 range)      → int16         C9 01 2C              3 bytes
      −17   →  142 true  (INT8 range)       → int8          C8 EF                 2 bytes
2^40 (1.1e12) → 148 false, falls through    → int64         CB 00 00 01 00 ...    9 bytes
```

−17 is the interesting one: it misses the tiny range by exactly one, and the
cost of that one step is a doubling, 1 byte → 2. Spec agrees: its table gives
INT_8 for −128…−17 and TINY_INT for −16…+127.

Compare [topic 7 §1](README.md#1-resp-a-protocol-optimized-for-the-parser):
RESP optimizes the *parser* (ASCII lengths, `memchr` for CRLF, never scan the
payload); PackStream optimizes the *type round-trip* — one marker byte
dispatches to a decoder that already knows the target type.

### Step 4 — structures: one mechanism for messages AND graph types

> **In:** the scalar markers from Step 3.
> **Out:** the one composite form — the structure — that both protocol messages
> (Step 5) and graph values (this step) are built from, plus the byte count that
> shows what typing buys against RESP.

A **structure** is PackStream's extension point: marker `0xB0 + n_fields`, then
a **tag byte** naming what the structure *is*, then that many
PackStream-encoded fields, arbitrarily nested. The spec is explicit that the
size is a *field count*, not a byte count, and that a structure holds "up to 15
fields" (spec, *PackStream* § *Structure*). FalkorDB's writer is five lines:

```c
// src/bolt/bolt.c — bolt_reply_structure, 248-260
   248  // write structure header to client response buffer
   249  // expected 'size' number of items to follow
   250  void bolt_reply_structure
   251  (
   252  	bolt_client_t *client,     // client to write to
   253  	bolt_structure_type type,  // structure type
   254  	uint32_t size              // number of items to follow
   255  ) {
   256  	ASSERT(client != NULL);
   257
   258  	int8_t values[2] = {STRUCTURE_BASE_MARKER + size, type};
   259      buffer_write(&client->write_buf.write, values, 2);
   260  }
```

Line 258 carries the argument, and note what it does *not* have: unlike
`bolt_reply_string` (170–194), `bolt_reply_list` (198–219) and `bolt_reply_map`
(225–246), which each branch four ways on size, the structure writer has no
large form and no bounds check. `0xB0 + size` is written unconditionally, so a
17-field structure would emit `0xC1` — the FLOAT marker. That is safe only
because the spec caps structures at 15 fields and every call site in this tree
passes a literal 0–8.

The elegant part is the tag byte's namespace. One enum covers the protocol's
*messages* and its *data types*:

```c
// src/bolt/bolt.h — bolt_structure_type, 27-49 (elided rows are more messages)
    27  typedef enum bolt_structure_type {
    28  	BST_HELLO = 0x01,                 // hello message from client
   // ... 29-30: GOODBYE 0x02, RESET 0x0F ...
    31  	BST_RUN = 0x10,                   // run query message from client
   // ... 32-34: BEGIN 0x11, COMMIT 0x12, ROLLBACK 0x13 ...
    35  	BST_DISCARD = 0x2F,               // discard all message from client
    36  	BST_PULL = 0x3F,                  // pull records message from client
    37  	BST_NODE = 0x4E,                  // node value
    38  	BST_PATH = 0x50,                  // path value
    39  	BST_RELATIONSHIP = 0x52,          // relationship value
   // ... 40-43: POINT2D 0x58, ROUTE 0x66, LOGON 0x6A, LOGOFF 0x6B ...
    44  	BST_SUCCESS = 0x70,               // success message
    45  	BST_RECORD = 0x71,                // record message
   // ... 46-48: UNBOUND_RELATIONSHIP 0x72, IGNORED 0x7E, FAILURE 0x7F ...
    49  } bolt_structure_type;
```

Lines 37–39 sit in the same enum as lines 31 and 44–45: a `RUN` message and a
`Node` value are the same kind of thing on the wire, distinguished only by the
tag. A Path (0x50) is a structure whose fields are lists of Node and
Relationship structures. RESP has no equivalent — there is one composite type,
the array, and no way to label it.

```rust
// ILLUSTRATION — not quoted from FalkorDB. The real writers are
// src/bolt/bolt.c:250-260 (structure header) and src/bolt/bolt.c:133-151
// (integers); this is the same two decisions in one readable place.
fn write_struct_header(out: &mut Vec<u8>, n_fields: u8, tag: u8) {
    out.push(0xB0 + n_fields);   // bolt.c:258 — no large form, no bounds check
    out.push(tag);               // 0x4E Node, 0x52 Relationship, 0x50 Path, 0x10 RUN
}                                // then n_fields values follow, each PackStream-encoded
```

Worked example — what the typing actually costs and saves. Take one node: id
42, label `Person`, properties `name="Alice"` and `age=30`. FalkorDB's Bolt
formatter emits it as a 4-field Node structure (`resultset_replybolt.c:134`,
following the field list in the comment at :127–132):

```
B4 4E                      structure, 4 fields; tag Node                     2 bytes
CB 00 00 00 00 00 00 00 2A id — bolt_reply_int64 at :135, NOT the minimal form 9 bytes
91 86 "Person"             tiny list of 1, tiny string of 6                  8 bytes
A2 84 "name" 85 "Alice"    tiny map of 2 pairs; key 4, value 5
   83 "age"  1E              key 3, value 30 as a tiny int                  17 bytes
87 "node_42"               element_id, built by sprintf at :117             8 bytes
                                                                    total = 44 bytes
```

The same node through the RESP verbose formatter
(`resultset_replyverbose.c:131–168`, whose reply shape is the comment at
:132–138) is a 3-element array of `["id", n]`, `["labels", [...]]`,
`["properties", [[k, v], ...]]`:

```
*3\r\n *2\r\n $2\r\nid\r\n :42\r\n            4 + 4 +  8 +  5   =  21 bytes
*2\r\n $6\r\nlabels\r\n *1\r\n $6\r\nPerson\r\n  4 + 12 + 4 + 12  =  32 bytes
*2\r\n $10\r\nproperties\r\n *2\r\n         4 + 17 + 4
   *2\r\n $4\r\nname\r\n $5\r\nAlice\r\n     + 4 + 10 + 11
   *2\r\n $3\r\nage\r\n :30\r\n              + 4 +  9 +  5      =  68 bytes
                                                                total = 121 bytes
```

121 / 44 = **2.75×**, and the RESP version does not even carry the element id.
Two details make the comparison honest rather than flattering. Bolt spends 9
bytes on an id that would fit in one, because `_ResultSet_BoltReplyWithNode`
calls `bolt_reply_int64` (:135) rather than the minimal `bolt_reply_int` from
Step 3 — property *values* do use the minimal form (:45), node ids do not. And
the RESP reply's comment at :136 promises `[name, value, value type]` triples
while the code at :117 replies an array of 2; the third element is not emitted.
Read the code, not the comment.

Wrapped for the wire, that node costs a little more: `bolt_client_reply_for`
writes a 2-byte chunk-length placeholder plus `B1 71` (a 1-field RECORD
structure), then a `91` list header for the single column, then the 44 bytes,
then a 2-byte terminator — **51 bytes** per single-column RECORD. Step 5
explains the chunk bytes.

### Step 5 — RUN/PULL: the client drives the stream (as specified)

> **In:** the structures from Step 4, now used as messages rather than values.
> **Out:** the message sequence a Bolt driver expects, and the chunk framing
> that carries it. Step 6 checks this sequence against FalkorDB's code and finds
> half of it missing.

Bolt splits "execute" from "fetch". `RUN` (tag 0x10, spec *Messages* §
*Request message RUN*) carries the query, its parameters and an extras map, and
the server answers only *metadata* — `SUCCESS {fields}`, the column names.
Records flow only in response to `PULL` (tag 0x3F): its `extra` map has `n`,
"how many records to fetch", which "has no default and must be present" since
Bolt 4.0, and `qid`, naming which open result to pull from. The server answers
with zero or more RECORDs and then `SUCCESS {has_more}` — "true if there are
more records to stream" (spec, *Messages* § *Request message PULL*). The client
either pulls again or sends `DISCARD` (0x2F) to stop paying for rows it does not
want.

```
client                                server
  │ 60 60 B0 17 + 4×4 version bytes    │  20 bytes in, 4 back:
  ├────────────────────────────────────►│  bolt_client.c:679 (magic),
  │◄──────────────── chosen version ────┤  :690-693 (proposals), api.c:850/:864
  │ HELLO {auth...}          0x01       │  bolt_api.c:708-710
  │◄─────────────── SUCCESS  0x70 ──────┤
  │ RUN "MATCH..." {} {}     0x10       │  case at bolt_api.c:721-723
  │◄─────────────── SUCCESS {fields} ───┤  resultset_replybolt.c:278-295
  │ PULL {n: 1000}           0x3F       │  case at bolt_api.c:726-730
  │◄─────────────── RECORD × n  0x71 ───┤  resultset_replybolt.c:263-275
  │◄─────────────── SUCCESS {has_more}──┤  ← spec says so; FalkorDB never
  │ DISCARD                  0x2F       │    sends has_more (Step 6)
```

The framing is the last piece. PackStream values carry no overall message
length, so messages are wrapped in **chunks**: each chunk is a 2-byte
big-endian length header followed by that many bytes, and each message ends with
a zero-length chunk, `00 00`. The spec's reason is the interesting part: "a
message can be divided across multiple chunks, allowing client and server alike
to transfer large messages without having to determine the length of the entire
message in advance" (spec, *Message* § *Chunking*). The header is 16 bits, so
one chunk holds at most 65,535 bytes.

FalkorDB writes the header *first* and fills it in *afterwards*:

```c
// src/bolt/bolt_client.c — the chunk header placeholder, inside
// bolt_client_reply_for, 569-575
   569  	msg.bolt_header = client->write_buf.write;
   570  	buffer_write_uint16(&client->write_buf.write, 0x0000);
   571  	msg.start = client->write_buf.write;
   572  	msg.end = client->write_buf.write;
   573  	arr_append(client->write_messages, msg);
   574
   575  	bolt_reply_structure(client, response_type, size);
```

```c
// src/bolt/bolt_client.c — the back-patch, inside bolt_client_end_message,
// 586-598
   586  	bolt_message_t *msg = client->write_messages + arr_len(client->write_messages) - 1;
   587  	msg->end = client->write_buf.write;
   588  	uint64_t n = buffer_index_diff(&msg->end, &msg->start);
   // ... 589-594: the WebSocket variant writes a frame header too ...
   595  	buffer_index_t write_bolt_header = msg->bolt_header;
   596  	buffer_write_uint16(&write_bolt_header, htons(n));
   597  	buffer_write_uint16(&client->write_buf.write, 0x0000);
   598  	msg->end = client->write_buf.write;
```

Line 588 measures the finished message and line 596 writes that length back into
the two bytes reserved at 570; line 597 appends the `00 00` terminator. So this
implementation emits **exactly one chunk per message** and never splits — which
means it buys none of the "without having to determine the length in advance"
property the spec's chunking exists for, and depends on every message fitting in
the 16-bit header (`htons` of a `uint64_t` truncates above 65,535).

### Step 6 — what FalkorDB's server actually implements

> **In:** the specified message flow from Step 5.
> **Out:** the audit — which parts of the flow exist in `src/bolt/`, which are
> empty, and what that means for the backpressure claim the chapter opened with.

The dispatch loop is as small as advertised. `BoltRequestHandler`
(bolt_api.c:670) reassembles a chunked message and switches on its tag —
11 labelled cases plus `default`, at :706–745. Two of those cases are the whole
finding:

```c
// src/bolt/bolt_api.c — inside BoltRequestHandler's switch, 721-730
   721  		case BST_RUN:
   722  			BoltRunCommand(client);
   723  			break;
   724  		case BST_DISCARD:
   725  			break;
   726  		case BST_PULL:
   727  			BoltPullCommand(client);
   728  			client->processing = false;
   729  			BoltRequestHandler(client);
   730  			break;
```

Line 724 is an empty case: `DISCARD` does nothing at all. And line 727 calls
this:

```c
// src/bolt/bolt_api.c — BoltPullCommand, in full, 539-552
   539  // handle the PULL message
   540  void BoltPullCommand
   541  (
   542  	bolt_client_t *client  // the client that sent the message
   543  ) {
   544  	// The PULL message requests data from the remainder of the result stream
   545  	// input:
   546  	// extra::Dictionary{
   547  	//   n::Integer,
   548  	//   qid::Integer,
   549  	// }
   550
   551  	ASSERT(client != NULL);
   552  }
```

The line to look at is 551, because it is the only one: the body is an assert.
`n` is documented in the comment and never read. **FalkorDB's Bolt server does
not implement the cursor.** The rows are produced by the query, not by the pull:
`BoltRunCommand` dispatches `graph.QUERY` with a `--bolt` marker argument
(bolt_api.c:526–532, strings created at :979–980), and the result set formatter
emits everything as it goes —

```c
// src/resultset/formatters/resultset_replybolt.c — ResultSet_EmitBoltRow, 263-275
   263  void ResultSet_EmitBoltRow
   264  (
   265  	ResultSet *set,
   266  	SIValue **row
   267  ) {
   268  	bolt_client_t *bolt_client = set->bolt_client;
   269  	bolt_client_reply_for(set->bolt_client, BST_PULL, BST_RECORD, 1);
   270  	bolt_reply_list(set->bolt_client, set->column_count);
   271  	for(int i = 0; i < set->column_count; i++) {
   272  		_ResultSet_BoltReplyWithSIValue(bolt_client, set->gc, *row[i]);
   273  	}
   274  	bolt_client_end_message(bolt_client);
   275  }
```

Line 269 is the tell: every RECORD is *labelled* a reply to `PULL`, and this
function is called once per row by the executor as rows are produced. The
matching header (`ResultSet_ReplyWithBoltHeader`, :278–295) labels its metadata
a reply to `RUN` at :283. A driver therefore sees a well-formed Bolt
conversation — SUCCESS-for-RUN, RECORDs-for-PULL, SUCCESS-for-PULL from
`ResultSet_EmitBoltStats` (:297, :301) — while the server behind it never
suspended anything. `has_more` appears nowhere in `src/bolt/` or in the Bolt
formatter; grep both and you get nothing.

So the honest version of the chapter's opening claim: **Bolt specifies
protocol-level backpressure, and FalkorDB implemented Bolt's typing without it.**
A 10M-row `MATCH` buffers 10M rows into `client->write_buf` exactly as RESP
would — the axe of [topic 7 §4](README.md#4-backpressure--the-part-everyone-forgets)
just falls on a different buffer.

There is a second thing the code does *not* do, and it costs more than it looks.
`BoltRequestHandler` refuses to start a message while one is in flight:

```c
// src/bolt/bolt_api.c — the in-flight gate, 676-680 and 704
   676  	// if there is a message already in process or
   677  	// not enough data to read the message
   678  	if(client->processing || buffer_index_length(&client->read_buf.read) <= 2) {
   679  		return;
   680  	}
   // ... 682-702: reassemble the chunked message into msg_buf ...
   704  	client->processing = true;
```

Line 678 is **head-of-line blocking** — a queue discipline where one unfinished
item stops every item behind it, however ready they are. One Bolt message per
connection at a time means no **pipelining** (sending k requests back-to-back
without waiting for replies), so every message pays a full **round trip**: the
wire, kernel and wakeup cost of one send-and-receive exchange. The flag is
cleared in `BoltResponseHandler` after the socket write (:894) and, for PULL,
inline at :728. Topic 7's own lane prices that discipline: identical zero-work
requests run at **44,088 ops/s at pipeline depth P=1 and 12,321,414 at P=256**
([FINDINGS.md](../../FINDINGS.md) row 7, full table in [notes.md](notes.md)) —
a 279× swing that is nothing but round trips. A protocol that structurally
forbids depth > 1 has chosen the left-hand end of that curve.

What the implementation *does* get right is the decoupling that makes a second
protocol cheap, in three parts. First, the listener is its own port riding
redis's event loop rather than a thread of its own:

```c
// src/bolt/bolt_api.c — inside BoltApi_Register, 965-980
   965      socket_t bolt = socket_bind(port);
   // ... 966-972: bail out if the bind failed; detach a thread-safe context ...
   973  	if(RedisModule_EventLoopAdd(bolt, REDISMODULE_EVENTLOOP_READABLE, BoltAcceptHandler, global_ctx) == REDISMODULE_ERR) {
   // ... 974-976: log and fail ...
   977  	RedisModule_Log(NULL, "notice", "Bolt protocol initialized. Port: %d", port);
   978
   979  	COMMAND = RedisModule_CreateString(global_ctx, "graph.QUERY", 11);
   980  	BOLT = RedisModule_CreateString(global_ctx, "--bolt", 6);
```

Line 973 is the one that matters: the Bolt socket is registered on the *same*
`ae` loop that serves RESP. `RedisModule_EventLoopAdd` is a thin wrapper that
ends in `aeCreateFileEvent(server.el, …)` — redis `src/module.c:10115`, at the
`a176d1225` pin — so two protocols share one thread and one engine, and
everything [reading-redis-ae-networking.md](reading-redis-ae-networking.md) says
about the loop applies to Bolt unchanged. Second, line 979: RUN turns into the
ordinary `graph.QUERY` command with a `--bolt` marker (:980), so only the result
*formatter* differs. Third, `ws_handshake` (called at bolt_api.c:831, defined in
`src/bolt/ws.c:110`) sniffs a WebSocket upgrade on the same port when the magic
bytes are absent — which is how browser clients speak Bolt.

Why it matters for M7: the stretch goal is exactly this shape — a Bolt listener
beside your RESP one, sharing the executor and result set — and this
implementation tells you which half is cheap (the listener, the formatter) and
which half is the actual work (the cursor, question 6).

## Where each step lives in the code

All in the removed `src/bolt/` tree plus the Bolt result formatter, at
`0b11a00b3^` = `40780e992`:

| Anchor | What | Step |
|--------|------|------|
| `bolt_client.c:672-680` — `bolt_check_handshake` | magic `0x6060B017`, compared at :679 | 2 |
| `bolt_client.c:682-695` — `bolt_read_supported_version` | reads 16 bytes, uses `data[2..3]` — the first proposal only | 2 |
| `bolt_api.c:845-866` | accept test at :850 (major 5, minor ≥ 1); reply `MIN(minor,7)` at :864 | 2 |
| `bolt.c:11-39` | marker table; `TINY_SIZE 16` at :38 is why nibbles work | 3 |
| `bolt.c:130-151` — `bolt_reply_int` | varint-by-cases; the tiny test is :140 | 3 |
| `bolt.c:170-194 / :198-219 / :225-246` | string, list, map — each branches four ways on size | 3 |
| `bolt.c:248-260` — `bolt_reply_structure` | `0xB0 + size` at :258, no large form, no bounds check | 4 |
| `bolt.h:27-49` — `BST_*` | messages *and* Node 0x4E (:37) / Path 0x50 (:38) / Relationship 0x52 (:39) | 4 |
| `resultset_replybolt.c:121-161` | the 4-field Node; `bolt_reply_int64` for the id at :135 | 4 |
| `resultset_replybolt.c:109-119` | `element_id` = `sprintf("%s_%llu")` at :117 | 4 |
| `bolt_client.c:569-575 / :586-598` | chunk header placeholder, then back-patched at :596 | 5 |
| `bolt_api.c:670`, switch :706-745 | `BoltRequestHandler` — 11 cases + default | 6 |
| `bolt_api.c:539-552` — `BoltPullCommand` | **empty**: one `ASSERT` at :551 | 6 |
| `bolt_api.c:724-725` | `BST_DISCARD` — an empty case | 6 |
| `bolt_api.c:676-680`, :704, :894 | the `processing` gate: one message in flight per connection | 6 |
| `resultset_replybolt.c:263-275` / :278-295 / :297 | RECORD, header, stats — labelled replies to PULL/RUN | 6 |
| `bolt_api.c:949-984` — `BoltApi_Register` | own port at :965, on redis's event loop at :973 | 6 |
| `bolt_api.c:831`, `ws.c:110` | WebSocket sniff-and-upgrade on the same port | 6 |

Suggested route: `bolt.h` (the enum, Step 4) → `bolt.c` top-down (markers →
ints → containers, Steps 3–4) → `bolt_api.c` following one session in the
Step 5 diagram's order — and when you reach `BoltPullCommand`, stop and check
whether you believe the chapter's Step 6 or its Step 5.

## Questions

1. RUN/PULL splits "execute" from "fetch". What would the server have to *hold*
   between the two to implement it properly, and what does that cost under 10K
   idle cursors? (Compare pgwire portals,
   [topic 7 §4](README.md#4-backpressure--the-part-everyone-forgets).)
2. PackStream has no length prefix on messages — chunking (2-byte headers,
   `00 00` terminator) wraps it. The spec's stated reason is streaming without
   knowing the total length; FalkorDB back-patches a single chunk header
   instead (bolt_client.c:596). What would have to change in
   `bolt_client_end_message` to emit a genuinely streamed multi-chunk RECORD,
   and what breaks if a message exceeds 65,535 bytes today?
3. The handshake takes four proposals in preference order and FalkorDB reads
   the first (bolt_client.c:692-693), rejecting anything that is not 5.x with
   minor ≥ 1. Compare RESP's in-band `HELLO 2|3`. Which design lets a proxy
   transparently downgrade a connection, and why?
4. Node/Relationship on the wire carry element ids and property maps. What does
   that rule out that RESP's "everything is arrays" allows — and which side of
   the trade does a *new* graph database want?
5. Why might FalkorDB have removed Bolt (#2170)? List the real costs a second
   protocol imposes (state machines, result encoders, auth, tests, fuzz
   surface), then say which of them the *unfinished* parts you found in Step 6
   — the empty PULL, the missing `has_more` — make better or worse.
6. **M7 mapping**: the stretch goal is a Bolt listener beside RESP. Which pieces
   of your M7 server are protocol-neutral (executor, result set) and which need
   a Bolt twin? Sketch the `bolt_reply_*`-equivalent trait your result set must
   implement, and decide whether you implement `PULL {n}` for real.

## Done when

Answer each before unfolding it.

- [ ] You can write a PackStream marker byte from memory and decode type and size from its nibbles.

  <details><summary>Answer</summary>

  The high nibble selects the type family and, for the "tiny" variants, the low
  nibble is the size: `0x8_` string, `0x9_` list, `0xA_` map, `0xB_` structure.
  So `0x85` is a 5-character string, `0x93` is a 3-item list, `0xA2` is a
  2-pair map, and `0xB4` is a 4-field structure whose *next* byte is the tag.
  The cut-off is 16 because a nibble holds 0–15 — `TINY_SIZE` at `bolt.c:38`,
  used by `TINY_MARKER_CHECK` at :39 and by each writer's first branch
  (`bolt_reply_string` :179, `bolt_reply_list` :205, `bolt_reply_map` :232).

  Scalars have fixed markers rather than sizes: `0xC0` null (`bolt.c:11`),
  `0xC2`/`0xC3` false/true, `0xC1` float, `0xC8`–`0xCB` int8/16/32/64. Integers
  in −16…+127 have no marker at all — the value *is* the byte
  (`bolt_reply_tiny_int`, bolt.c:68–77, guarded at :140 by `TINY_INT8_MIN`
  `0xF0` and `TINY_INT8_MAX` `0x7F`). That is why 42 costs 1 byte, −17 costs 2,
  and 300 costs 3.

  </details>

- [ ] You can explain how one structure mechanism serves both protocol messages and graph types, and why that is more than an aesthetic choice.

  <details><summary>Answer</summary>

  A structure is `0xB0 + n_fields`, a tag byte, then the fields
  (`bolt_reply_structure`, bolt.c:250–260, the write at :258). The tag is drawn
  from a single enum that contains `BST_RUN = 0x10` (bolt.h:31) and
  `BST_RECORD = 0x71` (:45) alongside `BST_NODE = 0x4E` (:37),
  `BST_PATH = 0x50` (:38) and `BST_RELATIONSHIP = 0x52` (:39). A message and a
  node are the same shape; only the tag differs.

  It is not aesthetic because it makes the *encoder* recursive and the *decoder*
  a single dispatch table. `_ResultSet_BoltReplyWithSIValue`
  (resultset_replybolt.c:33) is one switch that reaches nodes (:57), edges
  (:60) and paths by calling the same primitives that write a RUN's parameter
  map — a Path is a structure of lists of Node and Relationship structures, and
  nothing special is needed to nest it. On the client side a driver registers
  one handler per tag and gets graph objects out of the decoder rather than
  reconstructing them: for the node in Step 4, 44 typed bytes against RESP's
  121 untyped ones, with the element id included in the smaller number.

  </details>

- [ ] You can say what RUN/PULL buys by splitting execute from fetch, and what the server must therefore hold between them.

  <details><summary>Answer</summary>

  The split buys client-driven flow control. `RUN` (0x10) returns only
  `SUCCESS {fields}`; rows arrive only when the client sends `PULL {n}` (0x3F),
  whose `n` "has no default and must be present" since Bolt 4.0, and the server
  answers `SUCCESS {has_more: true}` if there is more (spec, *Messages* §
  *Request message PULL*). The client sizes its own bites and can walk away with
  `DISCARD` (0x2F).

  What the server must hold between the two is the expensive part: a suspended
  execution — the plan, its iterators, the read transaction or snapshot the rows
  are being read under, and enough identity (`qid`) to route a later PULL to the
  right one. That is per-connection state that survives across event-loop turns,
  which is why 10K idle cursors is a real cost and why pgwire's portals carry
  the same liability.

  FalkorDB declined to pay it. `BoltPullCommand` (bolt_api.c:539–552) is one
  `ASSERT` and reads neither `n` nor `qid`; `BST_DISCARD` (:724–725) is an empty
  case; `has_more` appears nowhere. Rows are emitted by
  `ResultSet_EmitBoltRow` (resultset_replybolt.c:263–275) as the executor
  produces them, merely *labelled* as replies to PULL at :269. The wire looks
  like a cursor; the server is buffer-or-die, the same as RESP.

  </details>

- [ ] You can explain how chunking substitutes for a message length prefix, and what that costs a parser.

  <details><summary>Answer</summary>

  A PackStream value's marker tells you the size of *that value*, never of the
  message containing it, so the transport adds a layer: each chunk is a 2-byte
  big-endian length followed by that many bytes, and a message ends with a
  zero-length chunk `00 00`. Because a message may span several chunks, a sender
  can start transmitting before it knows the total length (spec, *Message* §
  *Chunking*); the 16-bit header caps one chunk at 65,535 bytes.

  The cost lands on the reader, which now has two loops: reassemble chunks until
  the terminator, *then* parse PackStream. That is exactly `BoltRequestHandler`
  at bolt_api.c:690–696 — read a `uint16`, copy that many bytes into `msg_buf`,
  repeat until the length reads zero — and it must also handle "the next chunk
  has not arrived yet" by returning and resuming (:693).

  FalkorDB's *writer* takes none of the benefit. `bolt_client_reply_for`
  reserves two bytes at bolt_client.c:570 and `bolt_client_end_message`
  back-patches the finished length at :596 — one chunk per message, always,
  which means the whole message was buffered before the header could be
  written, and a message over 65,535 bytes truncates in `htons`.

  </details>

- [ ] You can state, from the code rather than the specification, how much of Bolt's backpressure FalkorDB implemented — and what that means for a 10M-row query.

  <details><summary>Answer</summary>

  None of it. The evidence is three anchors: `BoltPullCommand`
  (bolt_api.c:539–552) has an empty body, the `BST_DISCARD` case (:724–725) is
  empty, and no `has_more` key is written anywhere in `src/bolt/` or
  `resultset_replybolt.c`. The RECORDs come out of `ResultSet_EmitBoltRow`
  (:263–275) as the executor produces rows, into `client->write_buf`, and the
  socket write happens later in `BoltResponseHandler` (bolt_api.c:875, send at
  :893). A 10M-row `MATCH` therefore materialises 10M encoded rows in the
  module's buffer before a byte is guaranteed to move — the same buffer-or-die
  failure mode as RESP, reached through a protocol whose spec exists partly to
  prevent it.

  The connection is also strictly one message at a time: `BoltRequestHandler`
  returns early while `client->processing` is set (:678, set at :704, cleared at
  :894 and inline for PULL at :728). That is head-of-line blocking, so a Bolt
  client cannot pipeline, and every message pays a full round trip. Topic 7's
  lane prices that at 44,088 ops/s against 12,321,414 for the same zero-work
  request at depth 256 ([FINDINGS.md](../../FINDINGS.md) row 7) — a 279× gap
  that this design forgoes by construction.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the honest cost list for why FalkorDB removed Bolt.

  <details><summary>Answer</summary>

  The cost list should be concrete, and the code gives you most of it: a second
  message state machine (`bolt_change_client_state`, called from
  bolt_client.c:576), a second result encoder (`resultset_replybolt.c`, 375
  lines, one branch per SIValue type), a second auth path (HELLO/LOGON at
  bolt_api.c:708–713), a second framing layer to fuzz (chunk reassembly at
  :690–696, plus the WebSocket variant at :686–689), a second listener and its
  configuration (`BoltApi_Register` :949–984), and the version matrix Step 2
  showed is easy to get subtly wrong.

  Then note which way the unfinished parts cut. An *empty* `BoltPullCommand` is
  cheap to maintain but is a promise the wire makes and the server does not
  keep, so every driver that batches with `n` is silently getting
  buffer-everything behaviour — a bug report that costs more than the code it
  saved. The way to keep a second protocol cheap is the opposite of what
  happened here: share the executor and the result set (which this
  implementation does, via `graph.QUERY` at bolt_api.c:979 and
  `CommandDispatch` at :532), and make the protocol layer a thin encoder over a
  cursor abstraction that *both* protocols use — so `PULL {n}` and a future
  `GRAPH.CURSOR` are the same code path.

  </details>

## References

**Specification**
- Neo4j — *Bolt Protocol* and *PackStream* specifications
  (<https://neo4j.com/docs/bolt/current/>). Sections cited above: *Handshake* §
  *Version negotiation* (four 4-byte proposals, first match wins) and § *Bolt
  version 5.7* (the manifest handshake); *PackStream* § *General
  representation*, § *Sized values*, § *Endianness*, § *Integer* (the optimal
  representation table) and § *Structure* (tag byte, up to 15 fields);
  *Message* § *Chunking* (2-byte headers, `00 00` terminator, 65,535 maximum);
  *Messages* § *Request message RUN* (signature 10) and § *Request message
  PULL* (signature 3F, `n` and `qid`, `has_more`).

**Code**
- [FalkorDB/FalkorDB](https://github.com/FalkorDB/FalkorDB) `src/bolt/`
  (`bolt.c`, `bolt.h`, `bolt_api.c`, `bolt_client.c`, `ws.c`) and
  `src/resultset/formatters/resultset_replybolt.c` — removed by #2170 on
  2026-07-08; read at `0b11a00b3^` = `40780e992ecc11f598ce3f4f65e04367f9abae2f`
  with `tools/pinned-source.py --ref 40780e992… show FalkorDB <path>`, or
  `git show 0b11a00b3^:src/bolt/<file>` in a clone.

| File | Lines | What |
|------|-------|------|
| `src/bolt/bolt.c` | 11-39 | the marker table, and `TINY_SIZE 16` |
| `src/bolt/bolt.c` | 133-151 | `bolt_reply_int` — varint by cases |
| `src/bolt/bolt.c` | 250-260 | `bolt_reply_structure` — the whole extension point |
| `src/bolt/bolt.h` | 27-49 | one tag enum for messages and graph types |
| `src/bolt/bolt_client.c` | 672-695 | handshake magic and version read |
| `src/bolt/bolt_client.c` | 569-598 | chunk header written, then back-patched |
| `src/bolt/bolt_api.c` | 539-552 | `BoltPullCommand` — the empty cursor |
| `src/bolt/bolt_api.c` | 670-746 | `BoltRequestHandler` — the state machine |
| `src/bolt/bolt_api.c` | 845-866 | version acceptance and reply |
| `src/bolt/bolt_api.c` | 949-984 | second port, same event loop |
| `src/resultset/formatters/resultset_replybolt.c` | 121-161 | the Node structure on the wire |
| `src/resultset/formatters/resultset_replybolt.c` | 263-275 | RECORDs, emitted as rows are produced |

**Measured in this repo**
- [FINDINGS.md](../../FINDINGS.md) row 7 — 44k ops/s at P=1 against 12.3M at
  P=256, the price of the round trips a non-pipelining protocol pays.
