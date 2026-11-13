#!/usr/bin/env python3
"""Maestro's governance contract, run by the `stability-check` job (BRO-3702).

    python3 -I scripts/governance-check.py [ROOT]     # check the tree at ROOT (default: this checkout)
    python3 -I scripts/governance-check.py --selftest # seed one break per check in a copy: each must fail

Exit 0 when every check holds, 1 when one does not, 2 on usage. A file that cannot be read, or a
phrase that cannot be found, is a failure, never a pass. Standard library only (tomllib: Python
3.11+), because the job runs the runner's own python3, with no setup-python.
"""
import os
import re
import shutil
import sys
import tempfile
import tomllib
from pathlib import Path

# The workspace policy's governance tier (broomva/workspace .control/policy.yaml,
# auto_merge.gates.governance.paths). p9 needs a passing `stability-check` on a PR touching any of
# them. The ones present in this repository must trigger the job and be counted by the rate gate.
GOVERNANCE_PATHS = (
    "CLAUDE.md",
    "AGENTS.md",
    "METALAYER.md",
    ".control/policy.yaml",
    ".control/rcs-parameters.toml",
    "docs/references/agent-operating-protocols.md",
    "docs/references/current-work.md",
    "docs/references/workspace-context.md",
)
WORKFLOW = ".github/workflows/l3-stability.yml"
CI = ".github/workflows/ci.yml"
PARAMETERS = ".control/rcs-parameters.toml"
SELF = "scripts/governance-check.py"
# Besides the governance paths, a change to any input of a check must run the job.
CONTRACT_INPUTS = (WORKFLOW, CI, SELF)

UNITS = ("zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen "
         "fifteen sixteen seventeen eighteen nineteen").split()
TENS = {"twenty": 20, "thirty": 30, "forty": 40, "fifty": 50,
        "sixty": 60, "seventy": 70, "eighty": 80, "ninety": 90}


class Broken(Exception):
    pass


def read(root, rel):
    try:
        return (root / rel).read_text(encoding="utf-8")
    except OSError as e:
        raise Broken(f"cannot read {rel}: {e.strerror or e}")


def number(text):
    """A count written as digits or English words below one hundred ("sixteen", "twenty-four")."""
    t = text.strip().lower()
    if re.fullmatch(r"[0-9]+", t):
        return int(t)
    words = re.split(r"[\s-]+", t)
    if len(words) == 1 and words[0] in UNITS:
        return UNITS.index(words[0])
    if len(words) == 1 and words[0] in TENS:
        return TENS[words[0]]
    if len(words) == 2 and words[0] in TENS and words[1] in UNITS[1:10]:
        return TENS[words[0]] + UNITS.index(words[1])
    raise Broken(f"not a count: {text!r}")


def yaml_list(lines, i, indent):
    """The items of the YAML list whose key is on line i: flow style ([a, b]) or block style (- a)."""
    value = lines[i].split(":", 1)[1].split(" #", 1)[0].strip()
    if value.startswith("["):
        flow, j = value, i
        while "]" not in flow:
            j += 1
            if j == len(lines):
                raise Broken("unterminated flow list")
            flow += " " + lines[j].split(" #", 1)[0].strip()
        inner = flow[1:flow.index("]")]
        return [x.strip() for x in inner.split(",") if x.strip()]
    if value:
        raise Broken(f"not a list: {value!r}")
    items = []
    for line in lines[i + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indent:
            break
        m = re.match(r"\s*-\s*(.*?)\s*(?:#.*)?$", line)
        if not m:
            raise Broken(f"not a list item: {line.strip()!r}")
        items.append(m[1].strip("'\""))
    return items


def check_claude_pointer(root):
    """CLAUDE.md is a pointer file (not a copy, not a symlink) that sends agents to AGENTS.md, and
    every repository path it names exists."""
    if (root / "CLAUDE.md").is_symlink():
        raise Broken("CLAUDE.md is a symlink; it is a pointer file that names AGENTS.md")
    text = read(root, "CLAUDE.md")
    named = re.findall(r"`([^`\s]+)`", text)
    if "AGENTS.md" not in named:
        raise Broken("CLAUDE.md does not point to `AGENTS.md`")
    paths = [p for p in named if "/" in p or re.search(r"\.(md|html|ya?ml|toml|sh|py|rs)$", p)]
    missing = [p for p in paths if not (root / p).is_file()]
    if missing:
        raise Broken("CLAUDE.md names paths that do not exist: " + ", ".join(missing))
    return "points to " + ", ".join(paths)


def check_shard_count(root):
    """The number of `mutants` jobs AGENTS.md names is the length of ci.yml's mutants matrix."""
    found = re.findall(r"\bafter\s+(\S+(?:[ -]\S+)??)\s+`mutants`\s+jobs\b", read(root, "AGENTS.md"))
    if len(found) != 1:
        raise Broken(f"AGENTS.md names the `mutants` job count {len(found)} times, not once "
                     "(\"after <count> `mutants` jobs\")")
    named = number(found[0])
    lines = read(root, CI).splitlines()
    keys = [i for i, line in enumerate(lines) if re.match(r"\s*shard\s*:", line)]
    if len(keys) != 1:
        raise Broken(f"{CI} has {len(keys)} `shard:` matrix keys, not one")
    i = keys[0]
    try:
        shards = yaml_list(lines, i, len(lines[i]) - len(lines[i].lstrip()))
    except Broken as e:
        raise Broken(f"{CI} shard matrix: {e}")
    if named != len(shards):
        raise Broken(f"AGENTS.md says {found[0]} ({named}) `mutants` jobs, "
                     f"but the {CI} mutants matrix has {len(shards)} shards")
    return f"AGENTS.md and {CI} both run {named} shards"


def check_triggers(root):
    """Every governance path in this repository, and every input of these checks, runs the job."""
    head = read(root, WORKFLOW).split("\njobs:", 1)[0]
    triggers = set(re.findall(r"^\s+-\s+'([^']+)'\s*$", head, re.M))
    want = [p for p in GOVERNANCE_PATHS if (root / p).exists()] + list(CONTRACT_INPUTS)
    missing = [p for p in want if p not in triggers]
    if missing:
        raise Broken(f"{WORKFLOW} does not run on: " + ", ".join(missing))
    return f"{len(want)} paths run the job"


def check_rate_paths(root):
    """The L3 rate gate counts every governance path in this repository."""
    try:
        data = tomllib.loads(read(root, PARAMETERS))
    except tomllib.TOMLDecodeError as e:
        raise Broken(f"{PARAMETERS} is not valid TOML: {e}")
    patterns = data.get("gates", {}).get("l3_paths", {}).get("patterns")
    if not isinstance(patterns, list):
        raise Broken(f"{PARAMETERS} has no [gates.l3_paths] patterns list")
    missing = [p for p in GOVERNANCE_PATHS if (root / p).exists() and p not in patterns]
    if missing:
        raise Broken(f"{PARAMETERS} [gates.l3_paths] does not count: " + ", ".join(missing))
    return f"{len(patterns)} patterns count every governance path present"


CHECKS = (
    ("claude-points-to-agents", check_claude_pointer),
    ("shard-count", check_shard_count),
    ("trigger-paths", check_triggers),
    ("rate-paths", check_rate_paths),
)


def run(root, quiet=False):
    """The names of the checks that failed."""
    failed = []
    for name, check in CHECKS:
        try:
            detail = check(root)
        except Broken as e:
            failed.append(name)
            if not quiet:
                print(f"FAIL {name}: {e}")
                if os.environ.get("GITHUB_ACTIONS") == "true":
                    print(f"::error title=governance contract ({name})::{e}")
        else:
            if not quiet:
                print(f"ok   {name}: {detail}")
    return failed


def edit(rel, old, new, count=1):
    def apply(root):
        text = (root / rel).read_text(encoding="utf-8")
        if text.count(old) < 1:
            raise Broken(f"selftest seed anchor missing in {rel}: {old!r}")
        (root / rel).write_text(text.replace(old, new, count), encoding="utf-8")
    return apply


def matrix(shards, style="flow"):
    def apply(root):
        text = (root / CI).read_text(encoding="utf-8")
        m = re.search(r"^(\s*)shard\s*:.*$", text, re.M)
        if not m:
            raise Broken("selftest seed anchor missing: shard matrix")
        items = [str(i) for i in range(shards)]
        if style == "flow":
            new = f"{m[1]}shard: [{', '.join(items)}]"
        else:
            new = f"{m[1]}shard:" + "".join(f"\n{m[1]}  - {i}" for i in items)
        (root / CI).write_text(text[:m.start()] + new + text[m.end():], encoding="utf-8")
    return apply


def selftest(root):
    """Each seeded break fails exactly the check that guards it; each benign rewrite passes."""
    count = re.search(r"\bafter\s+(\S+(?:[ -]\S+)??)\s+`mutants`\s+jobs\b", read(root, "AGENTS.md"))
    if not count:
        raise Broken("AGENTS.md has no shard count to seed")
    n, word = number(count[1]), count[1]
    phrase = f"after {word} `mutants` jobs"

    def to_symlink(r):
        (r / "CLAUDE.md").unlink()
        (r / "CLAUDE.md").symlink_to("AGENTS.md")

    cases = [
        # (description, seed, the checks that must fail; empty means it must pass)
        ("unchanged tree", lambda r: None, []),
        ("CLAUDE.md no longer names AGENTS.md", edit("CLAUDE.md", "`AGENTS.md`", "`RULES.md`"),
         ["claude-points-to-agents"]),
        ("CLAUDE.md is a symlink to AGENTS.md", to_symlink, ["claude-points-to-agents"]),
        ("a path CLAUDE.md names is gone", lambda r: (r / "README.md").unlink(),
         ["claude-points-to-agents"]),
        ("AGENTS.md names one shard more", edit("AGENTS.md", phrase,
                                                f"after {n + 1} `mutants` jobs"), ["shard-count"]),
        ("ci.yml drops a shard", matrix(n - 1), ["shard-count"]),
        ("AGENTS.md no longer names the count", edit("AGENTS.md", phrase, "after the `mutants` run"),
         ["shard-count"]),
        ("a second shard matrix", edit(CI, "    strategy:", "    strategy:\n      # shard: [0]\n"
                                       "      x:\n        shard: [0]"), ["shard-count"]),
        ("ci.yml shard list in block style", matrix(n, "block"), []),
        ("both sides grow, written as words", lambda r: (
            edit("AGENTS.md", phrase, "after twenty-four `mutants` jobs")(r), matrix(24)(r)), []),
        ("both sides grow, written as digits", lambda r: (
            edit("AGENTS.md", phrase, "after 24 `mutants` jobs")(r), matrix(24)(r)), []),
        ("the workflow stops running on AGENTS.md", edit(WORKFLOW, "      - 'AGENTS.md'\n", ""),
         ["trigger-paths"]),
        ("the workflow stops running on ci.yml", edit(WORKFLOW, f"      - '{CI}'\n", ""),
         ["trigger-paths"]),
        ("the rate gate stops counting AGENTS.md", edit(PARAMETERS, '    "AGENTS.md",\n', ""),
         ["rate-paths"]),
        ("rcs-parameters.toml is unreadable", lambda r: (r / PARAMETERS).unlink(), ["rate-paths"]),
    ]
    inputs = set(GOVERNANCE_PATHS) | set(CONTRACT_INPUTS) | {"README.md"}
    inputs |= set(re.findall(r"`([^`\s]+/[^`\s]+)`", read(root, "CLAUDE.md")))
    bad = 0
    for desc, seed, want in cases:
        with tempfile.TemporaryDirectory() as tmp:
            copy = Path(tmp)
            for rel in inputs:
                if (root / rel).is_file():
                    (copy / rel).parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(root / rel, copy / rel)
            seed(copy)
            got = run(copy, quiet=True)
        ok = got == want
        bad += not ok
        print(f"{'ok  ' if ok else 'FAIL'} selftest: {desc}: "
              f"{'fails ' + ', '.join(got) if got else 'passes'}"
              + ("" if ok else f" (want {'fails ' + ', '.join(want) if want else 'passes'})"))
    return bad


def main(argv):
    here = Path(__file__).resolve().parent.parent
    if argv == ["--selftest"]:
        try:
            return 1 if selftest(here) else 0
        except Broken as e:
            print(f"FAIL selftest: {e}")
            return 1
    if len(argv) > 1 or (argv and argv[0].startswith("-")):
        print("\n".join(__doc__.strip().splitlines()[2:4]), file=sys.stderr)
        return 2
    root = Path(argv[0]) if argv else here
    return 1 if run(root) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
