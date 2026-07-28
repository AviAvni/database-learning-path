//! Lane 1 (PROVIDED): what a request actually costs when the work is zero.
//!
//!   cargo run --release --bin loopback_bench
//!
//! This is the measurement the whole topic rests on, and it deliberately
//! contains no protocol parsing at all — the frame is a fixed 32 bytes in,
//! 8 bytes out, so nothing here depends on your `src/resp.rs`. What is left
//! when you remove the parser and the store is the part nobody budgets for:
//! the syscalls, the wakeups, and the round trip.
//!
//! The knob is pipeline depth P — how many requests the client puts on the
//! wire before it reads any reply. At P=1 every request pays a full
//! write→wake→read→write→wake→read round trip. At P=64 that same cost is
//! amortized over 64 requests, which is why `redis-benchmark -P 64` prints
//! numbers that look like a different database from `-P 1`.
//!
//! Predict in notes.md BEFORE running:
//!   - the P=1 → P=64 throughput ratio (redis's own docs claim ~10x)
//!   - whether per-request latency gets better or worse as P grows, and why
//!   - where the curve stops improving, and what has become the bottleneck
//!
//! Then run `server.rs` under `redis-benchmark -P 1` and `-P 64` and check
//! that the shape of this curve survives a real protocol on top of it.

use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const REQ: usize = 32;
const REP: usize = 8;
const OPS: usize = 200_000;
const DEPTHS: [usize; 6] = [1, 2, 8, 32, 64, 256];

/// The server side: read whatever arrived, reply once per complete frame,
/// flush once per read. This is the "flush only when drained" trick from
/// redis's handleClientsWithPendingWrites, which is the entire reason
/// pipelining pays off — see the design notes in server.rs.
async fn serve(mut stream: TcpStream) {
    stream.set_nodelay(true).unwrap();
    let mut inbuf = vec![0u8; 1 << 16];
    let mut filled = 0usize;
    let mut outbuf = Vec::with_capacity(1 << 16);
    loop {
        let n = match stream.read(&mut inbuf[filled..]).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        filled += n;
        let frames = filled / REQ;
        if frames > 0 {
            outbuf.clear();
            for _ in 0..frames {
                outbuf.extend_from_slice(&[b'+'; REP]);
            }
            if stream.write_all(&outbuf).await.is_err() {
                return;
            }
            let consumed = frames * REQ;
            inbuf.copy_within(consumed..filled, 0);
            filled -= consumed;
        }
    }
}

/// The client side: keep `depth` requests in flight, then drain their
/// replies. Returns (ops/sec, mean per-request latency in µs).
async fn client(addr: std::net::SocketAddr, depth: usize) -> (f64, f64) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.set_nodelay(true).unwrap();
    let req = vec![b'x'; REQ * depth];
    let mut rep = vec![0u8; REP * depth];
    let batches = OPS / depth;

    // warm the connection so the first batch's page faults are not in the number
    stream.write_all(&req[..REQ]).await.unwrap();
    stream.read_exact(&mut rep[..REP]).await.unwrap();

    let start = Instant::now();
    for _ in 0..batches {
        stream.write_all(&req).await.unwrap();
        stream.read_exact(&mut rep).await.unwrap();
    }
    let secs = start.elapsed().as_secs_f64();
    let ops = (batches * depth) as f64;
    // per-request latency as the client experiences it: a batch's round trip
    // divided over the requests that shared it
    (ops / secs, secs / ops * 1e6)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream));
        }
    });

    println!("{OPS} ops per depth, {REQ} B request / {REP} B reply, loopback TCP,");
    println!("TCP_NODELAY on, one connection, no protocol parsing and no store.\n");
    println!(
        "{:>5} {:>14} {:>16} {:>18} {:>12}",
        "P", "ops/s", "µs per request", "syscalls per op", "vs P=1"
    );

    let mut base = 0.0;
    for depth in DEPTHS {
        let (ops, us) = client(addr, depth).await;
        if base == 0.0 {
            base = ops;
        }
        println!(
            "{depth:>5} {ops:>14.0} {us:>16.2} {:>18.3} {:>11.1}x",
            2.0 / depth as f64,
            ops / base
        );
    }

    println!("\nnotes:");
    println!("- 'syscalls per op' is the floor 2/P (one write + one read per batch);");
    println!("  the throughput column should track it until something else binds");
    println!("- the µs column is per REQUEST, not per batch: pipelining improves");
    println!("  throughput and per-request latency at the same time, which is why");
    println!("  a client-side batch is not the same trade as a server-side one");
    println!("- record the P=1 and P=64 rows in notes.md, then compare them to");
    println!("  redis-benchmark against real redis and against server.rs");
    Ok(())
}
