#!/usr/bin/env python3
"""A C FalkorDB datapoint for topic 51: the same seeded graph, bulk-loaded,
then two-hop queries and edge inserts against a real delta matrix.

Setup (from this crate's directory; port 6390 so it never meets a real server):

    cargo run --release --bin dump_edges -- /tmp/t51 200 20000
    redis-server --port 6390 --save '' --appendonly no \
        --loadmodule ~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so &
    python3 scripts/falkordb_datapoint.py load    /tmp/t51 --port 6390 \
        --loader ~/repos/falkordb-bulk-loader
    python3 scripts/falkordb_datapoint.py measure /tmp/t51 --port 6390
    redis-cli -p 6390 shutdown nosave

`measure` prints the server's own "Query internal execution time" (what the
engine spent, without the socket) next to the client's wall clock. Neither is
comparable to the in-process ns/edge of dyngraph_bench: a Cypher query pays
parsing, planning, a result set and a round trip on top of the scan. What the
datapoint shows is the *shape* — what the delta matrix does to the same
queries as pending changes accumulate and then flush.
"""
import argparse
import csv
import os
import re
import subprocess
import sys
import time

import redis

GRAPH = "t51"
INTERNAL = re.compile(rb"internal execution time: ([0-9.]+) milliseconds")


def rows(path):
    with open(path) as f:
        r = csv.reader(f)
        next(r)
        for row in r:
            yield [int(x) for x in row]


def query(r, q):
    t = time.perf_counter()
    res = r.execute_command("GRAPH.QUERY", GRAPH, q)
    wall = (time.perf_counter() - t) * 1e3
    stats = b" ".join(x if isinstance(x, bytes) else str(x).encode() for x in res[-1])
    m = INTERNAL.search(stats)
    return res, wall, float(m.group(1)) if m else float("nan")


def load(a):
    r = redis.Redis(port=a.port)
    r.delete(GRAPH)
    env = dict(os.environ, PYTHONPATH=os.path.expanduser(a.loader))
    cmd = [sys.executable, "-m", "falkordb_bulk_loader.bulk_insert", GRAPH, "-u", f"redis://127.0.0.1:{a.port}",
           "-N", "V", os.path.join(a.dir, "V.csv"), "-R", "E", os.path.join(a.dir, "E.csv")]
    t = time.time()
    subprocess.run(cmd, check=True, env=env)
    print(f"bulk load: {time.time() - t:.1f} s")
    res, _, _ = query(r, "MATCH (n) RETURN count(n)")
    print("nodes:", res[1][0][0])
    res, _, _ = query(r, "MATCH ()-[e:E]->() RETURN count(e)")
    print("edges:", res[1][0][0])


def two_hop_batch(r, starts, label):
    wall = internal = 0.0
    touched = 0
    bad = 0
    first_lat = None
    for i, (s, first, second, distinct) in enumerate(starts):
        res, w, ms = query(r, f"MATCH (a)-[:E]->(b)-[:E]->(c) WHERE ID(a) = {s} RETURN count(c)")
        if i == 0:
            first_lat = w
        got = res[1][0][0] if res[1] else 0
        # After inserts the answer legitimately moves; only the pre-write batch is checked exactly.
        if label.startswith("empty") and got != distinct:
            bad += 1
        wall += w
        internal += ms
        touched += first + second
    n = len(starts)
    print(f"{label:<34} {n:>4} queries  wall {wall / n * 1e3:>8.0f} us/query  engine {internal / n * 1e3:>8.0f} us/query  "
          f"(first query {first_lat * 1e3:.0f} us)  answers {'match the Rust distinct-vertex counts' if label.startswith('empty') and not bad else ('MISMATCH x%d' % bad if bad else 'n/a')}")
    return touched


def inserts(r, pairs, label):
    lat = []
    for u, v in pairs:
        t = time.perf_counter()
        r.execute_command("GRAPH.QUERY", GRAPH,
                          f"MATCH (a), (b) WHERE ID(a) = {u} AND ID(b) = {v} MERGE (a)-[:E]->(b)")
        lat.append((time.perf_counter() - t) * 1e6)
    srt = sorted(lat)
    n = len(lat)
    worst = max(range(n), key=lambda i: lat[i])
    print(f"{label:<34} {n:>4} inserts  mean {sum(lat) / n:>7.0f} us  median {srt[n // 2]:>6.0f} us  "
          f"p99 {srt[int(n * 0.99)]:>7.0f} us  max {srt[-1]:>9.0f} us (insert #{worst})  (wall, end to end)")


def measure(a):
    r = redis.Redis(port=a.port)
    starts = list(rows(os.path.join(a.dir, "starts.csv")))
    ins = list(rows(os.path.join(a.dir, "inserts.csv")))
    assert len(ins) >= 20000, "dump_edges with 20000 inserts"
    cfg = r.execute_command("GRAPH.CONFIG", "GET", "DELTA_MAX_PENDING_CHANGES")
    print("DELTA_MAX_PENDING_CHANGES:", cfg)
    # one warm-up pass so the first number is not the cold one
    two_hop_batch(r, starts[:20], "warm-up")
    two_hop_batch(r, starts, "empty delta (after bulk load)")
    # 2,500 inserts at a time, so the per-insert latency can be seen to climb
    # with the pending count and fall back after the flush; a query batch at
    # the marked points. Candidate inserts are MERGEd, so a few already exist
    # and "pending" runs slightly behind "inserted".
    marks = {2: "5,000 inserted (< threshold)", 4: "10,000 inserted (at the threshold)",
             5: "12,500 inserted (flush crossed)", 8: "20,000 inserted"}
    for b in range(8):
        inserts(r, ins[2500 * b:2500 * (b + 1)], f"inserts {2500 * b:>5} -> {2500 * (b + 1):>5}")
        if b + 1 in marks:
            two_hop_batch(r, starts, marks[b + 1])


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("phase", choices=["load", "measure"])
    ap.add_argument("dir")
    ap.add_argument("--port", type=int, default=6390)
    ap.add_argument("--loader", default="~/repos/falkordb-bulk-loader")
    a = ap.parse_args()
    {"load": load, "measure": measure}[a.phase](a)
