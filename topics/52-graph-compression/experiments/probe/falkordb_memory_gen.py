"""Writes d1/ (200,000 nodes, 1,000,000 random edges) and d4/ (the same nodes,
4,000,000 edges) as bulk-loader CSVs next to this script. Seeded: the files
are identical on every run. Used to measure FalkorDB's own bytes per edge:

    redis-server --port 6399 --loadmodule <FalkorDB>/bin/<arch>/falkordb.so --save ""
    python3 -m falkordb_bulk_loader.bulk_insert g1 -u redis://localhost:6399 -d -n d1/P.csv -r d1/R.csv
    python3 -m falkordb_bulk_loader.bulk_insert g4 -u redis://localhost:6399 -d -n d4/P.csv -r d4/R.csv
    redis-cli -p 6399 GRAPH.MEMORY USAGE g1      # and g4; the slope is bytes/edge

(falkordb-bulk-loader from github.com/FalkorDB/falkordb-bulk-loader.)
"""
import os
import random
random.seed(52)
n = 200000
s = set()
while len(s) < 4000000:
    a = random.randrange(n)
    b = random.randrange(n)
    if a != b:
        s.add((a, b))
L = sorted(s)
random.Random(1).shuffle(L)
base = os.path.dirname(os.path.abspath(__file__))
for m, d in [(1000000, 'd1'), (4000000, 'd4')]:
    os.makedirs(os.path.join(base, d), exist_ok=True)
    with open(os.path.join(base, d, 'P.csv'), 'w') as f:
        f.write(":ID\n")
        for i in range(n):
            f.write(f"{i}\n")
    with open(os.path.join(base, d, 'R.csv'), 'w') as f:
        f.write(":START_ID,:END_ID\n")
        for a, b in sorted(L[:m]):
            f.write(f"{a},{b}\n")
