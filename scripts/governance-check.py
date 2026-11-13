#!/usr/bin/env python3
"""Maestro's governance contract, run by the `stability-check` job (BRO-3702).

    python3 -I scripts/governance-check.py [ROOT]          # check the tree at ROOT (default: this checkout)
    python3 -I scripts/governance-check.py --selftest [CASE]  # seed each case in a copy; each must hold
    python3 -I scripts/governance-check.py --mutation-proof   # remove each guard in turn: its case must fail

Exit 0 when every check holds, 1 when one does not, 2 on usage. A file that cannot be read, or a
shape the checks do not read, is a failure with a named reason, never a pass. Needs Python 3.11
(tomllib) and mikefarah yq v4, which reads the YAML; GitHub's ubuntu images ship both.

This file comes from the PR it judges, like the workflow that runs it: it catches mistakes, and
review of scripts/** and .github/workflows/** (p9's P20 floor) catches intent.
"""
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

# The workspace policy's governance tier (broomva/workspace .control/policy.yaml,
# auto_merge.gates.governance.paths). p9 needs a passing `stability-check` on a PR touching any of
# them. All eight must trigger the job and be counted by the rate gate, present here or not: a PR
# that adds one is governance-class too.
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
# The levels of bstack's rcs-parameters.toml template, in its order, and the parameters its
# compute-lambda.sh reads for each.
LEVELS = ("L0", "L1", "L2", "L3")
LEVEL_PARAMETERS = ("gamma", "L_theta", "rho", "L_d", "eta", "beta", "tau_bar", "nu", "tau_a")

# How the job reaches bstack. The install step is this recipe exactly, and outside it the only
# workflow lines that mention bstack or its scripts are these two calls: a cleared environment
# bound to this checkout, and compute-lambda.sh with --strict and the checkout's parameters.
INSTALL_STEP = "Install bstack (pinned, never from the PR)"
INSTALL_RUN = """set -euo pipefail
DEST="$RUNNER_TEMP/bstack-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT"
rm -rf "$DEST"
git init -q "$DEST"
git -C "$DEST" fetch -q --depth 1 https://github.com/broomva/bstack.git "$BSTACK_REV"
git -C "$DEST" checkout -q --detach FETCH_HEAD
test "$(git -C "$DEST" rev-parse HEAD)" = "$BSTACK_REV"
echo "bstack $BSTACK_REV"
echo "BSTACK=$DEST" >> "$GITHUB_ENV"
"""
BSTACK_ENV = 'env -i PATH="$PATH" HOME="$HOME" BROOMVA_WORKSPACE="$PWD" bash "$BSTACK"/scripts/'
BSTACK_CALLS = [
    ("compute", "out=$(" + BSTACK_ENV + "compute-lambda.sh --strict --human"
     ' --config="$PWD/.control/rcs-parameters.toml" 2>&1)'),
    ("rate", "out=$(" + BSTACK_ENV + "l3-rate-gate.sh 2>&1)"),
]
BSTACK_MENTION = re.compile(r"bstack|compute-lambda|l3-rate-gate|git[ \t]+clone", re.I)

# How many keys and values (yq's `...`) carry an anchor, an alias, a merge key or a tag outside
# YAML's core schema; an alias has an empty tag, a merge key `!!merge`. The document itself is
# read by a separate `.` query: yq drops duplicate keys when it builds an object around it.
YQ_EXOTIC = '[... | select(anchor != "" or (tag | test("^!!(map|seq|str|int|float|bool|null)$") | not))] | length'


UNITS = ("zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen "
         "fifteen sixteen seventeen eighteen nineteen").split()
TENS = {"twenty": 20, "thirty": 30, "forty": 40, "fifty": 50,
        "sixty": 60, "seventy": 70, "eighty": 80, "ninety": 90}
COUNT = re.compile(r"\bafter[ \t]+([A-Za-z0-9]+(?:[ -][A-Za-z]+)??)[ \t]+`mutants`[ \t]+jobs\b")


class Broken(Exception):
    pass


def read(root, rel):
    try:
        return (root / rel).read_text(encoding="utf-8")
    except OSError as e:
        raise Broken(f"cannot read {rel}: {e.strerror or e}")
    except UnicodeDecodeError:
        raise Broken(f"{rel} is not UTF-8")


def number(text):
    """A count written as digits or English words below one hundred ("sixteen", "twenty-four")."""
    t = text.lower()
    if re.fullmatch(r"[0-9]+", t):
        return int(t)
    parts = re.split(r"[ -]", t)
    if len(parts) == 1 and parts[0] in UNITS:
        return UNITS.index(parts[0])
    if len(parts) == 1 and parts[0] in TENS:
        return TENS[parts[0]]
    if len(parts) == 2 and parts[0] in TENS and parts[1] in UNITS[1:10]:
        return TENS[parts[0]] + UNITS.index(parts[1])
    raise Broken(f"not a count below one hundred: {text!r}")


def mapping(x, where):
    if not isinstance(x, dict):
        raise Broken(f"{where} is not a mapping")
    return x


def no_duplicates(pairs):
    keys = [k for k, _ in pairs]
    dups = sorted({k for k in keys if keys.count(k) > 1})
    if dups:
        raise Broken("duplicate keys " + ", ".join(map(repr, dups)))
    return dict(pairs)


_YAML = {}


def load_yaml(root, rel):
    """The YAML document at rel, read by yq. Refused: bytes outside printable ASCII and newline
    (no tab, carriage return, form feed or non-breaking space for parsers to read differently),
    duplicate keys, anchors, aliases, merge keys and non-core tags."""
    try:
        raw = (root / rel).read_bytes()
    except OSError as e:
        raise Broken(f"cannot read {rel}: {e.strerror or e}")
    bad = sorted({b for b in raw if b != 0x0A and not 0x20 <= b <= 0x7E})
    if bad:
        raise Broken(f"{rel} has bytes outside printable ASCII and newline: "
                     + ", ".join(f"0x{b:02x}" for b in bad))
    if raw not in _YAML:
        _YAML[raw] = [yq(root, rel, query) for query in (".", YQ_EXOTIC)]
    doc, exotic = _YAML[raw]
    if exotic != 0:
        raise Broken(f"{rel} uses anchors, aliases, merge keys or non-core tags, which are not read")
    return doc


def yq(root, rel, query):
    """yq's reading of rel under query, as one JSON document."""
    try:
        out = subprocess.run(["yq", "-o=json", "-I=0", query, str(root / rel)],
                             capture_output=True, text=True, timeout=60)
    except FileNotFoundError:
        raise Broken("yq not found: the checks read YAML with mikefarah yq v4")
    if out.returncode:
        raise Broken(f"yq cannot read {rel}: {out.stderr.strip()[:300]}")
    try:
        return json.loads(out.stdout, object_pairs_hook=no_duplicates)
    except json.JSONDecodeError as e:
        raise Broken(f"yq's reading of {rel} is not one JSON document: {e}")
    except Broken as e:
        raise Broken(f"{rel} has {e}")


def claude_paths(text):
    """The repository paths CLAUDE.md names: backticked, with a slash or a file extension. Each must
    exist (claude-points-to-agents) and trigger the job (trigger-paths), so a PR that moves one
    runs the check; the selftest copies them. Each must be a plain relative path, as git and
    GitHub's path filters read it the same way: no leading / or ./, no .. segment, no glob."""
    named = re.findall(r"`([^` \t\n]+)`", text)
    paths = [p for p in named if "/" in p or re.search(r"\.(md|html|ya?ml|toml|sh|py|rs)$", p)]
    unplain = [p for p in paths if p.startswith(("/", "./")) or ".." in p.split("/")
               or re.search(r"[*?\[\]{}!\\]", p) or not (p.isascii() and p.isprintable())]
    if unplain:
        raise Broken("CLAUDE.md names paths that are not plain repository paths: " + ", ".join(unplain))
    return named, list(dict.fromkeys(paths))


def check_claude_pointer(root):
    """CLAUDE.md is a pointer file, neither a symlink nor a copy of AGENTS.md, that names
    `AGENTS.md`, and every repository path it names exists."""
    if (root / "CLAUDE.md").is_symlink():
        raise Broken("CLAUDE.md is a symlink; it is a pointer file that names AGENTS.md")
    text = read(root, "CLAUDE.md")
    if text == read(root, "AGENTS.md"):
        raise Broken("CLAUDE.md is a copy of AGENTS.md; it is a pointer file that names AGENTS.md")
    named, paths = claude_paths(text)
    if "AGENTS.md" not in named:
        raise Broken("CLAUDE.md does not point to `AGENTS.md`")
    missing = [p for p in paths if not (root / p).is_file()]
    if missing:
        raise Broken("CLAUDE.md names paths that do not exist: " + ", ".join(missing))
    return "points to " + ", ".join(paths)


def check_shard_count(root):
    """The number of `mutants` jobs AGENTS.md names is the number of jobs ci.yml's mutants matrix
    runs. GitHub runs one job per combination after include and exclude, so the count is the
    shard list's length only for exactly that shape; any other shape fails."""
    found = COUNT.findall(read(root, "AGENTS.md"))
    if len(found) != 1:
        raise Broken(f"AGENTS.md names the `mutants` job count {len(found)} times, not once "
                     "(\"after <count> `mutants` jobs\")")
    named = number(found[0])
    doc = mapping(load_yaml(root, CI), CI)
    mutants = mapping(mapping(doc.get("jobs"), f"{CI} jobs").get("mutants"), "jobs.mutants")
    strategy = mapping(mutants.get("strategy"), "jobs.mutants.strategy")
    shaping = sorted(set(strategy) - {"matrix", "fail-fast", "max-parallel"})
    if shaping:
        raise Broken(f"unsupported matrix shape in {CI}: strategy has " + ", ".join(shaping))
    matrix = mapping(strategy.get("matrix"), "jobs.mutants.strategy.matrix")
    if list(matrix) != ["shard"]:
        raise Broken(f"unsupported matrix shape in {CI}: the matrix must hold exactly `shard`, "
                     "and holds " + ", ".join(matrix))
    shards = matrix["shard"]
    if not isinstance(shards, list):
        raise Broken(f"unsupported matrix shape in {CI}: shard is not a list")
    if not all(type(x) in (int, str) for x in shards):
        raise Broken(f"unsupported matrix shape in {CI}: a shard is not an integer or a string")
    if len(set(shards)) != len(shards):
        raise Broken(f"unsupported matrix shape in {CI}: the shard list repeats a value")
    if named != len(shards):
        raise Broken(f"AGENTS.md says {found[0]} ({named}) `mutants` jobs, "
                     f"but the {CI} mutants matrix has {len(shards)} shards")
    return f"AGENTS.md and {CI} both run {named} shards"


def check_triggers(root):
    """The workflow runs on exactly on.pull_request, on main, with paths that hold every governance
    path, every input of these checks and every path CLAUDE.md names. Another event,
    paths-ignore, types or a `!` entry would change when the check runs."""
    doc = mapping(load_yaml(root, WORKFLOW), WORKFLOW)
    on = mapping(doc.get("on"), f"{WORKFLOW} on")
    if list(on) != ["pull_request"]:
        raise Broken(f"unsupported trigger shape in {WORKFLOW}: `on` must hold exactly "
                     "pull_request, and holds " + ", ".join(on))
    pr = mapping(on["pull_request"], "on.pull_request")
    if sorted(pr) != ["branches", "paths"]:
        raise Broken(f"unsupported trigger shape in {WORKFLOW}: on.pull_request must hold exactly "
                     "branches and paths, and holds " + ", ".join(pr))
    if pr["branches"] != ["main"]:
        raise Broken(f"unsupported trigger shape in {WORKFLOW}: on.pull_request.branches must be [main]")
    paths = pr["paths"]
    if not isinstance(paths, list) or not all(isinstance(x, str) for x in paths):
        raise Broken(f"unsupported trigger shape in {WORKFLOW}: on.pull_request.paths is not a "
                     "list of strings")
    negated = [x for x in paths if x.startswith("!")]
    if negated:
        raise Broken(f"unsupported trigger shape in {WORKFLOW}: on.pull_request.paths negates "
                     + ", ".join(negated))
    want = list(dict.fromkeys([*GOVERNANCE_PATHS, *CONTRACT_INPUTS,
                               *claude_paths(read(root, "CLAUDE.md"))[1]]))
    missing = [p for p in want if p not in paths]
    if missing:
        raise Broken(f"{WORKFLOW} does not run on: " + ", ".join(missing))
    return f"{len(want)} paths run the job"


def parameters(root):
    try:
        return tomllib.loads(read(root, PARAMETERS))
    except tomllib.TOMLDecodeError as e:
        raise Broken(f"{PARAMETERS} is not valid TOML: {e}")


def check_rate_paths(root):
    """The L3 rate gate counts every governance path. It passes the list to `git log --`, where
    pathspec magic such as :(exclude), :! or :^ removes paths from the count: none is allowed."""
    gates = mapping(parameters(root).get("gates"), f"{PARAMETERS} [gates]")
    patterns = mapping(gates.get("l3_paths"), f"{PARAMETERS} [gates.l3_paths]").get("patterns")
    if not isinstance(patterns, list) or not all(isinstance(x, str) for x in patterns):
        raise Broken(f"{PARAMETERS} [gates.l3_paths] patterns is not a list of strings")
    unplain = [x for x in patterns if not (x.isascii() and x.isprintable())]
    if unplain:
        raise Broken(f"{PARAMETERS} [gates.l3_paths] has patterns outside printable ASCII: "
                     + ", ".join(map(repr, unplain)))
    magic = [x for x in patterns if x.startswith(":")]
    if magic:
        raise Broken(f"{PARAMETERS} [gates.l3_paths] uses pathspec magic: " + ", ".join(magic))
    missing = [p for p in GOVERNANCE_PATHS if p not in patterns]
    if missing:
        raise Broken(f"{PARAMETERS} [gates.l3_paths] does not count: " + ", ".join(missing))
    return f"{len(patterns)} patterns count every governance path"


def finite(x):
    return isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x)


def check_levels(root):
    """The parameters hold exactly the template's four levels, with finite numbers, each with a
    cached lambda: with none, bstack's compute-lambda.sh finds all([]) stable, a level with no
    cached value is never checked for drift, and inf in a parameter and its cache drifts by nan,
    which --strict does not catch."""
    data = parameters(root)
    levels = data.get("levels")
    if not isinstance(levels, list) or not all(isinstance(x, dict) for x in levels):
        raise Broken(f"{PARAMETERS} has no [[levels]] tables")
    ids = [x.get("id") for x in levels]
    if ids != list(LEVELS):
        raise Broken(f"{PARAMETERS} levels are {ids}, not {list(LEVELS)}")
    cached = mapping(mapping(data.get("derived"), f"{PARAMETERS} [derived]").get("lambda"),
                     f"{PARAMETERS} [derived.lambda]")
    if sorted(cached) != sorted(LEVELS):
        raise Broken(f"{PARAMETERS} [derived.lambda] caches {sorted(cached)}, not {list(LEVELS)}")
    bad = [f"{lvl['id']}.{k}" for lvl in levels for k in LEVEL_PARAMETERS if not finite(lvl.get(k))]
    bad += [f"derived.lambda.{k}" for k in LEVELS if not finite(cached[k])]
    if bad:
        raise Broken(f"{PARAMETERS} has missing or non-finite numbers: " + ", ".join(bad))
    return "levels " + ", ".join(LEVELS) + ", finite, each with a cached lambda"


def check_bstack_calls(root):
    """The job fetches bstack at a pinned full SHA with the exact install recipe, and its only
    other mention of bstack or its scripts is the two exact calls, in their own steps."""
    doc = mapping(load_yaml(root, WORKFLOW), WORKFLOW)
    job = mapping(mapping(doc.get("jobs"), "jobs").get("stability-check"), "jobs.stability-check")
    steps = job.get("steps")
    if not isinstance(steps, list) or not all(isinstance(s, dict) for s in steps):
        raise Broken(f"{WORKFLOW} jobs.stability-check.steps is not a list of steps")
    installs = [s for s in steps if s.get("name") == INSTALL_STEP]
    if len(installs) != 1:
        raise Broken(f"{WORKFLOW} has {len(installs)} {INSTALL_STEP!r} steps, not one")
    install = installs[0]
    if sorted(install) != ["env", "name", "run"]:
        raise Broken(f"the install step holds {', '.join(install)}, not exactly name, env and run")
    if install["run"] != INSTALL_RUN:
        raise Broken("the install step's run is not the pinned fetch, detached checkout and "
                     "verification recipe")
    env = mapping(install["env"], "the install step's env")
    if list(env) != ["BSTACK_REV"]:
        raise Broken("the install step's env must set exactly BSTACK_REV")
    if not re.fullmatch(r"[0-9a-f]{40}", str(env["BSTACK_REV"])):
        raise Broken(f"BSTACK_REV is {env['BSTACK_REV']!r}, not a full 40-character commit SHA")
    calls = []
    for s in steps:
        if s.get("name") == INSTALL_STEP:
            continue
        for key, value in s.items():
            text = value if isinstance(value, str) else json.dumps(value)
            for line in text.split("\n"):
                if BSTACK_MENTION.search(line):
                    calls.append((s.get("id"), line.strip() if key == "run" else f"{key}: {line}"))
    if calls != BSTACK_CALLS:
        raise Broken("outside the install step, the workflow's mentions of bstack are not exactly "
                     "the compute and rate calls, bound and --strict: "
                     + "; ".join(f"{i}: {c}" for i, c in calls))
    for s in steps:
        if s.get("id") in ("compute", "rate") and sorted(s) != ["id", "name", "run"]:
            raise Broken(f"the {s['id']} step holds {', '.join(s)}, not exactly name, id and run")
    return f"bstack {env['BSTACK_REV'][:12]}, fetched and called as pinned"


CHECKS = (
    ("claude-points-to-agents", check_claude_pointer),
    ("shard-count", check_shard_count),
    ("trigger-paths", check_triggers),
    ("rate-paths", check_rate_paths),
    ("rcs-levels", check_levels),
    ("bstack-calls", check_bstack_calls),
)


def run(root, quiet=False):
    """The names of the checks that failed. A check that crashes fails as `<name> (crashed)`: it
    fails closed, and no selftest case expects it."""
    failed = []
    for name, check in CHECKS:
        try:
            detail = check(root)
        except Exception as e:
            crashed = not isinstance(e, Broken)
            failed.append(name + (" (crashed)" if crashed else ""))
            reason = f"{type(e).__name__}: {e}" if crashed else str(e)
            if not quiet:
                print(f"FAIL {name}: {reason}")
                if os.environ.get("GITHUB_ACTIONS") == "true":
                    print(f"::error title=governance contract ({name})::{reason}")
        else:
            if not quiet:
                print(f"ok   {name}: {detail}")
    return failed


# mutation proof below: every guard above is listed in MUTANTS with an anchor that occurs once
# above this line, and the selftest case that only it can fail.


def words(k):
    """k in the words number() reads ("twenty-four"); counts from one hundred are not supported."""
    if not 0 <= k < 100:
        raise Broken(f"the selftest grows the count to {k}; number() reads counts below one hundred")
    if k < 20:
        return UNITS[k]
    tens = next(w for w, v in TENS.items() if v == k // 10 * 10)
    return tens + ("-" + UNITS[k % 10] if k % 10 else "")


def edit(rel, old, new):
    """Replace the one occurrence of old in rel."""
    def apply(root):
        text = (root / rel).read_text(encoding="utf-8")
        if text.count(old) != 1:
            raise Broken(f"selftest seed anchor occurs {text.count(old)} times in {rel}: {old!r}")
        (root / rel).write_text(text.replace(old, new), encoding="utf-8")
    return apply


def resub(rel, pattern, repl, count=1):
    def apply(root):
        text = (root / rel).read_text(encoding="utf-8")
        new, n = re.subn(pattern, repl, text, count=count, flags=re.M)
        if not n:
            raise Broken(f"selftest seed anchor missing in {rel}: {pattern!r}")
        (root / rel).write_text(new, encoding="utf-8")
    return apply


def insert_after(rel, pattern, added):
    """Add lines after the first line matching pattern (whose group 1 is its indentation), each
    indented as that line plus its own leading spaces."""
    return resub(rel, pattern, lambda m: m[0] + "".join("\n" + m[1] + line for line in added))


def write(rel, text):
    def apply(root):
        (root / rel).write_text(text, encoding="utf-8")
    return apply


def matrix(shards):
    items = [str(i) for i in (range(shards) if isinstance(shards, int) else shards)]
    return resub(CI, r"^( *)shard *:.*$", lambda m: f"{m[1]}shard: [{', '.join(items)}]")


def seq(*seeds):
    def apply(root):
        for s in seeds:
            s(root)
    return apply


def bytes_into(rel, pattern, raw):
    """Put raw bytes after the first line matching pattern (bytes the text helpers cannot write)."""
    def apply(root):
        data = (root / rel).read_bytes()
        m = re.search(pattern.encode(), data, re.M)
        if not m:
            raise Broken(f"selftest seed anchor missing in {rel}: {pattern!r}")
        (root / rel).write_bytes(data[:m.end()] + raw + data[m.end():])
    return apply


def cases_for(root):
    """(description, seed, the checks that must fail; empty means it must pass). Each break is
    isolated: it fails one check, through one guard, which the mutation proof confirms."""
    count = COUNT.search(read(root, "AGENTS.md"))
    if not count:
        raise Broken("AGENTS.md has no shard count to seed")
    n, word = number(count[1]), count[1]
    phrase = f"after {word} `mutants` jobs"
    claude = read(root, "CLAUDE.md")
    trigger = "      - 'CLAUDE.md'\n"
    level = r'^\[\[levels\]\]\nid *= *"{}"\n(?:\w+ *=.*\n)*'
    rate = '    "AGENTS.md",\n'
    shard_line = r"^( *)shard *:.*$"
    branches = r"^( *)branches: \[main\]$"
    compute = 'bash "$BSTACK"/scripts/compute-lambda.sh --strict --human'

    def triggered(*paths):
        return edit(WORKFLOW, trigger, trigger + "".join(f"      - '{p}'\n" for p in paths))

    def named(*paths):
        return edit("CLAUDE.md", "`AGENTS.md`", "`AGENTS.md` and " + " and ".join(f"`{p}`" for p in paths))

    def symlink_to_pointer(r):
        (r / "POINTER.md").write_text(claude, encoding="utf-8")
        (r / "CLAUDE.md").unlink()
        (r / "CLAUDE.md").symlink_to("POINTER.md")

    def absolute(r):
        path = f"{r.resolve()}/README.md"
        seq(named(path), triggered(path))(r)

    def nested(copy):
        """The selftest of a tree whose CLAUDE.md also names another root file: it must pass."""
        seq(named("NOTES.md"), triggered("NOTES.md"), write("NOTES.md", "notes\n"))(copy)
        return selftest(copy, quiet=True)

    # A step that mentions bstack outside the install step and the two calls.
    def extra_step(*lines):
        return edit(WORKFLOW, "      - name: Report\n",
                    "      - name: Extra\n        run: |\n"
                    + "".join(f"          {x}\n" for x in lines) + "\n      - name: Report\n")

    return [
        ("unchanged tree", lambda r: None, []),
        # claude-points-to-agents
        ("CLAUDE.md is a symlink to a separate pointer file", symlink_to_pointer,
         ["claude-points-to-agents"]),
        ("CLAUDE.md is a copy of an AGENTS.md that names only itself", seq(
            write("AGENTS.md", f"Rules are in `AGENTS.md`, {phrase}.\n"),
            write("CLAUDE.md", f"Rules are in `AGENTS.md`, {phrase}.\n")), ["claude-points-to-agents"]),
        ("CLAUDE.md names only README.md", write("CLAUDE.md", "Read `README.md`.\n"),
         ["claude-points-to-agents"]),
        ("CLAUDE.md names a path that does not exist", seq(named("docs/missing.md"),
                                                           triggered("docs/missing.md")),
         ["claude-points-to-agents"]),
        # claude_paths() feeds both checks: an unplain path fails both.
        ("CLAUDE.md names a path through ..", seq(named("docs/../README.md"), triggered("docs/../README.md")),
         ["claude-points-to-agents", "trigger-paths"]),
        ("CLAUDE.md names a ./ path", seq(named("./README.md"), triggered("./README.md")),
         ["claude-points-to-agents", "trigger-paths"]),
        ("CLAUDE.md names a glob", seq(named("docs/[x].md"), triggered("docs/[x].md"),
                                       write("docs/[x].md", "x\n")), ["claude-points-to-agents", "trigger-paths"]),
        ("CLAUDE.md names an absolute path", absolute, ["claude-points-to-agents", "trigger-paths"]),
        # shard-count
        ("AGENTS.md names the count twice", edit("AGENTS.md", phrase, f"{phrase}, after {word} `mutants` jobs"),
         ["shard-count"]),
        ("AGENTS.md names one shard more", edit("AGENTS.md", phrase, f"after {n + 1} `mutants` jobs"),
         ["shard-count"]),
        ("ci.yml drops a shard", matrix(n - 1), ["shard-count"]),
        ("AGENTS.md no longer names the count", edit("AGENTS.md", phrase, "after the `mutants` run"),
         ["shard-count"]),
        ("AGENTS.md names a count that is not a number", edit("AGENTS.md", phrase, "after many `mutants` jobs"),
         ["shard-count"]),
        ("strategy gains another key", insert_after(CI, r"^( *)strategy:$", ["  x: 1"]), ["shard-count"]),
        ("strategy sets max-parallel", insert_after(CI, r"^( *)strategy:$", ["  max-parallel: 4"]), []),
        ("the matrix is an expression", seq(resub(CI, shard_line + r"\n", ""),
                                            resub(CI, r"^( *)matrix:$", r"\1matrix: ${{ fromJSON(vars.SHARDS) }}")),
         ["shard-count"]),
        ("the matrix excludes a shard", insert_after(CI, shard_line, ["exclude:", "  - shard: 0"]),
         ["shard-count"]),
        ("the matrix includes another job", insert_after(CI, shard_line, ["include:", "  - shard: 99"]),
         ["shard-count"]),
        ("the matrix gains a dimension", insert_after(CI, shard_line, ["os: [a, b]"]), ["shard-count"]),
        ("the shard list is a string", resub(CI, shard_line, r"\1shard: " + (
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"[:n] if n <= 62 else "x")),
         ["shard-count"]),
        ("a shard is a list", matrix(["[0]"] + list(range(1, n))), ["shard-count"]),
        ("a shard is a boolean", matrix(["true"] + list(range(2, n + 1))), ["shard-count"]),
        ("the shard list repeats a value", matrix(list(range(n - 1)) + [0]), ["shard-count"]),
        (f"both sides grow to {n + 8}, written as words",
         seq(edit("AGENTS.md", phrase, f"after {words(n + 8)} `mutants` jobs"), matrix(n + 8)), []),
        (f"both sides grow to {n + 8}, written as digits",
         seq(edit("AGENTS.md", phrase, f"after {n + 8} `mutants` jobs"), matrix(n + 8)), []),
        # The YAML loader (each break shows on the first check that reads the file).
        ("ci.yml has a non-breaking space before a comment", bytes_into(CI, r"^( *)fail-fast: false", b"\xc2\xa0# x"),
         ["shard-count"]),
        ("ci.yml has a carriage return", bytes_into(CI, r"^( *)fail-fast: false", b"\r"), ["shard-count"]),
        ("ci.yml has a form feed in a comment", bytes_into(CI, r"^( *)fail-fast: false", b" # \x0c"),
         ["shard-count"]),
        ("ci.yml repeats a key", insert_after(CI, r"^( *)fail-fast: false$", ["fail-fast: false"]),
         ["shard-count"]),
        ("ci.yml uses an anchor", resub(CI, shard_line, r"\1shard: &s [" + ", ".join(map(str, range(n))) + "]"),
         ["shard-count"]),
        ("ci.yml uses a merge key", insert_after(CI, r"^( *)fail-fast: false$", ["<<: {}"]), ["shard-count"]),
        ("ci.yml uses a custom tag", resub(CI, r"^( *)fail-fast: false$", r"\1fail-fast: !flag false"),
         ["shard-count"]),
        ("ci.yml is two documents", lambda r: (r / CI).write_text((r / CI).read_text() + "---\nx: 1\n"),
         ["shard-count"]),
        # trigger-paths
        ("push runs beside pull_request", edit(WORKFLOW, "concurrency:", "  push:\nconcurrency:"),
         ["trigger-paths"]),
        ("pull_request_target runs beside pull_request",
         edit(WORKFLOW, "concurrency:", "  pull_request_target:\nconcurrency:"), ["trigger-paths"]),
        # A workflow the loader refuses fails both checks that read it.
        ('"on" repeats on', edit(WORKFLOW, "concurrency:", '"on":\n  pull_request:\nconcurrency:'),
         ["trigger-paths", "bstack-calls"]),
        ("paths-ignore is added", insert_after(WORKFLOW, branches, ["paths-ignore:", "  - 'docs/**'"]),
         ["trigger-paths"]),
        ("types narrows the events", insert_after(WORKFLOW, branches, ["types: [closed]"]), ["trigger-paths"]),
        ("branches widens past main", edit(WORKFLOW, "branches: [main]", "branches: [main, dev]"),
         ["trigger-paths"]),
        ("a trigger path is a mapping", insert_after(WORKFLOW, r"^( *)- 'CLAUDE.md'$", ["- x: y"]),
         ["trigger-paths"]),
        ("the trigger list negates AGENTS.md", insert_after(WORKFLOW, r"^( *)- 'AGENTS.md'$", ["- '!AGENTS.md'"]),
         ["trigger-paths"]),
        ("the workflow stops running on AGENTS.md", edit(WORKFLOW, "      - 'AGENTS.md'\n", ""),
         ["trigger-paths"]),
        ("the workflow stops running on METALAYER.md", edit(WORKFLOW, "      - 'METALAYER.md'\n", ""),
         ["trigger-paths"]),
        ("the workflow stops running on ci.yml", edit(WORKFLOW, f"      - '{CI}'\n", ""), ["trigger-paths"]),
        ("the workflow stops running on a file CLAUDE.md names",
         resub(WORKFLOW, r"^ *- 'docs/specs/.*'\n", ""), ["trigger-paths"]),
        ("trigger paths double-quoted", edit(WORKFLOW, "      - 'AGENTS.md'\n", '      - "AGENTS.md"\n'), []),
        ("trigger paths unquoted", edit(WORKFLOW, "      - 'AGENTS.md'\n", "      - AGENTS.md\n"), []),
        # rate-paths
        ("[gates] is a string", seq(resub(PARAMETERS, r"^\[gates\.l3_paths\]\npatterns = \[[^\]]*\]\n", ""),
                                    edit(PARAMETERS, "schema_version = 1\n", 'schema_version = 1\ngates = "x"\n')),
         ["rate-paths"]),
        ("[gates.l3_paths] is a string", resub(PARAMETERS, r"^\[gates\.l3_paths\]\npatterns = \[[^\]]*\]\n",
                                               '[gates]\nl3_paths = "x"\n'), ["rate-paths"]),
        ("a rate pattern is a number", edit(PARAMETERS, rate, rate + "    1,\n"), ["rate-paths"]),
        ("a rate pattern has a non-breaking space", edit(PARAMETERS, rate, rate + '    "AGENTS.md ",\n'),
         ["rate-paths"]),
        ("the rate list excludes AGENTS.md", edit(PARAMETERS, rate, rate + '    ":(exclude)AGENTS.md",\n'),
         ["rate-paths"]),
        ("the rate list excludes with :!", edit(PARAMETERS, rate, rate + '    ":!AGENTS.md",\n'), ["rate-paths"]),
        ("the rate list excludes with :^", edit(PARAMETERS, rate, rate + '    ":^AGENTS.md",\n'), ["rate-paths"]),
        ("the rate gate stops counting AGENTS.md", edit(PARAMETERS, rate, ""), ["rate-paths"]),
        ("the rate gate stops counting METALAYER.md", edit(PARAMETERS, '    "METALAYER.md",\n', ""),
         ["rate-paths"]),
        # rcs-levels
        ("levels is a list of numbers", seq(resub(PARAMETERS, level.format(r"L\d"), "", count=0),
                                            edit(PARAMETERS, "schema_version = 1\n",
                                                 "schema_version = 1\nlevels = [1, 2, 3, 4]\n")),
         ["rcs-levels"]),
        ("a level is removed", resub(PARAMETERS, level.format("L2"), ""), ["rcs-levels"]),
        ("every level is removed", resub(PARAMETERS, level.format(r"L\d"), "", count=0), ["rcs-levels"]),
        ("[derived] is a string", seq(resub(PARAMETERS, r"^\[derived\.lambda\]\n(?:L\d = .*\n)+", ""),
                                      resub(PARAMETERS, r"^\[derived\.omega\]\n(?:\w+ *=.*\n)+", ""),
                                      edit(PARAMETERS, "schema_version = 1\n", 'schema_version = 1\nderived = "x"\n')),
         ["rcs-levels"]),
        ("[derived.lambda] is a string", resub(PARAMETERS, r"^\[derived\.lambda\]\n(?:L\d = .*\n)+",
                                               '[derived]\nlambda = "x"\n'), ["rcs-levels"]),
        ("a cached lambda is removed", resub(PARAMETERS, r"^L3 = .*\n", ""), ["rcs-levels"]),
        ("a cached lambda is added", resub(PARAMETERS, r"^L3 = .*$", lambda m: m[0] + "\nL4 = 0.1"), ["rcs-levels"]),
        ("a rho is a string", resub(PARAMETERS, r"^rho( *)= (.*)$", r'rho\1= "\2"'), ["rcs-levels"]),
        ("a rho is a boolean", resub(PARAMETERS, r"^rho( *)= (.*)$", r"rho\1= true"), ["rcs-levels"]),
        ("a gamma and its cached lambda are both infinite",
         seq(resub(PARAMETERS, r"^gamma( *)= .*$", r"gamma\1= inf"), resub(PARAMETERS, r"^L0 = .*$", "L0 = inf")),
         ["rcs-levels"]),
        ("a tau_a is not a number", resub(PARAMETERS, r"^tau_a( *)= .*$", r"tau_a\1= nan"), ["rcs-levels"]),
        # bstack-calls
        ("a step is a string", edit(WORKFLOW, "      - name: Report\n", "      - just-a-string\n\n      - name: Report\n"),
         ["bstack-calls"]),
        ("the install step appears twice", lambda r: edit(WORKFLOW, "      - name: Report\n", re.search(
            r"(?ms)^      - name: Install bstack.*?\n\n", (r / WORKFLOW).read_text())[0] + "      - name: Report\n")(r),
         ["bstack-calls"]),
        ("the install step gains a key", insert_after(WORKFLOW, r"^( *)- name: Install bstack.*$",
                                                      ["  continue-on-error: true"]), ["bstack-calls"]),
        ("the fetched bstack is not verified", resub(WORKFLOW, r"^ *test .*BSTACK_REV.*\n", ""), ["bstack-calls"]),
        ("the checkout is not detached", edit(WORKFLOW, "checkout -q --detach FETCH_HEAD", "checkout -q FETCH_HEAD"),
         ["bstack-calls"]),
        ("the install step sets another variable", insert_after(WORKFLOW, r"^( *)BSTACK_REV: .*$", ["X: y"]),
         ["bstack-calls"]),
        ("bstack pinned to a branch", resub(WORKFLOW, r"(BSTACK_REV: )[0-9a-f]{40}", r"\1main"), ["bstack-calls"]),
        ("bstack pinned to a short SHA", resub(WORKFLOW, r"(BSTACK_REV: [0-9a-f]{7})[0-9a-f]{33}", r"\1"),
         ["bstack-calls"]),
        ("compute-lambda.sh runs without --strict", edit(WORKFLOW, " --strict --human", " --human"),
         ["bstack-calls"]),
        ("compute-lambda.sh reads no explicit config", edit(WORKFLOW, ' --config="$PWD/.control/rcs-parameters.toml"', ""),
         ["bstack-calls"]),
        ("compute-lambda.sh inherits the environment", edit(WORKFLOW, BSTACK_ENV + "compute", 'bash "$BSTACK"/scripts/compute'),
         ["bstack-calls"]),
        ("l3-rate-gate.sh inherits the environment", edit(WORKFLOW, BSTACK_ENV + "l3-rate", 'bash "$BSTACK"/scripts/l3-rate'),
         ["bstack-calls"]),
        ("a second compute-lambda.sh without --strict", extra_step(f'bash "$BSTACK"/scripts/compute-lambda.sh --human'),
         ["bstack-calls"]),
        ("an extra git clone, the verify line kept", extra_step("git clone https://github.com/broomva/bstack.git /tmp/b"),
         ["bstack-calls"]),
        ("a step uses a bstack action", edit(WORKFLOW, "      - name: Report\n",
                                             "      - uses: broomva/bstack@main\n\n      - name: Report\n"),
         ["bstack-calls"]),
        ("the compute step sets BSTACK", insert_after(WORKFLOW, r"^( *)id: compute$", ["env:", "  BSTACK: /tmp/b"]),
         ["bstack-calls"]),
        ("the compute step gains continue-on-error", insert_after(WORKFLOW, r"^( *)id: compute$",
                                                                  ["continue-on-error: true"]), ["bstack-calls"]),
        # The selftest itself.
        ("the selftest of a tree whose CLAUDE.md names another root file", nested, None),
    ]


def selftest(root, quiet=False, only=None):
    """Seed each case into a copy of every file the checks read; the number of cases that did not
    hold. A case whose want is None is a nested selftest, which must report no failed case."""
    inputs = set(GOVERNANCE_PATHS) | set(CONTRACT_INPUTS) | set(claude_paths(read(root, "CLAUDE.md"))[1])
    cases = cases_for(root)
    if only is not None:
        cases = [c for c in cases if c[0] == only]
        if not cases:
            raise Broken(f"no selftest case named {only!r}")
    if quiet:
        cases = [c for c in cases if c[2] is not None]
    bad = 0
    for desc, seed, want in cases:
        with tempfile.TemporaryDirectory() as tmp:
            copy = Path(tmp)
            for rel in inputs:
                if (root / rel).is_file():
                    (copy / rel).parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(root / rel, copy / rel)
            if want is None:
                failed = seed(copy)
                got, want = ([f"{failed} selftest cases"] if failed else []), []
            else:
                seed(copy)
                got = run(copy, quiet=True)
        ok = got == want
        bad += not ok
        if not quiet:
            print(f"{'ok  ' if ok else 'FAIL'} selftest: {desc}: "
                  f"{'fails ' + ', '.join(got) if got else 'passes'}"
                  + ("" if ok else f" (want {'fails ' + ', '.join(want) if want else 'passes'})"))
    return bad


# (guard, anchor in the checks above, its neutered form, the selftest case only it can fail)
MUTANTS = [
    ("symlink refused", 'if (root / "CLAUDE.md").is_symlink():', "if False:",
     "CLAUDE.md is a symlink to a separate pointer file"),
    ("copy refused", 'if text == read(root, "AGENTS.md"):', "if False:",
     "CLAUDE.md is a copy of an AGENTS.md that names only itself"),
    ("names AGENTS.md", 'if "AGENTS.md" not in named:', "if False:", "CLAUDE.md names only README.md"),
    ("named paths exist", "missing = [p for p in paths if not (root / p).is_file()]", "missing = []",
     "CLAUDE.md names a path that does not exist"),
    ("no .. segment", 'or ".." in p.split("/")', "", "CLAUDE.md names a path through .."),
    ("no leading / or ./", 'p.startswith(("/", "./"))', "False", "CLAUDE.md names a ./ path"),
    ("no glob", r're.search(r"[*?\[\]{}!\\]", p)', "False", "CLAUDE.md names a glob"),
    ("count named once", "if len(found) != 1:", "if not found:", "AGENTS.md names the count twice"),
    ("strategy keys", "if shaping:", "if False:", "strategy gains another key"),
    ("matrix is exactly shard", 'if list(matrix) != ["shard"]:', "if False:", "the matrix excludes a shard"),
    ("shard is a list", "if not isinstance(shards, list):", "if False:", "the shard list is a string"),
    ("shards are scalars", "if not all(type(x) in (int, str) for x in shards):", "if False:",
     "a shard is a boolean"),
    ("shards are unique", "if len(set(shards)) != len(shards):", "if False:", "the shard list repeats a value"),
    ("count equals the matrix", "if named != len(shards):", "if False:", "ci.yml drops a shard"),
    ("YAML is printable ASCII", "if bad:\n        raise Broken(f\"{rel} has bytes", "if False:\n        raise Broken(f\"{rel} has bytes",
     "ci.yml has a non-breaking space before a comment"),
    ("YAML has no duplicate keys", "    if dups:\n", "    if False:\n", "ci.yml repeats a key"),
    ("YAML has no anchors or tags", "if exotic != 0:", "if False:", "ci.yml uses an anchor"),
    ("a mapping is a mapping", "    if not isinstance(x, dict):\n        raise Broken(f\"{where} is not a mapping\")",
     "    if False:\n        raise Broken(f\"{where} is not a mapping\")", "[gates] is a string"),
    ("on is exactly pull_request", 'if list(on) != ["pull_request"]:', "if False:",
     "push runs beside pull_request"),
    ("pull_request is exactly branches and paths", 'if sorted(pr) != ["branches", "paths"]:', "if False:",
     "paths-ignore is added"),
    ("branches is main", 'if pr["branches"] != ["main"]:', "if False:", "branches widens past main"),
    ("trigger paths are strings", "if not isinstance(paths, list) or not all(isinstance(x, str) for x in paths):",
     "if False:", "a trigger path is a mapping"),
    ("no negated trigger", "if negated:", "if False:", "the trigger list negates AGENTS.md"),
    ("triggers cover", "missing = [p for p in want if p not in paths]", "missing = []",
     "the workflow stops running on AGENTS.md"),
    ("rate patterns are strings",
     "if not isinstance(patterns, list) or not all(isinstance(x, str) for x in patterns):", "if False:",
     "a rate pattern is a number"),
    ("rate patterns are printable ASCII", "if unplain:\n        raise Broken(f\"{PARAMETERS}",
     "if False:\n        raise Broken(f\"{PARAMETERS}", "a rate pattern has a non-breaking space"),
    ("no pathspec magic", "if magic:", "if False:", "the rate list excludes AGENTS.md"),
    ("rate patterns cover", "missing = [p for p in GOVERNANCE_PATHS if p not in patterns]", "missing = []",
     "the rate gate stops counting AGENTS.md"),
    ("levels are tables", "if not isinstance(levels, list) or not all(isinstance(x, dict) for x in levels):",
     "if False:", "levels is a list of numbers"),
    ("level ids", "if ids != list(LEVELS):", "if False:", "a level is removed"),
    ("cached lambda ids", "if sorted(cached) != sorted(LEVELS):", "if False:", "a cached lambda is added"),
    ("numbers are numbers", "return isinstance(x, (int, float)) and", "return True and", "a rho is a string"),
    ("numbers are not booleans", "and not isinstance(x, bool) and math", "and math", "a rho is a boolean"),
    ("numbers are finite", "and math.isfinite(x)", "", "a gamma and its cached lambda are both infinite"),
    ("steps are a list of steps", "if not isinstance(steps, list) or not all(isinstance(s, dict) for s in steps):",
     "if False:", "a step is a string"),
    ("one install step", "if len(installs) != 1:", "if False:", "the install step appears twice"),
    ("install step keys", 'if sorted(install) != ["env", "name", "run"]:', "if False:",
     "the install step gains a key"),
    ("install recipe", 'if install["run"] != INSTALL_RUN:', "if False:", "the fetched bstack is not verified"),
    ("install env is BSTACK_REV only", 'if list(env) != ["BSTACK_REV"]:', "if False:",
     "the install step sets another variable"),
    ("BSTACK_REV is a full SHA", r'if not re.fullmatch(r"[0-9a-f]{40}", str(env["BSTACK_REV"])):', "if False:",
     "bstack pinned to a branch"),
    ("bstack mentioned only in the two calls", "if calls != BSTACK_CALLS:", "if False:",
     "a second compute-lambda.sh without --strict"),
    ("call steps keys", 'if s.get("id") in ("compute", "rate") and sorted(s) != ["id", "name", "run"]:',
     "if False:", "the compute step gains continue-on-error"),
]


def mutation_proof(root):
    """Neuter each guard in turn, in a copy of this file, and run its case: the case must fail.
    Each anchor must occur exactly once in the checks (above the mutation proof marker)."""
    source = (root / SELF).read_text(encoding="utf-8")
    marker = "# mutation proof " + "below"
    checks, rest = source.split(marker, 1)
    inputs = set(GOVERNANCE_PATHS) | set(CONTRACT_INPUTS) | set(claude_paths(read(root, "CLAUDE.md"))[1])
    names = {c[0] for c in cases_for(root)}
    survived = 0
    print("guard | case | killed")
    for guard, anchor, neutered, case in MUTANTS:
        if checks.count(anchor) != 1 or case not in names:
            print(f"{guard} | {case} | NO: anchor occurs {checks.count(anchor)} times"
                  + ("" if case in names else ", and no case has that name"))
            survived += 1
            continue
        with tempfile.TemporaryDirectory() as tmp:
            copy = Path(tmp)
            for rel in inputs:
                if (root / rel).is_file():
                    (copy / rel).parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(root / rel, copy / rel)
            (copy / SELF).write_text(checks.replace(anchor, neutered) + marker + rest, encoding="utf-8")
            out = subprocess.run([sys.executable, "-I", str(copy / SELF), "--selftest", case],
                                 capture_output=True, text=True, timeout=300)
        killed = out.returncode == 1 and f"FAIL selftest: {case}:" in out.stdout
        survived += not killed
        print(f"{guard} | {case} | {'yes' if killed else 'NO: ' + (out.stdout + out.stderr).strip()[-200:]}")
    print(f"{len(MUTANTS) - survived} of {len(MUTANTS)} guards proven")
    return survived


def main(argv):
    here = Path(__file__).resolve().parent.parent
    try:
        if argv[:1] == ["--selftest"] and len(argv) <= 2:
            return 1 if selftest(here, only=argv[1] if len(argv) == 2 else None) else 0
        if argv == ["--mutation-proof"]:
            return 1 if mutation_proof(here) else 0
    except Broken as e:
        print(f"FAIL selftest: {e}")
        return 1
    if len(argv) > 1 or (argv and argv[0].startswith("-")):
        print("\n".join(__doc__.strip().splitlines()[2:5]), file=sys.stderr)
        return 2
    root = Path(argv[0]) if argv else here
    return 1 if run(root) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
