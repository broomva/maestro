#!/usr/bin/env python3
"""Maestro's governance contract, run by the `stability-check` job (BRO-3702).

    python3 -I scripts/governance-check.py [ROOT]     # check the tree at ROOT (default: this checkout)
    python3 -I scripts/governance-check.py --selftest # seed one break per check in a copy: each must fail

Exit 0 when every check holds, 1 when one does not, 2 on usage. A file that cannot be read, or a
phrase that cannot be found, is a failure, never a pass. Standard library only (tomllib: Python
3.11+), because the job runs the runner's own python3, with no setup-python.
"""
import math
import os
import re
import shutil
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
# The levels of bstack's rcs-parameters.toml template, in its order.
LEVELS = ("L0", "L1", "L2", "L3")
# The parameters bstack's compute-lambda.sh reads for each level.
LEVEL_PARAMETERS = ("gamma", "L_theta", "rho", "L_d", "eta", "beta", "tau_bar", "nu", "tau_a")
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


def words(k):
    """k below one hundred in the words number() reads ("twenty-four")."""
    if k < 20:
        return UNITS[k]
    tens = [w for w, v in TENS.items() if v == k // 10 * 10]
    if k >= 100 or not tens:
        raise Broken(f"no words for {k}")
    return tens[0] + ("-" + UNITS[k % 10] if k % 10 else "")


# A strict reader for the block YAML these checks read, standard library only: PyYAML is not
# guaranteed on a self-hosted runner, and nothing is installed. It reads plain block mappings of
# unique keys and lists of scalars. Anything else (anchors, aliases, merge keys, tags, flow
# mappings, tabs, a list where a key belongs) raises Broken: the checks fail closed on a shape
# they cannot read, never pass over it.
KEY = re.compile(r"""^ *('[^']*'|"[^"]*"|[^\s#'"{}\[\],&*!|>-][^:#]*?|-[^\s:#][^:#]*?) *:(?: +(.*))?$""")
SCALAR = re.compile(r"""^(?:'[^']*'|"[^"\\]*"|[^\s'"#&*!|>{}\[\],][^#{}\[\],]*?)$""")


def indent(line):
    return len(line) - len(line.lstrip(" "))


def unquote(s):
    return s[1:-1] if len(s) >= 2 and s[0] == s[-1] and s[0] in "'\"" else s


def content(lines, lo, hi, where):
    """The line numbers in lines[lo:hi] that are not blank or comments."""
    body = []
    for i in range(lo, hi):
        stripped = lines[i].strip()
        if not stripped or stripped.startswith("#"):
            continue
        if lines[i][:indent(lines[i]) + 1].endswith("\t"):
            raise Broken(f"{where}: tab indentation on line {i + 1}")
        body.append(i)
    return body


def mapping(lines, lo, hi, where):
    """The block mapping in lines[lo:hi]: {key: (value on the key's line, lo, hi of its block)}."""
    body = content(lines, lo, hi, where)
    if not body:
        raise Broken(f"{where} is empty")
    at = indent(lines[body[0]])
    keys = []
    for i in body:
        if indent(lines[i]) < at:
            raise Broken(f"{where}: line {i + 1} is outdented")
        if indent(lines[i]) > at:
            continue  # inside the previous key's block
        m = KEY.match(lines[i])
        if not m:
            raise Broken(f"{where}: line {i + 1} is not a `key: value` entry")
        key, value = unquote(m[1].strip()), re.sub(r"\s+#.*$", "", m[2] or "").strip()
        if key == "<<" or value[:1] in ("&", "*", "!"):
            raise Broken(f"{where}: anchors, aliases, merge keys and tags are not read (line {i + 1})")
        if any(key == k for k, _, _ in keys):
            raise Broken(f"{where}: duplicate key {key!r} (line {i + 1})")
        keys.append((key, value, i))
    ends = [i for _, _, i in keys[1:]] + [hi]
    return {k: (v, i + 1, end) for (k, v, i), end in zip(keys, ends)}


def block(parent, key, lines, where):
    """The block mapping under `key`, which must hold one (not a scalar, flow value or expression)."""
    if key not in parent:
        raise Broken(f"{where} has no `{key}`")
    value, lo, hi = parent[key]
    if value:
        raise Broken(f"{where}.{key} is {value!r}, not a block mapping")
    return mapping(lines, lo, hi, f"{where}.{key}")


def scalars(lines, entry, where):
    """The list of scalars an entry holds, in flow style ([a, b]) or block style (- a)."""
    value, lo, hi = entry
    body = content(lines, lo, hi, where)
    if value.startswith("["):
        flow = " ".join([value] + [re.sub(r"\s+#.*$", "", lines[i]).strip() for i in body])
        if not flow.endswith("]") or flow.count("[") != 1 or flow.count("]") != 1:
            raise Broken(f"{where} is not a flat flow list")
        items = [x.strip() for x in flow[1:-1].split(",")]
        if items and not items[-1]:
            items.pop()
    elif not value:
        if not body:
            raise Broken(f"{where} is empty")
        at = indent(lines[body[0]])
        items = []
        for i in body:
            m = re.match(r"^ *- +(.*?)(?:\s+#.*)?$", lines[i])
            if indent(lines[i]) != at or not m:
                raise Broken(f"{where}: line {i + 1} is not a list item")
            items.append(m[1].strip())
    else:
        raise Broken(f"{where} is {value!r}, not a list")
    for x in items:
        if not SCALAR.match(x) or re.search(r":\s|:$", x):
            raise Broken(f"{where}: {x!r} is not a plain scalar")
    return [unquote(x) for x in items]


def claude_paths(text):
    """The repository paths CLAUDE.md names: backticked, with a slash or a file extension. Each must
    exist (claude-points-to-agents) and trigger the job (trigger-paths), so a PR that moves one
    runs the check; the selftest copies them. A path outside the repository fails."""
    named = re.findall(r"`([^`\s]+)`", text)
    paths = [p for p in named if "/" in p or re.search(r"\.(md|html|ya?ml|toml|sh|py|rs)$", p)]
    outside = [p for p in paths if p.startswith("/") or ".." in p.split("/")]
    if outside:
        raise Broken("CLAUDE.md names paths outside the repository: " + ", ".join(outside))
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
    """The number of `mutants` jobs AGENTS.md names is the length of ci.yml's mutants matrix."""
    found = re.findall(r"\bafter\s+(\S+(?:[ -]\S+)??)\s+`mutants`\s+jobs\b", read(root, "AGENTS.md"))
    if len(found) != 1:
        raise Broken(f"AGENTS.md names the `mutants` job count {len(found)} times, not once "
                     "(\"after <count> `mutants` jobs\")")
    named = number(found[0])
    lines = read(root, CI).splitlines()
    jobs = block(mapping(lines, 0, len(lines), CI), "jobs", lines, CI)
    mutants = block(jobs, "mutants", lines, f"{CI} jobs")
    # GitHub runs one job per matrix combination, after include and exclude. The count is the
    # shard list's length only for one dimension and no include or exclude; any other shape fails.
    try:
        strategy = block(mutants, "strategy", lines, "mutants")
        shaping = sorted(set(strategy) - {"matrix", "fail-fast", "max-parallel"})
        if shaping:
            raise Broken("strategy has " + ", ".join(shaping))
        matrix = block(strategy, "matrix", lines, "mutants.strategy")
        if list(matrix) != ["shard"]:
            raise Broken("the matrix must hold exactly `shard`, and holds " + ", ".join(matrix))
        shards = scalars(lines, matrix["shard"], "mutants.strategy.matrix.shard")
        if len(set(shards)) != len(shards):
            raise Broken("the shard list repeats a value")
    except Broken as e:
        raise Broken(f"unsupported matrix shape in {CI}: {e}")
    if named != len(shards):
        raise Broken(f"AGENTS.md says {found[0]} ({named}) `mutants` jobs, "
                     f"but the {CI} mutants matrix has {len(shards)} shards")
    return f"AGENTS.md and {CI} both run {named} shards"


def check_triggers(root):
    """Every governance path, every input of these checks and every path CLAUDE.md names runs
    the job."""
    lines = read(root, WORKFLOW).splitlines()
    # Exactly on.pull_request, on main, with paths: another event, paths-ignore or types would
    # change when the check runs, and none of them is read here.
    try:
        on = block(mapping(lines, 0, len(lines), WORKFLOW), "on", lines, WORKFLOW)
        if list(on) != ["pull_request"]:
            raise Broken("`on` must hold exactly pull_request, and holds " + ", ".join(on))
        pr = block(on, "pull_request", lines, "on")
        if sorted(pr) != ["branches", "paths"]:
            raise Broken("on.pull_request must hold exactly branches and paths, and holds "
                         + ", ".join(pr))
        if scalars(lines, pr["branches"], "on.pull_request.branches") != ["main"]:
            raise Broken("on.pull_request.branches must be [main]")
        paths = scalars(lines, pr["paths"], "on.pull_request.paths")
        # GitHub applies paths in order, and a `!` entry excludes what an earlier entry matched.
        negated = [x for x in paths if x.startswith("!")]
        if negated:
            raise Broken("on.pull_request.paths negates " + ", ".join(negated))
        triggers = set(paths)
    except Broken as e:
        raise Broken(f"unsupported trigger shape in {WORKFLOW}: {e}")
    want = list(dict.fromkeys([*GOVERNANCE_PATHS, *CONTRACT_INPUTS,
                               *claude_paths(read(root, "CLAUDE.md"))[1]]))
    missing = [p for p in want if p not in triggers]
    if missing:
        raise Broken(f"{WORKFLOW} does not run on: " + ", ".join(missing))
    return f"{len(want)} paths run the job"


def parameters(root):
    try:
        return tomllib.loads(read(root, PARAMETERS))
    except tomllib.TOMLDecodeError as e:
        raise Broken(f"{PARAMETERS} is not valid TOML: {e}")


def check_rate_paths(root):
    """The L3 rate gate counts every governance path in this repository."""
    patterns = parameters(root).get("gates", {}).get("l3_paths", {}).get("patterns")
    if not isinstance(patterns, list) or not all(isinstance(x, str) for x in patterns):
        raise Broken(f"{PARAMETERS} has no [gates.l3_paths] patterns list of strings")
    # The rate gate passes the list to `git log --`, where pathspec magic such as :(exclude),
    # :! or :^ removes paths from the count. None is read here, so none is allowed.
    magic = [x for x in patterns if x.startswith(":")]
    if magic:
        raise Broken(f"{PARAMETERS} [gates.l3_paths] uses pathspec magic: " + ", ".join(magic))
    missing = [p for p in GOVERNANCE_PATHS if p not in patterns]
    if missing:
        raise Broken(f"{PARAMETERS} [gates.l3_paths] does not count: " + ", ".join(missing))
    return f"{len(patterns)} patterns count every governance path"


def check_levels(root):
    """The parameters hold exactly the template's four levels, each with a cached lambda: with
    none, bstack's compute-lambda.sh finds all([]) stable, and a level with no cached value is
    never checked for drift."""
    data = parameters(root)
    levels = data.get("levels")
    if not isinstance(levels, list) or not all(isinstance(x, dict) for x in levels):
        raise Broken(f"{PARAMETERS} has no [[levels]] tables")
    ids = [x.get("id") for x in levels]
    if ids != list(LEVELS):
        raise Broken(f"{PARAMETERS} levels are {ids}, not {list(LEVELS)}")
    cached = data.get("derived", {}).get("lambda")
    if not isinstance(cached, dict) or sorted(cached) != sorted(LEVELS):
        raise Broken(f"{PARAMETERS} [derived.lambda] caches "
                     f"{sorted(cached) if isinstance(cached, dict) else 'nothing'}, not {list(LEVELS)}")
    # A non-finite value computes a non-finite lambda (gamma = inf gives omega = inf, exit 0).
    def finite(x):
        return isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x)
    bad = [f"{lvl['id']}.{k}" for lvl in levels for k in LEVEL_PARAMETERS if not finite(lvl.get(k))]
    bad += [f"derived.lambda.{k}" for k in LEVELS if not finite(cached[k])]
    if bad:
        raise Broken(f"{PARAMETERS} has missing or non-finite numbers: " + ", ".join(bad))
    return "levels " + ", ".join(LEVELS) + ", finite, each with a cached lambda"


def check_lambda_strict(root):
    """The job runs compute-lambda.sh once, with --strict: without it a cached lambda that drifts
    from its recomputed value by more than 1e-4 still exits 0."""
    runs = re.findall(r'"\$BSTACK"/scripts/compute-lambda\.sh([^\n]*)', read(root, WORKFLOW))
    if len(runs) != 1 or "--strict" not in runs[0].split():
        raise Broken(f"{WORKFLOW} must run compute-lambda.sh once, with --strict "
                     f"(runs it {len(runs)} times: {'; '.join(r.strip() for r in runs) or '-'})")
    return "compute-lambda.sh --strict"


# How the job calls bstack: a cleared environment with this checkout bound. compute-lambda.sh also
# takes the checkout's parameters by path, so no fallback config is ever read.
BSTACK_ENV = 'env -i PATH="$PATH" HOME="$HOME" BROOMVA_WORKSPACE="$PWD" bash "$BSTACK"/scripts/'
BSTACK_CALLS = {
    "compute-lambda.sh": '--config="$PWD/.control/rcs-parameters.toml"',
    "l3-rate-gate.sh": "",
}


def check_bstack_env(root):
    """Every bstack script call runs in a cleared environment bound to this checkout: an inherited
    BROOMVA_WORKSPACE, GIT_DIR or BSTACK_L3_CORRECTION would point it at another checkout."""
    calls = re.findall(r'^.*"\$BSTACK"/scripts/([\w.-]+).*$', read(root, WORKFLOW), re.M)
    lines = re.findall(r'^.*"\$BSTACK"/scripts/[\w.-]+.*$', read(root, WORKFLOW), re.M)
    if sorted(calls) != sorted(BSTACK_CALLS):
        raise Broken(f"{WORKFLOW} calls bstack's {', '.join(calls) or 'nothing'}, "
                     f"not exactly {', '.join(BSTACK_CALLS)} once each")
    for name, line in zip(calls, lines):
        if BSTACK_ENV + name not in line or BSTACK_CALLS[name] not in line.split(name, 1)[1]:
            raise Broken(f"{WORKFLOW} calls {name} without {BSTACK_ENV.split(' bash')[0]}"
                         + (f" and {BSTACK_CALLS[name]}" if BSTACK_CALLS[name] else ""))
    return "bstack runs in a cleared environment bound to this checkout"


def check_bstack_pin(root):
    """The job runs the bstack commit the workflow pins, fetched by its full SHA and checked out
    detached: never a branch or tag, which can move under a reviewed workflow."""
    text = read(root, WORKFLOW)
    pins = re.findall(r"^ +BSTACK_REV: *(\S*) *$", text, re.M)
    if len(pins) != 1 or not re.fullmatch(r"[0-9a-f]{40}", unquote(pins[0])):
        raise Broken(f"{WORKFLOW} must set BSTACK_REV once, to a full 40-character commit SHA "
                     f"(found {', '.join(pins) or 'none'})")
    if re.search(r"\bgit\s+clone\b", text):
        raise Broken(f"{WORKFLOW} clones a repository, which takes its moving default branch")
    for what, pattern in (
        ("fetch BSTACK_REV", r'git -C "\$DEST" fetch [^\n]*"\$BSTACK_REV"'),
        ("check out FETCH_HEAD detached", r'git -C "\$DEST" checkout [^\n]*--detach FETCH_HEAD'),
        ("verify HEAD is BSTACK_REV", r'test "\$\(git -C "\$DEST" rev-parse HEAD\)" = "\$BSTACK_REV"'),
    ):
        if not re.search(pattern, text):
            raise Broken(f"{WORKFLOW} does not {what}")
    return f"bstack {unquote(pins[0])[:12]}"


CHECKS = (
    ("claude-points-to-agents", check_claude_pointer),
    ("shard-count", check_shard_count),
    ("trigger-paths", check_triggers),
    ("rate-paths", check_rate_paths),
    ("rcs-levels", check_levels),
    ("lambda-strict", check_lambda_strict),
    ("bstack-env", check_bstack_env),
    ("bstack-pin", check_bstack_pin),
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


def resub(rel, pattern, repl):
    def apply(root):
        text = (root / rel).read_text(encoding="utf-8")
        new, n = re.subn(pattern, repl, text, count=1, flags=re.M)
        if not n:
            raise Broken(f"selftest seed anchor missing in {rel}: {pattern!r}")
        (root / rel).write_text(new, encoding="utf-8")
    return apply


def insert_after(rel, pattern, added):
    """Add lines after the first line matching pattern (whose group 1 is its indentation), each
    indented as that line plus its own leading spaces."""
    return resub(rel, pattern, lambda m: m[0] + "".join("\n" + m[1] + line for line in added))


def matrix(shards, style="flow"):
    items = [str(i) for i in (range(shards) if isinstance(shards, int) else shards)]
    def apply(root):
        text = (root / CI).read_text(encoding="utf-8")
        m = re.search(r"^( *)shard *:.*$", text, re.M)
        if not m:
            raise Broken("selftest seed anchor missing: shard matrix")
        if style == "flow":
            new = f"{m[1]}shard: [{', '.join(items)}]"
        else:
            new = f"{m[1]}shard:" + "".join(f"\n{m[1]}  - {i}" for i in items)
        (root / CI).write_text(text[:m.start()] + new + text[m.end():], encoding="utf-8")
    return apply


def selftest(root, quiet=False):
    """Each seeded break fails exactly the check that guards it; each benign rewrite passes."""
    count = re.search(r"\bafter\s+(\S+(?:[ -]\S+)??)\s+`mutants`\s+jobs\b", read(root, "AGENTS.md"))
    if not count:
        raise Broken("AGENTS.md has no shard count to seed")
    n, word = number(count[1]), count[1]
    phrase = f"after {word} `mutants` jobs"

    def to_symlink(r):
        (r / "CLAUDE.md").unlink()
        (r / "CLAUDE.md").symlink_to("AGENTS.md")

    SHARD_LINE = r"^( *)shard *:.*$"
    LEVEL = r'^\[\[levels\]\]\nid *= *"{}"\n(?:\w+ *=.*\n)*'
    cases = [
        # (description, seed, the checks that must fail; empty means it must pass)
        ("unchanged tree", lambda r: None, []),
        ("CLAUDE.md no longer names AGENTS.md", edit("CLAUDE.md", "`AGENTS.md`", "`RULES.md`"),
         ["claude-points-to-agents", "trigger-paths"]),
        ("CLAUDE.md is a symlink to AGENTS.md", to_symlink, ["claude-points-to-agents", "trigger-paths"]),
        ("CLAUDE.md names a path that does not exist", edit("CLAUDE.md", "`AGENTS.md`",
                                                            "`AGENTS.md` and `docs/missing.md`"),
         ["claude-points-to-agents", "trigger-paths"]),
        ("CLAUDE.md names an absolute path", lambda r: edit(
            "CLAUDE.md", "`AGENTS.md`", f"`AGENTS.md` and `{r.resolve()}/AGENTS.md`")(r),
         ["claude-points-to-agents", "trigger-paths"]),
        ("CLAUDE.md names a path through ..", edit("CLAUDE.md", "`AGENTS.md`",
                                                   "`AGENTS.md` and `docs/../AGENTS.md`"),
         ["claude-points-to-agents", "trigger-paths"]),
        ("the workflow stops running on a file CLAUDE.md names", resub(
            WORKFLOW, r"^ *- 'docs/specs/.*'\n", ""), ["trigger-paths"]),
        ("CLAUDE.md is a copy of AGENTS.md", lambda r: shutil.copyfile(r / "AGENTS.md", r / "CLAUDE.md"),
         ["claude-points-to-agents", "trigger-paths"]),
        ("AGENTS.md names one shard more", edit("AGENTS.md", phrase,
                                                f"after {n + 1} `mutants` jobs"), ["shard-count"]),
        ("ci.yml drops a shard", matrix(n - 1), ["shard-count"]),
        ("AGENTS.md no longer names the count", edit("AGENTS.md", phrase, "after the `mutants` run"),
         ["shard-count"]),
        ("a second shard matrix", edit(CI, "    strategy:", "    strategy:\n      # shard: [0]\n"
                                       "      x:\n        shard: [0]"), ["shard-count"]),
        ("ci.yml shard list in block style", matrix(n, "block"), []),
        # Grown from the current count, so each always changes it.
        (f"both sides grow to {n + 8}, written as words", lambda r: (
            edit("AGENTS.md", phrase, f"after {words(n + 8)} `mutants` jobs")(r), matrix(n + 8)(r)), []),
        (f"both sides grow to {n + 8}, written as digits", lambda r: (
            edit("AGENTS.md", phrase, f"after {n + 8} `mutants` jobs")(r), matrix(n + 8)(r)), []),
        ("the workflow stops running on AGENTS.md", edit(WORKFLOW, "      - 'AGENTS.md'\n", ""),
         ["trigger-paths"]),
        ("the workflow stops running on ci.yml", edit(WORKFLOW, f"      - '{CI}'\n", ""),
         ["trigger-paths"]),
        ("the rate gate stops counting AGENTS.md", edit(PARAMETERS, '    "AGENTS.md",\n', ""),
         ["rate-paths"]),
        # A governance path the repository lacks still triggers the job and counts: adding it is a
        # governance change.
        ("the workflow stops running on METALAYER.md", edit(WORKFLOW, "      - 'METALAYER.md'\n", ""),
         ["trigger-paths"]),
        ("the workflow stops running on a docs reference", edit(
            WORKFLOW, "      - 'docs/references/current-work.md'\n", ""), ["trigger-paths"]),
        ("the rate gate stops counting METALAYER.md", edit(PARAMETERS, '    "METALAYER.md",\n', ""),
         ["rate-paths"]),
        ("rcs-parameters.toml is unreadable", lambda r: (r / PARAMETERS).unlink(),
         ["rate-paths", "rcs-levels"]),
        # The matrix's real shape: GitHub runs one job per combination, after include and exclude.
        ("the matrix excludes a shard", insert_after(CI, SHARD_LINE, ["exclude:", "  - shard: 0"]),
         ["shard-count"]),
        ("the matrix includes another job", insert_after(CI, SHARD_LINE, ["include:", "  - shard: 99"]),
         ["shard-count"]),
        ("the matrix gains a dimension", insert_after(CI, SHARD_LINE, ["os: [a, b]"]), ["shard-count"]),
        ("the matrix is an expression", lambda r: (
            resub(CI, SHARD_LINE + r"\n", "")(r),
            resub(CI, r"^( *)matrix:$", r"\1matrix: ${{ fromJSON(vars.SHARD_LINES) }}")(r)),
         ["shard-count"]),
        ("the shard list repeats a value", matrix(list(range(n - 1)) + [0]), ["shard-count"]),
        ("the shard list is an anchor", resub(CI, r"^( *)shard: \[", r"\1shard: &s ["),
         ["shard-count"]),
        ("strategy gains another key", insert_after(CI, r"^( *)strategy:$", ["  x: 1"]),
         ["shard-count"]),
        ("strategy sets max-parallel", insert_after(CI, r"^( *)strategy:$", ["  max-parallel: 4"]),
         []),
        # The trigger: exactly on.pull_request, on main, with paths.
        ("the event is push, not pull_request", edit(WORKFLOW, "\n  pull_request:\n", "\n  push:\n"),
         ["trigger-paths"]),
        ("a path moves under another event", lambda r: (
            edit(WORKFLOW, "      - 'AGENTS.md'\n", "")(r),
            edit(WORKFLOW, "\nconcurrency:", "\n  push:\n    paths:\n      - 'AGENTS.md'\nconcurrency:")(r)),
         ["trigger-paths"]),
        ("paths-ignore is added", insert_after(WORKFLOW, r"^( *)branches: \[main\]$",
                                               ["paths-ignore:", "  - 'docs/**'"]), ["trigger-paths"]),
        ("types narrows the events", insert_after(WORKFLOW, r"^( *)branches: \[main\]$",
                                                  ["types: [closed]"]), ["trigger-paths"]),
        ("branches widens past main", edit(WORKFLOW, "branches: [main]", "branches: [main, dev]"),
         ["trigger-paths"]),
        # The bstack that judges the PR: one commit, by full SHA.
        ("bstack pinned to a branch", resub(WORKFLOW, r"(BSTACK_REV: )[0-9a-f]{40}", r"\1main"),
         ["bstack-pin"]),
        ("bstack pinned to a short SHA", resub(WORKFLOW, r"(BSTACK_REV: [0-9a-f]{7})[0-9a-f]{33}", r"\1"),
         ["bstack-pin"]),
        ("bstack cloned from its default branch", resub(
            WORKFLOW, r"^( *)git init .*\n(?: *git -C .*\n)+",
            r'\1git clone --depth 1 https://github.com/broomva/bstack.git "$DEST"\n'), ["bstack-pin"]),
        ("the fetched bstack is not verified", resub(WORKFLOW, r"^ *test .*BSTACK_REV.*\n", ""),
         ["bstack-pin"]),
        # Drift: compute-lambda.sh fails on it only with --strict.
        ("compute-lambda.sh runs without --strict", edit(WORKFLOW, "compute-lambda.sh --strict ",
                                                          "compute-lambda.sh "), ["lambda-strict"]),
        # bstack reads BROOMVA_WORKSPACE (and GIT_DIR) from its environment: unbound, a runner's
        # value would point it at another checkout.
        ("compute-lambda.sh inherits BROOMVA_WORKSPACE", edit(
            WORKFLOW, 'env -i PATH="$PATH" HOME="$HOME" BROOMVA_WORKSPACE="$PWD" bash "$BSTACK"/scripts/compute-lambda.sh',
            'bash "$BSTACK"/scripts/compute-lambda.sh'), ["bstack-env"]),
        ("compute-lambda.sh reads no explicit config", edit(
            WORKFLOW, ' --config="$PWD/.control/rcs-parameters.toml"', ""), ["bstack-env"]),
        ("compute-lambda.sh keeps the runner's environment", edit(
            WORKFLOW, 'env -i PATH="$PATH" HOME="$HOME" BROOMVA_WORKSPACE="$PWD" bash "$BSTACK"/scripts/compute-lambda.sh',
            'BROOMVA_WORKSPACE="$PWD" bash "$BSTACK"/scripts/compute-lambda.sh'), ["bstack-env"]),
        ("l3-rate-gate.sh keeps the runner's environment", edit(
            WORKFLOW, 'env -i PATH="$PATH" HOME="$HOME" BROOMVA_WORKSPACE="$PWD" bash "$BSTACK"/scripts/l3-rate-gate.sh',
            'BROOMVA_WORKSPACE="$PWD" bash "$BSTACK"/scripts/l3-rate-gate.sh'), ["bstack-env"]),
        ("another bstack script is called", insert_after(
            WORKFLOW, r"^( *)set \+e$", ['bash "$BSTACK"/scripts/doctor.sh || true']), ["bstack-env"]),
        # The levels: none, or one fewer, would pass bstack's all() unchecked.
        ("a level is removed", resub(PARAMETERS, LEVEL.format("L2"), ""), ["rcs-levels"]),
        ("every level is removed", lambda r: [resub(PARAMETERS, LEVEL.format(x), "")(r) for x in LEVELS],
         ["rcs-levels"]),
        ("a cached lambda is removed", resub(PARAMETERS, r"^L3 = .*\n", ""), ["rcs-levels"]),
        ("a gamma is infinite", resub(PARAMETERS, r"^gamma( *)= .*$", r"gamma\1= inf"), ["rcs-levels"]),
        ("a tau_a is not a number", resub(PARAMETERS, r"^tau_a( *)= .*$", r"tau_a\1= nan"),
         ["rcs-levels"]),
        ("a rho is a string", resub(PARAMETERS, r"^rho( *)= (.*)$", r'rho\1= "\2"'), ["rcs-levels"]),
        ("a cached lambda is infinite", resub(PARAMETERS, r"^L3 = .*$", "L3 = inf"), ["rcs-levels"]),
        # inf - inf is nan, and nan > 1e-4 is false: compute-lambda.sh --strict exits 0 on this.
        ("a gamma and its cached lambda are both infinite", lambda r: (
            resub(PARAMETERS, r"^gamma( *)= .*$", r"gamma\1= inf")(r),
            resub(PARAMETERS, r"^L0 = .*$", "L0 = inf")(r)), ["rcs-levels"]),
        # Pathspec magic in the rate gate's list removes paths from its count.
        ("the rate list excludes AGENTS.md", edit(PARAMETERS, '    "AGENTS.md",\n',
                                                  '    "AGENTS.md",\n    ":(exclude)AGENTS.md",\n'),
         ["rate-paths"]),
        ("the rate list excludes with :!", edit(PARAMETERS, '    "AGENTS.md",\n',
                                                '    "AGENTS.md",\n    ":!AGENTS.md",\n'), ["rate-paths"]),
        ("the rate list excludes with :^", edit(PARAMETERS, '    "AGENTS.md",\n',
                                                '    "AGENTS.md",\n    ":^AGENTS.md",\n'), ["rate-paths"]),
        # GitHub's paths: a later `!` entry excludes an earlier match; any quoting reads the same.
        ("the trigger list negates AGENTS.md", edit(WORKFLOW, "      - 'AGENTS.md'\n",
                                                    "      - 'AGENTS.md'\n      - '!AGENTS.md'\n"),
         ["trigger-paths"]),
        ("a double-quoted negation", edit(WORKFLOW, "      - 'AGENTS.md'\n",
                                          "      - 'AGENTS.md'\n      - \"!AGENTS.md\"\n"),
         ["trigger-paths"]),
        ("trigger paths double-quoted", edit(WORKFLOW, "      - 'AGENTS.md'\n", '      - "AGENTS.md"\n'),
         []),
        ("trigger paths unquoted", edit(WORKFLOW, "      - 'AGENTS.md'\n", "      - AGENTS.md\n"), []),
    ]
    # Every file a check reads: the governance paths, the checks' inputs, and every path CLAUDE.md
    # names, whatever directory it is in.
    inputs = set(GOVERNANCE_PATHS) | set(CONTRACT_INPUTS) | set(claude_paths(read(root, "CLAUDE.md"))[1])

    def nested(copy):
        """The selftest of a tree whose CLAUDE.md also names another root file: it must pass."""
        edit("CLAUDE.md", "`AGENTS.md`", "`AGENTS.md` and `NOTES.md`")(copy)
        (copy / "NOTES.md").write_text("notes\n", encoding="utf-8")
        edit(WORKFLOW, "      - 'CLAUDE.md'\n", "      - 'CLAUDE.md'\n      - 'NOTES.md'\n")(copy)
        return selftest(copy, quiet=True)

    bad = 0
    for desc, seed, want in cases + ([] if quiet else [
            ("the selftest of a tree whose CLAUDE.md names another root file", nested, None)]):
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
