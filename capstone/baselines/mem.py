"""Memory for the BASELINES.md graphs, per engine.

For each (shape, n) this starts a fresh redis-server under `/usr/bin/time -l`,
loads the graph with the bench's own CREATE query, records INFO memory (and
GRAPH.MEMORY USAGE if the engine answers it), runs the bench's MATCH query once,
records INFO memory again, shuts down, and takes the process's peak RSS
("maximum resident set size") from time -l.

usage: python mem.py <module.so|.dylib> <label> <out.json>
"""

import json
import os
import re
import subprocess
import sys
import time

from falkordb import FalkorDB
from redis import Redis

MODULE, LABEL, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
REDIS = os.environ.get("REDIS_SERVER", "/usr/local/bin/redis-server")
PORT = int(os.environ.get("BENCH_PORT", "6391"))

SHAPES = {
    "empty": (None, None),
    "nodes": ("UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})", "MATCH (n:N) RETURN n"),
    "rels": ("UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})-[:R]->(:N {{id: x + 1}})",
             "MATCH (n)-[r:R]->(m) RETURN n, r, m"),
}
CASES = [("empty", 0)] + [(s, n) for s in ("nodes", "rels") for n in (100_000, 1_000_000)]

KEYS = ("used_memory", "used_memory_rss", "used_memory_peak")


def info(r):
    m = r.info("memory")
    return {k: m[k] for k in KEYS}


def one(shape, n):
    proc = subprocess.Popen(
        ["/usr/bin/time", "-l", REDIS, "--save", "", "--appendonly", "no",
         "--port", str(PORT), "--logfile", "/dev/null", "--loadmodule", MODULE],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    r = Redis(port=PORT)
    for _ in range(200):
        try:
            r.ping()
            break
        except Exception:
            time.sleep(0.05)
    row = {"engine": LABEL, "shape": shape, "n": n, "idle": info(r)}
    create, match = SHAPES[shape]
    if create:
        g = FalkorDB(port=PORT).select_graph("test")
        g.query(create.format(n=n))
        time.sleep(1)  # let any deferred work (e.g. matrix sync) settle
        row["after_load"] = info(r)
        try:
            row["graph_memory_usage_mb"] = r.execute_command("GRAPH.MEMORY", "USAGE", "test")
        except Exception as e:
            row["graph_memory_usage_mb"] = f"unsupported: {e}"
        rows = len(g.query(match).result_set)
        row["match_rows"] = rows
        row["after_match"] = info(r)
    r.shutdown(nosave=True)
    _, err = proc.communicate(timeout=60)
    m = re.search(r"(\d+)\s+maximum resident set size", err)
    row["peak_rss_bytes"] = int(m.group(1)) if m else None
    return row


def decode(x):
    if isinstance(x, bytes):
        return x.decode()
    if isinstance(x, list):
        return [decode(i) for i in x]
    if isinstance(x, dict):
        return {decode(k): decode(v) for k, v in x.items()}
    return x


results = []
for shape, n in CASES:
    row = one(shape, n)
    if "graph_memory_usage_mb" in row:
        row["graph_memory_usage_mb"] = decode(row["graph_memory_usage_mb"])
    print(json.dumps(row), flush=True)
    results.append(row)
with open(OUT, "w") as f:
    json.dump(results, f, indent=1)
