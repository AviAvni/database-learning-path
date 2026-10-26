#!/usr/bin/env python3
"""Run topic 46's k-hop path pattern on the C FalkorDB engine, over the same seeded graph.

pgq_bench measures a flat hash join, a CSR expansion and a factorized count on
`power_law(seed 46, avg out-degree 8, 70% preferential)`. This script loads that
exact edge list (from `pgq_edges`) into FalkorDB and times

    MATCH (a)-[:E]->(b)-[:E]->(c)[-[:E]->(d)] RETURN count(*)

so the C engine's row can sit next to the Rust crate's. It uses plain redis-py
(`GRAPH.QUERY`), not the FalkorDB client, and reads the server's own
"Query internal execution time" from the reply statistics so the number excludes
the client and the socket.

    FALKORDB_MODULE=~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so \
    DUMP=/path/to/target/release/pgq_edges \
    python3 cengine.py 10000 20000        # |V| list; add --explain to print plans

Exits 0 with a `[skip — ...]` note when the module or redis-server is absent, so a
verify run on a fresh machine does not fail.
"""
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

PORT = int(os.environ.get("CENGINE_PORT", "16479"))
MODULE = os.environ.get("FALKORDB_MODULE", os.path.expanduser("~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so"))
DUMP = os.environ.get("DUMP", "pgq_edges")
REPS = int(os.environ.get("REPS", "3"))
BATCH = 5000


def skip(why):
    print(f"[skip — {why}]")
    sys.exit(0)


def main():
    sizes = [int(a) for a in sys.argv[1:] if a.isdigit()] or [10000]
    explain = "--explain" in sys.argv
    if not os.path.exists(MODULE):
        skip(f"FalkorDB module not found at {MODULE}; set FALKORDB_MODULE")
    if shutil.which("redis-server") is None:
        skip("redis-server not on PATH")
    try:
        import redis
    except ImportError:
        skip("python module `redis` not installed (pip install redis)")
    dump = shutil.which(DUMP) or (DUMP if os.path.exists(DUMP) else None)
    if dump is None:
        skip("pgq_edges binary not found; cargo build --release --bin pgq_edges and set DUMP")

    tmp = tempfile.mkdtemp(prefix="cengine-")
    srv = subprocess.Popen(
        ["redis-server", "--port", str(PORT), "--save", "", "--appendonly", "no", "--dir", tmp,
         "--loadmodule", MODULE],
        stdout=open(os.path.join(tmp, "server.log"), "w"), stderr=subprocess.STDOUT)
    try:
        r = redis.Redis(port=PORT, decode_responses=True, socket_timeout=1800)
        for _ in range(100):
            try:
                r.ping()
                break
            except redis.exceptions.ConnectionError:
                time.sleep(0.1)
        else:
            skip("redis-server did not come up with the module (see %s/server.log)" % tmp)
        print("server:", r.info("server")["redis_version"], "| module:", [m["name"] + " v" + str(m["ver"]) for m in r.module_list()])
        print("loadavg:", os.getloadavg())
        for n in sizes:
            run_size(r, dump, n, explain)
    finally:
        srv.send_signal(signal.SIGTERM)
        try:
            srv.wait(timeout=10)
        except subprocess.TimeoutExpired:
            srv.kill()
        shutil.rmtree(tmp, ignore_errors=True)


def internal_ms(reply):
    for line in reply[-1]:
        m = re.match(r"Query internal execution time: ([0-9.]+) milliseconds", line)
        if m:
            return float(m.group(1))
    raise RuntimeError("no execution-time statistic in reply")


def run_size(r, dump, n, explain):
    g = f"g{n}"
    edges = subprocess.run([dump, str(n), "70"], capture_output=True, text=True, check=True).stdout.split()
    pairs = [(int(edges[i]), int(edges[i + 1])) for i in range(0, len(edges), 2)]
    r.execute_command("GRAPH.QUERY", g, "CREATE INDEX FOR (v:V) ON (v.id)")
    t0 = time.time()
    for lo in range(0, n, BATCH):
        ids = ",".join(str(i) for i in range(lo, min(n, lo + BATCH)))
        r.execute_command("GRAPH.QUERY", g, f"UNWIND [{ids}] AS i CREATE (:V {{id: i}})")
    for lo in range(0, len(pairs), BATCH):
        rows = ",".join(f"[{a},{b}]" for a, b in pairs[lo:lo + BATCH])
        r.execute_command("GRAPH.QUERY", g,
                          f"UNWIND [{rows}] AS e MATCH (a:V {{id: e[0]}}), (b:V {{id: e[1]}}) CREATE (a)-[:E]->(b)")
    load_s = time.time() - t0
    nv = r.execute_command("GRAPH.QUERY", g, "MATCH (v:V) RETURN count(v)")[1][0][0]
    ne = r.execute_command("GRAPH.QUERY", g, "MATCH ()-[e:E]->() RETURN count(e)")[1][0][0]
    print(f"\n|V| = {n}: loaded {nv} vertices, {ne} edges in {load_s:.1f} s (bench says {len(pairs)} edges)")
    assert nv == n and ne == len(pairs)
    for hops in (2, 3):
        pat = "(a)" + "".join(f"-[:E]->(v{i})" for i in range(1, hops + 1))
        mids = " + ".join(f"0*id(v{i})" for i in range(1, hops))
        forms = [
            # nothing but `a` and the last vertex is used, so the planner folds the whole chain into ONE
            # algebraic expression (a)->(v_last) and counts distinct endpoint pairs
            ("endpoints only", f"MATCH {pat} RETURN count(*)"),
            # every intermediate vertex is touched, so the chain stays a chain of traversals and the
            # count is the number of paths (what pgq_bench counts)
            ("every hop used", f"MATCH {pat} RETURN sum(1 + {mids})"),
        ]
        for label, query in forms:
            if explain:
                print("  plan:", query)
                for line in r.execute_command("GRAPH.EXPLAIN", g, query):
                    print("   ", line)
            best, count = float("inf"), None
            for _ in range(REPS):
                reply = r.execute_command("GRAPH.QUERY", g, query)
                count = int(reply[1][0][0])
                best = min(best, internal_ms(reply))
            print(f"  {hops}-hop {label:<15}: count {count:>14,}  server time best of {REPS}: {best:10.1f} ms  ({best * 1e6 / count:.2f} ns per counted row)")


if __name__ == "__main__":
    main()
