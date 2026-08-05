#!/usr/bin/env python3
"""Read a source file at the exact commit a reading guide was written against.

Why this exists
---------------
CLAUDE.md's reading-guide depth rules say every `file:line` anchor is verified
against the pinned clone, file *and* line, and that a quoted snippet carries the
line numbers it actually occupies. Both are only checkable if the writer can open
the pinned revision — and `tools/pin-table.py` records exactly which revision that
is, once, for every clone the guides reference.

This tool opens that revision. It prefers a real clone under ~/repos when one is
present (same source `pin-table.py` reads), and otherwise fetches the file from
raw.githubusercontent.com at the pinned SHA into a gitignored cache. Both paths
answer the same question, because the SHA is the same.

Usage
-----
    tools/pinned-source.py ref lmdb
    tools/pinned-source.py list lmdb --glob '*.c'
    tools/pinned-source.py show lmdb libraries/liblmdb/mdb.c -r 1350:1365
    tools/pinned-source.py grep lmdb 'mdb_env_pick_meta'
    tools/pinned-source.py check lmdb mdb.c:1356 --contains 'MDB_meta'

`<repo>` is a clone name from the pin table (`lmdb`), an `owner/name` pair
(`bheisler/criterion.rs`), or a GitHub URL. Paths may be given in full or by any
unique suffix — `mdb.c` resolves to `libraries/liblmdb/mdb.c`.

The revision is the pin table's SHA. Repos that are not in the pin table (a crate
read from the cargo registry, say) need `--ref`, and the guide must state which
version its line numbers belong to.
"""

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PIN_TABLE = REPO / "resources" / "codebases.md"
CLONES = Path(os.environ.get("DLP_CLONES", Path.home() / "repos"))
CACHE = Path(os.environ.get("DLP_CACHE", REPO / ".cache" / "pinned-sources"))
RAW = "https://raw.githubusercontent.com/{owner}/{name}/{ref}/{path}"
TREE_API = "repos/{owner}/{name}/git/trees/{ref}?recursive=1"


class Fail(Exception):
    """Anything the user can fix by passing different arguments."""


# ---------------------------------------------------------------- pin table


def pins() -> dict:
    """clone name -> {"sha": ..., "owner": ..., "name": ...} from the pin table."""
    if not PIN_TABLE.exists():
        raise Fail(f"no pin table at {PIN_TABLE}")
    out = {}
    row = re.compile(
        r"^\|\s*`([^`]+)`\s*\|\s*`([0-9a-f]+)`\s*\|[^|]*\|[^|]*\|\s*\[[^\]]*\]"
        r"\((https://github\.com/([^/]+)/([^)/]+))\)\s*\|",
        re.M,
    )
    for name, sha, _url, owner, repo in row.findall(PIN_TABLE.read_text("utf8")):
        out[name] = {"sha": sha, "owner": owner, "name": repo}
    return out


def resolve(repo: str, ref: str | None) -> tuple[str, str, str, str | None]:
    """(owner, name, ref, clone_dir_or_None) for a pin-table name, owner/name or URL."""
    table = pins()
    key = repo.rstrip("/")
    m = re.match(r"(?:https?://github\.com/)?([^/]+)/([^/]+?)(?:\.git)?$", key)

    if key in table:
        p = table[key]
        owner, name, pinned, clone = p["owner"], p["name"], p["sha"], key
    elif m:
        owner, name = m.group(1), m.group(2)
        hit = [k for k, v in table.items() if v["owner"] == owner and v["name"] == name]
        clone = hit[0] if hit else None
        pinned = table[clone]["sha"] if clone else None
    else:
        raise Fail(
            f"cannot resolve {repo!r}: not in the pin table, and not owner/name.\n"
            f"pin table has: {', '.join(sorted(table))}"
        )

    ref = ref or pinned
    if not ref:
        raise Fail(
            f"{owner}/{name} is not in the pin table, so there is no recorded "
            f"revision — pass --ref (a tag or SHA), and say in the guide which "
            f"version its line numbers belong to."
        )

    local = CLONES / clone if clone else None
    if local and (local / ".git").exists():
        return owner, name, ref, str(local)
    return owner, name, ref, None


# ------------------------------------------------------------------ fetching


def _git(clone: str, *args: str) -> str:
    r = subprocess.run(
        ["git", "-C", clone, *args], capture_output=True, text=True, errors="replace"
    )
    if r.returncode:
        raise Fail(r.stderr.strip() or f"git {' '.join(args)} failed in {clone}")
    return r.stdout


def _cache_path(owner: str, name: str, ref: str, path: str) -> Path:
    return CACHE / f"{owner}__{name}__{ref}" / path


def tree(owner: str, name: str, ref: str, clone: str | None) -> list[str]:
    """Every file path at the pinned revision."""
    if clone:
        return _git(clone, "ls-tree", "-r", "--name-only", ref).splitlines()

    cached = _cache_path(owner, name, ref, ".tree.json")
    if cached.exists():
        return json.loads(cached.read_text("utf8"))

    r = subprocess.run(
        ["gh", "api", TREE_API.format(owner=owner, name=name, ref=ref)],
        capture_output=True,
        text=True,
    )
    if r.returncode:
        raise Fail(
            f"could not list {owner}/{name}@{ref}: {r.stderr.strip()[:300]}\n"
            f"(a shortened SHA sometimes needs the full 40-character one here)"
        )
    data = json.loads(r.stdout)
    paths = [e["path"] for e in data.get("tree", []) if e["type"] == "blob"]
    if data.get("truncated"):
        print(
            f"warning: {owner}/{name}@{ref} tree is truncated by the API; "
            f"path-suffix resolution may miss files",
            file=sys.stderr,
        )
    cached.parent.mkdir(parents=True, exist_ok=True)
    cached.write_text(json.dumps(paths), "utf8")
    return paths


def full_path(owner: str, name: str, ref: str, clone: str | None, path: str) -> str:
    """Resolve a path given in full or by suffix; a tie is an error, not a guess."""
    paths = tree(owner, name, ref, clone)
    if path in paths:
        return path
    hits = [p for p in paths if p == path or p.endswith("/" + path)]
    if len(hits) == 1:
        return hits[0]
    if not hits:
        raise Fail(f"no file matching {path!r} in {owner}/{name}@{ref}")
    raise Fail(
        f"{path!r} is ambiguous in {owner}/{name}@{ref} — "
        f"{len(hits)} matches:\n  " + "\n  ".join(sorted(hits)[:12])
    )


def source(owner: str, name: str, ref: str, clone: str | None, path: str) -> str:
    if clone:
        return _git(clone, "show", f"{ref}:{path}")

    cached = _cache_path(owner, name, ref, path)
    if cached.exists():
        return cached.read_text("utf8", errors="replace")

    url = RAW.format(owner=owner, name=name, ref=ref, path=path)
    try:
        with urllib.request.urlopen(url, timeout=60) as r:
            body = r.read().decode("utf8", errors="replace")
    except urllib.error.HTTPError as e:
        raise Fail(f"{e.code} fetching {url}") from None
    cached.parent.mkdir(parents=True, exist_ok=True)
    cached.write_text(body, "utf8")
    return body


# ------------------------------------------------------------------ commands


def cmd_ref(a) -> int:
    owner, name, ref, clone = resolve(a.repo, a.ref)
    where = f"clone {clone}" if clone else "raw.githubusercontent (cached)"
    print(f"{owner}/{name}@{ref}  via {where}")
    return 0


def cmd_list(a) -> int:
    owner, name, ref, clone = resolve(a.repo, a.ref)
    paths = tree(owner, name, ref, clone)
    if a.glob:
        paths = [p for p in paths if fnmatch.fnmatch(p, a.glob)]
    for p in sorted(paths)[: a.limit]:
        print(p)
    return 0


def cmd_show(a) -> int:
    owner, name, ref, clone = resolve(a.repo, a.ref)
    path = full_path(owner, name, ref, clone, a.path)
    lines = source(owner, name, ref, clone, path).splitlines()
    lo, hi = 1, len(lines)
    if a.range:
        m = re.match(r"^(\d+)(?:[:\-–](\d+))?$", a.range)
        if not m:
            raise Fail(f"--range wants N or A:B, got {a.range!r}")
        lo = int(m.group(1))
        hi = int(m.group(2) or m.group(1))
    print(f"# {owner}/{name}@{ref} — {path} ({len(lines)} lines)")
    for i in range(max(1, lo), min(hi, len(lines)) + 1):
        print(f"{i:6d}  {lines[i - 1]}")
    return 0


def cmd_grep(a) -> int:
    owner, name, ref, clone = resolve(a.repo, a.ref)
    pat = re.compile(a.pattern)
    paths = tree(owner, name, ref, clone)
    if a.path:
        paths = [p for p in paths if p == a.path or p.endswith("/" + a.path)]
    elif a.glob:
        paths = [p for p in paths if fnmatch.fnmatch(p, a.glob)]
    else:
        raise Fail("grep needs --path or --glob; a whole-repo grep would fetch every file")
    shown = 0
    for p in sorted(paths):
        for i, line in enumerate(source(owner, name, ref, clone, p).splitlines(), 1):
            if pat.search(line):
                print(f"{p}:{i}: {line}")
                shown += 1
                if shown >= a.limit:
                    return 0
    return 0 if shown else 1


def cmd_check(a) -> int:
    owner, name, ref, clone = resolve(a.repo, a.ref)
    m = re.match(r"^(.*):(\d+)(?:[-–](\d+))?$", a.anchor)
    if not m:
        raise Fail(f"anchor wants path:LINE or path:A-B, got {a.anchor!r}")
    path = full_path(owner, name, ref, clone, m.group(1))
    lo, hi = int(m.group(2)), int(m.group(3) or m.group(2))
    lines = source(owner, name, ref, clone, path).splitlines()
    if hi > len(lines):
        print(f"FAIL {a.anchor}: {path} has {len(lines)} lines at {ref}")
        return 1
    body = "\n".join(lines[lo - 1 : hi])
    if a.contains and a.contains not in body:
        print(f"FAIL {a.anchor}: {path}:{lo}-{hi} does not contain {a.contains!r}")
        for i in range(lo, hi + 1):
            print(f"  {i:6d}  {lines[i - 1]}")
        return 1
    print(f"ok   {owner}/{name}@{ref} {path}:{lo}" + (f"-{hi}" if hi != lo else ""))
    for i in range(lo, hi + 1):
        print(f"  {i:6d}  {lines[i - 1]}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("Usage")[0].strip())
    ap.add_argument("--ref", help="tag or SHA, when the repo is not in the pin table")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("ref", help="print the revision that would be read")
    p.add_argument("repo")
    p.set_defaults(fn=cmd_ref)

    p = sub.add_parser("list", help="list files at the pinned revision")
    p.add_argument("repo")
    p.add_argument("--glob")
    p.add_argument("--limit", type=int, default=200)
    p.set_defaults(fn=cmd_list)

    p = sub.add_parser("show", help="print a file with its real line numbers")
    p.add_argument("repo")
    p.add_argument("path")
    p.add_argument("-r", "--range", help="N or A:B")
    p.set_defaults(fn=cmd_show)

    p = sub.add_parser("grep", help="search files at the pinned revision")
    p.add_argument("repo")
    p.add_argument("pattern")
    p.add_argument("--path", help="one file, given in full or by suffix")
    p.add_argument("--glob", help="e.g. 'src/**/*.rs'")
    p.add_argument("--limit", type=int, default=40)
    p.set_defaults(fn=cmd_grep)

    p = sub.add_parser("check", help="assert an anchor still says what the guide claims")
    p.add_argument("repo")
    p.add_argument("anchor", help="path:LINE or path:A-B")
    p.add_argument("--contains", help="text the anchored lines must contain")
    p.set_defaults(fn=cmd_check)

    a = ap.parse_args()
    try:
        return a.fn(a)
    except Fail as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
