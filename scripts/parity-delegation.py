#!/usr/bin/env python3
"""Parity of Maestro's delegation and economy ports with the workspace's Python (BRO-3204).

The Python is the reference: scripts/delegation.py and scripts/fleet_economy.py at the workspace's
origin/main (--rev). This harness

  1. runs the Python on deterministic cases and checks fixtures/delegation/golden.json is its output
     (--write regenerates it; review the diff). crates/core/tests/parity_delegation.rs holds the Rust
     to that golden, so `cargo test` and the mutation check need no Python;
  2. runs the same cases through `maestro parity-eval` and compares;
  3. runs both CLIs on every real docs/plans/*.graph.yaml (plan, plan --json, brief per node), on CLI
     scenarios in scratch repos (bindings, nested and absent repos, policies) and on assess;
  4. runs both capacity forecasts on the live usage snapshot (provider_manager.py --json usage) and the
     coordinator's sample cache, read-only, and both tier readers on Maestro's economy.json if present;
  5. checks the plugin's tier ladder and model mapping (maestro-paseo shared/tiers.ts at the commit in
     fixtures/delegation/tiers-rev, run under bun) against the Rust and the Python.

Exit 0 only on full agreement. Needs git, cargo, bun and network (for step 5).

    python3 scripts/parity-delegation.py --workspace ~/broomva [--write] [--maestro target/debug/maestro]
"""
from __future__ import annotations

import copy
import argparse
import datetime as dt
import importlib.util
import itertools
import json
import os
import random
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN = os.path.join(REPO, "fixtures", "delegation", "golden.json")
CLI_GOLDEN = os.path.join(REPO, "fixtures", "delegation", "cli-golden.json")
TIERS_REV = os.path.join(REPO, "fixtures", "delegation", "tiers-rev")
PROVIDER_MANAGER = os.path.expanduser("~/.claude/skills/provider-manager/scripts/provider_manager.py")

failures: list[str] = []
cli_runs: list[int] = []  # the Python's exit code of each CLI comparison


def check(name, ok, detail=""):
    if not ok:
        failures.append(f"{name}: {detail}")
    return ok


def load(ws, rev, name, tmp):
    src = subprocess.run(["git", "-C", ws, "show", f"{rev}:scripts/{name}.py"], check=True, capture_output=True,
                         text=True).stdout
    path = os.path.join(tmp, f"{name}.py")
    with open(path, "w") as f:
        f.write(src)
    spec = importlib.util.spec_from_file_location(f"ref_{name}", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod, path


# --- the Python side of each case ----------------------------------------------------------------
def py_eval(D, E, case, tmp):
    fn, a = case["fn"], case["args"]
    try:
        if fn == "assess":
            return {"ok": D.assess(dict(a["sig"]), a.get("human_paths") or [])}
        if fn == "validate":
            errors, nodes = D.validate(json.loads(json.dumps(a["graph"])), a.get("human_paths") or [])
            return {"ok": {"errors": errors, "nodes": [plan_json(D, n) for n in nodes]}}
        if fn == "validate_disk":
            files = set(a.get("files") or [])
            human = a.get("human_paths") or []
            errors, nodes = D.validate(json.loads(json.dumps(a["graph"])),
                                       exists=lambda r: f"{r.repo}:{r.path}" in files,
                                       policy_for=lambda name: human, dirs=dict(a.get("dirs") or []),
                                       skip=set(a.get("skip") or []))
            return {"ok": {"errors": errors, "nodes": [plan_json(D, n) for n in nodes]}}
        if fn == "add_hours":  # the timedelta the forecast adds, to the microsecond
            now = E.parse_ts(a["now"])
            return {"ok": (now + dt.timedelta(hours=a["hours"]) - now) // dt.timedelta(microseconds=1)}
        if fn == "brief":
            errors, nodes = D.validate(json.loads(json.dumps(a["graph"])))
            if errors:
                return {"err": "\n".join(errors)}
            node = next((n for n in nodes if str(n["id"]) == a["node"]), None)
            if node is None:
                return {"err": "no node"}
            return {"ok": D.brief_block({**node, "repo_dirs": {}}, a.get("ticket"))}
        if fn == "rule_matches":
            return {"ok": D.rule_matches(a["rule"], a["path"])}
        if fn == "is_governance":
            return {"ok": D.is_governance(a["path"], a.get("human_paths") or [])}
        if fn == "is_test_path":
            return {"ok": D.is_test_path(a["path"])}
        if fn == "normpath":
            return {"ok": os.path.normpath(a["path"])}
        if fn == "require_human_paths":
            path = os.path.join(tmp, "policy.yaml")
            with open(path, "w") as f:
                json.dump(a["policy"], f)
            try:
                return {"ok": D.require_human_paths(path)}
            except D.PolicyUnreadable as e:
                return {"err": str(e).removeprefix(f"{path}: ")}
        if fn == "parse_ts":
            return {"ok": E.iso(E.parse_ts(a["value"]))}
        if fn in ("capacity", "resume_gate", "gate_bounds"):
            now = E.parse_ts(a["now"])
            gate = E.gate_bounds(a["state"], now, a["usage"]) if "state" in a else {}
            if fn == "gate_bounds":
                return {"ok": gate}
            if fn == "resume_gate":
                return {"ok": E.resume_gate(E.sample_from_usage(a["usage"], now)["accounts"], now, **gate)}
            return {"ok": E.capacity(a["usage"], a["samples"], now, **gate)}
        if fn == "session_gates":
            real = E.session_tokens
            E.session_tokens = lambda path, now: {"context_k": a["context_k"]}
            try:
                r = E.session_row("x" * 8, "", "unused", None, a["driven"])
            finally:
                E.session_tokens = real
            return {"ok": {"handoff_candidate": r["handoff_candidate"], "successor_due": r["successor_due"]}}
        if fn == "tier_signals":
            rows, signals = E.tier_signals(a["data"])
            return {"ok": {"rows": rows, "replan": signals}}
        raise KeyError(fn)
    except ValueError as e:
        return {"err": str(e)}
    except Exception as e:  # what the Python does not catch: a traceback, compared by its kind
        return {"crash": type(e).__name__}


def plan_json(D, n):
    return {**{k: v for k, v in n.items() if k != "input_refs"}, "contract": D.contract(n["tier"], n["thinking"])}


def same(a, b):
    """Equal values of equal JSON types (an int is not a float, a bool is not an int)."""
    if type(a) is not type(b):
        return False
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(same(a[k], b[k]) for k in a)
    if isinstance(a, list):
        return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b))
    return a == b


def same_result(want, got):
    """A crash is compared by the Python exception's type name, like every other value."""
    return same(want, got)


# --- deterministic cases -------------------------------------------------------------------------
KINDS = ["rerun", "update-branch", "merge-on-green", "archive", "cleanup", "closeout", "relay", "resume",
         "implement", "research", "docs", "design", "decompose", "review", "bogus", None, ""]
PATH_SETS = [[], ["src/a.rs"], ["src/a.rs", "src/b.rs"], ["tests/x.rs"],
             ["src/a.rs", "b.py", "c.ts", "d.go", "e.rb"],
             ["a_test.go", "x.spec.ts", "openapi.spec.yaml", "b.rs", "c.rs"], [".control/policy.yaml"],
             ["CLAUDE.md"], ["./AGENTS.md"], ["scripts/x-hook.sh"], ["docs/notes/README.md"],
             ["Scripts/Delegation.py"], ["deploy/prod.yaml", "src/a.rs"], ["sub/claude.md"],
             ["a.rs", "b.rs", "c.rs", "d.rs"], ["test_a.py", "spec/b.rb", "__tests__/c.js", "x.test.ts"]]
HUMAN = [[], ["scripts/*-hook.sh", "deploy/"]]


def assess_cases(rng):
    dims = {"kind": KINDS, "paths": PATH_SETS, "est_loc": [None, 0, 150, 200, 201, "300", " 1_000 ", "abc", 3.7, True],
            "files": [None, 0, 1, 2, 5], "repos": [None, 1, 2, 3], "public_api": [False, True],
            "one_way_door": [False, True], "spec": [None, "docs/specs/x.html"], "failed_attempts": [0, 1, 2, 3, -1],
            "observed_tier": [None, "mechanical", "standard", "frontier", "apex", "bogus"],
            "allow_apex": [False, True], "mode": [None, "normal", "conserve", "critical", "read_only", "read-only",
                                                 "bogus"]}
    base = {"kind": "implement", "paths": ["src/a.rs"]}
    cases = []
    for k, values in dims.items():  # each dimension alone, from a bounded base
        for v in values:
            for h in HUMAN:
                cases.append({"sig": {**base, k: v}, "human_paths": h})
    for _ in range(600):  # and random mixtures of all of them
        sig = {k: rng.choice(v) for k, v in dims.items() if rng.random() < 0.8}
        cases.append({"sig": sig, "human_paths": rng.choice(HUMAN)})
    return [{"fn": "assess", "args": c} for c in cases]


def node(nid, **kw):
    n = {"id": nid, "kind": "implement", "signals": {"paths": ["src/a.rs"]}, "done": "make test"}
    n.update(kw)
    return n


DESIGN = node("spec", kind="design", signals={}, produces=["docs/specs/a.html"], done="spec_check")
GRAPHS = [
    {"nodes": []},
    {"nodes": [node("a")]},
    {"nodes": [node("a"), node("a")]},
    {"nodes": ["not a node", {"kind": "implement"}, node("a")]},
    {"nodes": [node("a", depends_on=["b"])]},
    {"nodes": [node("a", depends_on=["b"]), node("b", depends_on=["a"])]},
    {"nodes": [node("a", depends_on="ab"), node("b")]},
    {"repo": "w", "repos": {"w": ".", "m": "../m"}, "nodes": [node("a", repo="x")]},
    {"repo": "zz", "repos": {"w": "."}, "nodes": [node("a")]},
    {"nodes": [node("a", signals="nope")]},
    {"nodes": [node("a", inputs="docs/specs/a.html", produces=7)]},
    {"nodes": [node("a", signals={"paths": ["../escape.rs"]})]},
    {"nodes": [node("a", signals={"paths": ["/abs.rs"]})]},
    {"nodes": [node("a", signals={"paths": ["other:x.rs"]})]},
    {"repo": "w", "repos": {"w": ".", "m": "m"}, "nodes": [node("a", signals={"paths": ["m:x.rs"]})]},
    {"nodes": [node("a", produces=["out.txt"]), node("b", produces=["./out.txt"])]},
    {"nodes": [node("a", signals={"paths": ["a.rs", "b.rs"], "est_loc": 400})]},
    {"nodes": [DESIGN, node("a", inputs=["docs/specs/a.html"], depends_on=["spec"],
                            signals={"paths": ["a.rs", "b.rs"], "est_loc": 400, "public_api": True})]},
    {"nodes": [DESIGN, node("a", inputs=["docs/specs/a.html"],
                            signals={"paths": ["a.rs", "b.rs"], "est_loc": 400})]},
    {"nodes": [node("a", signals={"paths": ["a.rs"], "spec": "notes/x.md", "est_loc": 400})]},
    {"nodes": [node("a", signals={"paths": ["a.rs"], "spec": "docs/specs/x.html", "est_loc": 400})]},
    {"nodes": [node("a", signals={"paths": ["a.rs"], "repos": 2})]},
    {"nodes": [node("a", tier="mechanical")]},
    {"nodes": [node("a", tier="frontier")]},
    {"nodes": [node("a", tier="frontier", reason="hard")]},
    {"nodes": [node("a", tier="apex")]},
    {"nodes": [node("a", tier="apex", reason="hardest", thinking="xhigh")]},
    {"nodes": [node("a", tier="bogus")]},
    {"nodes": [node("r", kind="review", tier="standard")]},
    {"nodes": [node("r", kind="review", signals={})]},
    {"nodes": [node("r", kind="review", signals={}, thinking="low")]},
    {"nodes": [node("g", signals={"paths": [".control/policy.yaml"]}, thinking="medium")]},
    {"nodes": [node("g", signals={"paths": [".control/policy.yaml"]}, verify="p20")]},
    {"nodes": [node("a", done="  ")]},
    {"nodes": [node("a", done=None)]},
    {"nodes": [node("a", verify="bogus", thinking="max", spawn_default="nope")]},
    {"nodes": [node("a", signals={"paths": ["a.rs"], "est_loc": "many"})]},
    {"nodes": [node("a", kind="bogus")]},
    {"nodes": [node("a", signals={"kind": "rerun"})]},
    {"nodes": [node("a", kind="rerun", signals={})]},
    {"nodes": [node("a", signals={"paths": ["a.rs", "x/test_b.py"]}, spawn_default="mechanical")]},
    {"nodes": [node(1), node("b", depends_on=[1, 2])]},
    {"nodes": [node("a", done=3)]},
    {"repo": "w", "repos": ["w"], "nodes": [node("a")]},
    {"nodes": [node("a", tier="bad\u00a0tier\u200b\u2028\ue000\xad")]},
    {"repo": "w", "repos": "w", "nodes": [node("a")]},
    {"nodes": [node("a", signals={"paths": ["a.rs"], "est_loc": [1]})]},
    {"nodes": [node("a", signals={"paths": 7})]},
    {"nodes": [node("a", signals={"paths": ["scripts/x-hook.sh"]})], "_human": ["scripts/*-hook.sh"]},
    {"ticket": "BRO-1", "repo": "w", "repos": {"w": ".", "m": "m"},
     "nodes": [node("a", inputs=["docs/specs/a.html", "m:docs/adrs/b.html"], signals={"paths": ["a.rs"]}),
               node("b", repo="m", depends_on=["a"], inputs=["w:docs/specs/a.html"], verify="p20-full")]},
]


def graph_cases():
    cases = []
    for g in GRAPHS:
        human = g.pop("_human", [])
        cases.append({"fn": "validate", "args": {"graph": g, "human_paths": human}})
    for g in GRAPHS:
        for n in g.get("nodes") or []:
            if isinstance(n, dict) and isinstance(n.get("id"), str):
                cases.append({"fn": "brief", "args": {"graph": g, "node": n["id"], "ticket": g.get("ticket")}})
    return cases


def disk_cases(rng):
    """validate with files on disk and bound repo directories."""
    w, n = "/r/w", "/r/w/nested"
    graphs = [
        ({"nodes": [node("a", signals={"paths": ["a.rs"], "spec": "docs/specs/x.html", "est_loc": 400})]}, []),
        ({"nodes": [node("a", signals={"paths": ["a.rs"], "spec": "docs/specs/x.html", "est_loc": 400})]},
         [":docs/specs/x.html"]),
        ({"nodes": [node("a", inputs=["docs/specs/x.html"], signals={"paths": ["a.rs", "b.rs"], "est_loc": 400})]},
         [":docs/specs/x.html"]),
        ({"nodes": [node("a", inputs=["docs/specs/x.html"], signals={"paths": ["a.rs", "b.rs"], "est_loc": 400})]}, []),
        ({"nodes": [node("a", inputs=["notes/x.html", "docs/adrs/y.html"],
                         signals={"paths": ["a.rs"], "public_api": True})]}, [":notes/x.html", ":docs/adrs/y.html"]),
        ({"nodes": [DESIGN, node("a", inputs=["docs/specs/a.html"], depends_on=["spec"],
                                 signals={"paths": ["a.rs"], "est_loc": 400})]}, []),
    ]
    cases = [{"fn": "validate_disk", "args": {"graph": g, "files": f, "dirs": []}} for g, f in graphs]
    nested = [
        {"repo": "w", "repos": {"w": w, "n": n}, "nodes": [node("a", signals={"paths": ["nested/x.rs"]})]},
        {"repo": "w", "repos": {"w": w, "n": n}, "nodes": [node("a", signals={"paths": ["nested"]})]},
        {"repo": "w", "repos": {"w": w, "n": n}, "nodes": [node("a", inputs=["nested/docs/specs/a.html"])]},
        {"repo": "w", "repos": {"w": w, "n": n}, "nodes": [node("a", signals={"paths": ["nestedx/a.rs"]})]},
        {"repo": "w", "repos": {"w": w, "n": n}, "nodes": [node("a", repo="n", signals={"paths": ["x.rs"]})]},
    ]
    for g in nested:
        for dirs in ([["w", w], ["n", n]], [["n", n], ["w", w]]):
            cases.append({"fn": "validate_disk", "args": {"graph": g, "files": [], "dirs": dirs}})
    # Nodes whose repo is not here: structure only (a done check), never assessed or returned.
    skipped = {"repo": "w", "repos": {"w": w, "g": "/r/gone"}, "nodes": [
        node("a"), node("g", repo="g", tier="mechanical", signals={"paths": ["a.rs", "b.rs"], "est_loc": 900}),
        node("h", repo="g", done="  ")]}
    for skip in ([], ["g"], ["g", "h"]):
        cases.append({"fn": "validate_disk", "args": {"graph": skipped, "files": [], "dirs": [], "skip": skip}})
    for _ in range(300):
        h = rng.choice([rng.uniform(0, 400), rng.uniform(0, 1), rng.randrange(1, 10**6) / 8, rng.random() * 1e4])
        cases.append({"fn": "add_hours", "args": {"now": "2026-10-22T12:00:00Z", "hours": h}})
    cases += [{"fn": "add_hours", "args": {"now": "2026-10-22T12:00:00Z", "hours": h}}
              for h in [0.0, 1.5, 1 / 3, 2.5e-10, 1.25e-10, 3.75e-10, 1e-12, 123.456789,
                        # where hours * 3600 then rounding (not CPython's timedelta) is 1 µs off
                        6687.660218274861, 9987.080269162361, 7009.91369442625, 9937.848669440416]]
    return cases


def small_cases():
    rules = ["scripts/*-hook.sh", "a?c", "*", "**", "x*y*z", "Docs/", "[x]", "a.b", "*.md", "?"]
    paths = ["scripts/a-hook.sh", "scripts/x/a-hook.sh", "abc", "a/c", "docs/x", "xay/z", "xyz", "[x]", "a.b",
             "axb", "readme.md", "dir/readme.md", "", "a"]
    cases = [{"fn": "rule_matches", "args": {"rule": r, "path": p}} for r in rules for p in paths]
    gov = ["CLAUDE.md", "./agents.md", "x/claude.md", ".control/policy.yaml", "a/.broomva/loops/x", "METALAYER.md",
           "references/delegation.md", "scripts/fleet_economy.py", "docs/references/current-work.md",
           "x/docs/references/workspace-context.md", "docs/references/agent-operating-protocols.md", "deploy/x", "scripts/a-hook.sh", "src/a.rs"]
    cases += [{"fn": "is_governance", "args": {"path": p, "human_paths": h}} for p in gov for h in HUMAN]
    tests = ["tests/a.rs", "src/test/a.rs", "__tests__/a.js", "spec/a.rb", "test_a.py", "a.test.ts", "a_test.go",
             "a_spec.rb", "a.spec.ts", "openapi.spec.yaml", "x.spec.md", "src/a.rs", "Tests/A.RS", "contest.rs",
             "attest/a.rs"]
    cases += [{"fn": "is_test_path", "args": {"path": p}} for p in tests]
    norms = ["", ".", "a/./b/", "../a", "a/../..", "/../a", "//a", "///a/b/../c", "a/..", "a//b", "./x", "x/"]
    cases += [{"fn": "normpath", "args": {"path": p}} for p in norms]
    policies = [{}, None, [], {"auto_merge": None}, {"auto_merge": []}, {"auto_merge": {"rules": "x"}},
                {"auto_merge": {"rules": 5}},
                {"auto_merge": {"rules": [{"action": "require_human", "path_touched": ".control/"},
                                          {"action": "require_human"}, {"action": "allow", "path_touched": "x"},
                                          "junk", {"action": "require_human", "path_touched": 7}]}}]
    cases += [{"fn": "require_human_paths", "args": {"policy": p}} for p in policies]
    stamps = ["2026-10-22T12:00:00Z", "2026-10-22T12:00:00+00:00", "2026-10-22T07:00:00-05:00",
              "2026-10-22T12:00:00.123456Z", "2026-10-22T12:00:00.5Z", "2026-10-22 12:00", "2026-10-22T12",
              "2026-10-22", " 2026-10-22T12:00:00Z ", "2026-02-30T00:00:00", "2026-10-22T25:00:00", "garbage",
              "", None, 1_700_000_000, 1_700_000_000_000, 1_700_000_000.25, True, "2026-10-22T12:00:00.1234567Z",
              "2026-10-22T12:00:00+05:30"]
    cases += [{"fn": "parse_ts", "args": {"value": s}} for s in stamps]
    cases += [{"fn": "session_gates", "args": {"context_k": c, "driven": d}}
              for c in [None, 0, 399, 400, 401, 600, 601] for d in [False, True]]
    datas = [{}, {"tiers": None}, {"tiers": {"standard": {"runs": 0}}},
             {"tiers": {"standard": {"runs": 10, "raisedRuns": 3, "approved": 4, "firstPass": 0.25},
                        "frontier": {"runs": 4, "raisedRuns": 1, "approved": 2, "firstPass": 0.125},
                        "unknown": {"runs": 2.0, "raisedRuns": 2.0, "approved": 3, "firstPass": 0.005},
                        "apex": "nope"}}]
    cases += [{"fn": "tier_signals", "args": {"data": d}} for d in datas]
    return cases


def capacity_cases(rng):
    now = dt.datetime(2026, 10, 22, 12, 0, tzinfo=dt.timezone.utc)
    iso = lambda d: d.isoformat().replace("+00:00", "Z")  # noqa: E731
    cases = []
    for _ in range(150):
        rows = []
        for i in range(rng.choice([0, 1, 2, 3])):
            reset = rng.choice([now + dt.timedelta(hours=2), now + dt.timedelta(days=3, minutes=7),
                                now - dt.timedelta(hours=1), None, now + dt.timedelta(days=6, hours=23)])
            rows.append({"email": rng.choice([f"acct{i}@x.io", f"acct{i}", "dup@a.io"]) if i else f"main@x.io",
                         "fiveHourUtil": rng.choice([None, 10, 50, 95, 96.5, 99.99]),
                         "sevenDayUtil": rng.choice([None, 0, 12.5, 45.5, 89, 90, 99.9, 100, "x"]),
                         "sevenDayResetsAt": None if reset is None else rng.choice(
                             [iso(reset), int(reset.timestamp() * 1000)]),
                         "fiveHourResetsAt": iso(now + dt.timedelta(hours=1)),
                         "isActive": i == 0 or rng.random() < 0.2, "isRateLimited": rng.random() < 0.15,
                         "stale": rng.random() < 0.1})
        samples = []
        names = [str(r.get("email", "?")).split("@")[0] for r in rows]
        for h in range(rng.choice([0, 1, 2, 8, 30]), 0, -1):
            at = now - dt.timedelta(hours=h * rng.choice([0.4, 1, 1.5]))
            accts = {}
            for n, r in zip(names, rows):
                seven = r["sevenDayUtil"]
                if isinstance(seven, (int, float)):
                    seven = max(0.0, seven - h * rng.choice([0, 0.5, 1.25]))
                    if rng.random() < 0.05:
                        seven += 30  # a drop afterwards: a reset
                accts[n] = {"7d": seven, "reset_7d": rng.choice([r["sevenDayResetsAt"], r["sevenDayResetsAt"],
                                                                 iso(now + dt.timedelta(days=5))]),
                            "stale": rng.random() < 0.05}
            samples.append({"at": iso(at) if rng.random() < 0.97 else iso(now + dt.timedelta(hours=1)),
                            "accounts": accts})
        cases.append({"fn": "capacity", "args": {"usage": rows, "samples": samples, "now": iso(now)}})
    return cases


def gate_rows(rng, now, iso):
    """Usage rows on the gate's boundaries (BRO-3223): 5h at 70 and 90, 7d at 95, stale ages either
    side of 30 minutes, the 5h window open or reset, needs-login, no stored credentials."""
    rows = []
    for i in range(rng.choice([0, 1, 2, 3, 4])):
        reset7 = rng.choice([now + dt.timedelta(days=2), now - dt.timedelta(minutes=5), None])
        rows.append({"id": f"id{i}", "email": f"a{i}@x.io" if rng.random() < 0.9 else "a0@y.io",
                     "fiveHourUtil": rng.choice([None, 0, 10, 69.9, 70, 70.1, 89.9, 90, 92, "x"]),
                     "sevenDayUtil": rng.choice([None, 5, 50.5, 89, 94.9, 95, 99]),
                     "sevenDayResetsAt": None if reset7 is None else iso(reset7),
                     "fiveHourResetsAt": rng.choice([iso(now + dt.timedelta(hours=2)), iso(now - dt.timedelta(minutes=1)),
                                                     None, int((now + dt.timedelta(hours=1)).timestamp())]),
                     "isActive": (i == 0 and rng.random() < 0.9) or rng.random() < 0.1,
                     "isRateLimited": rng.random() < 0.15, "stale": rng.random() < 0.45,
                     "numbersAgeSeconds": rng.choice([None, 0, 1799, 1800, 1801, 7200, 59.5]),
                     **rng.choice([{}, {}, {"needsLogin": True}, {"needsLogin": 0}, {"hasStoredCredentials": False},
                                   {"hasStoredCredentials": True}, {"hasStoredCredentials": None}])})
    return rows


def gate_state(rng, now, rows):
    """`provider_manager.py --json state` shapes: failover on or off, holds, pending signals at the TTL,
    probes either side of the hour, bounds as numbers or strings, and a few malformed values."""
    t = now.timestamp()
    st = {}
    if rng.random() < 0.9:
        st["versionGate"] = rng.choice([{"automaticSwitching": "on"}] * 9 + [{"automaticSwitching": "off"}, {}, None])
    if rng.random() < 0.9:
        cfg = {"autoBalance": rng.choice([True] * 8 + [False, 1, "true", None])}
        for k, vals in (("staleNumbersMinutes", [30, 10, 60.5, "45", " 5 ", "1_0"]),
                        ("standbyMax", [70, 50, 80.0, "70"]), ("standbyWeeklyMax", [95, 90, 99.5])):
            if rng.random() < 0.5:
                cfg[k] = rng.choice(vals)
        st["config"] = cfg
    if rng.random() < 0.3:
        st["holdUntil"] = rng.choice([None, "", iso_z(now + dt.timedelta(minutes=10)),
                                      iso_z(now - dt.timedelta(minutes=10)), int((t + 600) * 1000)])
    if rng.random() < 0.4:
        st["pendingSignals"] = rng.choice([[], [{"at": t - 899}], [{"at": t - 900}], [{"at": t - 3000}, {"at": t - 10}],
                                           [{"at": str(t - 5)}], [{}], [{"at": None}]])
    if rng.random() < 0.6 and rows:
        probes = {}
        for r in rng.sample(rows, rng.randint(1, len(rows))):
            probes[r["id"]] = {"result": rng.choice(["limited", "limited", "ok"]),
                               "at": rng.choice([t - 3599, t - 3600, t - 10, t - 7200, str(t - 60)])}
        if rng.random() < 0.2:
            probes["ghost"] = {"result": "limited", "at": t}
        st["probes"] = dict(sorted(probes.items()))  # Rust's map iterates sorted; so does this dict
    return st


def iso_z(d):
    return d.isoformat().replace("+00:00", "Z")


def gate_cases(rng):
    now = dt.datetime(2026, 10, 22, 12, 0, tzinfo=dt.timezone.utc)
    cases = []
    for _ in range(600):
        rows = gate_rows(rng, now, iso_z)
        args = {"usage": rows, "now": iso_z(now)}
        if rng.random() < 0.85:
            args["state"] = gate_state(rng, now, rows)
        cases.append({"fn": "resume_gate", "args": args})
        if "state" in args:
            cases.append({"fn": "gate_bounds", "args": args})
        if rng.random() < 0.3:
            cases.append({"fn": "capacity", "args": {**args, "samples": []}})
    # Failover live and at least two accounts: who covers a stale or unspendable active account.
    for _ in range(200):
        rows = []
        while len(rows) < 2:
            rows = gate_rows(rng, now, iso_z)
        st = gate_state(rng, now, rows)
        st.update(versionGate={"automaticSwitching": "on"}, config={**(st.get("config") or {}), "autoBalance": True})
        st.pop("holdUntil", None), st.pop("pendingSignals", None)
        args = {"usage": rows, "now": iso_z(now), "state": st}
        cases += [{"fn": "resume_gate", "args": args}, {"fn": "capacity", "args": {**args, "samples": []}}]
    # Exact boundaries (P20 r1, Stratum B): resets and holds at now, a duplicate id, a bool bound, a rank tie.
    on = {"versionGate": {"automaticSwitching": "on"}, "config": {"autoBalance": True}}
    acct = lambda name, **kw: {"id": name, "email": f"{name}@x.io", "fiveHourUtil": 10, "sevenDayUtil": 10,  # noqa: E731
                               "isActive": False, **kw}
    at_now, t = iso_z(now), now.timestamp()
    edge = [
        ([acct("a", isActive=True, sevenDayUtil=96, sevenDayResetsAt=at_now)], {}),
        ([acct("a", isActive=True, stale=True, numbersAgeSeconds=7200, fiveHourUtil=92, fiveHourResetsAt=at_now),
          acct("b")], on),
        ([acct("a", isActive=True, stale=True, numbersAgeSeconds=7200), acct("b")], {**on, "holdUntil": at_now}),
        ([acct("a", isActive=True, stale=True, numbersAgeSeconds=7200), acct("b", fiveHourUtil=20, sevenDayUtil=None),
          acct("c", fiveHourUtil=20, sevenDayUtil=0.5)], on),
        ([acct("a", isActive=True), {**acct("x"), "email": "b@x.io"}, {**acct("x"), "email": "c@x.io"}],
         {**on, "probes": {"x": {"result": "limited", "at": t}}}),
        ([acct("a", isActive=True, stale=True, numbersAgeSeconds=90)],
         {**on, "config": {"autoBalance": True, "staleNumbersMinutes": True}}),
    ]
    for rows, st in edge:
        args = {"usage": rows, "now": at_now, "state": st}
        cases += [{"fn": "resume_gate", "args": args}, {"fn": "gate_bounds", "args": args}]
    # Malformed state the Python crashes or raises on, named by kind.
    bad = [{"pendingSignals": {"a": 1}}, {"pendingSignals": "x"}, {"pendingSignals": 5}, {"pendingSignals": [1]},
           {"versionGate": "on"}, {"versionGate": {"automaticSwitching": "on"}, "config": "x"}, {"config": [1]},
           {"probes": [1]}, {"config": {"staleNumbersMinutes": None}}, {"config": {"standbyMax": "abc"}},
           {"config": {"standbyMax": "1__0"}}, {"holdUntil": "soon"}, {"pendingSignals": [{"at": "x"}]},
           {"probes": {"id0": "limited"}}, {"probes": {"id0": {"result": "limited", "at": "x"}}}]
    rows = [{"id": "id0", "email": "a0@x.io", "fiveHourUtil": 10, "sevenDayUtil": 10, "isActive": True}]
    cases += [{"fn": "gate_bounds", "args": {"usage": rows, "now": iso_z(now), "state": b}} for b in bad]
    cases += [{"fn": "gate_bounds", "args": {"usage": [{"id": [1], "email": "a"}], "now": iso_z(now), "state": {}}},
              {"fn": "gate_bounds", "args": {"usage": rows, "now": iso_z(now), "state": []}}]
    return cases


# Every field, given a value of each type: where the Python raises (P20 Stratum A, port arc 2: an
# unhashable id or mode, a non-mapping usage row or sample, a non-numeric tier count, a timestamp or
# runway past datetime's years), Maestro must raise the same kind, never decide. Not swept: numbers
# between 6.7e16 s and time_t's limit, where the kind is the platform's (OSError on macOS).
TYPE_VALS = {"list": ["x"], "dict": {"k": 1}, "null": None, "int": 7, "float": 1.5, "bool": True, "str": "zz",
             "emptylist": [], "emptydict": {}, "huge": 1e300, "tiny": 1e-6, "neg": -5,
             "year10k": 253402300800, "ts": "2026-10-29T00:00:00Z"}


def type_cases():
    cases = []
    tv = TYPE_VALS.items
    base = {"repo": "workspace", "nodes": [
        {"id": "a", "kind": "implement", "done": "true", "inputs": [], "produces": ["b.rs"], "depends_on": [],
         "tier": "standard", "verify": "check", "thinking": "medium", "spawn_default": "standard",
         "signals": {"paths": ["a.rs"], "mode": "conserve", "est_loc": 10, "files": 1, "observed_tier": "standard",
                     "failed_attempts": 0, "public_api": False, "repos": 1, "kind": "implement"}},
        {"id": "b", "kind": "implement", "done": "true", "signals": {"paths": ["c.rs"]}, "depends_on": ["a"]}]}
    for name, v in tv():
        for k in ("repo", "repos", "nodes"):
            if k == "repo" and name in ("int", "float", "bool", "huge", "tiny", "neg", "year10k"):
                continue  # a numeric repo name is printed as a string (README, known differences)
            g = copy.deepcopy(base); g[k] = v
            cases.append({"fn": "validate", "args": {"graph": g}})
        for k in ("id", "kind", "done", "signals", "inputs", "produces", "depends_on", "tier", "verify", "thinking",
                  "spawn_default", "repo"):
            g = copy.deepcopy(base); g["nodes"][0][k] = v
            cases.append({"fn": "validate", "args": {"graph": g}})
        for k in base["nodes"][0]["signals"]:
            g = copy.deepcopy(base); g["nodes"][0]["signals"][k] = v
            cases.append({"fn": "validate", "args": {"graph": g}})
        for where in ("depends_on", "inputs", "produces"):
            g = copy.deepcopy(base); g["nodes"][1 if where == "depends_on" else 0][where] = [v]
            cases.append({"fn": "validate", "args": {"graph": g}})
    now = "2026-10-23T00:00:00Z"
    row = {"id": "a", "email": "a@x.io", "fiveHourUtil": 10, "sevenDayUtil": 10,
           "sevenDayResetsAt": "2026-10-28T00:00:00Z", "fiveHourResetsAt": "2026-10-23T01:00:00Z", "isActive": True,
           "isRateLimited": False, "stale": False, "numbersAgeSeconds": 10, "needsLogin": False,
           "hasStoredCredentials": True}
    row2 = {**row, "id": "b", "email": "b@x.io", "isActive": False}
    sample = {"at": "2026-10-22T20:00:00Z",
              "accounts": {"a": {"7d": 8, "reset_7d": "2026-10-28T00:00:00Z", "stale": False}}}
    state = {"versionGate": {"automaticSwitching": "on"},
             "config": {"autoBalance": True, "staleNumbersMinutes": 30, "standbyMax": 70, "standbyWeeklyMax": 95},
             "holdUntil": None, "pendingSignals": [], "probes": {"b": {"result": "ok", "at": 1761177600}}}
    tiers = {"tiers": {"standard": {"runs": 10, "raisedRuns": 3, "approved": 4, "firstPass": 0.25,
                                    "tokensPerApproved": 5, "raises": 1, "escalations": 0, "unmeasured": 0}}}
    for name, v in tv():
        for k in row:
            r = {**row, k: v}
            for fn in ("capacity", "resume_gate"):
                cases.append({"fn": fn, "args": {"usage": [r, row2], "samples": [sample], "now": now}})
                cases.append({"fn": fn, "args": {"usage": [r, row2], "samples": [sample], "now": now, "state": state}})
        cases.append({"fn": "capacity", "args": {"usage": [row, v], "samples": [], "now": now}})
        cases.append({"fn": "capacity", "args": {"usage": [row], "samples": [v], "now": now}})
        cases.append({"fn": "capacity", "args": {"usage": [], "samples": [v], "now": now}})
        for k in ("at", "accounts"):
            cases.append({"fn": "capacity", "args": {"usage": [row], "samples": [{**sample, k: v}], "now": now}})
        for k in ("7d", "reset_7d", "stale"):
            s = copy.deepcopy(sample); s["accounts"]["a"][k] = v
            cases.append({"fn": "capacity", "args": {"usage": [row], "samples": [s], "now": now}})
        s = copy.deepcopy(sample); s["accounts"]["a"] = v
        cases.append({"fn": "capacity", "args": {"usage": [row], "samples": [s], "now": now}})
        for k in state:
            st = {**state, k: v}
            for fn in ("gate_bounds", "resume_gate", "capacity"):
                cases.append({"fn": fn, "args": {"usage": [row, row2], "samples": [], "now": now, "state": st}})
        for k in tiers["tiers"]["standard"]:
            d = copy.deepcopy(tiers); d["tiers"]["standard"][k] = v
            cases.append({"fn": "tier_signals", "args": {"data": d}})
        cases.append({"fn": "tier_signals", "args": {"data": {"tiers": {"standard": v}}}})
        cases.append({"fn": "tier_signals", "args": {"data": {"tiers": v}}})
        cases.append({"fn": "tier_signals", "args": {"data": v}})
        cases.append({"fn": "parse_ts", "args": {"value": v}})
    for h in (6.7e7, 7e7, -1e8, 1e8, 1.1e8, 1e300):
        cases.append({"fn": "add_hours", "args": {"now": "2026-10-22T12:00:00Z", "hours": h}})
    for s in (253402300799, 253402300799.9999999, -62135596800, -62135596801, 1e13, -1e13, 1e15, 1e20, -1e20):
        cases.append({"fn": "parse_ts", "args": {"value": s}})
    return cases


def all_cases():
    rng = random.Random(3204)
    return (assess_cases(rng) + graph_cases() + disk_cases(rng) + small_cases() + capacity_cases(rng)
            + gate_cases(random.Random(3223)) + type_cases())


# --- runners -------------------------------------------------------------------------------------
def run(cmd, cwd=None, stdin=None):
    p = subprocess.run(cmd, cwd=cwd, input=stdin, capture_output=True, text=True)
    return p.returncode, p.stdout, p.stderr


def rust_eval(maestro, cases):
    code, out, err = run([maestro, "parity-eval"], stdin=json.dumps(cases))
    if code:
        raise SystemExit(f"maestro parity-eval failed ({code}): {err}")
    return json.loads(out)


def compare_cli(name, py_cmd, rs_cmd, cwd, parse_json=False):
    pc, po, pe = run(py_cmd, cwd)
    rc, ro, re_ = run(rs_cmd, cwd)
    cli_runs.append(pc)
    if pc == 1 and "Traceback" in pe:  # an uncaught Python error: the Rust exits 1 with the same exception kind
        kind = pe.strip().splitlines()[-1].split(":", 1)[0]
        return check(name, rc == 1 and "Traceback" in re_ and f"\n  {kind}: " in re_ and po == ro,
                     f"python raised {kind}; rust exit {rc}: {re_[:200]}")
    ok = check(name, pc == rc, f"exit python {pc} rust {rc}\n  py: {pe[:300]}\n  rs: {re_[:300]}")
    ok &= check(name, pe == re_, f"stderr differs\n  py: {pe[:600]}\n  rs: {re_[:600]}")
    if parse_json and pc == 0:
        ok &= check(name, same(json.loads(po), json.loads(ro)), "JSON stdout differs")
    else:
        ok &= check(name, po == ro, f"stdout differs\n  py: {po[:600]}\n  rs: {ro[:600]}")
    return ok


def real_graphs(ws, dpy, maestro):
    import yaml
    n = 0
    for g in sorted(os.listdir(os.path.join(ws, "docs", "plans"))):
        if not g.endswith(".graph.yaml"):
            continue
        path = os.path.join("docs", "plans", g)
        with open(os.path.join(ws, path)) as f:
            graph = yaml.safe_load(f) or {}
        bind = ["--repo", "workspace=."] if "workspace" in (graph.get("repos") or {}) else []
        pre = bind + ["--skip-absent-repos"]
        rs_bind = bind + ["--skip-absent-repos"]
        compare_cli(f"plan {g}", ["python3", dpy, *pre, "plan", path], [maestro, "delegation", "plan", path, *rs_bind], ws)
        compare_cli(f"plan --json {g}", ["python3", dpy, *pre, "plan", path, "--json"],
                    [maestro, "--json", "delegation", "plan", path, *rs_bind], ws, parse_json=True)
        for nd in graph.get("nodes") or []:
            nid = str(nd.get("id"))
            compare_cli(f"brief {g} {nid}", ["python3", dpy, *pre, "brief", path, nid],
                        [maestro, "delegation", "brief", path, nid, *rs_bind], ws)
        n += 1
    return n


# --- CLI scenarios: scratch repos, both CLIs, and the committed CLI golden ----------------------------
# Paths are written under "{ROOT}", the scratch root, so fixtures/delegation/cli-golden.json is the same
# on every machine; crates/cli/tests/delegation.rs rebuilds LAYOUT and replays every case through the
# binary, so the CLI's binding, policy and absent-repo guards are held by `cargo test` as well.
R = "{ROOT}"
W, NESTED, BARE = f"{R}/w", f"{R}/w/nested", f"{R}/bare"


def cli_layout():
    good = node("a", inputs=["docs/specs/a.html", "bare:docs/adrs/b.html", "docs/specs/missing.html"],
                signals={"paths": ["src/a.rs"]})
    graphs = {
        "ok": {"repo": "w", "repos": {"w": W, "bare": {"path": BARE, "policy": "none"}}, "nodes": [
            good, node("b", repo="bare", depends_on=["a"], inputs=["w:docs/specs/a.html"])]},
        "nested": {"repo": "w", "repos": {"w": W, "n": NESTED}, "nodes": [node("a", signals={"paths": ["nested/x.rs"]})]},
        "same-dir": {"repo": "w", "repos": {"w": W, "v": f"{R}/w-link"}, "nodes": [node("a")]},
        "absent": {"repo": "w", "repos": {"w": W, "gone": f"{R}/gone"},
                   "nodes": [node("a"), node("g", repo="gone", inputs=["docs/specs/x.html"],
                                             signals={"paths": ["a.rs"], "spec": "docs/specs/x.html"})]},
        "absent-spec": {"repo": "w", "repos": {"w": W, "gone": f"{R}/gone"}, "nodes": [
            node("a", inputs=["gone:docs/specs/x.html"],
                 signals={"paths": ["a.rs"], "spec": "gone:docs/specs/x.html", "est_loc": 400})]},
        "no-policy": {"repo": "b", "repos": {"b": BARE}, "nodes": [node("a")]},
        "policy-none-but-file": {"repo": "w", "repos": {"w": {"path": W, "policy": "none"}}, "nodes": [node("a")]},
        "governed": {"repo": "w", "repos": {"w": W}, "nodes": [node("a", signals={"paths": ["deploy/x.yaml"]})]},
        "bad-name": {"repo": "w", "repos": {"w": W, "a:b": BARE}, "nodes": [node("a")]},
        "no-path": {"repo": "w", "repos": {"w": W, "x": {"policy": "none"}}, "nodes": [node("a")]},
        "single": {"nodes": [node("a", inputs=["docs/specs/a.html"])]},
        "repos-list": {"repo": "w", "repos": ["w"], "nodes": [node("a")]},
        "int-id": {"repo": "w", "repos": {"w": W}, "nodes": [node(1), node("a", depends_on=[1])]},
        "int-id-only": {"repo": "w", "repos": {"w": W}, "nodes": [node(1)]},
        # P20 port arc 2, Stratum A: values the Python hashes, so a list raises before anything is planned.
        "list-id": {"repo": "w", "repos": {"w": W}, "nodes": [node(["evil"])]},
        "list-mode": {"repo": "w", "repos": {"w": W}, "nodes": [node("a", signals={"paths": ["a.rs"], "mode": ["conserve"]})]},
        "list-dep": {"repo": "w", "repos": {"w": W}, "nodes": [node("a"), node("b", depends_on=[["a"]])]},
        # The CLI's skipped set hashes `(repo or home)` before validate sees the graph.
        "list-repo-absent": {"repo": "w", "repos": {"w": W, "gone": f"{R}/gone"},
                             "nodes": [node("a", repo=["gone"]), node("g", repo="gone")]},
        # ...even where validate would stop first, on a home the graph does not name.
        "list-repo-unnamed-home": {"repo": "zz", "repos": {"w": W, "gone": f"{R}/gone"},
                                   "nodes": [node("a", repo=["gone"])]},
        "list-id-unnamed-home": {"repo": "zz", "repos": {"w": W, "gone": f"{R}/gone"},
                                 "nodes": [node(["x"], repo="gone")]},
    }
    files = {f"{name}.json": json.dumps(g) for name, g in graphs.items()}
    files["astral.json"] = json.dumps({"repo": "w", "repos": {"w": W, "n": NESTED},
                                       "nodes": [node("a", done="ship it \U0001F680", signals={"paths": ["nested/x.rs"]})]})
    files["list.graph.yaml"] = "- a\n- b\n"
    files["empty.graph.yaml"] = ""
    files["w/.control/policy.yaml"] = "auto_merge:\n  rules:\n    - {action: require_human, path_touched: deploy/}\n"
    files["w/docs/specs/a.html"] = ""
    files["bare/docs/adrs/b.html"] = ""
    files["economy-str-runs.json"] = json.dumps({"tiers": {"standard": {"runs": "10", "raisedRuns": 3, "approved": 4,
                                                                         "firstPass": 0.25}}})
    files["economy-list.json"] = json.dumps({"tiers": ["standard"]})
    files["economy.json"] = json.dumps({"generatedAt": "2026-10-22T00:00:00Z", "tiers": {
        "standard": {"runs": 10, "raisedRuns": 3, "approved": 4, "firstPass": 0.25, "tokensPerApproved": 1234567},
        "frontier": {"runs": 2, "approved": 0, "firstPass": None, "tokensPerApproved": 980}}})
    return {"dirs": ["w/nested", "bare"], "files": files, "symlinks": {"w-link": "w"}}


def cli_cases():
    cases = []

    def add(name, py, rs, cwd="w", parse_json=False, tool="delegation"):
        cases.append({"name": name, "tool": tool, "py": py, "rs": rs, "cwd": cwd, "json": parse_json})

    names = ["ok", "nested", "same-dir", "absent", "absent-spec", "no-policy", "policy-none-but-file", "governed",
             "bad-name", "no-path", "single", "repos-list", "int-id", "int-id-only", "list-id", "list-mode",
             "list-dep", "list-repo-absent", "list-repo-unnamed-home", "list-id-unnamed-home"]
    for name in names:
        path = f"{R}/{name}.json"
        variants = [[], ["--skip-absent-repos"]]
        if name == "single":
            variants += [["--root", W], ["--root", BARE]]
        if name == "ok":
            variants += [["--repo", f"bare={BARE}"], ["--repo", "nope=x"], ["--repo", "bad"], ["--root", W],
                         ["--repo", f"bare={R}/gone", "--repo", f"bare={BARE}"],
                         ["--repo", f"bare={BARE}", "--repo", f"bare={R}/gone"]]
        for flags in variants:
            add(f"{name} plan {' '.join(flags)}", [*flags, "plan", path], ["delegation", "plan", path, *flags])
            add(f"{name} plan --json {' '.join(flags)}", [*flags, "plan", path, "--json"],
                ["--json", "delegation", "plan", path, *flags], parse_json=True)
            add(f"{name} brief a {' '.join(flags)}", [*flags, "brief", path, "a"], ["delegation", "brief", path, "a", *flags])
    for g, extra in (("list.graph.yaml", ["plan"]),
                     ("astral.json", ["plan"]),
                     ("empty.graph.yaml", ["plan"])):
        sub, rest = extra[0], extra[1:]
        add(f"{g} {' '.join(extra)}", [sub, f"{R}/{g}", *rest], ["delegation", sub, f"{R}/{g}", *rest])
    assess = [["--kind", "implement", "--paths", "src/a.rs", "deploy/x"],
              ["--kind", "implement", "--paths", "a.rs", "b.rs", "--est-loc", "300"],
              ["--kind", "implement", "--paths", "a.rs", "--est-loc", "300", "--spec", "docs/specs/a.html"],
              ["--kind", "implement", "--paths", "a.rs", "--spec", "docs/specs/none.html"],
              ["--kind", "rerun", "--done", "gh run rerun 1 --failed", "--brief", "--ticket", "BRO-1"],
              ["--kind", "rerun", "--brief"],
              ["--kind", "implement", "--paths", "a.rs", "b.rs", "--est-loc", "300", "--done", "x", "--brief"],
              ["--kind", "review", "--mode", "read-only", "--observed-tier", "apex"],
              ["--kind", "implement", "--paths", "a.rs", "--failed-attempts", "3", "--allow-apex", "--mode", "conserve"],
              ["--kind", "implement", "--paths", "a.rs", "--failed-attempts", "-1", "--est-loc", "-5", "--files", "-2"],
              ["--kind", "implement", "--paths", "a.rs", "--est-loc", " 1_000 ", "--mode", "critical", "--mode", "normal"]]
    for args in assess:
        for root_args in (["--root", W], ["--root", BARE], ["--root", BARE, "--no-policy"],
                          ):
            add(f"assess {' '.join(root_args + args)}", [*root_args, "assess", *args],
                ["delegation", "assess", *root_args, *args], parse_json="--brief" not in args)
    add("ladder", ["ladder"], ["delegation", "ladder"], parse_json=True)
    add("tiers", ["tiers", "--path", f"{R}/economy.json"], ["economy", "tiers", "--path", f"{R}/economy.json"],
        tool="economy")
    add("tiers --json", ["tiers", "--json", "--path", f"{R}/economy.json"],
        ["--json", "economy", "tiers", "--path", f"{R}/economy.json"], tool="economy", parse_json=True)
    add("tiers missing", ["tiers", "--path", f"{R}/nope.json"], ["economy", "tiers", "--path", f"{R}/nope.json"],
        tool="economy")
    for bad in ("economy-str-runs.json", "economy-list.json"):
        add(f"tiers {bad}", ["tiers", "--path", f"{R}/{bad}"], ["economy", "tiers", "--path", f"{R}/{bad}"], tool="economy")
    return cases


def build_layout(layout, root):
    for d in layout["dirs"]:
        os.makedirs(os.path.join(root, d), exist_ok=True)
    for rel, text in layout["files"].items():
        path = os.path.join(root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text.replace(R, root))
    for link, target in layout["symlinks"].items():
        os.symlink(os.path.join(root, target), os.path.join(root, link))


def cli_scenarios(dpy, fpy, maestro, tmp, write):
    root = os.path.join(tmp, "repos")
    layout, cases = cli_layout(), cli_cases()
    build_layout(layout, root)
    sub = lambda xs: [x.replace(R, root) for x in xs]  # noqa: E731
    out = []
    for c in cases:
        script = dpy if c["tool"] == "delegation" else fpy
        cwd = os.path.join(root, c["cwd"])
        compare_cli(f"cli {c['name']}", ["python3", script, *sub(c["py"])], [maestro, *sub(c["rs"])], cwd,
                    parse_json=c["json"])
        code, po, pe = run(["python3", script, *sub(c["py"])], cwd)
        if code == 1 and "Traceback" in pe:  # the golden holds the exception kind, not Python's traceback
            pe = pe.strip().splitlines()[-1].split(":", 1)[0]
        out.append({**c, "want": {"code": code, "stdout": po.replace(root, R), "stderr": pe.replace(root, R),
                                  "crash": code == 1 and pe.isidentifier()}})
    text = json.dumps({"layout": layout, "cases": out}, indent=1, sort_keys=True) + "\n"
    if write:
        with open(CLI_GOLDEN, "w") as f:
            f.write(text)
    else:
        check("cli golden", os.path.isfile(CLI_GOLDEN) and open(CLI_GOLDEN).read() == text,
              f"{CLI_GOLDEN} is not the Python CLIs' output: rerun with --write and review the diff")
    return len(out)


def live_capacity(E, maestro):
    if not os.path.isfile(PROVIDER_MANAGER):
        return check("live usage", False, f"{PROVIDER_MANAGER} is missing")
    code, out, err = run(["python3", PROVIDER_MANAGER, "--json", "usage"])
    if not check("live usage", code == 0, f"provider_manager exit {code}: {err[:200]}"):
        return False
    rows = json.loads(out)
    now = dt.datetime.now(dt.timezone.utc).replace(microsecond=0)
    home = os.environ.get("BROOMVA_HOME") or os.path.expanduser("~/.broomva")
    cache = os.path.join(home, "cache", "coordinator-tick", "usage-samples.jsonl")
    samples = []
    if os.path.isfile(cache):  # read-only: the tick owns this cache
        with open(cache) as f:
            for line in f:
                try:
                    s = json.loads(line)
                except ValueError:
                    continue
                at = E.parse_ts(s.get("at"))
                if at and (now - at).total_seconds() <= E.KEEP_H * 3600:
                    samples.append(s)
    samples.append(E.sample_from_usage(rows, now))
    code, out, err = run(["python3", PROVIDER_MANAGER, "--json", "state"])
    if not check("live balancer state", code == 0, f"provider_manager state exit {code}: {err[:200]}"):
        return False
    state = json.loads(out)
    case = {"fn": "capacity", "args": {"usage": rows, "samples": samples, "now": E.iso(now), "state": state}}
    gate = {"fn": "resume_gate", "args": {"usage": rows, "now": E.iso(now), "state": state}}
    want, want_gate = py_eval(None, E, case, None), py_eval(None, E, gate, None)
    got, got_gate = rust_eval(maestro, [case, gate])
    print(f"live      resume gate {(want_gate.get('ok') or {}).get('resume')}: {(want_gate.get('ok') or {}).get('why')}")
    check("live resume gate", same_result(want_gate, got_gate),
          f"\n  py: {json.dumps(want_gate)[:800]}\n  rs: {json.dumps(got_gate)[:800]}")
    print(f"live      {len(rows)} accounts, {len(samples)} samples, mode "
          f"{(want.get('ok') or {}).get('cadence', {}).get('mode')}")
    return check("live capacity", same_result(want, got), f"\n  py: {json.dumps(want)[:800]}\n  rs: {json.dumps(got)[:800]}")


def plugin_tiers(maestro, D, tmp):
    rev = open(TIERS_REV).read().strip()
    url = os.environ.get("MAESTRO_PASEO_URL", "https://github.com/broomva/maestro-paseo")
    plugin = os.path.join(tmp, "plugin")
    subprocess.run(["git", "init", "-q", plugin], check=True)
    if run(["git", "-C", plugin, "fetch", "-q", "--depth", "1", url, rev])[0]:
        return check("plugin tiers", False, f"cannot fetch {url} at {rev}")
    subprocess.run(["git", "-C", plugin, "checkout", "-q", "FETCH_HEAD"], check=True)
    models = ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5-20261001", "claude-fable-5-1", "Claude-OPUS",
              "gpt-5.6-terra", "", "haiku-and-opus", "codex"]
    providers = ["claude/claude-sonnet-5-5", "claude/", "claude", " claude/opus ", "", "a/b/c"]
    probe = os.path.join(plugin, ".tiers-probe.ts")
    with open(probe, "w") as f:
        f.write('import { DEFAULT_TIER_LADDER, tierOfModel, modelOfProvider } from "./shared/tiers";\n'
                f"const models = {json.dumps(models)}; const providers = {json.dumps(providers)};\n"
                "console.log(JSON.stringify({ladder: DEFAULT_TIER_LADDER, "
                "tiers: models.map(m => tierOfModel(m)), models: providers.map(p => modelOfProvider(p))}));\n")
    code, out, err = run(["bun", probe], cwd=plugin)
    if not check("plugin tiers", code == 0, f"bun: {err[:300]}"):
        return False
    ts = json.loads(out)
    _, lad, _ = run([maestro, "delegation", "ladder"])
    lad = json.loads(lad)
    ok = check("plugin ladder", all(ts["ladder"][t] == {"provider": lad[t]["provider"], "thinkingOptionId": lad[t]["thinking"]}
                                    for t in ts["ladder"]) and set(ts["ladder"]) == set(lad), f"{ts['ladder']} vs {lad}")
    ok &= check("python ladder", all(D.LADDER[t] == lad[t] for t in D.LADDER) and set(D.LADDER) == set(lad),
                f"{D.LADDER} vs {lad}")
    got = rust_eval(maestro, [{"fn": "tier_of_model", "args": {"model": m}} for m in models]
                    + [{"fn": "model_of_provider", "args": {"provider": p}} for p in providers])
    got = [g["ok"] for g in got]
    ok &= check("plugin tierOfModel/modelOfProvider", got == ts["tiers"] + ts["models"], f"{got} vs {ts}")
    return ok


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--workspace", default=os.path.expanduser("~/broomva"))
    p.add_argument("--rev", default="origin/main", help="the workspace revision whose Python is the reference")
    p.add_argument("--maestro", help="a built maestro binary (default: cargo build, then target/debug/maestro)")
    p.add_argument("--write", action="store_true", help="regenerate fixtures/delegation/golden.json")
    p.add_argument("--offline", action="store_true", help="skip the plugin tiers check (needs network)")
    a = p.parse_args(argv)
    ws = os.path.realpath(os.path.expanduser(a.workspace))
    maestro = a.maestro
    if not maestro:
        subprocess.run(["cargo", "build", "-q", "--locked", "-p", "maestro"], cwd=REPO, check=True)
        target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(REPO, "target")
        maestro = os.path.join(target, "debug", "maestro")
    maestro = os.path.abspath(maestro)
    with tempfile.TemporaryDirectory(prefix="parity-delegation.") as tmp:
        tmp = os.path.realpath(tmp)
        D, dpy = load(ws, a.rev, "delegation", tmp)
        E, fpy = load(ws, a.rev, "fleet_economy", tmp)
        cases = all_cases()
        want = [py_eval(D, E, c, tmp) for c in cases]
        golden = [{**c, "want": w} for c, w in zip(cases, want)]
        # One case per line, so a regenerated golden diffs by case.
        text = "[\n" + ",\n".join(json.dumps(g, sort_keys=True, separators=(",", ":")) for g in golden) + "\n]\n"
        if a.write:
            os.makedirs(os.path.dirname(GOLDEN), exist_ok=True)
            with open(GOLDEN, "w") as f:
                f.write(text)
            print(f"golden    wrote {len(golden)} cases from {a.rev}")
        else:
            check("golden", os.path.isfile(GOLDEN) and open(GOLDEN).read() == text,
                  f"{GOLDEN} is not the Python's output at {a.rev}: rerun with --write and review the diff")
        got = rust_eval(maestro, cases)
        bad = [(c, w, g) for c, w, g in zip(cases, want, got) if not same_result(w, g)]
        for c, w, g in bad[:10]:
            failures.append(f"case {c['fn']}: {json.dumps(c['args'])[:300]}\n  py: {json.dumps(w)[:500]}\n  rs: {json.dumps(g)[:500]}")
        if len(bad) > 10:
            failures.append(f"... and {len(bad) - 10} more cases")
        by_fn = {}
        for c in cases:
            by_fn[c["fn"]] = by_fn.get(c["fn"], 0) + 1
        print(f"cases     {len(cases) - len(bad)} of {len(cases)} agree ("
              + ", ".join(f"{k} {v}" for k, v in sorted(by_fn.items())) + ")")
        before = len(failures)
        graphs = real_graphs(ws, dpy, maestro)
        print(f"graphs    {graphs} real plan graphs: plan, plan --json, brief per node "
              + ("agree" if len(failures) == before else "DIFFER"))
        before = len(failures)
        n_cli = cli_scenarios(dpy, fpy, maestro, tmp, a.write)
        codes = {c: cli_runs.count(c) for c in sorted(set(cli_runs))}
        print(f"cli       {n_cli} golden cases; {len(cli_runs)} invocations of both CLIs (by exit code {codes}) "
              + ("agree" if len(failures) == before else "DIFFER"))
        live_capacity(E, maestro)
        # The forecast's timedelta, densely: 100,000 random runways to the microsecond.
        rng = random.Random()
        sweep = [{"fn": "add_hours", "args": {"now": "2026-10-22T12:00:00Z",
                                              "hours": rng.choice([rng.uniform(0, 400), rng.random() * 1e4])}}
                 for _ in range(100_000)]
        got = rust_eval(maestro, sweep)
        off = sum(1 for c, g in zip(sweep, got) if not same(py_eval(D, E, c, tmp), g))
        print(f"timedelta {len(sweep) - off} of {len(sweep)} random runways agree to the microsecond")
        check("timedelta sweep", off == 0, f"{off} runways differ")
        if not a.offline:
            before = len(failures)
            plugin_tiers(maestro, D, tmp)
            print("plugin    tiers.ts at " + open(TIERS_REV).read().strip()[:12] + " "
                  + ("agrees" if len(failures) == before else "DIFFERS"))
    for f in failures:
        print(f"FAIL {f}")
    print("parity-delegation " + ("ok" if not failures else f"FAILED ({len(failures)})"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
