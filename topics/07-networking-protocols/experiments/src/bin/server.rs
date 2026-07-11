//! A tokio RESP server speaking PING/GET/SET/DEL over your src/resp.rs.
//!
//! Runs once resp.rs is implemented:  cargo run --release --bin server
//! Bench:   redis-benchmark -p 7379 -t get,set -n 1000000 -P 1
//!          redis-benchmark -p 7379 -t get,set -n 1000000 -P 64
//! Compare against real redis on 6379; record both in notes.md, then
//! flamegraph this process under -P 64 and name the top three entries.
//!
//! Design notes to revisit while benching:
//! - one task per connection; the store is a sharded RwLock<HashMap> —
//!   contention appears only at high -P with few shards (try SHARDS=1).
//! - replies are written per-command through a BufWriter and flushed when
//!   the input buffer has no more complete commands: that's the
//!   handleClientsWithPendingWrites trick, tokio-style. Remove the
//!   "flush only when drained" logic and measure -P 64 again to feel it.

use bytes::BytesMut;
use networking_experiments::resp::{as_command, encode, parse, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::{TcpListener, TcpStream};

const SHARDS: usize = 16;

type Store = Arc<Vec<RwLock<HashMap<Vec<u8>, Vec<u8>>>>>;

fn shard(store: &Store, key: &[u8]) -> usize {
    let mut h = 0xcbf29ce484222325u64;
    for &b in key {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    (h as usize) % store.len()
}

fn execute(store: &Store, args: &[&[u8]]) -> Value {
    match args {
        [cmd, rest @ ..] => match cmd.to_ascii_uppercase().as_slice() {
            b"PING" => Value::Simple("PONG".into()),
            b"SET" => match rest {
                [k, v] => {
                    store[shard(store, k)].write().unwrap().insert(k.to_vec(), v.to_vec());
                    Value::Simple("OK".into())
                }
                _ => Value::Error("ERR wrong number of arguments for 'set'".into()),
            },
            b"GET" => match rest {
                [k] => match store[shard(store, k)].read().unwrap().get(*k) {
                    Some(v) => Value::Bulk(v.clone()),
                    None => Value::NullBulk,
                },
                _ => Value::Error("ERR wrong number of arguments for 'get'".into()),
            },
            b"DEL" => {
                let mut n = 0i64;
                for k in rest {
                    if store[shard(store, k)].write().unwrap().remove(*k).is_some() {
                        n += 1;
                    }
                }
                Value::Integer(n)
            }
            b"CONFIG" | b"COMMAND" => Value::Array(vec![]), // redis-benchmark handshake noise
            other => Value::Error(format!(
                "ERR unknown command '{}'",
                String::from_utf8_lossy(other)
            )),
        },
        [] => Value::Error("ERR empty command".into()),
    }
}

async fn handle(stream: TcpStream, store: Store) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let (mut rd, wr) = stream.into_split();
    let mut wr = BufWriter::new(wr);
    let mut inbuf = BytesMut::with_capacity(16 * 1024);
    let mut outbuf = BytesMut::with_capacity(16 * 1024);

    loop {
        // parse everything already buffered (pipelining), then flush once
        loop {
            match parse(&mut inbuf) {
                Ok(Some(v)) => {
                    let reply = match as_command(&v) {
                        Some(args) => execute(&store, &args),
                        None => Value::Error("ERR expected array of bulk strings".into()),
                    };
                    encode(&reply, &mut outbuf);
                }
                Ok(None) => break,
                Err(e) => {
                    let err = Value::Error(format!("ERR protocol error: {}", e.0));
                    encode(&err, &mut outbuf);
                    wr.write_all(&outbuf).await?;
                    wr.flush().await?;
                    return Ok(());
                }
            }
        }
        if !outbuf.is_empty() {
            wr.write_all(&outbuf).await?;
            wr.flush().await?;
            outbuf.clear();
        }
        if 0 == rd.read_buf(&mut inbuf).await? {
            return Ok(()); // client closed
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let store: Store = Arc::new((0..SHARDS).map(|_| RwLock::new(HashMap::new())).collect());
    let listener = TcpListener::bind("127.0.0.1:7379").await?;
    println!("listening on 127.0.0.1:7379 — try: redis-cli -p 7379 ping");
    loop {
        let (stream, _) = listener.accept().await?;
        let store = store.clone();
        tokio::spawn(async move {
            let _ = handle(stream, store).await;
        });
    }
}
