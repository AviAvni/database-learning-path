//! RESP2 parser + encoder. YOU implement this.
//!
//! The contract (fixed by the tests):
//! - `parse` consumes from the FRONT of `buf` and returns:
//!     Ok(Some(value)) — one complete value parsed; its bytes were consumed
//!     Ok(None)        — input incomplete; buf untouched, wait for more bytes
//!     Err(_)          — protocol error (kill the connection)
//! - Incomplete input is NORMAL, not an error — a read() boundary can land
//!   mid-command (this is the redis multibulklen/bulklen state, except we
//!   re-parse from the buffer start instead of keeping counters; measure the
//!   cost of that simplification later if you care).
//! - Bulk strings are binary-safe (may contain \r\n).
//! - `encode` writes any Value back to wire format.
//!
//! Wire refresher:
//!   +OK\r\n            simple string      -ERR msg\r\n      error
//!   :42\r\n            integer            $-1\r\n           null bulk
//!   $5\r\nhello\r\n    bulk string        *-1\r\n           null array
//!   *2\r\n<v><v>       array (nested ok)

use bytes::{Buf, BytesMut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Vec<u8>),
    NullBulk,
    Array(Vec<Value>),
    NullArray,
}

#[derive(Debug)]
pub struct ProtocolError(pub String);

/// Try to parse ONE complete value from the front of `buf`.
pub fn parse(buf: &mut BytesMut) -> Result<Option<Value>, ProtocolError> {
    let _ = buf.remaining();
    todo!()
}

/// Append the wire encoding of `v` to `out`.
pub fn encode(v: &Value, out: &mut BytesMut) {
    let (_, _) = (v, out);
    todo!()
}

/// Convenience for command dispatch: an Array of Bulks → Vec of args.
pub fn as_command(v: &Value) -> Option<Vec<&[u8]>> {
    match v {
        Value::Array(items) => items
            .iter()
            .map(|i| match i {
                Value::Bulk(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &[u8]) -> BytesMut {
        BytesMut::from(s)
    }

    #[test]
    fn parses_a_get_command() {
        let mut b = buf(b"*2\r\n$3\r\nGET\r\n$3\r\nfoo\r\n");
        let v = parse(&mut b).unwrap().unwrap();
        assert_eq!(
            v,
            Value::Array(vec![Value::Bulk(b"GET".to_vec()), Value::Bulk(b"foo".to_vec())])
        );
        assert!(b.is_empty(), "consumed exactly one command");
    }

    #[test]
    fn incomplete_input_returns_none_and_keeps_bytes() {
        // split mid-bulk: the \r\n and part of the payload are missing
        let mut b = buf(b"*2\r\n$3\r\nGET\r\n$3\r\nfo");
        let before = b.clone();
        assert!(parse(&mut b).unwrap().is_none());
        assert_eq!(b, before, "must not consume on incomplete input");
        // feed the rest — now it parses
        b.extend_from_slice(b"o\r\n");
        assert!(parse(&mut b).unwrap().is_some());
    }

    #[test]
    fn pipelined_commands_parse_one_at_a_time() {
        let mut b = buf(b"*1\r\n$4\r\nPING\r\n*1\r\n$4\r\nPING\r\n");
        assert!(parse(&mut b).unwrap().is_some());
        assert!(parse(&mut b).unwrap().is_some());
        assert!(parse(&mut b).unwrap().is_none());
        assert!(b.is_empty());
    }

    #[test]
    fn bulk_strings_are_binary_safe() {
        let mut b = buf(b"$8\r\nab\r\ncd\r\n\r\n");
        let v = parse(&mut b).unwrap().unwrap();
        assert_eq!(v, Value::Bulk(b"ab\r\ncd\r\n".to_vec()));
    }

    #[test]
    fn simple_error_integer_null() {
        let mut b = buf(b"+OK\r\n-ERR boom\r\n:42\r\n$-1\r\n*-1\r\n");
        assert_eq!(parse(&mut b).unwrap().unwrap(), Value::Simple("OK".into()));
        assert_eq!(parse(&mut b).unwrap().unwrap(), Value::Error("ERR boom".into()));
        assert_eq!(parse(&mut b).unwrap().unwrap(), Value::Integer(42));
        assert_eq!(parse(&mut b).unwrap().unwrap(), Value::NullBulk);
        assert_eq!(parse(&mut b).unwrap().unwrap(), Value::NullArray);
    }

    #[test]
    fn garbage_is_a_protocol_error() {
        let mut b = buf(b"?what\r\n");
        assert!(parse(&mut b).is_err());
    }

    #[test]
    fn encode_roundtrips() {
        let vals = vec![
            Value::Simple("OK".into()),
            Value::Error("ERR nope".into()),
            Value::Integer(-7),
            Value::Bulk(b"bin\r\nary".to_vec()),
            Value::NullBulk,
            Value::Array(vec![Value::Integer(1), Value::Bulk(b"x".to_vec())]),
            Value::NullArray,
        ];
        for v in vals {
            let mut wire = BytesMut::new();
            encode(&v, &mut wire);
            let parsed = parse(&mut wire).unwrap().unwrap();
            assert_eq!(parsed, v);
            assert!(wire.is_empty());
        }
    }

    #[test]
    fn nested_arrays() {
        let mut b = buf(b"*2\r\n*2\r\n:1\r\n:2\r\n$1\r\nz\r\n");
        let v = parse(&mut b).unwrap().unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Array(vec![Value::Integer(1), Value::Integer(2)]),
                Value::Bulk(b"z".to_vec()),
            ])
        );
    }
}
