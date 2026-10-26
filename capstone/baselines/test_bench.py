"""Reference baselines — the workload of the reference's old tests/test_bench.py.

The queries, the graph name ("test") and the per-benchmark setup are copied from
falkordb-rs-next-gen tests/test_bench.py as it stood before FalkorDB/FalkorDB
commit 3e7690dc1 removed it (`git show 3e7690dc1^:tests/test_bench.py`).
Two things differ, both to make a p99 meaningful:

- the redis-server is started here from $FALKORDB_MODULE, so the same file drives
  the C engine and the Rust engine;
- the pedantic (create/delete) benches run $BENCH_ROUNDS rounds (default 30)
  instead of 5. Every round still starts from a fresh graph.

Scales are the subset BASELINES.md has always reported: n in {1000, 100000}.
"""

import os
import subprocess
import time

import pytest
from falkordb import FalkorDB
from redis import Redis

PORT = int(os.environ.get("BENCH_PORT", "6390"))
MODULE = os.environ["FALKORDB_MODULE"]
REDIS = os.environ.get("REDIS_SERVER", "/usr/local/bin/redis-server")
LOGDIR = os.environ.get("BENCH_LOGDIR", ".")
ROUNDS = int(os.environ.get("BENCH_ROUNDS", "30"))
SCALES = [1000, 100000]

g = None
client = None


@pytest.fixture(scope="module", autouse=True)
def server():
    global g, client
    proc = subprocess.Popen(
        [REDIS, "--save", "", "--appendonly", "no", "--port", str(PORT),
         "--logfile", os.path.join(LOGDIR, "redis-bench.log"),
         "--loadmodule", MODULE],
        stdout=subprocess.DEVNULL)
    r = Redis(port=PORT)
    for _ in range(200):
        try:
            r.ping()
            break
        except Exception:
            time.sleep(0.05)
    client = FalkorDB(port=PORT)
    g = client.select_graph("test")
    yield
    try:
        r.shutdown(nosave=True)
    except Exception:
        pass
    proc.wait(timeout=30)


@pytest.fixture(autouse=True)
def clean_graph():
    reset_graph()


def run_query(q, params=None):
    g.query(q, params)


def reset_graph():
    if g.name in client.list_graphs():
        g.delete()


def test_return(benchmark):
    benchmark(run_query, "RETURN 1")


@pytest.mark.parametrize("n", SCALES)
def test_unwind(benchmark, n):
    benchmark(run_query, f"UNWIND range(1, {n}) AS x RETURN x")


@pytest.mark.parametrize("n", SCALES)
def test_create_node(benchmark, n):
    benchmark.pedantic(run_query,
                       args=(f"UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})",),
                       setup=reset_graph, rounds=ROUNDS, warmup_rounds=1)


@pytest.mark.parametrize("n", SCALES)
def test_create_relationship(benchmark, n):
    benchmark.pedantic(run_query,
                       args=(f"UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})-[:R]->(:N {{id: x + 1}})",),
                       setup=reset_graph, rounds=ROUNDS, warmup_rounds=1)


@pytest.mark.parametrize("n", SCALES)
def test_match_node(benchmark, n):
    run_query(f"UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})")
    benchmark(run_query, "MATCH (n:N) RETURN n")


@pytest.mark.parametrize("n", SCALES)
def test_match_relationship(benchmark, n):
    run_query(f"UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})-[:R]->(:N {{id: x + 1}})")
    benchmark(run_query, "MATCH (n)-[r:R]->(m) RETURN n, r, m")


@pytest.mark.parametrize("n", SCALES)
def test_delete_node(benchmark, n):
    def setup():
        reset_graph()
        run_query(f"UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})")
    benchmark.pedantic(run_query, args=("MATCH (n:N) DELETE n",),
                       setup=setup, rounds=ROUNDS, warmup_rounds=1)


@pytest.mark.parametrize("n", SCALES)
def test_delete_relationship(benchmark, n):
    def setup():
        reset_graph()
        run_query(f"UNWIND range(1, {n}) AS x CREATE (:N {{id: x}})-[:R]->(:N {{id: x + 1}})")
    benchmark.pedantic(run_query, args=("MATCH ()-[r:R]->() DELETE r",),
                       setup=setup, rounds=ROUNDS, warmup_rounds=1)
