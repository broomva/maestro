#!/usr/bin/env python3
"""check_control_readers.py: the ratchet on code that reads the legacy folder name alone.

The unified-layout ADR renames a repo's `.control` folder to `.broomva`. Until that rename every
reader must accept both names. This check lists the code files that do not, and compares the list
with the committed baseline (scripts/control_readers_baseline.json): a file may leave the list, no
file may join it. Ported from the workspace's sensor and ratchet (broomva/workspace,
scripts/control_readers_count.py and scripts/check_control_readers.py; BRO-3200, BRO-4083).

The unit is a file and an area: the first path component after the folder name (`asks` in
`.control/asks/private`). It answers one question per file: does this file handle both names for
every area it reads under the legacy name? It does not pair individual references.

  unpaired  some legacy area `<X>` has no new `<X>` in the same file: a reader that is not
            dual-keyed.
  paired    every legacy area is matched. The file is not listed.

A reader switched to the path registry names neither and is not listed.

Pairing is by SUFFIX, not by "the file mentions both names": in the workspace's review 24 of 25
files that mentioned both were single-keyed, because the second name was the machine folder under
the home directory, the loops folder or a launchd label. So:
  - a slash path and a join of quoted segments both normalise to the area after the folder name;
  - machine-scope mentions of the new name (under ~, $HOME or BROOMVA_HOME) are ignored, as is the
    loops folder under it, which is not a renamed path;
  - a bare legacy name (the directory itself) pairs with a bare new name.

Only code is counted (by extension), from the tracked files of the checkout. Prose, snapshots,
event logs, JSON and YAML fixtures and ledgers are not readers. A file under a directory named
exactly docs, research, archive, node_modules or either folder name is skipped; the names are
whole path components, so `crates/mydocs/x.rs` and `archived/x.rs` are counted. The workspace
sensor matches those names as substrings and skips such files, so this count can be higher than
the sensor's for the same tree. Folder references are whole names too: `my.control/x` is not one.

The baseline can only shrink. Given `--base-ref REF` (CI passes the pull request's base branch),
the check also reads the baseline at REF (`git show REF:scripts/control_readers_baseline.json`)
and fails when this checkout's baseline lists a path the base's does not. The one exception is a
rename: an added path passes when the same change removes a path from the baseline and git reports
the added file as a rename of the removed one (`git diff --name-status -M REF...HEAD`). Without
`--base-ref`, or when REF has no baseline yet, that comparison is skipped and a note says so.

What this check does not do. It is a ratchet against drift, not a defence against evasion:
  - It does not pair individual references, only areas within a file. A file that names
    `.broomva/asks/unrelated` may add `.control/asks/private` and is not listed, because both are
    in the `asks` area. This is the unit of the workspace's sensor, kept so that counts compare
    across the repos that run this ratchet. Review covers the paths inside an area.
  - It finds literal references only. A path assembled from pieces, such as
    `format!("{}{}", ".", "control/asks")` or a chain of `.join` calls on separate segments, is
    not found, and a textual scan cannot be made to find every one. Review covers those.
  - A comment counts. A code file whose comment names only the legacy folder is listed like a
    reader, as in the workspace's checker. Mention both names in the comment, or reword it.

Usage:
  python3 scripts/check_control_readers.py                     compare the checkout with the baseline
  python3 scripts/check_control_readers.py --base-ref REF      also compare the baseline with REF's
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
# A file is skipped when one of its DIRECTORY components equals one of these names. Whole
# components, compared exactly: `crates/mydocs/x.rs`, `archived/x.rs` and `my.control/x.py` are
# code. The workspace sensor tests these as substrings ("docs/" in path), which skips them, so this
# repo's count can be higher than that sensor's for the same tree.
SKIP_DIRS = frozenset({"docs", "research", "archive", LEGACY, CURRENT, "node_modules"})

# Files that name the legacy folder at all: the cheap filter before the classifier. It may match
# more than the patterns below (git grep -E has no lookbehind); the classifier decides.
MENTIONS = r"\.control([/\"'`]|$)"
# Not preceded by a name character: `my.control/x` and `self.control/2` are not the folder.
START = r"(?<![A-Za-z0-9_\-])"
# The folder name followed by `/suffix`, or a quoted folder name followed by further quoted
# segments (a join of segments), or a bare final segment.
SLASH = re.compile(START + r"\.(control|broomva)/([A-Za-z0-9_.\-]+)")
JOINED = re.compile(r"""["'`]\.(control|broomva)["'`]\s*,\s*["'`]([A-Za-z0-9_.\-]+)["'`]""")
BARE = re.compile(r"""[/"'`]\.(control|broomva)(?=["'`]|$|\s)""", re.M)
# Machine scope: these name the folder under the home directory, never a repo root. HOME and
# BROOMVA_HOME are whole names: `REPO_HOME/.broomva` and `my_home()` are not the home directory.
_HOME = r"(?<![A-Za-z0-9_])(?:BROOMVA_)?HOME"
MACHINE = re.compile(r"(~|" + _HOME + r"\}?|" + _HOME + r"""["']?\s*[,)]|(?<![A-Za-z0-9_])home\(\)"""
                     r"|(?<![A-Za-z0-9_])homedir\(\))\s*/?\s*\.broomva")
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
    *dirs, name = path.split("/")
    if any(part in SKIP_DIRS for part in dirs):
        return False
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
        print("The baseline only shrinks. Make each file read both names instead of listing it.")
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
        print("The unit is a file and an area (the first path component after the folder name): the check")
        print("asks whether the file handles both names for each area, and does not pair individual paths.")
        print(f"A file that names {CURRENT}/asks/unrelated may add {LEGACY}/asks/private and is not listed.")
        print(f"The check finds literal {LEGACY} references in code files, comments included: a comment")
        print("that names only the legacy folder counts, so mention both names or reword it. It does not")
        print("find a path assembled from pieces; review covers those.")
        print("With --base-ref, the baseline is also compared with the base's, except when the base has no")
        print("baseline file yet (the change that introduces it): that comparison is skipped with a note.")
        return 1
    if count < limit:
        print(f"Reader count fell to {count}; tighten the baseline. Remove:")
        for path in sorted(expected - current):
            print(f"  {path}")
    return grown


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
        ('read("@C/asks"); q = "my@B/asks"', "unpaired"),
        ('read("@C/asks"); q = "$REPO_HOME/@B/asks"', "paired"),
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
    parser = argparse.ArgumentParser(description="Ratchet the files that read the legacy folder name alone.")
    parser.add_argument("--root", type=Path, default=ROOT, help="the checkout to scan (default: this one)")
    parser.add_argument("--selftest", action="store_true", help="run the classifier's controls")
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
