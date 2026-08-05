#!/usr/bin/env python3
"""Check `topics/*/reading-*.md` against CLAUDE.md's reading-guide depth rules.

Why this exists
---------------
The depth rules were written after studying one chapter and finding that its
failures — borrowed jargon, unstated data lineage, a snippet anchored forty lines
from the code it quoted — were properties of the format rather than of that
chapter. There are 230 chapters. Rules that are only enforced by care do not
survive that many files, so the mechanical part of each rule is enforced here.

What it checks (and what it deliberately does not)
--------------------------------------------------
Four of the eight rules have a checkable shape:

  spine       the section skeleton every guide shares
  step-io     each `### Step N` opens with `> **In:** … **Out:** …`
  done-when   `## Done when` is a checklist, introduced by "Answer each before
              unfolding it.", and every item carries a collapsed <details> answer
  snippets    a quoted code block carries the line numbers it occupies, or is
              labelled ILLUSTRATION

The other four — define every term at first use, work every formula on concrete
numbers, verify anchors against the pinned clone, describe what the code actually
does — are judgement, not syntax. `tools/pinned-source.py` is the instrument for
the anchor one; the rest are the writer's job and the reviewer's.

Usage
-----
    tools/check-reading-depth.py                  # report every failing guide
    tools/check-reading-depth.py --check          # exit 1 if a converted guide fails
    tools/check-reading-depth.py topics/03-*/     # only these
    tools/check-reading-depth.py --stats          # how far the rollout has got

`--check` is a ratchet, not a wall: it holds a guide to the rules once that guide
has started following them (it carries a step's In/Out blockquote or a collapsed
answer), and merely reports the ones the rollout has not reached. `--all` drops
the exemption, which is what the last commit of the rollout has to pass.
"""

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Info strings that mean "this block quotes source code". Blocks with no info
# string, or a non-source one, are ASCII diagrams, worked arithmetic, command
# output or mermaid — none of which have line numbers to carry.
SOURCE_LANGS = {
    "rust", "rs", "c", "cpp", "c++", "cc", "go", "python", "py", "java", "js",
    "javascript", "ts", "typescript", "zig", "cuda", "wgsl", "tla", "lean",
    "scala", "kotlin", "swift", "csharp", "cs", "ruby", "erlang", "elixir",
}

ANSWER_PROMPT = "Answer each before unfolding it."

# A source path with a line or line range: `mdb.c:1356`, `analysis/mod.rs:124–129`.
ANCHOR = re.compile(r"[\w./+-]+\.\w+:\d+(?:\s*[-–—]\s*\d+)?")
# A source file named without a line number, as the reference chapter's headers do
# it: `// routine.rs, inside Routine::sample`, the numbers being in the gutter.
NAMED_FILE = re.compile(
    r"[\w./+-]+\.(?:rs|c|cc|cpp|cxx|h|hpp|hh|hxx|go|py|java|js|ts|zig|cu|cuh|wgsl"
    r"|tla|lean|scala|kt|swift|cs|rb|erl|ex|sql|toml|proto|m|mm)\b"
)
GUTTER = re.compile(r"^\s*\d+\s")
COMMENT = re.compile(r"^\s*(//|#|--|/\*|\*|;)")


def blocks(lines):
    """Yield (start, end, info) for every fenced block, ends exclusive of the fence."""
    fence = None
    for i, line in enumerate(lines):
        m = re.match(r"^(\s*)(`{3,}|~{3,})\s*(\S*)", line)
        if not m:
            continue
        if fence is None:
            fence = (i, m.group(2)[0], len(m.group(2)), m.group(3).lower())
        elif m.group(2)[0] == fence[1] and len(m.group(2)) >= fence[2] and not m.group(3):
            yield fence[0], i, fence[3]
            fence = None
    if fence is not None:
        yield fence[0], len(lines), fence[3]


def mask(lines):
    """Line indices that sit inside a fenced block, fences included."""
    inside = set()
    for start, end, _ in blocks(lines):
        inside.update(range(start, end + 1))
    return inside


def check_spine(lines, inside, errs):
    if not lines or not lines[0].startswith("# "):
        errs.append("spine: no H1 on the first line")

    heads = [l for i, l in enumerate(lines) if l.startswith("## ") and i not in inside]
    text = "\n".join(heads)

    for required in ("## The problem in one sentence", "## The concepts, step by step",
                     "## Done when", "## References"):
        if required not in heads:
            errs.append(f"spine: missing `{required}`")

    if not re.search(r"^## (How to read|Where each step lives|Reading)", text, re.M):
        errs.append("spine: no `How to read …` / `Where each step lives …` section")
    if not re.search(r"^## Questions", text, re.M):
        errs.append("spine: no `## Questions …` section")

    order = [h for h in heads if h in ("## The problem in one sentence",
                                       "## The concepts, step by step",
                                       "## Done when", "## References")]
    if order != sorted(order, key=["## The problem in one sentence",
                                   "## The concepts, step by step",
                                   "## Done when", "## References"].index):
        errs.append(f"spine: sections out of order — {' → '.join(order)}")


def check_step_io(lines, inside, errs):
    steps = [i for i, l in enumerate(lines)
             if re.match(r"^### Step \d+", l) and i not in inside]
    if not steps:
        errs.append("step-io: no `### Step N` sections")
        return
    for i in steps:
        j = i + 1
        while j < len(lines) and not lines[j].strip():
            j += 1
        quote = []
        while j < len(lines) and lines[j].startswith(">"):
            quote.append(lines[j])
            j += 1
        joined = " ".join(quote)
        name = lines[i].strip()
        if not quote:
            errs.append(f"step-io: `{name}` has no `> **In:** … **Out:** …` blockquote")
        elif not quote[0].startswith("> **In:**"):
            errs.append(f"step-io: `{name}` blockquote does not open with `> **In:**`")
        elif "**Out:**" not in joined:
            errs.append(f"step-io: `{name}` blockquote never says `**Out:**`")


def check_done_when(lines, inside, errs):
    try:
        start = next(i for i, l in enumerate(lines)
                     if l.strip() == "## Done when" and i not in inside)
    except StopIteration:
        return  # the spine check already reported it
    end = next((i for i in range(start + 1, len(lines))
                if lines[i].startswith("## ") and i not in inside), len(lines))
    body = lines[start:end]
    body_inside = mask(body)

    items = [i for i, l in enumerate(body)
             if re.match(r"^\s*- \[[ xX]\] ", l) and i not in body_inside]
    if not items:
        errs.append("done-when: no `- [ ]` checklist items (prose is not a self-test)")
        return
    if ANSWER_PROMPT not in "\n".join(body):
        errs.append(f'done-when: missing the line "{ANSWER_PROMPT}"')

    for n, i in enumerate(items):
        stop = items[n + 1] if n + 1 < len(items) else len(body)
        chunk = "\n".join(body[i:stop])
        label = re.sub(r"^\s*- \[[ xX]\] ", "", body[i]).strip()
        label = (label[:60] + "…") if len(label) > 60 else label
        if "<details>" not in chunk:
            errs.append(f"done-when: no collapsed answer under “{label}”")
        elif "</details>" not in chunk:
            errs.append(f"done-when: unclosed <details> under “{label}”")
        elif "<summary>" not in chunk:
            errs.append(f"done-when: <details> without a <summary> under “{label}”")


def check_snippets(lines, errs):
    """A quoted block says where it came from, and shows the lines it occupies.

    The reference chapter's convention: a leading comment names the file (and
    usually the range and what was elided), and every quoted line carries its
    real line number in the gutter. Pseudocode says ILLUSTRATION instead, and
    points at the code it is illustrating.
    """
    for start, end, info in blocks(lines):
        if info not in SOURCE_LANGS:
            continue
        body = lines[start + 1:end]
        if not body:
            continue
        header = "\n".join(l for l in body[:4] if COMMENT.match(l))
        gutter = [l for l in body if GUTTER.match(l)]
        where = f"the ```{info} block at line {start + 1}"

        if "ILLUSTRATION" in header:
            if not ANCHOR.search("\n".join(body)):
                errs.append(
                    f"snippets: {where} is marked ILLUSTRATION but points at no real "
                    f"code — give the `file:line` the reader should read instead"
                )
            continue

        named = NAMED_FILE.search(header)
        if not named:
            errs.append(
                f"snippets: {where} has no header comment naming its source file — "
                f"anchor it, or mark it ILLUSTRATION"
            )
        elif not gutter:
            errs.append(
                f"snippets: {where} cites `{named.group(0)}` but carries no "
                f"line-number gutter, so no line in it can be found"
            )


def lint(path: Path) -> list[str]:
    lines = path.read_text("utf8").splitlines()
    inside = mask(lines)
    errs: list[str] = []
    check_spine(lines, inside, errs)
    check_step_io(lines, inside, errs)
    check_done_when(lines, inside, errs)
    check_snippets(lines, errs)
    return errs


def started(path: Path) -> bool:
    """Has this guide begun following the depth rules?

    The rollout converts 230 guides over many commits, so a check that demanded
    all of them at once would be red until the last one landed and would
    therefore be ignored. Instead the gate is a ratchet: a guide that shows any
    sign of the format — a step's In/Out blockquote, or a collapsed answer — is
    held to all of it. Converted work cannot regress, and unconverted work is
    reported without failing the build.
    """
    text = path.read_text("utf8")
    return "> **In:**" in text or "<details>" in text


def rel(p: Path) -> str:
    try:
        return str(p.resolve().relative_to(REPO))
    except ValueError:
        return str(p)


def guides(args) -> list[Path]:
    if not args:
        return sorted(REPO.glob("topics/*/reading-*.md"))
    out = []
    for a in args:
        p = Path(a)
        if p.is_dir():
            out += sorted(p.glob("reading-*.md"))
        elif p.is_file():
            out.append(p)
        else:
            out += sorted(REPO.glob(a))
    return sorted(set(out))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("Usage")[0].strip())
    ap.add_argument("paths", nargs="*", help="guides, topic directories or globs")
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if a guide that has started following the rules "
                         "does not follow all of them")
    ap.add_argument("--all", action="store_true",
                    help="with --check, hold every guide to the rules, converted or not")
    ap.add_argument("--stats", action="store_true", help="counts only, no detail")
    a = ap.parse_args()

    files = guides(a.paths)
    if not files:
        print("no reading guides matched", file=sys.stderr)
        return 2

    failed = 0        # guides in scope for --check that fail
    pending = 0       # not yet converted
    by_rule: dict[str, int] = {}
    for f in files:
        errs = lint(f)
        in_scope = a.all or started(f)
        for e in errs:
            by_rule[e.split(":")[0]] = by_rule.get(e.split(":")[0], 0) + 1
        if not errs:
            continue
        if in_scope:
            failed += 1
        else:
            pending += 1
        if not a.stats:
            tag = "" if in_scope else "  [not converted yet]"
            print(f"\n{rel(f)}{tag}")
            for e in errs:
                print(f"  {e}")

    passed = len(files) - failed - pending
    if a.stats and by_rule:
        for rule, n in sorted(by_rule.items(), key=lambda kv: -kv[1]):
            print(f"{n:6d}  {rule}")
    print(f"\n{passed}/{len(files)} guides meet the depth rules"
          f"{'' if failed or pending else ' — all of them'}")
    if pending:
        print(f"{pending} not converted yet (reported, not failed)")
    if failed:
        print(f"{failed} converted guide(s) do not meet the rules")
    return 1 if (a.check and failed) else 0


if __name__ == "__main__":
    sys.exit(main())
