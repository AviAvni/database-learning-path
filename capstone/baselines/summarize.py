"""Median and p99 per benchmark from pytest-benchmark JSON saved with --benchmark-save-data.

p99 is nearest-rank over the raw per-round times (stats.data): the ceil(0.99*N)-th
smallest sample. Every bench here runs with iterations == 1, so a sample is one query;
the script refuses otherwise rather than reporting a per-iteration mean as a tail.

usage: python summarize.py bench-A.json [bench-B.json ...]
"""

import json
import math
import statistics
import sys


def fmt(s):
    if s >= 1:
        return f"{s:.2f} s"
    if s >= 1e-3:
        return f"{s * 1e3:.3g} ms" if s < 0.1 else f"{s * 1e3:.0f} ms"
    return f"{s * 1e6:.0f} µs"


def p99(xs):
    xs = sorted(xs)
    return xs[math.ceil(0.99 * len(xs)) - 1]


for path in sys.argv[1:]:
    d = json.load(open(path))
    print(f"## {path}  ({d['machine_info'].get('cpu', {}).get('brand_raw', '?')}, {d['datetime']})")
    print("| bench | N | median | p99 | max |")
    print("|---|--:|--:|--:|--:|")
    for b in d["benchmarks"]:
        st = b["stats"]
        assert st["iterations"] == 1, (b["name"], st["iterations"])
        xs = st["data"]
        print(f"| {b['name']} | {len(xs)} | {fmt(statistics.median(xs))} | {fmt(p99(xs))} | {fmt(max(xs))} |")
    print()
