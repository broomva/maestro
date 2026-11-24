#!/usr/bin/env python3
"""check_control_readers.py: the ratchet on code that mentions the legacy folder name.

The contract, in two sentences. The ratchet lists every code file that mentions the legacy folder
name (a dot, then "control", the folder the unified-layout ADR renames) at all; a pull request may
not add a file to that list. New code spells neither folder name and resolves paths through the
generated path reader; existing mentions are grandfathered in the committed baseline
(scripts/control_readers_baseline.json), and the list only shrinks.

The generated path reader in this repository is `maestro_store::broomva_paths`
(crates/store/src/broomva_paths.rs). It is generated in bstack by scripts/gen_paths.py and vendored
here byte for byte by scripts/vendor-paths.sh; do not edit it. It is the one place meant to keep
naming both folders, and its mentions are in the baseline like any other file's.

A mention is the one pattern MENTION below, used for every file; there is no second definition. A
code file with at least one mention is listed. Nothing is paired: a file that also names the new
folder is listed all the same. This count is of mentioning files. It is not the workspace sensor's
"unpaired" count (broomva/workspace, scripts/control_readers_count.py), and the two do not compare.

Only code is counted (by extension or file name), from the tracked files of the checkout. A file
under a directory named exactly docs, research, archive, node_modules or either state folder is
skipped; the names are whole path components, so crates/mydocs/x.rs and archived/x.rs are code.

The baseline can only shrink. Given `--base-ref REF` (CI passes the pull request's base branch),
the check also reads the baseline at REF (`git show REF:scripts/control_readers_baseline.json`)
and fails when this checkout's baseline lists a path the base's does not. The one exception is a
rename: an added path passes when the same change removes a path from the baseline and git reports
the added file as a rename of the removed one (`git diff --name-status -M REF...HEAD`). Without
`--base-ref`, or when REF has no baseline yet (the change that introduces it), that comparison is
skipped and a note says so.

What this check does not do. It is a ratchet against drift, not a defence against evasion:
  - A mention assembled from pieces, such as `format!("{}{}", ".", "control/asks")`, is not found.
  - A comment counts: a code file whose comment names the legacy folder is listed. Reword it.

Usage:
  python3 scripts/check_control_readers.py                     compare the checkout with the baseline
  python3 scripts/check_control_readers.py --base-ref REF      also compare the baseline with REF's
  python3 scripts/check_control_readers.py --selftest          the pattern's and the filter's controls
  python3 scripts/check_control_readers.py --print-baseline    the checkout's list, as a baseline
  --root DIR                                                   scan DIR in place of this checkout

Exit status: 0 no new file, 1 a new file, 2 the check could not run.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = Path(__file__).resolve().parent / "control_readers_baseline.json"

# Spelled in pieces so that this file does not mention the folder itself.
LEGACY = "." + "control"
CURRENT = "." + "broomva"

CODE_EXT = {".py", ".sh", ".bash", ".zsh", ".ts", ".tsx", ".js", ".mjs", ".cjs",
            ".rs", ".go", ".yml", ".toml", ".template"}
CODE_NAMES = {"Makefile", "pre-commit", "pre-push", "commit-msg"}
# A file is skipped when one of its DIRECTORY components equals one of these names. Whole
# components, compared exactly: crates/mydocs/x.rs and archived/x.rs are code.
SKIP_DIRS = frozenset({"docs", "research", "archive", LEGACY, CURRENT, "node_modules"})

# MENTION, the one pattern. The dot and "control", where
#   left:  the text starts there, or the character before the dot is NOT a letter, digit, `_`,
#          `)` or `]`. Those make it a member access (`self` + dot + control, `x)` + dot +
#          control), not a path. Any other character, including `/`, a quote, whitespace, `=`,
#          `(`, `,` and another `.`, may come before a path.
#   right: the next character is `/`, a single or double quote, a backtick, or the end of the line:
#          the folder as a path component, not a longer name such as "controller".
MENTION = re.compile(r"(?<![A-Za-z0-9_)\]])" + re.escape(LEGACY) + r"""(?=[/"'`]|$)""", re.M)


def mentions(text: str) -> bool:
    return MENTION.search(text) is not None


def is_code(path: str) -> bool:
    *dirs, name = path.split("/")
    if any(part in SKIP_DIRS for part in dirs):
        return False
    return name in CODE_NAMES or Path(name).suffix in CODE_EXT


def readers(root: Path) -> set[str]:
    """The tracked code files under `root` that mention the legacy folder."""
    result = subprocess.run(["git", "-C", str(root), "ls-files", "-z"],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.decode(errors="replace"))
    found = set()
    for raw_path in result.stdout.split(b"\0"):
        if not raw_path:
            continue
        path = raw_path.decode("utf-8", errors="surrogateescape")
        target = root / path
        if not is_code(path) or not target.is_file():
            continue
        if mentions(target.read_text(encoding="utf-8", errors="replace")):
            found.add(path)
    return found


def as_baseline(found: set[str]) -> str:
    return json.dumps({"count": len(found), "readers": sorted(found)}, indent=2) + "\n"


def load_baseline(text: str) -> set[str]:
    baseline = json.loads(text)
    listed = set(baseline["readers"])
    if len(listed) != baseline["count"]:
        raise ValueError("baseline count does not match its reader list")
    return listed


def git(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(root), *args],
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def renames(root: Path, base_ref: str) -> set[tuple[str, str]]:
    """(old, new) for every file git reports as renamed between the merge base and HEAD."""
    result = git(root, "diff", "--name-status", "-M", f"{base_ref}...HEAD")
    if result.returncode != 0:
        raise RuntimeError(f"git diff against {base_ref} failed: {result.stderr.strip()}")
    pairs = set()
    for line in result.stdout.splitlines():
        fields = line.split("\t")
        if fields[0].startswith("R") and len(fields) == 3:
            pairs.add((fields[1], fields[2]))
    return pairs


def compare_with_base(root: Path, listed: set[str], base_ref: str) -> int:
    """Fail when this checkout's baseline lists a path the base's baseline does not, unless the
    path is a git rename of one the same change removed."""
    if git(root, "rev-parse", "--verify", "--quiet", f"{base_ref}^{{commit}}").returncode != 0:
        raise RuntimeError(f"base ref {base_ref} is not a commit here; fetch it first")
    rel = f"scripts/{BASELINE.name}"
    shown = git(root, "show", f"{base_ref}:{rel}")
    if shown.returncode != 0:
        print(f"Note: {base_ref} has no {rel}; the baseline comparison is skipped.")
        return 0
    base = load_baseline(shown.stdout)
    added = sorted(listed - base)
    removed = base - listed
    if not added:
        print(f"Baseline: no path added since {base_ref}.")
        return 0
    moved = renames(root, base_ref)
    refused = []
    for path in added:
        source = next((old for old, new in moved if new == path and old in removed), None)
        if source is None:
            refused.append(path)
        else:
            removed.discard(source)
            print(f"Baseline: {path} replaces {source}, a git rename.")
    if refused:
        print(f"Paths added to the baseline since {base_ref}:")
        for path in refused:
            print(f"  {path}")
        print("The baseline only shrinks. Remove each file's mention of the legacy folder instead of listing it.")
        print("(This comparison is skipped only when the base has no baseline file yet.)")
        return 1
    return 0


def check(root: Path, base_ref: str | None = None) -> int:
    expected = load_baseline(BASELINE.read_text())
    if base_ref is None:
        print("Note: no --base-ref; the baseline comparison is skipped.")
        grown = 0
    else:
        grown = compare_with_base(root, expected, base_ref)
    current = readers(root)
    count = len(current)
    limit = len(expected)
    print(f"Files mentioning {LEGACY}: {count}; baseline: {limit}")
    # The ratchet is on the set, not the number: a file may leave, none may be added. Comparing
    # counts alone would let a new file in whenever an old one was removed in the same change.
    new = sorted(current - expected)
    if new:
        print("New files since baseline:")
        for path in new:
            print(f"  {path}")
        print(f"Each file above mentions the legacy {LEGACY} folder. New code spells neither folder name:")
        print("resolve the path through the generated path reader, maestro_store::broomva_paths")
        print("(crates/store/src/broomva_paths.rs, vendored by scripts/vendor-paths.sh). Do not add the file")
        print("to the baseline. A file that also names the new folder is listed all the same, and a comment")
        print("counts, so reword it. A mention assembled from pieces is not found; review covers those.")
        print("With --base-ref, the baseline is also compared with the base's, except when the base has no")
        print("baseline file yet (the change that introduces it): that comparison is skipped with a note.")
        return 1
    if count < limit:
        print(f"Count fell to {count}; tighten the baseline. Remove:")
        for path in sorted(expected - current):
            print(f"  {path}")
    return grown


def controls() -> tuple[tuple[str, bool], ...]:
    """(text, is a mention). Both answers occur, so neither can pass by a dead pattern."""
    rows = (
        ('read("@C/asks")', True),
        ('[ -d "$W/@C" ]', True),
        ("# reads @C/asks", True),
        ('a = "@C/asks"; b = "my.@B/asks"', True),
        ('a = "@C/asks"; b = "@B/asks"', True),
        ("self@C/asks", False),
        ("x)@C/asks", False),
        ('"@Cler/x"', False),
        ("the @C folder", False),
    )
    return tuple((text.replace("@C", LEGACY).replace("@B", CURRENT), want) for text, want in rows)


def selftest() -> int:
    rows = controls()
    bad = 0
    for text, want in rows:
        if mentions(text) != want:
            print(f"[FAIL] {text!r}: mention is {not want}, wanted {want}")
            bad += 1
    paths = (("crates/store/src/lib.rs", True), ("scripts/ci.sh", True),
             ("docs/adrs/layout.rs", False), ("fixtures/machine/asks.yaml", False),
             ("README.md", False), ("crates/mydocs/src/probe.rs", True),
             ("archived/x.rs", True), ("a/research/x.py", False))
    for path, want in paths:
        if is_code(path) != want:
            print(f"[FAIL] is_code({path!r}) is {not want}, wanted {want}")
            bad += 1
    total = len(rows) + len(paths)
    print(f"selftest: {total - bad}/{total}")
    return 1 if bad else 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Ratchet the code files that mention the legacy folder name.")
    parser.add_argument("--root", type=Path, default=ROOT, help="the checkout to scan (default: this one)")
    parser.add_argument("--selftest", action="store_true", help="run the pattern's and the filter's controls")
    parser.add_argument("--print-baseline", action="store_true", help="print the checkout's list as a baseline")
    parser.add_argument("--base-ref", metavar="REF",
                        help="also fail when the baseline lists a path REF's baseline does not")
    args = parser.parse_args()
    if args.selftest:
        return selftest()
    if args.print_baseline:
        sys.stdout.write(as_baseline(readers(args.root)))
        return 0
    return check(args.root, args.base_ref)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, RuntimeError) as exc:
        print(f"Reader check failed: {exc}", file=sys.stderr)
        sys.exit(2)
