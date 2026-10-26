#!/usr/bin/env python3
"""Fail if any internal link in the rendered book is dead.

Why this exists
---------------
CLAUDE.md asks that internal links resolve, and the book workflow checked only
half of that: every SUMMARY.md entry produced a page. A link *inside* a page
to another page, or to a heading anchor, could rot silently — 85 of them had,
all pointing at `README.html` pages mdbook never writes (see
tools/mdbook-readme-links.py). Markdown cannot tell you a link is dead; the
rendered HTML can, so this reads the HTML.

Usage
-----
    mdbook build && python3 tools/check-book-links.py book/html

Checks every relative href in each page's <main> — the page exists, and if it
carries a #fragment, an element with that id exists on the target page.
print.html is skipped (it concatenates every page and rewrites their links),
as is any stale build nested inside the output by `src = "."`.
"""

import html
import os
import re
import sys
from urllib.parse import unquote

EXTERNAL = re.compile(r"^(?:[a-z][a-z0-9+.-]*:|//|#$)", re.I)


def main() -> int:
    root = sys.argv[1] if len(sys.argv) > 1 else "book/html"
    if not os.path.isfile(os.path.join(root, "index.html")):
        print(f"no rendered book at {root}", file=sys.stderr)
        return 2

    ids_cache: dict[str, set] = {}

    def ids(path: str) -> set:
        if path not in ids_cache:
            with open(path, encoding="utf-8") as f:
                ids_cache[path] = set(re.findall(r'\bid="([^"]+)"', f.read()))
        return ids_cache[path]

    dead, checked = [], 0
    for dirpath, dirnames, files in os.walk(root):
        rel = os.path.relpath(dirpath, root)
        if rel.split(os.sep)[0] == "book":
            dirnames[:] = []
            continue
        for name in files:
            if not name.endswith(".html") or name in ("print.html", "404.html", "toc.html"):
                continue
            page = os.path.join(dirpath, name)
            with open(page, encoding="utf-8") as f:
                text = f.read()
            m = re.search(r"<main>(.*)</main>", text, re.S)
            for href in re.findall(r'href="([^"]+)"', m.group(1) if m else ""):
                href = html.unescape(href)
                if EXTERNAL.match(href):
                    continue
                checked += 1
                path, _, frag = href.partition("#")
                target = os.path.normpath(os.path.join(dirpath, unquote(path))) if path else page
                if os.path.isdir(target):
                    target = os.path.join(target, "index.html")
                where = os.path.relpath(page, root)
                if not os.path.exists(target):
                    dead.append(f"{where}: {href} (no such page)")
                elif frag and target.endswith(".html") and unquote(frag) not in ids(target):
                    dead.append(f"{where}: {href} (no such anchor)")

    print(f"checked {checked} internal links, {len(dead)} dead")
    for d in dead:
        print(f"::error::{d}" if os.environ.get("GITHUB_ACTIONS") else f"  {d}")
    return 1 if dead else 0


if __name__ == "__main__":
    sys.exit(main())
