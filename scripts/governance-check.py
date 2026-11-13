#!/usr/bin/env python3
"""Maestro's governance contract, run by the `stability-check` job (BRO-3702).

    python3 -I scripts/governance-check.py [ROOT]            # check the tree at ROOT (default: this checkout)
    python3 -I scripts/governance-check.py --selftest        # every case, the runner and completeness
    python3 -I scripts/governance-check.py --mutation-proof  # disable each guard alone

What is enforced, exactly:
- Every refusal is a registered guard: a check refuses only through refuse(gid, failing, message),
  against the registry GUARDS, and each gid has one refuse() call site. The input readers (read,
  read_bytes, read_bstack, yq, parameters) report input the checks cannot read; they are not guards.
- Each guard, disabled alone, fails its own cases, and no other case changes outcome. The cases
  are seeded into a fixture tree built from this checkout's live values (its shard list and count
  word, its trigger and rate paths, the paths CLAUDE.md names, its levels and their values, its
  bstack pin), written in one canonical format, so no case depends on how the files are quoted.
- A completeness scan (in --selftest) fails on any raise, assert, try, exit, quit, eval, exec or
  use of Broken above the mutation-proof marker outside refuse() and the readers; on code above the
  marker that names anything defined below it; on a gid used at two call sites; and on a registered
  guard the unchanged fixture never evaluates.
  The selftest and the proof run this scan first, and stop on a failure before any check's code
  runs.
- The data tables (GOVERNANCE_PATHS, CONTRACT_INPUTS, PLAIN, GLOB_CHARS, BYTE_CLASSES, MUTANTS_KEYS,
  STEPS, LEVELS) are part of the reviewed contract: deleting an entry removes its guard and its
  case together. LEVEL_PARAMETERS is the only table cross-checked, against the pinned bstack.
- A guard's predicate logic is verified by review, not proven correct for all inputs. This file
  comes from the PR it judges, like the workflow that runs it: it catches mistakes, and review of
  scripts/** and .github/workflows/** (p9's P20 floor) catches intent.

The bstack-parameters check reads $BSTACK/scripts/compute-lambda.sh, the pinned bstack the job
fetches before this step; to run the checks locally, set BSTACK to a bstack checkout at the pin.
Exit 0 when everything holds, 1 when something does not, 2 on usage. Needs Python 3.11 (tomllib)
and mikefarah yq v4; GitHub's ubuntu images ship both.
"""
import ast
import json
import math
import os
import re
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
# reads for each (the bstack-parameters check holds this list to the pinned script).
LEVELS = ("L0", "L1", "L2", "L3")
LEVEL_PARAMETERS = ("gamma", "L_theta", "rho", "L_d", "eta", "beta", "tau_bar", "nu", "tau_a")

# How the job reaches bstack. The install step is this recipe exactly (git in a cleared
# environment with a fixed PATH, so an inherited GIT_DIR or GIT_WORK_TREE cannot redirect it), and
# it hands the checkout's directory on as a step output, not through GITHUB_ENV. Outside it the
# only workflow lines that mention bstack or its scripts are these: the compute and rate calls, in
# a cleared environment with a fixed PATH bound to this checkout, compute-lambda.sh with --strict
# and its parameters, and the contract step reading the same directory. The steps run in STEPS
# order, so the PR's checker runs only after the pinned bstack's scripts.
INSTALL_STEP = "Install bstack (pinned, never from the PR)"
INSTALL_RUN = """set -euo pipefail
g() { env -i PATH=/usr/bin:/bin HOME="$HOME" git "$@"; }
DEST="$RUNNER_TEMP/bstack-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT"
rm -rf "$DEST"
g init -q "$DEST"
g -C "$DEST" fetch -q --depth 1 https://github.com/broomva/bstack.git "$BSTACK_REV"
g -C "$DEST" checkout -q --detach FETCH_HEAD
test "$(g -C "$DEST" rev-parse HEAD)" = "$BSTACK_REV"
echo "bstack $BSTACK_REV"
echo "dir=$DEST" >> "$GITHUB_OUTPUT"
"""
BSTACK_DIR = "${{ steps.install.outputs.dir }}"
BSTACK_ENV = f'env -i PATH=/usr/bin:/bin HOME="$HOME" BROOMVA_WORKSPACE="$PWD" bash "{BSTACK_DIR}"/scripts/'
BSTACK_CALLS = [
    ("compute", "out=$(" + BSTACK_ENV + "compute-lambda.sh --strict --human"
     ' --config="$PWD/.control/rcs-parameters.toml" 2>&1)'),
    ("rate", "out=$(" + BSTACK_ENV + "l3-rate-gate.sh 2>&1)"),
]
CONTRACT_BSTACK = ("contract", f'export BSTACK="{BSTACK_DIR}"')
# The first steps, in order (a step's id, else its name, else what it uses).
STEPS = ["actions/checkout@v4", "Python floor", "install", "compute", "rate", "contract"]
# The mutants job's keys: another (if, continue-on-error, needs, ...) could skip it or ignore it.
MUTANTS_KEYS = {"name", "runs-on", "timeout-minutes", "strategy", "env", "steps"}
BSTACK_MENTION = re.compile(r"bstack|compute-lambda|l3-rate-gate|git[ \t]+clone", re.I)

# Byte classes refused in the YAML files: parsers read them differently. yq itself rejects the C0
# controls other than tab, LF and CR, so DEL is the control byte only this guard can refuse.
BYTE_CLASSES = {
    "tab": rb"\t",
    "carriage-return": rb"\r",
    "control": rb"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]",
    "non-ascii": rb"[\x80-\xff]",
}
# A directive line (GitHub's parser honours %YAML 1.1, where yes, no and on are booleans; yq
# ignores it) and a document marker line (yq skips a leading empty document).
DIRECTIVE = re.compile(rb"^%", re.M)
DOCUMENT_MARKER = re.compile(rb"^(---|\.\.\.)([ \t]|$)", re.M)
# yq's decoded node tree, before it builds the JSON: a key whose value is `<<` in any spelling
# (yq merges it and drops it from the output; GitHub keeps it as a key), a key that is not a
# string, an anchor (an alias needs one), and a value tag outside the core schema.
YQ_NODES = ('{"merge": ([... | select(is_key and . == "<<")] | length), '
            '"key-tag": ([... | select(is_key and tag != "!!str")] | length), '
            '"anchor": ([... | select(anchor != "")] | length), '
            '"value-tag": ([... | select((is_key | not) and (tag | test("^(!!(map|seq|str|int|float|bool|null))?$")'
            ' | not))] | length)}')
# A plain relative path, as git and GitHub's path filters both read it: CLAUDE.md's paths and the
# rate patterns (git log exits 128 on a path outside the repository, and bstack counts that as 0).
PLAIN = {"not-absolute": lambda p: p.startswith("/"),
         "no-dot-slash": lambda p: p.startswith("./"),
         "no-dot-dot": lambda p: ".." in p.split("/"),
         "no-dot-segment": lambda p: "." in p.split("/")[1:],
         "no-empty-segment": lambda p: "" in p.split("/")[1:-1]}
# GitHub path filter and glob characters, refused in the paths CLAUDE.md names.
GLOB_CHARS = {"star": "*", "question": "?", "plus": "+", "bracket-open": "[", "bracket-close": "]",
              "brace-open": "{", "brace-close": "}", "bang": "!", "backslash": "\\"}

UNITS = ("zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen "
         "fifteen sixteen seventeen eighteen nineteen").split()
TENS = {"twenty": 20, "thirty": 30, "forty": 40, "fifty": 50,
        "sixty": 60, "seventy": 70, "eighty": 80, "ninety": 90}
COUNT = re.compile(r"\bafter[ \t]+([A-Za-z0-9]+(?:[ -][A-Za-z]+)??)[ \t]+`mutants`[ \t]+jobs\b")


class Broken(Exception):
    pass


# The registry. register() is called for every guard below the mutation-proof marker; refuse() is
# the only way a check refuses. SITES and CHECKED_BY record where each guard is evaluated.
GUARDS = {}       # id -> (the checks its cases fail, [(description, seed)])
DISABLED = set()  # the guard the mutation proof has disabled
EVALUATED, SITES, CHECKED_BY, STATE = set(), {}, {}, {"check": None}


def register(gid, checks, *cases):
    if gid in GUARDS or not cases:
        raise RuntimeError(f"guard {gid!r} is registered twice or without a case")
    GUARDS[gid] = (tuple(checks), list(cases))


def refuse(gid, failing, message):
    if gid not in GUARDS:
        raise RuntimeError(f"guard {gid!r} is not registered")
    EVALUATED.add(gid)
    SITES.setdefault(gid, set()).add(sys._getframe(1).f_lineno)
    CHECKED_BY.setdefault(gid, set()).add(STATE["check"])
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


def read_bstack():
    if not os.environ.get("BSTACK"):
        raise Broken("BSTACK is not set: the job sets it after fetching the pinned bstack; locally, "
                     "set it to a bstack checkout at the pin")
    return read(Path(os.environ["BSTACK"]), "scripts/compute-lambda.sh")


def yq(root, rel, query):
    """yq's reading of rel under query: one JSON value per document, and the keys repeated within
    one mapping, compared case-insensitively (GitHub's parser does)."""
    try:
        out = subprocess.run(["yq", "-o=json", "-I=0", query, str(root / rel)],
                             capture_output=True, text=True, timeout=60)
    except FileNotFoundError:
        raise Broken("yq not found: the checks read YAML with mikefarah yq v4")
    if out.returncode:
        raise Broken(f"yq cannot read {rel}: {out.stderr.strip()[:300]}")
    dups = []

    def pairs(items):
        keys = [k.casefold() for k, _ in items]
        dups.extend(k for k, _ in items if keys.count(k.casefold()) > 1)
        return dict(items)

    decoder, text, docs, i = json.JSONDecoder(object_pairs_hook=pairs), out.stdout, [], 0
    try:
        while text[i:].strip():
            i += len(text[i:]) - len(text[i:].lstrip())
            value, i = decoder.raw_decode(text, i)
            docs.append(value)
    except json.JSONDecodeError as e:
        raise Broken(f"yq's reading of {rel} is not JSON: {e}")
    return docs, sorted(set(dups))


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
    refuse("yaml.no-directive", DIRECTIVE.search(raw), f"{rel} has a % directive line")
    refuse("yaml.no-document-marker", DOCUMENT_MARKER.search(raw), f"{rel} has a --- or ... document marker line")
    if raw not in _YAML:
        _YAML[raw] = yq(root, rel, "."), yq(root, rel, YQ_NODES)[0]
    (docs, dups), nodes = _YAML[raw]
    refuse("yaml.no-duplicate-keys", dups, lambda: f"{rel} repeats keys, ignoring case: {', '.join(dups)}")
    for name, what in (("merge", "a merge key `<<`, in some spelling"), ("key-tag", "a key that is not a string"),
                       ("anchor", "anchors or aliases"), ("value-tag", "tags outside the core schema")):
        refuse(f"yaml.no-{name}", any(n[name] for n in nodes), f"{rel} uses {what}")
    return docs[0] if docs else None


def mapping(x, where):
    refuse(f"shape.{where}", not isinstance(x, dict), f"{where} is not a mapping")
    return x


def at(x, where, *keys):
    """The value at keys below the mapping x, each step of which must be a mapping."""
    for key in keys:
        where = f"{where}.{key}"
        x = mapping(x.get(key), where)
    return x


def claude_paths(text):
    """The repository paths CLAUDE.md names: every backticked token with a slash or a file
    extension, whatever the extension. Each must be a plain relative path, read the same way by
    git and by GitHub's path filters."""
    named = re.findall(r"`([^` \t\n]+)`", text)
    paths = list(dict.fromkeys(p for p in named if "/" in p or re.search(r"\.[^./]+$", p)))
    tests = {f"claude.path.{name}": test for name, test in PLAIN.items()}
    tests["claude.path.printable-ascii"] = lambda p: not (p.isascii() and p.isprintable())
    tests.update({f"claude.path.no-glob.{name}": (lambda c: lambda p: c in p)(c) for name, c in GLOB_CHARS.items()})
    for gid, test in tests.items():
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


def number(text):
    """A count written as digits or English words below one hundred ("sixteen", "twenty-four"), or
    None when it is neither."""
    t = text.lower()
    parts = re.split(r"[ -]", t)
    if re.fullmatch(r"[0-9]+", t):
        return int(t)
    if len(parts) == 1 and parts[0] in UNITS:
        return UNITS.index(parts[0])
    if len(parts) == 1 and parts[0] in TENS:
        return TENS[parts[0]]
    if len(parts) == 2 and parts[0] in TENS and parts[1] in UNITS[1:10]:
        return TENS[parts[0]] + UNITS.index(parts[1])
    return None


def check_shard_count(root):
    """The number of `mutants` jobs AGENTS.md names is the number of jobs ci.yml's mutants matrix
    runs. GitHub runs one job per combination after include and exclude, so the count is the
    shard list's length only for exactly that shape."""
    found = COUNT.findall(read(root, "AGENTS.md"))
    refuse("count.named", not found, "AGENTS.md does not name the count (\"after <count> `mutants` jobs\")")
    refuse("count.named-once", len(found) > 1, f"AGENTS.md names the `mutants` job count {len(found)} times")
    named = number(found[0]) if found else None
    refuse("count.readable", found and named is None, lambda: f"AGENTS.md's count {found[0]!r} is not a number below 100")
    mutants = at(mapping(load_yaml(root, CI), "ci"), "ci", "jobs", "mutants")
    refuse("ci.mutants-keys", set(mutants) != MUTANTS_KEYS,
           lambda: "jobs.mutants holds " + ", ".join(mutants) + ", not exactly " + ", ".join(sorted(MUTANTS_KEYS)))
    strategy = at(mutants, "ci.jobs.mutants", "strategy")
    shaping = sorted(set(strategy) - {"matrix", "fail-fast", "max-parallel"})
    refuse("matrix.strategy-keys", shaping, lambda: "unsupported matrix shape: strategy has " + ", ".join(shaping))
    matrix = at(strategy, "ci.jobs.mutants.strategy", "matrix")
    refuse("matrix.exactly-shard", set(matrix) != {"shard"},
           lambda: "unsupported matrix shape: the matrix holds " + ", ".join(matrix) + ", not exactly shard")
    shards = matrix.get("shard")
    items = shards if isinstance(shards, list) else []
    # Exactly `shard: [0, 1, ..., n-1]` in plain decimal, on one line: yq reads 1_0, 0x0 and +5 as
    # integers where GitHub's parser reads strings, and any other value or order is no shard index.
    lines = [x.strip() for x in read(root, CI).split("\n") if re.match(r" *shard *:", x)]
    canonical = "shard: [" + ", ".join(map(str, range(len(items)))) + "]"
    refuse("matrix.shard-canonical", lines != [canonical] or items != list(range(len(items))),
           lambda: f"unsupported matrix shape: the shard list is not written exactly as {canonical}")
    refuse("count.matches", named is not None and named != len(items),
           lambda: f"AGENTS.md says {found[0]} ({named}) `mutants` jobs, but the matrix has {len(items)} shards")
    return f"AGENTS.md and {CI} both run {named} shards"


def check_triggers(root):
    """The workflow runs on exactly on.pull_request, on main, with paths that hold every governance
    path, every input of these checks and every path CLAUDE.md names."""
    on = at(mapping(load_yaml(root, WORKFLOW), "workflow"), "workflow", "on")
    refuse("on.exactly-pull_request", set(on) != {"pull_request"},
           lambda: "`on` holds " + ", ".join(on) + ", not exactly pull_request")
    pr = at(on, "workflow.on", "pull_request")
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
    patterns = at(parameters(root), "parameters", "gates", "l3_paths").get("patterns")
    refuse("rate.strings", not isinstance(patterns, list) or any(not isinstance(x, str) for x in patterns),
           "[gates.l3_paths] patterns is not a list of strings")
    strings = [x for x in patterns if isinstance(x, str)] if isinstance(patterns, list) else []
    unplain = [x for x in strings if not (x.isascii() and x.isprintable())]
    refuse("rate.printable-ascii", unplain, lambda: f"[gates.l3_paths] has unprintable patterns {unplain!r}")
    magic = [x for x in strings if x.startswith(":")]
    refuse("rate.no-pathspec-magic", magic, lambda: "[gates.l3_paths] uses pathspec magic: " + ", ".join(magic))
    for name, test in PLAIN.items():
        bad = [x for x in strings if test(x)]
        refuse(f"rate.path.{name}", bad, lambda: "[gates.l3_paths] has patterns that are not plain paths: " + ", ".join(bad))
    for p in GOVERNANCE_PATHS:
        refuse(f"rate.{p}", p not in strings, f"[gates.l3_paths] does not count {p}")
    return f"{len(strings)} patterns count every governance path"


def check_levels(root):
    """Exactly the template's four levels, each parameter of each a finite number, each with a
    finite cached lambda. bstack's compute-lambda.sh --strict exits 0 with no levels (all([])
    holds), on a level with no cached value, and on inf in a parameter and its cache (inf - inf
    is nan, and nan > 1e-4 is false)."""
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
    by_id = {}
    for lvl in levels if isinstance(levels, list) else []:
        by_id.setdefault(lvl.get("id"), lvl)
    for level in LEVELS:
        for p in LEVEL_PARAMETERS:
            v = by_id.get(level, {}).get(p)
            refuse(f"param.{level}.{p}.number", level in by_id and type(v) not in (int, float),
                   f"{level}'s {p} is missing or not a number")
            refuse(f"param.{level}.{p}.finite", type(v) in (int, float) and not math.isfinite(v),
                   f"{level}'s {p} is not finite")
    cached = at(data, "parameters", "derived", "lambda")
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
    job = at(mapping(load_yaml(root, WORKFLOW), "workflow"), "workflow", "jobs", "stability-check")
    steps = job.get("steps")
    refuse("steps.list", not isinstance(steps, list) or any(not isinstance(s, dict) for s in steps),
           "jobs.stability-check.steps is not a list of steps")
    installs = [s for s in steps if s.get("name") == INSTALL_STEP]
    refuse("install.once", len(installs) != 1, f"{len(installs)} {INSTALL_STEP!r} steps, not one")
    install = installs[0] if installs else {}
    refuse("install.keys", set(install) != {"name", "id", "env", "run"},
           lambda: "the install step holds " + ", ".join(install) + ", not exactly name, id, env and run")
    refuse("install.recipe", install.get("run") != INSTALL_RUN,
           "the install step's run is not the pinned fetch, detached checkout and verify recipe")
    env = install.get("env") if isinstance(install.get("env"), dict) else {}
    refuse("install.env", set(env) != {"BSTACK_REV"}, "the install step's env does not set exactly BSTACK_REV")
    rev = str(env.get("BSTACK_REV"))
    refuse("install.full-sha", not re.fullmatch(r"[0-9a-f]{40}", rev),
           f"BSTACK_REV is {rev!r}, not a full 40-character commit SHA")
    order = [s.get("id") or s.get("name") or s.get("uses") for s in steps][:len(STEPS)]
    refuse("calls.order", order != STEPS, lambda: f"the first steps are {order}, not {STEPS}")
    calls = [(s.get("id"), line.strip() if key == "run" else f"{key}: {line}")
             for s in steps if s.get("name") != INSTALL_STEP
             for key, value in s.items()
             for line in (value if isinstance(value, str) else json.dumps(value)).split("\n")
             if BSTACK_MENTION.search(line)]
    refuse("calls.exact", sorted(calls, key=repr) != sorted(BSTACK_CALLS + [CONTRACT_BSTACK], key=repr),
           lambda: "outside the install step, the mentions of bstack are not exactly the bound, --strict "
                   "compute and rate calls: " + "; ".join(f"{i}: {c}" for i, c in calls))
    for step_id, _ in BSTACK_CALLS:
        loose = [s for s in steps if s.get("id") == step_id and set(s) != {"id", "name", "run"}]
        refuse(f"calls.step-keys.{step_id}", loose, f"the {step_id} step holds more than name, id and run")
    return f"bstack {rev[:12]}, fetched and called as pinned"


def check_bstack_parameters(root):
    """LEVEL_PARAMETERS is exactly what the pinned compute-lambda.sh reads from each level."""
    reads = set(re.findall(r"""lvl\[["'](\w+)["']\]""", read_bstack())) - {"id"}
    for p in LEVEL_PARAMETERS:
        refuse(f"bstack.reads.{p}", p not in reads, f"the pinned compute-lambda.sh does not read {p}")
    extra = sorted(reads - set(LEVEL_PARAMETERS))
    refuse("bstack.reads-only-these", extra, lambda: f"the pinned compute-lambda.sh also reads {', '.join(extra)}")
    return "the pinned compute-lambda.sh reads exactly " + ", ".join(LEVEL_PARAMETERS)


CHECKS = (
    ("claude-points-to-agents", check_claude_pointer),
    ("shard-count", check_shard_count),
    ("trigger-paths", check_triggers),
    ("rate-paths", check_rate_paths),
    ("rcs-levels", check_levels),
    ("bstack-calls", check_bstack_calls),
    ("bstack-parameters", check_bstack_parameters),
)


def run(root, quiet=False, checks=CHECKS):
    """The names of the checks that failed. A check that crashes, or exits (SystemExit, which quit()
    and sys.exit() raise), fails as `<name> (crashed)`: it fails closed, and no case but the
    runner's own expects it."""
    failed = []
    for name, check in checks:
        STATE["check"] = name
        try:
            detail = check(root)
        except (Exception, SystemExit) as e:
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
NOT_GUARDS = {"register", "refuse", "run", "read", "read_bytes", "read_bstack", "yq", "parameters"}
# Names a check may not use, and attributes it may not call (sys.exit, os._exit, os.exec*,
# os.spawn*, os.kill, contextlib.suppress).
FORBIDDEN = {"Broken", "exit", "quit", "eval", "exec", "compile", "__import__", "globals", "vars",
             "locals", "getattr", "setattr", "delattr", "breakpoint", "suppress"}
FORBIDDEN_ATTRIBUTES = re.compile(r"^(_?exit|exec\w*|spawn\w*|kill|suppress)$")

# mutation proof below: the fixture, the seeds that make every guard's cases, and the proof.


class Tree:
    """The fixture: this checkout's live values, written in one canonical format. The selftest and
    the proof seed every case into a fresh copy of it."""
    def __init__(self, root):
        try:
            self.word = COUNT.findall(read(root, "AGENTS.md"))[0]
            self.shards = load_yaml(root, CI)["jobs"]["mutants"]["strategy"]["matrix"]["shard"]
            workflow = load_yaml(root, WORKFLOW)
            self.triggers = workflow["on"]["pull_request"]["paths"]
            self.rev = next(s for s in workflow["jobs"]["stability-check"]["steps"]
                            if s.get("name") == INSTALL_STEP)["env"]["BSTACK_REV"]
            data = parameters(root)
            self.levels = [{"id": x["id"], **{p: x[p] for p in LEVEL_PARAMETERS}} for x in data["levels"]]
            self.cached = dict(data["derived"]["lambda"])
            self.patterns = data["gates"]["l3_paths"]["patterns"]
            self.claude_paths = claude_paths(read(root, "CLAUDE.md"))[1]
        except (Broken, KeyError, TypeError, IndexError, StopIteration) as e:
            raise Broken(f"the fixture is built from this checkout's values, which must pass the checks: "
                         f"{type(e).__name__}: {e}")
        self.n = len(self.shards)
        self.phrase = f"after {self.word} `mutants` jobs"
        self.claude = "Read " + " and ".join(f"`{p}`" for p in self.claude_paths) + ".\n"

    def files(self):
        def run_block(text):
            return "        run: |\n" + "".join(f"          {line}\n" for line in text.splitlines())

        workflow = ("name: stability\non:\n  pull_request:\n    branches: [main]\n    paths:\n"
                    + "".join(f"      - '{x}'\n" for x in self.triggers)
                    + "concurrency:\n  group: stability\njobs:\n  stability-check:\n    name: stability-check\n"
                    + "    steps:\n      - uses: actions/checkout@v4\n\n      - name: Python floor\n        run: python3 -VV\n"
                    + f"\n      - name: {INSTALL_STEP}\n        id: install\n        env:\n          BSTACK_REV: {self.rev}\n"
                    + run_block(INSTALL_RUN)
                    + "".join(f"\n      - name: {i}\n        id: {i}\n" + run_block(f"set +e\n{call}") for i, call in BSTACK_CALLS)
                    + "\n      - name: contract\n        id: contract\n"
                    + run_block(f"{CONTRACT_BSTACK[1]}\npython3 -I scripts/governance-check.py")
                    + "\n      - name: Report\n        run: echo done\n")
        levels = "".join(f'\n[[levels]]\nid = "{x["id"]}"\n' + "".join(f"{p} = {x[p]!r}\n" for p in LEVEL_PARAMETERS)
                         for x in self.levels)
        params = ("schema_version = 1\n" + levels + "\n[derived.lambda]\n"
                  + "".join(f"{k} = {v!r}\n" for k, v in self.cached.items())
                  + '\n[derived.omega]\nvalue = 0.5\nlevel = "L3"\n\n[gates.l3_paths]\npatterns = [\n'
                  + "".join(RATE.format(x) for x in self.patterns) + "]\n")
        script = ('for lvl in data.get("levels", []):\n    try:\n'
                  + "".join(f'        {p} = lvl["{p}"]\n' for p in LEVEL_PARAMETERS)
                  + '    except KeyError:\n        pass\n    results.append({"id": lvl["id"]})\n')
        files = {p: "x\n" for p in self.claude_paths}
        files.update({
            "CLAUDE.md": self.claude,
            "AGENTS.md": f"# Rules\n\nCI runs the `ci` check {self.phrase} split the mutation check.\n",
            CI: "name: ci\njobs:\n  mutants:\n    name: mutants\n    runs-on: x\n    timeout-minutes: 60\n    env:\n      A: b\n"
                "    steps:\n      - run: x\n    strategy:\n      fail-fast: false\n      matrix:\n"
                f"        shard: [{', '.join(json.dumps(x) for x in self.shards)}]\n",
            WORKFLOW: workflow,
            PARAMETERS: params,
            SCRIPT: script,
        })
        return files


def words(k):
    """k in the words number() reads ("twenty-four"); counts from one hundred are not supported."""
    if not 0 <= k < 100:
        raise Broken(f"the selftest grows the count to {k}; number() reads counts below one hundred")
    if k < 20:
        return UNITS[k]
    tens = next(w for w, v in TENS.items() if v == k // 10 * 10)
    return tens + ("-" + UNITS[k % 10] if k % 10 else "")


# Seeds: functions of (root, Tree) that change the fixture copy at root.
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
    """re.sub pattern with repl (a string, or a function of the match) in rel; it must match."""
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


def append(rel, text):
    def apply(r, t):
        (r / rel).write_text((r / rel).read_text(encoding="utf-8") + text, encoding="utf-8")
    return apply


def prepend(rel, text):
    def apply(r, t):
        (r / rel).write_text(text + (r / rel).read_text(encoding="utf-8"), encoding="utf-8")
    return apply


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


def live(t):
    """The live shard list, as YAML scalars."""
    return [json.dumps(x) for x in t.shards]


def matrix(items):
    """The shard list as items(t), YAML scalars."""
    return lambda r, t: resub(CI, SHARD, lambda m: f"{m[1]}shard: [{', '.join(items(t))}]")(r, t)


def named(path, last=False):
    """CLAUDE.md also names path, before AGENTS.md or at its end."""
    return append("CLAUDE.md", f"See `{path}`.\n") if last else edit("CLAUDE.md", "`AGENTS.md`", f"`{path}` and `AGENTS.md`")


def triggered(path, last=False):
    line = f"      - '{path}'\n"
    return edit(WORKFLOW, "concurrency:", line + "concurrency:") if last else edit(WORKFLOW, "    paths:\n", "    paths:\n" + line)


def rated(pattern, last=False):
    line = RATE.format(pattern)
    return edit(PARAMETERS, "\n]\n", "\n" + line + "]\n") if last else edit(PARAMETERS, "patterns = [\n", "patterns = [\n" + line)


def extra_step(*lines):
    return edit(WORKFLOW, "      - name: Report\n", "      - name: Extra\n        run: |\n"
                + "".join(f"          {x}\n" for x in lines) + "\n      - name: Report\n")


def ends(make):
    """Two cases from make(last): the offending entry first in its list, and last."""
    first, last = make(False), make(True)
    return [(f"{first[0]} (first)", first[1]), (f"{last[0]} (last)", last[1])]


def unplain(path, file=None, trigger=True):
    """CLAUDE.md names path, first and last; file is created and path triggered as asked."""
    return ends(lambda last: (f"CLAUDE.md names {path!r}", seq(
        named(path, last), *([write(file, "x\n")] if file else []), *([triggered(path)] if trigger else []))))


def absolute(last):
    def apply(r, t):
        p = f"{r.resolve()}/AGENTS.md"
        seq(named(p, last), triggered(p))(r, t)
    return ("CLAUDE.md names an absolute path", apply)


def in_level(level, p, value):
    return resub(PARAMETERS, rf'(^\[\[levels\]\]\nid = "{level}"\n(?:\w+ = .*\n)*?){p} = .*$', rf"\g<1>{p} = {value}")


def copy_level(source, as_id, first=False):
    def apply(r, t):
        text = (r / PARAMETERS).read_text(encoding="utf-8")
        block = re.search(LEVEL.format(source), text, re.M)[0].replace(f'"{source}"', f'"{as_id}"')
        text = text.replace("\n[[levels]]", "\n" + block + "\n[[levels]]", 1) if first else text + "\n" + block
        (r / PARAMETERS).write_text(text, encoding="utf-8")
    return apply


def duplicate_install(r, t):
    text = (r / WORKFLOW).read_text(encoding="utf-8")
    step = re.search(r"(?ms)^      - name: Install bstack.*?\n\n", text)[0]
    edit(WORKFLOW, "      - name: Report\n", step + "      - name: Report\n")(r, t)


def symlink_to_pointer(r, t):
    (r / "POINTER.md").write_text(t.claude, encoding="utf-8")
    (r / "CLAUDE.md").unlink()
    (r / "CLAUDE.md").symlink_to("POINTER.md")


SHARD = r"^( *)shard *:.*$"
STRATEGY = r"^( *)strategy:$"
FAIL_FAST = r"^( *)fail-fast: false$"
NAME = r"^()name: ci$"
BRANCHES = r"^( *)branches: \[main\]$"
LEVEL = r'^\[\[levels\]\]\nid = "{}"\n(?:\w+ = .*\n)*'
RATE = '    "{}",\n'
SCRIPT = "bstack/scripts/compute-lambda.sh"
TOP = "schema_version = 1\n"
WHY = ("inf", "-inf", "nan")
WF, CC = ("trigger-paths", "bstack-calls"), ("claude-points-to-agents", "trigger-paths")
SC, LV, RP, BC, BP = ["shard-count"], ["rcs-levels"], ["rate-paths"], ["bstack-calls"], ["bstack-parameters"]

# The registry: every guard and its own cases. Data-driven guards expand per entry.
for name, data in (("tab", b" #\tx"), ("carriage-return", b"\r"), ("control", b" # \x7f"), ("non-ascii", b"\xc2\xa0# x")):
    register(f"yaml.bytes.{name}", SC, (f"ci.yml has {name} bytes", raw_after(CI, FAIL_FAST, data)))
register("yaml.no-directive", SC, ("ci.yml has a %YAML 1.1 directive", prepend(CI, "%YAML 1.1\n")))
register("yaml.no-document-marker", SC, ("ci.yml starts with ---", prepend(CI, "---\n")),
         ("ci.yml ends with ...", append(CI, "...\n")), ("ci.yml holds two documents", append(CI, "---\nx: 1\n")))
register("yaml.no-duplicate-keys", SC, ("ci.yml repeats a key", after(CI, FAIL_FAST, "fail-fast: false")),
         ("ci.yml has Name beside name", after(CI, NAME, "Name: x")))
register("yaml.no-merge", SC, ('ci.yml has a quoted "<<" key', after(CI, FAIL_FAST, '"<<": {}')),
         (r'ci.yml has an escaped "\x3c\x3c" matrix dimension', after(CI, SHARD, r'"\x3c\x3c": [a, b]')),
         ('ci.yml has an explicit ? "<<" key', after(CI, FAIL_FAST, '? "<<"', ": {}")))
register("yaml.no-key-tag", SC, *[(f"ci.yml has a {k} key", after(CI, NAME, f"{k}: x")) for k in ("true", "~", "1", "null")])
register("yaml.no-anchor", SC, ("ci.yml uses an anchor", resub(CI, FAIL_FAST, r"\1fail-fast: &f false")))
register("yaml.no-value-tag", SC, ("ci.yml uses a custom tag", resub(CI, FAIL_FAST, r"\1fail-fast: !flag false")))
# One guard per mapping the checks walk; each case makes that node a scalar the next step crashes on.
for where, checks, seed in (
        ("ci", SC, write(CI, "0\n")),
        ("ci.jobs", SC, write(CI, "name: ci\njobs: 0\n")),
        ("ci.jobs.mutants", SC, write(CI, "name: ci\njobs:\n  mutants: 0\n")),
        ("ci.jobs.mutants.strategy", SC, resub(CI, r"^    strategy:\n(?:      .*\n)+", "    strategy: 0\n")),
        ("ci.jobs.mutants.strategy.matrix", SC, resub(CI, r"^      matrix:\n(?:        .*\n)+", "      matrix: 0\n")),
        ("workflow", WF, write(WORKFLOW, "0\n")),
        ("workflow.on", WF[:1], resub(WORKFLOW, r"^on:\n(?:  .*\n)+", "on: 0\n")),
        ("workflow.on.pull_request", WF[:1], resub(WORKFLOW, r"^  pull_request:\n(?:    .*\n)+", "  pull_request: 0\n")),
        ("workflow.jobs", WF[1:], resub(WORKFLOW, r"^jobs:\n(?:(?:  .*)?\n)+", "jobs: 0\n")),
        ("workflow.jobs.stability-check", WF[1:], resub(WORKFLOW, r"^  stability-check:\n(?:(?:    .*)?\n)+",
                                                        "  stability-check: 0\n")),
        ("parameters.gates", RP, seq(resub(PARAMETERS, r"^\[gates\.l3_paths\]\n", "[x]\n"),
                                     edit(PARAMETERS, TOP, TOP + 'gates = "x"\n'))),
        ("parameters.gates.l3_paths", RP, resub(PARAMETERS, r"^\[gates\.l3_paths\]\npatterns = \[\n(?:    .*\n)*\]\n",
                                                '[gates]\nl3_paths = "x"\n')),
        ("parameters.derived", LV, seq(resub(PARAMETERS, r"^\[derived\.lambda\]\n(?:L\d = .*\n)+", ""),
                                       resub(PARAMETERS, r"^\[derived\.omega\]\n(?:\w+ = .*\n)+", ""),
                                       edit(PARAMETERS, TOP, TOP + 'derived = "x"\n'))),
        ("parameters.derived.lambda", LV, resub(PARAMETERS, r"^\[derived\.lambda\]\n(?:L\d = .*\n)+", '[derived]\nlambda = "x"\n'))):
    register(f"shape.{where}", checks, (f"{where} is a scalar", seed))
# claude-points-to-agents
register("claude.not-symlink", ["claude-points-to-agents"], ("CLAUDE.md is a symlink to a separate pointer file", symlink_to_pointer))
register("claude.not-copy", ["claude-points-to-agents"], ("CLAUDE.md is a copy of an AGENTS.md that names only itself", seq(
    write("AGENTS.md", lambda t: f"Rules are in `AGENTS.md`, {t.phrase}.\n"),
    write("CLAUDE.md", lambda t: f"Rules are in `AGENTS.md`, {t.phrase}.\n"))))
register("claude.names-agents", ["claude-points-to-agents"], ("CLAUDE.md names everything but AGENTS.md", write(
    "CLAUDE.md", lambda t: "Read " + (" and ".join(f"`{p}`" for p in t.claude_paths if p != "AGENTS.md") or "nothing") + ".\n")))
register("claude.paths-exist", ["claude-points-to-agents"],
         *ends(lambda last: ("CLAUDE.md names a missing file", seq(named("missing.md", last), triggered("missing.md")))),
         ("CLAUDE.md names a missing .txt file", seq(named("RULES.txt"), triggered("RULES.txt"))))
register("claude.path.not-absolute", CC, *ends(absolute))
register("claude.path.no-dot-slash", CC, *unplain("./AGENTS.md"))
register("claude.path.no-dot-dot", CC, *unplain("x/../AGENTS.md", "x/keep"))
register("claude.path.no-dot-segment", CC, *unplain("x/./AGENTS.md", "x/AGENTS.md"))
register("claude.path.no-empty-segment", CC, *unplain("x//AGENTS.md", "x/AGENTS.md"))
register("claude.path.printable-ascii", CC, *unplain("xé.md", "xé.md", trigger=False))
for name, c in GLOB_CHARS.items():
    register(f"claude.path.no-glob.{name}", CC, *unplain(f"x{c}.md", f"x{c}.md"))
# shard-count
register("count.named", SC, ("AGENTS.md no longer names the count", edit("AGENTS.md", lambda t: t.phrase, "after the `mutants` run")))
register("count.named-once", SC, ("AGENTS.md names the count twice",
                                  edit("AGENTS.md", lambda t: t.phrase, lambda t: f"{t.phrase}, {t.phrase}")))
register("count.readable", SC, ("AGENTS.md names a count that is not a number",
                                edit("AGENTS.md", lambda t: t.phrase, "after many `mutants` jobs")))
register("ci.mutants-keys", SC,
         ("the mutants job gains if: false", after(CI, r"^( *)name: mutants$", "if: ${{ false }}")),
         ("the mutants job gains continue-on-error", after(CI, r"^( *)name: mutants$", "continue-on-error: true")))
register("matrix.strategy-keys", SC, ("strategy gains another key", after(CI, STRATEGY, "  x: 1")))
register("matrix.exactly-shard", SC,
         ("the matrix excludes a shard", after(CI, SHARD, "exclude:", "  - shard: 0")),
         ("the matrix includes another job", after(CI, SHARD, "include:", "  - shard: 99")),
         ("the matrix gains a dimension", after(CI, SHARD, "os: [a, b]")))
register("matrix.shard-canonical", SC,
         ("the first shard is written 0x0", matrix(lambda t: ["0x0"] + live(t)[1:])),
         ("the last shard is written with an underscore or a sign", matrix(lambda t: live(t)[:-1] + [
             f"{t.n - 1}"[0] + "_" + f"{t.n - 1}"[1:] if t.n > 10 else f"+{t.n - 1}"])),
         ("the shard list in block style", lambda r, t: resub(
             CI, SHARD, lambda m: f"{m[1]}shard:" + "".join(f"\n{m[1]}  - {x}" for x in live(t)))(r, t)),
         ("the shard list repeats a value", matrix(lambda t: live(t)[:-1] + live(t)[:1])),
         ("a shard is a string", matrix(lambda t: ['"0"'] + live(t)[1:])),
         ("a shard is a boolean", matrix(lambda t: live(t)[:-1] + ["true"])))
register("count.matches", SC, ("ci.yml drops a shard", matrix(lambda t: live(t)[:-1])),
         ("AGENTS.md names one shard more", edit("AGENTS.md", lambda t: t.phrase, lambda t: f"after {t.n + 1} `mutants` jobs")))
# trigger-paths
register("on.exactly-pull_request", WF[:1],
         ("push runs beside pull_request", edit(WORKFLOW, "concurrency:", "  push:\nconcurrency:")),
         ("pull_request_target runs beside pull_request", edit(WORKFLOW, "concurrency:", "  pull_request_target:\nconcurrency:")))
register("on.exactly-branches-paths", WF[:1],
         ("paths-ignore is added", after(WORKFLOW, BRANCHES, "paths-ignore:", "  - 'docs/**'")),
         ("types narrows the events", after(WORKFLOW, BRANCHES, "types: [closed]")))
register("on.branches-main", WF[:1], ("branches widens past main", edit(WORKFLOW, "branches: [main]", "branches: [main, dev]")))
register("on.paths-strings", WF[:1], *ends(lambda last: ("a trigger path is a mapping", edit(
    WORKFLOW, "concurrency:", "      - x: y\nconcurrency:") if last else edit(WORKFLOW, "    paths:\n", "    paths:\n      - x: y\n"))))
register("on.paths-no-negation", WF[:1], *ends(lambda last: ("the trigger list negates AGENTS.md", triggered("!AGENTS.md", last))))
for p in GOVERNANCE_PATHS + CONTRACT_INPUTS:
    register(f"trigger.{p}", WF[:1], (f"the workflow stops running on {p}", edit(WORKFLOW, f"      - '{p}'\n", "")))
register("trigger.claude-paths", WF[:1],
         *ends(lambda last: ("CLAUDE.md names a file the workflow does not run on",
                             seq(named("UNRUN.md", last), write("UNRUN.md", "x\n")))),
         ("CLAUDE.md names a .txt file the workflow does not run on", seq(named("RULES.txt"), write("RULES.txt", "x\n"))))
# rate-paths
register("rate.strings", RP, *ends(lambda last: ("a rate pattern is a number", edit(
    PARAMETERS, "\n]\n", "\n    1,\n]\n") if last else edit(PARAMETERS, "patterns = [\n", "patterns = [\n    1,\n"))))
register("rate.printable-ascii", RP, *ends(lambda last: ("a rate pattern has a non-breaking space", rated("AGENTS.md ", last))))
register("rate.no-pathspec-magic", RP, *ends(lambda last: ("the rate list excludes AGENTS.md", rated(":(exclude)AGENTS.md", last))),
         ("the rate list excludes with :!", rated(":!AGENTS.md")), ("the rate list excludes with :^", rated(":^AGENTS.md")))
for name, bad in (("not-absolute", "/x"), ("no-dot-slash", "./x"), ("no-dot-dot", "../x"),
                  ("no-dot-segment", "a/./b"), ("no-empty-segment", "a//b")):
    register(f"rate.path.{name}", RP, *ends(lambda last, bad=bad: (f"a rate pattern is {bad!r}", rated(bad, last))))
for p in GOVERNANCE_PATHS:
    register(f"rate.{p}", RP, (f"the rate gate stops counting {p}", edit(PARAMETERS, RATE.format(p), "")))
# rcs-levels
register("levels.tables", LV, ("levels is a list of numbers", seq(
    resub(PARAMETERS, LEVEL.format(r"L\d"), "", count=0), edit(PARAMETERS, TOP, TOP + "levels = [1, 2, 3, 4]\n"))))
for level in LEVELS:
    register(f"levels.{level}", LV, (f"level {level} is removed", resub(PARAMETERS, LEVEL.format(level), "")))
register("levels.known", LV, *ends(lambda last: ("a level L4 is added", copy_level("L3", "L4", first=not last))))
register("levels.once", LV, ("level L0 appears twice", copy_level("L0", "L0")))
for k, (level, p) in enumerate((level, p) for level in LEVELS for p in LEVEL_PARAMETERS):
    register(f"param.{level}.{p}.number", LV, (f"{level}'s {p} is a boolean", in_level(level, p, "true")))
    register(f"param.{level}.{p}.finite", LV, (f"{level}'s {p} is {WHY[k % 3]}, its cache finite", in_level(level, p, WHY[k % 3])))
for k, level in enumerate(LEVELS):
    register(f"cache.{level}", LV, (f"the cached {level} is removed", resub(PARAMETERS, rf"^{level} = .*\n", "")))
    register(f"cache.{level}.number", LV, (f"the cached {level} is a boolean", resub(PARAMETERS, rf"^{level} = .*$", f"{level} = true")))
    register(f"cache.{level}.finite", LV, (f"the cached {level} is {WHY[(k + 2) % 3]}, its parameters finite",
                                           resub(PARAMETERS, rf"^{level} = .*$", f"{level} = {WHY[(k + 2) % 3]}")))
register("cache.known", LV, *ends(lambda last: ("a cached L4 is added", resub(
    PARAMETERS, r"^L3 = .*$", lambda m: m[0] + "\nL4 = 0.1") if last else edit(PARAMETERS, "[derived.lambda]\n", "[derived.lambda]\nL4 = 0.1\n"))))
# bstack-calls
register("steps.list", BC, ("a step is a string", edit(WORKFLOW, "      - name: Report\n", "      - just-a-string\n\n      - name: Report\n")))
register("install.once", BC, ("the install step appears twice", duplicate_install))
register("install.keys", BC, ("the install step gains continue-on-error",
                              after(WORKFLOW, r"^( *)- name: Install bstack.*$", "  continue-on-error: true")))
register("install.recipe", BC,
         ("the fetched bstack is not verified", resub(WORKFLOW, r"^ *test .*BSTACK_REV.*\n", "")),
         ("the checkout is not detached", edit(WORKFLOW, "checkout -q --detach FETCH_HEAD", "checkout -q FETCH_HEAD")),
         ("git inherits the environment", edit(WORKFLOW, 'g() { env -i PATH=/usr/bin:/bin HOME="$HOME" git "$@"; }', 'g() { git "$@"; }')),
         ("git keeps the runner's PATH", edit(WORKFLOW, 'g() { env -i PATH=/usr/bin:/bin HOME="$HOME"', 'g() { env -i PATH="$PATH" HOME="$HOME"')),
         ("the install step hands BSTACK on through GITHUB_ENV",
          edit(WORKFLOW, 'echo "dir=$DEST" >> "$GITHUB_OUTPUT"', 'echo "BSTACK=$DEST" >> "$GITHUB_ENV"')))
register("install.env", BC, ("the install step sets another variable", after(WORKFLOW, r"^( *)BSTACK_REV: .*$", "X: y")))
register("install.full-sha", BC,
         ("bstack pinned to a branch", resub(WORKFLOW, r"(BSTACK_REV: )[0-9a-f]{40}", r"\1main")),
         ("bstack pinned to a short SHA", resub(WORKFLOW, r"(BSTACK_REV: [0-9a-f]{7})[0-9a-f]{33}", r"\1")))
register("calls.exact", BC,
         ("compute-lambda.sh runs without --strict", edit(WORKFLOW, " --strict --human", " --human")),
         ("compute-lambda.sh reads no explicit config", edit(WORKFLOW, ' --config="$PWD/.control/rcs-parameters.toml"', "")),
         ("compute-lambda.sh inherits the environment", edit(WORKFLOW, BSTACK_ENV + "compute", f'bash "{BSTACK_DIR}"/scripts/compute')),
         ("l3-rate-gate.sh inherits the environment", edit(WORKFLOW, BSTACK_ENV + "l3-rate", f'bash "{BSTACK_DIR}"/scripts/l3-rate')),
         ("compute-lambda.sh keeps the runner's PATH", edit(
             WORKFLOW, BSTACK_ENV + "compute", BSTACK_ENV.replace("PATH=/usr/bin:/bin", 'PATH="$PATH"') + "compute")),
         ("the contract step does not read the pinned bstack", edit(WORKFLOW, CONTRACT_BSTACK[1] + "\n", "")),
         ("a second compute-lambda.sh without --strict", extra_step('bash "$BSTACK"/scripts/compute-lambda.sh --human')),
         ("an extra git clone, the verify line kept", extra_step("git clone https://github.com/broomva/bstack.git /tmp/b")),
         ("a step uses a bstack action", edit(WORKFLOW, "      - name: Report\n", "      - uses: broomva/bstack@main\n\n      - name: Report\n")))
register("calls.order", BC,
         ("the install step has another id", edit(WORKFLOW, "        id: install\n", "        id: fetch\n")),
         ("the contract step runs before compute", lambda r, t: (lambda text, block: (r / WORKFLOW).write_text(
             text.replace(block, "").replace("      - name: compute\n", block + "      - name: compute\n")))(
             (r / WORKFLOW).read_text(), re.search(r"(?ms)^      - name: contract\n.*?\n\n", (r / WORKFLOW).read_text())[0])),
         ("a step runs the checker before the install step", edit(
             WORKFLOW, "      - name: Install bstack", "      - name: early\n        run: python3 -I scripts/governance-check.py\n\n"
                                                      "      - name: Install bstack")))
for step_id, _ in BSTACK_CALLS:
    register(f"calls.step-keys.{step_id}", BC, (f"the {step_id} step gains continue-on-error",
                                                after(WORKFLOW, rf"^( *)id: {step_id}$", "continue-on-error: true")))
# bstack-parameters
for p in LEVEL_PARAMETERS:
    register(f"bstack.reads.{p}", BP, (f"compute-lambda.sh no longer reads {p}", edit(SCRIPT, f'        {p} = lvl["{p}"]\n', "")))
register("bstack.reads-only-these", BP, ("compute-lambda.sh reads another parameter",
                                         edit(SCRIPT, "    except KeyError:", '        kappa = lvl["kappa"]\n    except KeyError:')))

# Cases no guard owns: each must pass.
BENIGN = [
    ("unchanged fixture", lambda r, t: None),
    ("strategy sets max-parallel", after(CI, STRATEGY, "  max-parallel: 4")),
    ("both sides grow by eight, in words", lambda r, t: seq(
        edit("AGENTS.md", t.phrase, f"after {words(t.n + 8)} `mutants` jobs"),
        matrix(lambda t: live(t) + [str(t.n + i) for i in range(8)]))(r, t)),
    ("both sides grow by eight, in digits", lambda r, t: seq(
        edit("AGENTS.md", t.phrase, f"after {t.n + 8} `mutants` jobs"),
        matrix(lambda t: live(t) + [str(t.n + i) for i in range(8)]))(r, t)),
    ("every trigger path double-quoted", resub(WORKFLOW, r"^(      - )'(.*)'$", r'\1"\2"', count=0)),
    ("every trigger path unquoted", resub(WORKFLOW, r"^(      - )'(.*)'$", r"\1\2", count=0)),
]


def cases():
    """(description, seed, the checks it must fail, the guard that owns it or None)."""
    return [(d, s, list(checks), gid) for gid, (checks, own) in GUARDS.items() for d, s in own] + \
           [(d, s, [], None) for d, s in BENIGN]


def seeded(tree, seed, tmp):
    """A copy of the fixture at tmp, with seed applied."""
    for rel, text in tree.files().items():
        (tmp / rel).parent.mkdir(parents=True, exist_ok=True)
        (tmp / rel).write_text(text, encoding="utf-8")
    seed(tmp, tree)
    return tmp


def run_case(path, checks=CHECKS):
    os.environ["BSTACK"] = str(path / "bstack")
    return run(path, quiet=True, checks=checks)


def completeness(fixture):
    """The failures that make the registry incomplete (see the module docstring)."""
    source = Path(__file__).read_text(encoding="utf-8")
    head, tail = source.split("# mutation proof " + "below", 1)
    above, below = ast.parse(head), ast.parse(tail.split("\n", 1)[1])
    below_names = {n.name for n in below.body if isinstance(n, (ast.FunctionDef, ast.ClassDef))} | {
        x.id for n in below.body if not isinstance(n, (ast.FunctionDef, ast.ClassDef))
        for x in ast.walk(n) if isinstance(x, ast.Name) and isinstance(x.ctx, ast.Store)}
    found, gids = [], {}
    for statement in above.body:
        if isinstance(statement, ast.FunctionDef) and statement.name in NOT_GUARDS:
            continue
        local = {n.id for n in ast.walk(statement) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Store)} | {
            n.arg for n in ast.walk(statement) if isinstance(n, ast.arg)}
        for node in ast.walk(statement):
            bad = isinstance(node, (ast.Raise, ast.Assert, ast.Try)) or (
                isinstance(node, ast.Name) and (node.id in FORBIDDEN or node.id in below_names - local)) or (
                isinstance(node, ast.Attribute) and FORBIDDEN_ATTRIBUTES.match(node.attr))
            if bad:
                what = getattr(node, "id", None) or getattr(node, "attr", None) or ""
                found.append(f"line {node.lineno}: {type(node).__name__} {what} outside refuse()".rstrip())
            if isinstance(node, ast.Call) and getattr(node.func, "id", None) == "refuse" and node.args:
                gids.setdefault(ast.dump(node.args[0]), []).append(node.lineno)
    found += [f"one gid at lines {lines}" for lines in gids.values() if len(lines) > 1]
    EVALUATED.clear()
    SITES.clear()
    run_case(fixture)
    found += [f"guard {g} is registered but never evaluated" for g in GUARDS if g not in EVALUATED]
    found += [f"guard {g} is refused at lines {sorted(s)}" for g, s in SITES.items() if len(s) > 1]
    return found


def selftest(tree):
    """Every case's outcome on a seeded copy of the fixture, the runner's crash rule and
    completeness; the number of failures."""
    with tempfile.TemporaryDirectory() as tmp:
        fixture = seeded(tree, lambda r, t: None, Path(tmp) / "fixture")
        incomplete = completeness(fixture)
        for line in incomplete:
            print(f"FAIL completeness: {line}")
        print(f"{'ok  ' if not incomplete else 'FAIL'} completeness: {len(GUARDS)} guards, each refused at one site and "
              "evaluated by the unchanged fixture; no refusal outside refuse() and the readers", flush=True)
        if incomplete:
            # The cases run the checks' code, which the scan has just refused: stop before it can exit
            # or exec with another status.
            return len(incomplete)
        bad = 0
        for name, crash in (("crashes", lambda r: [][0]), ("exits", lambda r: sys.exit(0))):
            probe = run(fixture, quiet=True, checks=[("probe", crash)])
            bad += probe != ["probe (crashed)"]
            print(f"{'ok  ' if probe == ['probe (crashed)'] else 'FAIL'} runner: a check that {name} fails as "
                  f"`probe (crashed)`: {probe}")
        for i, (desc, seed, want, gid) in enumerate(cases()):
            got = run_case(seeded(tree, seed, Path(tmp) / str(i)))
            bad += got != want
            print(f"{'ok  ' if got == want else 'FAIL'} selftest: {desc}: {'fails ' + ', '.join(got) if got else 'passes'}"
                  + ("" if got == want else f" (want {'fails ' + ', '.join(want) if want else 'passes'})"))
    return bad


def mutation_proof(tree):
    """Disable each guard alone: its own cases must change outcome, and every other case must keep
    its outcome. The baseline (no guard disabled) must give every case its want. A guard is re-run
    only through the checks that evaluate it."""
    rows, crashed = [], 0
    with tempfile.TemporaryDirectory() as tmp:
        incomplete = completeness(seeded(tree, lambda r, t: None, Path(tmp) / "fixture"))
        for line in incomplete:
            print(f"FAIL completeness: {line}", flush=True)
        if incomplete:
            print(f"FAIL completeness, first: {len(incomplete)} failures; the proof does not run", flush=True)
            return len(incomplete)
        built = []
        CHECKED_BY.clear()
        for i, (desc, seed, want, gid) in enumerate(cases()):
            path = seeded(tree, seed, Path(tmp) / str(i))
            built.append((desc, path, want, gid, run_case(path)))
        baseline = [d for d, _, want, _, got in built if got != want]
        for gid in GUARDS:
            names = CHECKED_BY.get(gid, set())
            subset = [(n, c) for n, c in CHECKS if n in names]
            DISABLED.clear()
            DISABLED.add(gid)
            own, others = [], []
            for desc, path, want, owner, base in built:
                got = run_case(path, subset)
                (own if owner == gid else others).append((desc, got, [x for x in base if x.split(" (")[0] in names]))
            DISABLED.clear()
            survived = [d for d, got, base in own if got == base]
            moved = [d for d, got, base in others if got != base]
            crash = any("(crashed)" in " ".join(got) for _, got, _ in own)
            crashed += crash and not survived and not moved
            rows.append((gid, len(own), "NO" if survived or moved else "yes (crash)" if crash else "yes", survived, moved))
    print("guard | own cases | killed")
    for gid, n, killed, survived, moved in rows:
        print(f"{gid} | {n} | {killed}" + (f": survived {survived}" if survived else "") + (f": also moved {moved}" if moved else ""))
    killed = sum(r[2] != "NO" for r in rows)
    print(f"baseline: {len(built) - len(baseline)} of {len(built)} cases give their want" + (f"; not: {baseline}" if baseline else ""))
    print(f"{killed} of {len(rows)} guards proven ({killed - crashed} by a changed outcome, {crashed} by a crash), "
          f"each alone, with every other case unmoved")
    print("ok   completeness, first: 0 failures")
    return len(rows) - killed + len(baseline)


def main(argv):
    here = Path(__file__).resolve().parent.parent
    try:
        if argv == ["--selftest"]:
            return 1 if selftest(Tree(here)) else 0
        if argv == ["--mutation-proof"]:
            return 1 if mutation_proof(Tree(here)) else 0
    except Broken as e:
        print(f"FAIL selftest: {e}")
        return 1
    if len(argv) > 1 or (argv and argv[0].startswith("-")):
        print("\n".join(__doc__.strip().splitlines()[2:5]), file=sys.stderr)
        return 2
    return 1 if run(Path(argv[0]) if argv else here) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
