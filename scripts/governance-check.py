#!/usr/bin/env python3
"""Maestro's governance contract, run by the `stability-check` job (BRO-3702).

    python3 -I scripts/governance-check.py [ROOT]            # check the tree at ROOT (default: this checkout)
    python3 -I scripts/governance-check.py --selftest        # every case, and the completeness checks
    python3 -I scripts/governance-check.py --mutation-proof  # disable each guard alone; see main()

Every refusal goes through refuse(), against one registry of guards (GUARDS). Each registered guard
has its own cases, and data-driven guards register one guard per entry. --selftest seeds every
case into a copy of a canonical fixture tree (Tree), never this repository's own files, and checks
its outcome; then it checks completeness: no code above the mutation-proof marker refuses outside
refuse() and the input readers, and the unchanged fixture evaluates every registered guard.
--mutation-proof disables each guard alone and requires its own cases to change outcome and every
other case to keep its outcome.

Exit 0 when everything holds, 1 when something does not, 2 on usage. Needs Python 3.11 (tomllib)
and mikefarah yq v4, which reads the YAML; GitHub's ubuntu images ship both.

This file comes from the PR it judges, like the workflow that runs it: it catches mistakes, and
review of scripts/** and .github/workflows/** (p9's P20 floor) catches intent.
"""
import ast
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
# The levels of bstack's rcs-parameters.toml template, and the parameters its compute-lambda.sh
# reads for each.
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

# Byte classes refused in the YAML files: parsers read them differently. yq itself rejects the other
# C0 controls, so DEL is the one control byte only this guard can refuse.
BYTE_CLASSES = {
    "tab": rb"\t",
    "carriage-return": rb"\r",
    "control": rb"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]",
    "non-ascii": rb"[\x80-\xff]",
}
# A merge key in any quoting: yq merges a quoted "<<" too, and tags it a plain string.
MERGE_KEY = re.compile(rb"""<<["']?[ \t]*:""")
# Per document: keys and values (yq's `...`) with an anchor, and with a tag outside the core
# schema. An alias has an empty tag and needs an anchor; a merge key is refused by MERGE_KEY.
YQ_EXOTIC = ('{"anchor": ([... | select(anchor != "")] | length), "tag": ([... | select(tag | '
             'test("^(!!(map|seq|str|int|float|bool|null|merge))?$") | not)] | length)}')
# GitHub path filter and glob characters, refused in the paths CLAUDE.md names.
GLOB = re.compile(r"[*?+\[\]{}!\\]")

UNITS = ("zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen "
         "fifteen sixteen seventeen eighteen nineteen").split()
TENS = {"twenty": 20, "thirty": 30, "forty": 40, "fifty": 50,
        "sixty": 60, "seventy": 70, "eighty": 80, "ninety": 90}
COUNT = re.compile(r"\bafter[ \t]+([A-Za-z0-9]+(?:[ -][A-Za-z]+)??)[ \t]+`mutants`[ \t]+jobs\b")


class Broken(Exception):
    pass


# The registry. register() is called for every guard below the mutation-proof marker; refuse()
# is the only way a check refuses.
GUARDS = {}      # id -> (the checks its cases fail, [(description, seed)])
DISABLED = set()  # the guard the mutation proof has disabled
EVALUATED = set()


def register(gid, checks, *cases):
    if gid in GUARDS or not cases:
        raise RuntimeError(f"guard {gid!r} is registered twice or without a case")
    GUARDS[gid] = (tuple(checks), list(cases))


def refuse(gid, failing, message):
    if gid not in GUARDS:
        raise RuntimeError(f"guard {gid!r} is not registered")
    EVALUATED.add(gid)
    if failing and gid not in DISABLED:
        raise Broken(f"{message() if callable(message) else message} [{gid}]")


# Input readers: they report input the checks cannot read, and are not guards.
def read(root, rel):
    try:
        return (root / rel).read_text(encoding="utf-8")
    except OSError as e:
        raise Broken(f"cannot read {rel}: {e.strerror or e}")
    except UnicodeDecodeError:
        raise Broken(f"{rel} is not UTF-8")


def read_bytes(root, rel):
    try:
        return (root / rel).read_bytes()
    except OSError as e:
        raise Broken(f"cannot read {rel}: {e.strerror or e}")


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


def yq(root, rel, query):
    """yq's reading of rel under query: one JSON value per document, and the duplicated keys."""
    try:
        out = subprocess.run(["yq", "-o=json", "-I=0", query, str(root / rel)],
                             capture_output=True, text=True, timeout=60)
    except FileNotFoundError:
        raise Broken("yq not found: the checks read YAML with mikefarah yq v4")
    if out.returncode:
        raise Broken(f"yq cannot read {rel}: {out.stderr.strip()[:300]}")
    dups = []

    def pairs(items):
        keys = [k for k, _ in items]
        dups.extend(k for k in set(keys) if keys.count(k) > 1)
        return dict(items)

    decoder, text, docs, i = json.JSONDecoder(object_pairs_hook=pairs), out.stdout, [], 0
    try:
        while text[i:].strip():
            i += len(text[i:]) - len(text[i:].lstrip())
            value, i = decoder.raw_decode(text, i)
            docs.append(value)
    except json.JSONDecodeError as e:
        raise Broken(f"yq's reading of {rel} is not JSON: {e}")
    return docs, sorted(dups)


def parameters(root):
    try:
        return tomllib.loads(read(root, PARAMETERS))
    except tomllib.TOMLDecodeError as e:
        raise Broken(f"{PARAMETERS} is not valid TOML: {e}")


_YAML = {}


def load_yaml(root, rel):
    """The YAML document at rel, read by yq, after the loader's guards."""
    raw = read_bytes(root, rel)
    for name, pattern in BYTE_CLASSES.items():
        refuse(f"yaml.bytes.{name}", re.search(pattern, raw), f"{rel} has {name} bytes")
    refuse("yaml.no-merge-keys", MERGE_KEY.search(raw), f"{rel} has a merge key")
    if raw not in _YAML:
        _YAML[raw] = yq(root, rel, "."), yq(root, rel, YQ_EXOTIC)[0]
    (docs, dups), exotic = _YAML[raw]
    refuse("yaml.one-document", len(docs) != 1, f"{rel} holds {len(docs)} documents, not one")
    refuse("yaml.no-duplicate-keys", dups, lambda: f"{rel} repeats keys {', '.join(dups)}")
    refuse("yaml.no-anchors", any(e["anchor"] for e in exotic), f"{rel} uses anchors or aliases")
    refuse("yaml.no-custom-tags", any(e["tag"] for e in exotic), f"{rel} uses tags outside the core schema")
    return docs[0] if docs else None


def mapping(x, where):
    refuse("shape.mapping", not isinstance(x, dict), f"{where} is not a mapping")
    return x


def at(x, where, *keys):
    """The value at keys, each level of which must be a mapping."""
    for key in keys:
        x = mapping(x, where).get(key)
        where = f"{where}.{key}"
    return x


def claude_paths(text):
    """The repository paths CLAUDE.md names: every backticked token with a slash or a file
    extension, whatever the extension. Each must be a plain relative path, read the same way by git
    and by GitHub's path filters."""
    named = re.findall(r"`([^` \t\n]+)`", text)
    paths = list(dict.fromkeys(p for p in named if "/" in p or re.search(r"\.[^./]+$", p)))
    for gid, test in (("claude.path.not-absolute", lambda p: p.startswith("/")),
                      ("claude.path.no-dot-slash", lambda p: p.startswith("./")),
                      ("claude.path.no-dot-dot", lambda p: ".." in p.split("/")),
                      ("claude.path.no-glob", lambda p: GLOB.search(p)),
                      ("claude.path.printable-ascii", lambda p: not (p.isascii() and p.isprintable()))):
        bad = [p for p in paths if test(p)]
        refuse(gid, bad, lambda: "CLAUDE.md names paths that are not plain repository paths: " + ", ".join(bad))
    return named, paths


def check_claude_pointer(root):
    """CLAUDE.md is a pointer file, neither a symlink nor a copy of AGENTS.md, that names
    `AGENTS.md`, and every repository path it names exists."""
    refuse("claude.not-symlink", (root / "CLAUDE.md").is_symlink(), "CLAUDE.md is a symlink")
    text = read(root, "CLAUDE.md")
    refuse("claude.not-copy", text == read(root, "AGENTS.md"), "CLAUDE.md is a copy of AGENTS.md")
    named, paths = claude_paths(text)
    refuse("claude.names-agents", "AGENTS.md" not in named, "CLAUDE.md does not point to `AGENTS.md`")
    missing = [p for p in paths if not (root / p).is_file()]
    refuse("claude.paths-exist", missing, lambda: "CLAUDE.md names paths that do not exist: " + ", ".join(missing))
    return "points to " + ", ".join(paths)


def check_shard_count(root):
    """The number of `mutants` jobs AGENTS.md names is the number of jobs ci.yml's mutants matrix
    runs. GitHub runs one job per combination after include and exclude, so the count is the
    shard list's length only for exactly that shape."""
    found = COUNT.findall(read(root, "AGENTS.md"))
    refuse("count.named", not found, "AGENTS.md does not name the count (\"after <count> `mutants` jobs\")")
    refuse("count.named-once", len(found) > 1, f"AGENTS.md names the `mutants` job count {len(found)} times")
    named = number(found[0]) if found else None
    strategy = mapping(at(load_yaml(root, CI), CI, "jobs", "mutants", "strategy"), "jobs.mutants.strategy")
    shaping = sorted(set(strategy) - {"matrix", "fail-fast", "max-parallel"})
    refuse("matrix.strategy-keys", shaping, lambda: "unsupported matrix shape: strategy has " + ", ".join(shaping))
    matrix = mapping(strategy.get("matrix"), "jobs.mutants.strategy.matrix")
    refuse("matrix.exactly-shard", set(matrix) != {"shard"},
           lambda: "unsupported matrix shape: the matrix holds " + ", ".join(matrix) + ", not exactly shard")
    shards = matrix.get("shard")
    refuse("matrix.shard-list", not isinstance(shards, list), "unsupported matrix shape: shard is not a list")
    items = list(shards) if isinstance(shards, (list, str)) else []
    refuse("matrix.shard-scalars", any(type(x) not in (int, str) for x in items),
           "unsupported matrix shape: a shard is not an integer or a string")
    refuse("matrix.shard-unique", len({json.dumps(x) for x in items}) != len(items),
           "unsupported matrix shape: the shard list repeats a value")
    refuse("count.matches", named is not None and named != len(items),
           lambda: f"AGENTS.md says {found[0]} ({named}) `mutants` jobs, but the matrix has {len(items)} shards")
    return f"AGENTS.md and {CI} both run {named} shards"


def check_triggers(root):
    """The workflow runs on exactly on.pull_request, on main, with paths that hold every governance
    path, every input of these checks and every path CLAUDE.md names."""
    on = mapping(at(load_yaml(root, WORKFLOW), WORKFLOW, "on"), "on")
    refuse("on.exactly-pull_request", set(on) != {"pull_request"},
           lambda: "`on` holds " + ", ".join(on) + ", not exactly pull_request")
    pr = mapping(on.get("pull_request"), "on.pull_request")
    refuse("on.exactly-branches-paths", set(pr) != {"branches", "paths"},
           lambda: "on.pull_request holds " + ", ".join(pr) + ", not exactly branches and paths")
    refuse("on.branches-main", pr.get("branches") != ["main"], "on.pull_request.branches is not [main]")
    paths = pr.get("paths")
    refuse("on.paths-strings", not isinstance(paths, list) or any(not isinstance(x, str) for x in paths),
           "on.pull_request.paths is not a list of strings")
    strings = [x for x in paths if isinstance(x, str)] if isinstance(paths, list) else []
    negated = [x for x in strings if x.startswith("!")]
    refuse("on.paths-no-negation", negated, lambda: "on.pull_request.paths negates " + ", ".join(negated))
    for p in GOVERNANCE_PATHS + CONTRACT_INPUTS:
        refuse(f"trigger.{p}", p not in strings, f"{WORKFLOW} does not run on {p}")
    named = [p for p in claude_paths(read(root, "CLAUDE.md"))[1] if p not in GOVERNANCE_PATHS + CONTRACT_INPUTS]
    missing = [p for p in named if p not in strings]
    refuse("trigger.claude-paths", missing, lambda: f"{WORKFLOW} does not run on " + ", ".join(missing))
    return f"{len(GOVERNANCE_PATHS + CONTRACT_INPUTS) + len(named)} paths run the job"


def check_rate_paths(root):
    """The L3 rate gate counts every governance path. It passes the list to `git log --`, where
    pathspec magic such as :(exclude), :! or :^ removes paths from the count: none is allowed."""
    patterns = mapping(at(parameters(root), PARAMETERS, "gates", "l3_paths"), "[gates.l3_paths]").get("patterns")
    refuse("rate.strings", not isinstance(patterns, list) or any(not isinstance(x, str) for x in patterns),
           "[gates.l3_paths] patterns is not a list of strings")
    strings = [x for x in patterns if isinstance(x, str)] if isinstance(patterns, list) else []
    unplain = [x for x in strings if not (x.isascii() and x.isprintable())]
    refuse("rate.printable-ascii", unplain, lambda: f"[gates.l3_paths] has unprintable patterns {unplain!r}")
    magic = [x for x in strings if x.startswith(":")]
    refuse("rate.no-pathspec-magic", magic, lambda: "[gates.l3_paths] uses pathspec magic: " + ", ".join(magic))
    for p in GOVERNANCE_PATHS:
        refuse(f"rate.{p}", p not in strings, f"[gates.l3_paths] does not count {p}")
    return f"{len(strings)} patterns count every governance path"


def check_levels(root):
    """Exactly the template's four levels, each parameter a finite number, each with a finite cached
    lambda. bstack's compute-lambda.sh --strict exits 0 with no levels (all([]) holds), on a level
    with no cached value, and on inf in a parameter and its cache (inf - inf is nan)."""
    data = parameters(root)
    levels = data.get("levels")
    refuse("levels.tables", not isinstance(levels, list) or any(not isinstance(x, dict) for x in levels),
           "[[levels]] is not a list of tables")
    ids = [x.get("id") for x in levels] if isinstance(levels, list) else []
    for level in LEVELS:
        refuse(f"levels.{level}", level not in ids, f"no level {level}")
    unknown = [i for i in ids if i not in LEVELS]
    refuse("levels.known", unknown, lambda: f"levels {unknown} are not the template's")
    repeated = sorted({str(i) for i in ids if ids.count(i) > 1})
    refuse("levels.once", repeated, lambda: f"levels {repeated} appear more than once")
    for p in LEVEL_PARAMETERS:
        values = [(lvl.get("id"), lvl.get(p)) for lvl in levels] if isinstance(levels, list) else []
        refuse(f"param.{p}.number", [i for i, v in values if type(v) not in (int, float)],
               f"a level's {p} is missing or not a number")
        refuse(f"param.{p}.finite", [i for i, v in values if type(v) in (int, float) and not math.isfinite(v)],
               f"a level's {p} is not finite")
    cached = mapping(at(data, PARAMETERS, "derived", "lambda"), "[derived.lambda]")
    for level in LEVELS:
        v = cached.get(level)
        refuse(f"cache.{level}", level not in cached, f"[derived.lambda] caches no {level}")
        refuse(f"cache.{level}.number", level in cached and type(v) not in (int, float),
               f"the cached {level} is not a number")
        refuse(f"cache.{level}.finite", type(v) in (int, float) and not math.isfinite(v),
               f"the cached {level} is not finite")
    extra = [k for k in cached if k not in LEVELS]
    refuse("cache.known", extra, lambda: f"[derived.lambda] caches {extra}, which are not levels")
    return "levels " + ", ".join(LEVELS) + ", finite, each with a finite cached lambda"


def check_bstack_calls(root):
    """The job fetches bstack at a pinned full SHA with the exact install recipe, and its only
    other mention of bstack or its scripts is the two exact calls, in their own steps."""
    steps = mapping(at(load_yaml(root, WORKFLOW), WORKFLOW, "jobs", "stability-check"),
                    "jobs.stability-check").get("steps")
    refuse("steps.list", not isinstance(steps, list) or any(not isinstance(s, dict) for s in steps),
           "jobs.stability-check.steps is not a list of steps")
    installs = [s for s in steps if s.get("name") == INSTALL_STEP]
    refuse("install.once", len(installs) != 1, f"{len(installs)} {INSTALL_STEP!r} steps, not one")
    install = installs[0] if installs else {}
    refuse("install.keys", set(install) != {"name", "env", "run"},
           lambda: "the install step holds " + ", ".join(install) + ", not exactly name, env and run")
    refuse("install.recipe", install.get("run") != INSTALL_RUN,
           "the install step's run is not the pinned fetch, detached checkout and verify recipe")
    env = install.get("env") if isinstance(install.get("env"), dict) else {}
    refuse("install.env", set(env) != {"BSTACK_REV"}, "the install step's env does not set exactly BSTACK_REV")
    rev = str(env.get("BSTACK_REV"))
    refuse("install.full-sha", not re.fullmatch(r"[0-9a-f]{40}", rev),
           f"BSTACK_REV is {rev!r}, not a full 40-character commit SHA")
    calls = [(s.get("id"), line.strip() if key == "run" else f"{key}: {line}")
             for s in steps if s.get("name") != INSTALL_STEP
             for key, value in s.items()
             for line in (value if isinstance(value, str) else json.dumps(value)).split("\n")
             if BSTACK_MENTION.search(line)]
    refuse("calls.exact", calls != BSTACK_CALLS,
           lambda: "outside the install step, the mentions of bstack are not exactly the bound, --strict "
                   "compute and rate calls: " + "; ".join(f"{i}: {c}" for i, c in calls))
    loose = [s.get("id") for s in steps if s.get("id") in ("compute", "rate") and set(s) != {"id", "name", "run"}]
    refuse("calls.step-keys", loose, lambda: f"the {', '.join(loose)} steps hold more than name, id and run")
    return f"bstack {rev[:12]}, fetched and called as pinned"


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
    fails closed, and no case expects it."""
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


# Above the mutation proof marker, only these functions may raise, assert, catch, exit or name
# Broken: the registry, the runner and the input readers.
NOT_GUARDS = {"register", "refuse", "run", "read", "read_bytes", "number", "yq", "parameters"}

# mutation proof below: the selftest, the seeds that make every guard's cases, and the proof.


RATE = '    "{}",\n'
FIXTURE_SHARDS = 3


class Tree:
    """The canonical fixture: a small tree that passes every check, generated from the constants
    above. The selftest and the proof seed every case into a fresh copy of it, so no case depends
    on how this repository's own files are written or quoted; run() is what checks those."""
    n = FIXTURE_SHARDS
    word = UNITS[FIXTURE_SHARDS]
    phrase = f"after {UNITS[FIXTURE_SHARDS]} `mutants` jobs"
    claude = "Read `AGENTS.md` (rules), `README.md` (commands) and `docs/spec.html` (design).\n"

    def files(self):
        paths = [*GOVERNANCE_PATHS, *CONTRACT_INPUTS, "README.md", "docs/spec.html"]

        def run_block(text):
            return "        run: |\n" + "".join(f"          {line}\n" for line in text.splitlines())

        workflow = ("name: stability\non:\n  pull_request:\n    branches: [main]\n    paths:\n"
                    + "".join(f"      - '{x}'\n" for x in paths)
                    + "concurrency:\n  group: stability\njobs:\n  stability-check:\n    name: stability-check\n    steps:\n"
                    + f"      - name: {INSTALL_STEP}\n        env:\n          BSTACK_REV: {'ab' * 20}\n" + run_block(INSTALL_RUN)
                    + "".join(f"\n      - name: {i}\n        id: {i}\n" + run_block(f"set +e\n{call}") for i, call in BSTACK_CALLS)
                    + "\n      - name: Report\n        run: echo done\n")
        levels = "".join(f'\n[[levels]]\nid = "{level}"\n' + "".join(f"{p} = 1.5\n" for p in LEVEL_PARAMETERS)
                         for level in LEVELS)
        parameters = ("schema_version = 1\n" + levels + "\n[derived.lambda]\n" + "".join(f"{x} = 0.5\n" for x in LEVELS)
                      + '\n[derived.omega]\nvalue = 0.5\nlevel = "L3"\n\n[gates.l3_paths]\npatterns = [\n'
                      + "".join(RATE.format(x) for x in GOVERNANCE_PATHS) + "]\n")
        return {
            "CLAUDE.md": self.claude,
            "AGENTS.md": f"# Rules\n\nCI runs the `ci` check {self.phrase} split the mutation check.\n",
            "README.md": "# Readme\n",
            "docs/spec.html": "<p>spec</p>\n",
            CI: "name: ci\njobs:\n  mutants:\n    strategy:\n      fail-fast: false\n      matrix:\n"
                f"        shard: [{', '.join(map(str, range(self.n)))}]\n",
            WORKFLOW: workflow,
            PARAMETERS: parameters,
        }


TREE = Tree()


def words(k):
    """k in the words number() reads ("twenty-four"); counts from one hundred are not supported."""
    if not 0 <= k < 100:
        raise Broken(f"the selftest grows the count to {k}; number() reads counts below one hundred")
    if k < 20:
        return UNITS[k]
    tens = next(w for w, v in TENS.items() if v == k // 10 * 10)
    return tens + ("-" + UNITS[k % 10] if k % 10 else "")


# Seeds: functions of (root, Tree) that change the copy at root.
def edit(rel, old, new):
    """Replace the one occurrence of old (a string, or a function of the Tree) in rel."""
    def apply(r, t):
        o, n = (old(t) if callable(old) else old), (new(t) if callable(new) else new)
        text = (r / rel).read_text(encoding="utf-8")
        if text.count(o) != 1:
            raise Broken(f"selftest seed anchor occurs {text.count(o)} times in {rel}: {o!r}")
        (r / rel).write_text(text.replace(o, n), encoding="utf-8")
    return apply


def resub(rel, pattern, repl, count=1):
    def apply(r, t):
        text = (r / rel).read_text(encoding="utf-8")
        new, k = re.subn(pattern, repl, text, count=count, flags=re.M)
        if not k:
            raise Broken(f"selftest seed anchor missing in {rel}: {pattern!r}")
        (r / rel).write_text(new, encoding="utf-8")
    return apply


def after(rel, pattern, *lines):
    """Add lines after the first line matching pattern (group 1 is its indentation), each indented
    as that line plus its own leading spaces."""
    return resub(rel, pattern, lambda m: m[0] + "".join("\n" + m[1] + line for line in lines))


def write(rel, text):
    def apply(r, t):
        (r / rel).parent.mkdir(parents=True, exist_ok=True)
        (r / rel).write_text(text(t) if callable(text) else text, encoding="utf-8")
    return apply


def raw_after(rel, pattern, data):
    def apply(r, t):
        b = (r / rel).read_bytes()
        m = re.search(pattern.encode(), b, re.M)
        if not m:
            raise Broken(f"selftest seed anchor missing in {rel}: {pattern!r}")
        (r / rel).write_bytes(b[:m.end()] + data + b[m.end():])
    return apply


def seq(*seeds):
    def apply(r, t):
        for s in seeds:
            s(r, t)
    return apply


def matrix(items):
    """The shard list as items, a function of the Tree."""
    return lambda r, t: resub(CI, SHARD, lambda m: f"{m[1]}shard: [{', '.join(map(str, items(t)))}]")(r, t)


def named(*paths):
    return edit("CLAUDE.md", "`AGENTS.md`", "`AGENTS.md` and " + " and ".join(f"`{p}`" for p in paths))


def triggered(*paths):
    return edit(WORKFLOW, "      - 'CLAUDE.md'\n", "      - 'CLAUDE.md'\n" + "".join(f"      - '{p}'\n" for p in paths))


def extra_step(*lines):
    return edit(WORKFLOW, "      - name: Report\n", "      - name: Extra\n        run: |\n"
                + "".join(f"          {x}\n" for x in lines) + "\n      - name: Report\n")


SHARD = r"^( *)shard *:.*$"
STRATEGY = r"^( *)strategy:$"
FAIL_FAST = r"^( *)fail-fast: false$"
BRANCHES = r"^( *)branches: \[main\]$"
LEVEL = r'^\[\[levels\]\]\nid *= *"{}"\n(?:\w+ *=.*\n)*'
WHY = ("inf", "-inf", "nan")


def copy_level(source, as_id):
    def apply(r, t):
        text = (r / PARAMETERS).read_text(encoding="utf-8")
        m = re.search(LEVEL.format(source), text, re.M)
        (r / PARAMETERS).write_text(text + "\n" + m[0].replace(f'"{source}"', f'"{as_id}"'), encoding="utf-8")
    return apply


def duplicate_install(r, t):
    text = (r / WORKFLOW).read_text(encoding="utf-8")
    step = re.search(r"(?ms)^      - name: Install bstack.*?\n\n", text)[0]
    edit(WORKFLOW, "      - name: Report\n", step + "      - name: Report\n")(r, t)


def symlink_to_pointer(r, t):
    (r / "POINTER.md").write_text(t.claude, encoding="utf-8")
    (r / "CLAUDE.md").unlink()
    (r / "CLAUDE.md").symlink_to("POINTER.md")


def absolute(r, t):
    path = f"{r.resolve()}/README.md"
    seq(named(path), triggered(path))(r, t)


# The registry: every guard and its own cases. Data-driven guards expand per entry.
WF, CC = ("trigger-paths", "bstack-calls"), ("claude-points-to-agents", "trigger-paths")
for name, data in (("tab", b" #\tx"), ("carriage-return", b"\r"), ("control", b" # \x7f"),
                   ("non-ascii", b"\xc2\xa0# x")):
    register(f"yaml.bytes.{name}", ["shard-count"], (f"ci.yml has {name} bytes", raw_after(CI, FAIL_FAST, data)))
register("yaml.no-merge-keys", ["shard-count"],
         ("ci.yml has a merge key", after(CI, FAIL_FAST, "<<: {}")),
         ('ci.yml has a quoted "<<" key', after(CI, FAIL_FAST, '"<<": {}')))
register("yaml.one-document", ["shard-count"],
         ("ci.yml holds two documents", lambda r, t: (r / CI).write_text((r / CI).read_text() + "---\nx: 1\n")))
register("yaml.no-duplicate-keys", ["shard-count"], ("ci.yml repeats a key", after(CI, FAIL_FAST, "fail-fast: false")))
register("yaml.no-anchors", ["shard-count"],
         ("ci.yml uses an anchor", lambda r, t: resub(CI, SHARD, r"\1shard: &s [" + ", ".join(map(str, range(t.n))) + "]")(r, t)))
register("yaml.no-custom-tags", ["shard-count"], ("ci.yml uses a custom tag", resub(CI, FAIL_FAST, r"\1fail-fast: !flag false")))
register("shape.mapping", ["rate-paths"], ("[gates] is a string", seq(
    resub(PARAMETERS, r"^\[gates\.l3_paths\]\npatterns = \[[^\]]*\]\n", ""),
    edit(PARAMETERS, "schema_version = 1\n", 'schema_version = 1\ngates = "x"\n'))))
# claude-points-to-agents
register("claude.not-symlink", ["claude-points-to-agents"], ("CLAUDE.md is a symlink to a separate pointer file", symlink_to_pointer))
register("claude.not-copy", ["claude-points-to-agents"], ("CLAUDE.md is a copy of an AGENTS.md that names only itself", seq(
    write("AGENTS.md", lambda t: f"Rules are in `AGENTS.md`, {t.phrase}.\n"),
    write("CLAUDE.md", lambda t: f"Rules are in `AGENTS.md`, {t.phrase}.\n"))))
register("claude.names-agents", ["claude-points-to-agents"], ("CLAUDE.md names only README.md", write("CLAUDE.md", "Read `README.md`.\n")))
register("claude.paths-exist", ["claude-points-to-agents"],
         ("CLAUDE.md names a path that does not exist", seq(named("docs/missing.md"), triggered("docs/missing.md"))),
         ("CLAUDE.md names a .txt file that does not exist", seq(named("RULES.txt"), triggered("RULES.txt"))))
register("claude.path.not-absolute", CC, ("CLAUDE.md names an absolute path", absolute))
register("claude.path.no-dot-slash", CC, ("CLAUDE.md names a ./ path", seq(named("./README.md"), triggered("./README.md"))))
register("claude.path.no-dot-dot", CC,
         ("CLAUDE.md names a path through ..", seq(named("docs/../README.md"), triggered("docs/../README.md"))))
register("claude.path.no-glob", CC,
         ("CLAUDE.md names a [glob]", seq(named("docs/[x].md"), triggered("docs/[x].md"), write("docs/[x].md", "x\n"))),
         ("CLAUDE.md names a + glob", seq(named("docs/x+.md"), triggered("docs/x+.md"), write("docs/x+.md", "x\n"))))
register("claude.path.printable-ascii", CC,
         ("CLAUDE.md names a non-ASCII path", seq(named("docs/é.md"), write("docs/é.md", "x\n"))))
# shard-count
register("count.named", ["shard-count"],
         ("AGENTS.md no longer names the count", edit("AGENTS.md", lambda t: t.phrase, "after the `mutants` run")))
register("count.named-once", ["shard-count"], ("AGENTS.md names the count twice", edit(
    "AGENTS.md", lambda t: t.phrase, lambda t: f"{t.phrase}, {t.phrase}")))
register("matrix.strategy-keys", ["shard-count"], ("strategy gains another key", after(CI, STRATEGY, "  x: 1")))
register("matrix.exactly-shard", ["shard-count"],
         ("the matrix excludes a shard", after(CI, SHARD, "exclude:", "  - shard: 0")),
         ("the matrix includes another job", after(CI, SHARD, "include:", "  - shard: 99")),
         ("the matrix gains a dimension", after(CI, SHARD, "os: [a, b]")))
register("matrix.shard-list", ["shard-count"], ("the shard list is a string", lambda r, t: resub(
    CI, SHARD, r"\1shard: " + "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"[:t.n])(r, t)))
register("matrix.shard-scalars", ["shard-count"],
         ("a shard is a boolean", matrix(lambda t: ["true"] + list(range(2, t.n + 1)))))
register("matrix.shard-unique", ["shard-count"],
         ("the shard list repeats a value", matrix(lambda t: list(range(t.n - 1)) + [0])))
register("count.matches", ["shard-count"],
         ("ci.yml drops a shard", matrix(lambda t: range(t.n - 1))),
         ("AGENTS.md names one shard more", edit("AGENTS.md", lambda t: t.phrase, lambda t: f"after {t.n + 1} `mutants` jobs")))
# trigger-paths
register("on.exactly-pull_request", WF[:1],
         ("push runs beside pull_request", edit(WORKFLOW, "concurrency:", "  push:\nconcurrency:")),
         ("pull_request_target runs beside pull_request", edit(WORKFLOW, "concurrency:", "  pull_request_target:\nconcurrency:")))
register("on.exactly-branches-paths", WF[:1],
         ("paths-ignore is added", after(WORKFLOW, BRANCHES, "paths-ignore:", "  - 'docs/**'")),
         ("types narrows the events", after(WORKFLOW, BRANCHES, "types: [closed]")))
register("on.branches-main", WF[:1], ("branches widens past main", edit(WORKFLOW, "branches: [main]", "branches: [main, dev]")))
register("on.paths-strings", WF[:1], ("a trigger path is a mapping", after(WORKFLOW, r"^( *)- 'CLAUDE.md'$", "- x: y")))
register("on.paths-no-negation", WF[:1],
         ("the trigger list negates AGENTS.md", after(WORKFLOW, r"^( *)- 'AGENTS.md'$", "- '!AGENTS.md'")))
for p in GOVERNANCE_PATHS + CONTRACT_INPUTS:
    register(f"trigger.{p}", WF[:1], (f"the workflow stops running on {p}", edit(WORKFLOW, f"      - '{p}'\n", "")))
register("trigger.claude-paths", WF[:1],
         ("CLAUDE.md names a file the workflow does not run on", seq(named("UNRUN.md"), write("UNRUN.md", "x\n"))),
         ("CLAUDE.md names a .txt file the workflow does not run on", seq(named("RULES.txt"), write("RULES.txt", "x\n"))))
# rate-paths
register("rate.strings", ["rate-paths"],
         ("a rate pattern is a number", edit(PARAMETERS, RATE.format("AGENTS.md"), RATE.format("AGENTS.md") + "    1,\n")))
register("rate.printable-ascii", ["rate-paths"], ("a rate pattern has a non-breaking space", edit(
    PARAMETERS, RATE.format("AGENTS.md"), RATE.format("AGENTS.md") + RATE.format("AGENTS.md "))))
register("rate.no-pathspec-magic", ["rate-paths"], *[
    (f"the rate list excludes AGENTS.md with {m}", edit(PARAMETERS, RATE.format("AGENTS.md"),
                                                          RATE.format("AGENTS.md") + RATE.format(f"{m}AGENTS.md")))
    for m in (":(exclude)", ":!", ":^")])
for p in GOVERNANCE_PATHS:
    register(f"rate.{p}", ["rate-paths"], (f"the rate gate stops counting {p}", edit(PARAMETERS, RATE.format(p), "")))
# rcs-levels
register("levels.tables", ["rcs-levels"], ("levels is a list of numbers", seq(
    resub(PARAMETERS, LEVEL.format(r"L\d"), "", count=0),
    edit(PARAMETERS, "schema_version = 1\n", "schema_version = 1\nlevels = [1, 2, 3, 4]\n"))))
for level in LEVELS:
    register(f"levels.{level}", ["rcs-levels"], (f"level {level} is removed", resub(PARAMETERS, LEVEL.format(level), "")))
register("levels.known", ["rcs-levels"], ("a level L4 is added", copy_level("L3", "L4")))
register("levels.once", ["rcs-levels"], ("level L0 appears twice", copy_level("L0", "L0")))
for k, p in enumerate(LEVEL_PARAMETERS):
    register(f"param.{p}.number", ["rcs-levels"],
             (f"L0's {p} is a boolean", resub(PARAMETERS, rf"^{p}( *)= .*$", rf"{p}\1= true")))
    register(f"param.{p}.finite", ["rcs-levels"],
             (f"L0's {p} is {WHY[k % 3]}, its cache finite", resub(PARAMETERS, rf"^{p}( *)= .*$", rf"{p}\1= {WHY[k % 3]}")))
for k, level in enumerate(LEVELS):
    register(f"cache.{level}", ["rcs-levels"], (f"the cached {level} is removed", resub(PARAMETERS, rf"^{level} = .*\n", "")))
    register(f"cache.{level}.number", ["rcs-levels"],
             (f"the cached {level} is a boolean", resub(PARAMETERS, rf"^{level} = .*$", f"{level} = true")))
    register(f"cache.{level}.finite", ["rcs-levels"],
             (f"the cached {level} is {WHY[(k + 2) % 3]}, its parameters finite",
              resub(PARAMETERS, rf"^{level} = .*$", f"{level} = {WHY[(k + 2) % 3]}")))
register("cache.known", ["rcs-levels"], ("a cached L4 is added", resub(PARAMETERS, r"^L3 = .*$", lambda m: m[0] + "\nL4 = 0.1")))
# bstack-calls
register("steps.list", ["bstack-calls"],
         ("a step is a string", edit(WORKFLOW, "      - name: Report\n", "      - just-a-string\n\n      - name: Report\n")))
register("install.once", ["bstack-calls"], ("the install step appears twice", duplicate_install))
register("install.keys", ["bstack-calls"],
         ("the install step gains continue-on-error", after(WORKFLOW, r"^( *)- name: Install bstack.*$", "  continue-on-error: true")))
register("install.recipe", ["bstack-calls"],
         ("the fetched bstack is not verified", resub(WORKFLOW, r"^ *test .*BSTACK_REV.*\n", "")),
         ("the checkout is not detached", edit(WORKFLOW, "checkout -q --detach FETCH_HEAD", "checkout -q FETCH_HEAD")))
register("install.env", ["bstack-calls"], ("the install step sets another variable", after(WORKFLOW, r"^( *)BSTACK_REV: .*$", "X: y")))
register("install.full-sha", ["bstack-calls"],
         ("bstack pinned to a branch", resub(WORKFLOW, r"(BSTACK_REV: )[0-9a-f]{40}", r"\1main")),
         ("bstack pinned to a short SHA", resub(WORKFLOW, r"(BSTACK_REV: [0-9a-f]{7})[0-9a-f]{33}", r"\1")))
register("calls.exact", ["bstack-calls"],
         ("compute-lambda.sh runs without --strict", edit(WORKFLOW, " --strict --human", " --human")),
         ("compute-lambda.sh reads no explicit config", edit(WORKFLOW, ' --config="$PWD/.control/rcs-parameters.toml"', "")),
         ("compute-lambda.sh inherits the environment", edit(WORKFLOW, BSTACK_ENV + "compute", 'bash "$BSTACK"/scripts/compute')),
         ("l3-rate-gate.sh inherits the environment", edit(WORKFLOW, BSTACK_ENV + "l3-rate", 'bash "$BSTACK"/scripts/l3-rate')),
         ("a second compute-lambda.sh without --strict", extra_step('bash "$BSTACK"/scripts/compute-lambda.sh --human')),
         ("an extra git clone, the verify line kept", extra_step("git clone https://github.com/broomva/bstack.git /tmp/b")),
         ("a step uses a bstack action", edit(WORKFLOW, "      - name: Report\n",
                                              "      - uses: broomva/bstack@main\n\n      - name: Report\n")))
register("calls.step-keys", ["bstack-calls"],
         ("the compute step gains continue-on-error", after(WORKFLOW, r"^( *)id: compute$", "continue-on-error: true")))

# Cases no guard owns: each must pass.
BENIGN = [
    ("unchanged tree", lambda r, t: None),
    ("strategy sets max-parallel", after(CI, STRATEGY, "  max-parallel: 4")),
    ("the shard list in block style", lambda r, t: resub(
        CI, SHARD, lambda m: f"{m[1]}shard:" + "".join(f"\n{m[1]}  - {i}" for i in range(t.n)))(r, t)),
    ("both sides grow by eight, in words", lambda r, t: seq(
        edit("AGENTS.md", t.phrase, f"after {words(t.n + 8)} `mutants` jobs"), matrix(lambda t: range(t.n + 8)))(r, t)),
    ("both sides grow by eight, in digits", lambda r, t: seq(
        edit("AGENTS.md", t.phrase, f"after {t.n + 8} `mutants` jobs"), matrix(lambda t: range(t.n + 8)))(r, t)),
    ("every trigger path double-quoted", resub(WORKFLOW, r"^(      - )'(.*)'$", r'\1"\2"', count=0)),
    ("every trigger path unquoted", resub(WORKFLOW, r"^(      - )'(.*)'$", r"\1\2", count=0)),
]


def cases():
    """(description, seed, the checks it must fail, the guard that owns it or None)."""
    return [(d, s, list(checks), gid) for gid, (checks, own) in GUARDS.items() for d, s in own] + \
           [(d, s, [], None) for d, s in BENIGN]


def seeded(seed, tmp):
    """A copy of the fixture at tmp, with seed applied."""
    for rel, text in TREE.files().items():
        (tmp / rel).parent.mkdir(parents=True, exist_ok=True)
        (tmp / rel).write_text(text, encoding="utf-8")
    seed(tmp, TREE)
    return tmp


def completeness(fixture):
    """The failures that make the registry incomplete: code above the marker that raises, asserts,
    catches, exits or names Broken outside NOT_GUARDS, and registered guards the unchanged fixture
    never evaluates."""
    source = Path(__file__).read_text(encoding="utf-8")
    checks = ast.parse(source.split("# mutation proof " + "below", 1)[0])
    found = []
    for fn in (n for n in ast.walk(checks) if isinstance(n, ast.FunctionDef) and n.name not in NOT_GUARDS):
        for node in ast.walk(fn):
            if isinstance(node, (ast.Raise, ast.Assert, ast.Try)) or (
                    isinstance(node, ast.Name) and node.id in ("Broken", "exit")) or (
                    isinstance(node, ast.Attribute) and node.attr in ("exit", "_exit")):
                found.append(f"{fn.name} line {node.lineno}: {type(node).__name__} outside refuse()")
    EVALUATED.clear()
    run(fixture, quiet=True)
    found += [f"guard {g} is registered but never evaluated" for g in GUARDS if g not in EVALUATED]
    return found


def selftest():
    """Every case's outcome on a seeded copy of the fixture, then completeness; the failures."""
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        for i, (desc, seed, want, gid) in enumerate(cases()):
            got = run(seeded(seed, Path(tmp) / str(i)), quiet=True)
            bad += got != want
            print(f"{'ok  ' if got == want else 'FAIL'} selftest: {desc}: {'fails ' + ', '.join(got) if got else 'passes'}"
                  + ("" if got == want else f" (want {'fails ' + ', '.join(want) if want else 'passes'})"))
        incomplete = completeness(seeded(lambda r, t: None, Path(tmp) / "fixture"))
    for line in incomplete:
        print(f"FAIL completeness: {line}")
    print(f"{'ok  ' if not incomplete else 'FAIL'} completeness: {len(GUARDS)} guards, every refusal goes through "
          "refuse(), and the unchanged fixture evaluates every guard")
    return bad + len(incomplete)


def mutation_proof():
    """Disable each guard alone: its own cases must change outcome, and every other case must keep
    its outcome. The baseline (no guard disabled) must give every case its want."""
    rows, crashed = [], 0
    with tempfile.TemporaryDirectory() as tmp:
        built = []
        for i, (desc, seed, want, gid) in enumerate(cases()):
            path = seeded(seed, Path(tmp) / str(i))
            built.append((desc, path, want, gid, run(path, quiet=True)))
        baseline = [d for d, _, want, _, got in built if got != want]
        for gid in GUARDS:
            DISABLED.clear()
            DISABLED.add(gid)
            own, others = [], []
            for desc, path, want, owner, base in built:
                got = run(path, quiet=True)
                (own if owner == gid else others).append((desc, got, base))
            DISABLED.clear()
            survived = [d for d, got, base in own if got == base]
            moved = [d for d, got, base in others if got != base]
            crash = any("(crashed)" in " ".join(got) for _, got, _ in own)
            crashed += crash and not survived
            rows.append((gid, len(own), "NO" if survived or moved else "yes (crash)" if crash else "yes",
                         survived, moved))
    print("guard | own cases | killed")
    for gid, n, killed, survived, moved in rows:
        print(f"{gid} | {n} | {killed}" + (f": survived {survived}" if survived else "")
              + (f": also moved {moved}" if moved else ""))
    killed = sum(r[2] != "NO" for r in rows)
    print(f"baseline: {len(built) - len(baseline)} of {len(built)} cases give their want"
          + (f"; not: {baseline}" if baseline else ""))
    print(f"{killed} of {len(rows)} guards proven ({killed - crashed} by a changed outcome, {crashed} by a crash), "
          f"each alone, with every other case unmoved")
    return len(rows) - killed + len(baseline)


def main(argv):
    here = Path(__file__).resolve().parent.parent
    try:
        if argv == ["--selftest"]:
            return 1 if selftest() else 0
        if argv == ["--mutation-proof"]:
            return 1 if mutation_proof() else 0
    except Broken as e:
        print(f"FAIL selftest: {e}")
        return 1
    if len(argv) > 1 or (argv and argv[0].startswith("-")):
        print("\n".join(__doc__.strip().splitlines()[2:5]), file=sys.stderr)
        return 2
    return 1 if run(Path(argv[0]) if argv else here) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
