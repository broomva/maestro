#!/usr/bin/env python3
"""check_control_readers.py: the ratchet on code that reads the legacy folder name alone.

The unified-layout ADR renames a repo's `.control` folder to `.broomva`. Until that rename every
reader must accept both names. This check lists the code files that do not, and compares the list
with the committed baseline (scripts/control_readers_baseline.json): a file may leave the list, no
file may join it. Ported from the workspace's sensor and ratchet (broomva/workspace,
scripts/control_readers_count.py and scripts/check_control_readers.py; BRO-3200, BRO-4083).

It answers one question per file: does every legacy reference have a matching new one?

  unpaired  some legacy `<X>` has no new `<X>` in the same file: a reader that is not dual-keyed.
  paired    every legacy `<X>` is matched. The file is not listed.

A reader switched to the path registry names neither and is not listed.

Pairing is by SUFFIX, not by "the file mentions both names": in the workspace's review 24 of 25
files that mentioned both were single-keyed, because the second name was the machine folder under
the home directory, the loops folder or a launchd label. So:
  - a slash path and a join of quoted segments both normalise to the suffix after the folder name;
  - machine-scope mentions of the new name (under ~, $HOME or BROOMVA_HOME) are ignored, as is the
    loops folder under it, which is not a renamed path;
  - a bare legacy name (the directory itself) pairs with a bare new name.

Only code is counted (by extension), from the tracked files of the checkout. The match is textual,
so a comment or a test that names the legacy folder counts as a reader. Prose, snapshots, event
logs, JSON and YAML fixtures and ledgers are not readers.

Usage:
  python3 scripts/check_control_readers.py                     compare the checkout with the baseline
  python3 scripts/check_control_readers.py --selftest          the classifier's controls
  python3 scripts/check_control_readers.py --print-baseline    the checkout's list, as a baseline
  --root DIR                                                   scan DIR in place of this checkout

Exit status: 0 no new reader, 1 a new reader, 2 the check could not run.
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

LEGACY = ".control"
CURRENT = ".broomva"

CODE_EXT = {".py", ".sh", ".bash", ".zsh", ".ts", ".tsx", ".js", ".mjs", ".cjs",
            ".rs", ".go", ".yml", ".toml", ".template"}
CODE_NAMES = {"Makefile", "pre-commit", "pre-push", "commit-msg"}
SKIP_PARTS = ("docs/", "research/", "archive/", LEGACY + "/", CURRENT + "/", "node_modules/")

# Files that name the legacy folder at all: the cheap filter before the classifier.
MENTIONS = r"\.control([/\"'`]|$)"
# The folder name followed by `/suffix`, or a quoted folder name followed by further quoted
# segments (a join of segments), or a bare final segment.
SLASH = re.compile(r"\.(control|broomva)/([A-Za-z0-9_.\-]+)")
JOINED = re.compile(r"""["'`]\.(control|broomva)["'`]\s*,\s*["'`]([A-Za-z0-9_.\-]+)["'`]""")
BARE = re.compile(r"""[/"'`]\.(control|broomva)(?=["'`]|$|\s)""", re.M)
# Machine scope: these name the folder under the home directory, never a repo root.
MACHINE = re.compile(r"""(~|HOME\}?|HOME["']?\s*[,)]|home\(\)|homedir\(\))\s*/?\s*\.broomva""")
NOT_RENAMED = {"loops"}


def refs(text: str) -> tuple[set[str], set[str]]:
    """Suffixes referenced under each folder name; "" stands for the bare directory."""
    control: set[str] = set()
    broomva: set[str] = set()
    machine_spans = [m.span() for m in MACHINE.finditer(text)]

    def is_machine(pos: int) -> bool:
        return any(a <= pos < b + 1 for a, b in machine_spans)

    for rx in (SLASH, JOINED):
        for m in rx.finditer(text):
            root, suffix = m.group(1), m.group(2)
            if root == "control":
                control.add(suffix)
            elif not is_machine(m.start()) and suffix not in NOT_RENAMED:
                broomva.add(suffix)
    # A quoted segment that a JOINED match already read with its suffix is not also a
    # bare-directory reference.
    joined_spans = [m.span() for m in JOINED.finditer(text)]
    for m in BARE.finditer(text):
        if any(a <= m.start() < b for a, b in joined_spans):
            continue
        root = m.group(1)
        if root == "control":
            control.add("")
        elif not is_machine(m.start()):
            broomva.add("")
    return control, broomva


def is_code(path: str) -> bool:
    if any(part in path for part in SKIP_PARTS) or path.startswith((LEGACY, CURRENT)):
        return False
    name = path.rsplit("/", 1)[-1]
    return name in CODE_NAMES or Path(name).suffix in CODE_EXT


def readers(root: Path) -> set[str]:
    """The tracked code files under `root` with a legacy reference that has no new twin."""
    result = subprocess.run(
        ["git", "-C", str(root), "grep", "-lzE", MENTIONS, "--"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if result.returncode not in (0, 1):
        raise RuntimeError(result.stderr.decode(errors="replace"))
    found = set()
    for raw_path in result.stdout.split(b"\0"):
        if not raw_path:
            continue
        path = raw_path.decode("utf-8", errors="surrogateescape")
        if not is_code(path):
            continue
        text = (root / path).read_text(encoding="utf-8", errors="replace")
        control, broomva = refs(text)
        if control and not control <= broomva:
            found.add(path)
    return found


def as_baseline(found: set[str]) -> str:
    return json.dumps({"count": len(found), "readers": sorted(found)}, indent=2) + "\n"


def check(root: Path) -> int:
    baseline = json.loads(BASELINE.read_text())
    expected = set(baseline["readers"])
    if len(expected) != baseline["count"]:
        raise ValueError("baseline count does not match its reader list")
    current = readers(root)
    count = len(current)
    limit = baseline["count"]
    print(f"Unpaired readers: {count}; baseline: {limit}")
    # The ratchet is on the set, not the number: a reader may leave, none may be added. Comparing
    # counts alone would let a new reader in whenever an old one was removed in the same change.
    new = sorted(current - expected)
    if new:
        print("New readers since baseline:")
        for path in new:
            print(f"  {path}")
        print(f"Each file above names the legacy {LEGACY} folder without the matching {CURRENT} name.")
        print("Make it read both names, or read through the path registry. Do not add it to the baseline.")
        return 1
    if count < limit:
        print(f"Reader count fell to {count}; tighten the baseline. Remove:")
        for path in sorted(expected - current):
            print(f"  {path}")
    return 0


def controls() -> tuple[tuple[str, str], ...]:
    """(text, expected). The paired rows are a positive control: an empty list means nothing
    unless the classifier can say "paired" at all."""
    rows = (
        ('read("@C/asks"); read("@B/asks")', "paired"),
        ('p = join(r, "@C", "asks"); q = "@B/asks/x"', "paired"),
        ('[ -d "$W/@C" ] || [ -d "$W/@B" ]', "paired"),
        ('let a = root.join("@C/asks"); let b = root.join("@B/asks");', "paired"),
        ('read("@C/asks"); home = "~/@B/asks"', "unpaired"),
        ('read("@C/policy.yaml"); loops = "@B/loops"', "unpaired"),
        ('ASKS = "@C/asks"; DEFAULT = HOME + "/@B"', "unpaired"),
        ('read("@C/asks"); read("@B/initiatives")', "unpaired"),
        ('let asks = ws.join("@C/asks");', "unpaired"),
    )
    return tuple((text.replace("@C", LEGACY).replace("@B", CURRENT), want) for text, want in rows)


def selftest() -> int:
    rows = controls()
    bad = 0
    for text, want in rows:
        control, broomva = refs(text)
        got = "paired" if control and control <= broomva else "unpaired"
        if got != want:
            print(f"[FAIL] {text!r}: {got}, wanted {want}")
            bad += 1
    for path, want in (("crates/store/src/lib.rs", True), ("scripts/ci.sh", True),
                       ("docs/adrs/layout.rs", False), ("fixtures/machine/asks.yaml", False),
                       ("README.md", False)):
        if is_code(path) != want:
            print(f"[FAIL] is_code({path!r}) is {not want}, wanted {want}")
            bad += 1
    total = len(rows) + 5
    print(f"selftest: {total - bad}/{total}")
    return 1 if bad else 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Ratchet the files that read the legacy folder name alone.")
    parser.add_argument("--root", type=Path, default=ROOT, help="the checkout to scan (default: this one)")
    parser.add_argument("--selftest", action="store_true", help="run the classifier's controls")
    parser.add_argument("--print-baseline", action="store_true", help="print the checkout's list as a baseline")
    args = parser.parse_args()
    if args.selftest:
        return selftest()
    if args.print_baseline:
        sys.stdout.write(as_baseline(readers(args.root)))
        return 0
    return check(args.root)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, RuntimeError) as exc:
        print(f"Reader check failed: {exc}", file=sys.stderr)
        sys.exit(2)
