#!/usr/bin/env python3
"""mdbook preprocessor: point links at `README.md` to the page mdbook renders.

Why this exists
---------------
mdbook renders a chapter named README.md as index.html, but it rewrites an
inline link `[x](topics/12-columnar-analytics/README.md)` to `.../README.html`,
a page that does not exist. The same link is correct on GitHub, which is where
most of these files are also read, so the source is right and the book is
wrong. 85 links were dead in the published book when this was written, 43 of
them in FINDINGS.md alone, and CI could not see them because it only checks
that every SUMMARY.md entry produced a page.

This rewrites relative `README.md` link targets to `index.md` in the book's
copy of each chapter only; the files on disk are untouched. mdbook then turns
`index.md` into `index.html` like any other link.

Wired in through book.toml `[preprocessor.readme-links]`.
"""

import json
import re
import sys

# `](path/README.md)`, `](README.md#anchor)`, `](../README.md "title")` — but
# never an absolute URL, whose README.md is someone else's page.
LINK = re.compile(r"(\]\()(?![a-z][a-z0-9+.-]*:)([^)\s]*?)README\.md(?=[#)\s])")


def rewrite(node):
    if isinstance(node, dict):
        if isinstance(node.get("content"), str):
            node["content"] = LINK.sub(r"\1\2index.md", node["content"])
        for v in node.values():
            rewrite(v)
    elif isinstance(node, list):
        for v in node:
            rewrite(v)


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "supports":
        return 0  # renderer-independent: html and pdf both want working links
    _context, book = json.load(sys.stdin)
    rewrite(book)
    json.dump(book, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
