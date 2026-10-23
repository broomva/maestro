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
        if fn == "capacity":
            return {"ok": E.capacity(a["usage"], a["samples"], E.parse_ts(a["now"]))}
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
    except Exception as e:  # what the Python does not catch: a traceback, compared by kind only
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
    if "crash" in want or "crash" in got:
        return "crash" in want and "crash" in got
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


def small_cases():
    rules = ["scripts/*-hook.sh", "a?c", "*", "**", "x*y*z", "Docs/", "[x]", "a.b", "*.md", "?"]
    paths = ["scripts/a-hook.sh", "scripts/x/a-hook.sh", "abc", "a/c", "docs/x", "xay/z", "xyz", "[x]", "a.b",
             "axb", "readme.md", "dir/readme.md", "", "a"]
    cases = [{"fn": "rule_matches", "args": {"rule": r, "path": p}} for r in rules for p in paths]
    gov = ["CLAUDE.md", "./agents.md", "x/claude.md", ".control/policy.yaml", "a/.broomva/loops/x", "METALAYER.md",
           "references/delegation.md", "scripts/fleet_economy.py", "deploy/x", "scripts/a-hook.sh", "src/a.rs"]
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


def all_cases():
    rng = random.Random(3204)
    return assess_cases(rng) + graph_cases() + small_cases() + capacity_cases(rng)


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
    if pc == 1 and "Traceback" in pe:  # an uncaught Python error: the Rust must also exit 1 with a traceback
        return check(name, rc == 1 and "Traceback" in re_, f"python crashed; rust exit {rc}: {re_[:200]}")
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


def cli_scenarios(dpy, maestro, ws, tmp):
    root = os.path.join(tmp, "repos")
    w, m, bare = os.path.join(root, "w"), os.path.join(root, "w", "nested"), os.path.join(root, "bare")
    for d in (os.path.join(w, ".control"), os.path.join(w, "docs", "specs"), m, os.path.join(bare, "docs", "adrs")):
        os.makedirs(d, exist_ok=True)
    with open(os.path.join(w, ".control", "policy.yaml"), "w") as f:
        f.write("auto_merge:\n  rules:\n    - {action: require_human, path_touched: deploy/}\n")
    for p in (os.path.join(w, "docs", "specs", "a.html"), os.path.join(bare, "docs", "adrs", "b.html")):
        open(p, "w").close()
    os.symlink(w, os.path.join(root, "w-link"))
    good = node("a", inputs=["docs/specs/a.html", "bare:docs/adrs/b.html", "docs/specs/missing.html"],
                signals={"paths": ["src/a.rs"]})
    graphs = {
        "ok": {"repo": "w", "repos": {"w": w, "bare": {"path": bare, "policy": "none"}}, "nodes": [
            good, node("b", repo="bare", depends_on=["a"], inputs=["w:docs/specs/a.html"])]},
        "nested": {"repo": "w", "repos": {"w": w, "n": m}, "nodes": [node("a", signals={"paths": ["nested/x.rs"]})]},
        "same-dir": {"repo": "w", "repos": {"w": w, "v": os.path.join(root, "w-link")}, "nodes": [node("a")]},
        "absent": {"repo": "w", "repos": {"w": w, "gone": os.path.join(root, "gone")},
                   "nodes": [node("a"), node("g", repo="gone", inputs=["docs/specs/x.html"])]},
        "no-policy": {"repo": "b", "repos": {"b": bare}, "nodes": [node("a")]},
        "policy-none-but-file": {"repo": "w", "repos": {"w": {"path": w, "policy": "none"}}, "nodes": [node("a")]},
        "governed": {"repo": "w", "repos": {"w": w}, "nodes": [node("a", signals={"paths": ["deploy/x.yaml"]})]},
        "bad-name": {"repo": "w", "repos": {"w": w, "a:b": bare}, "nodes": [node("a")]},
        "no-path": {"repo": "w", "repos": {"w": w, "x": {"policy": "none"}}, "nodes": [node("a")]},
        "single": {"nodes": [node("a", inputs=["docs/specs/a.html"])]},
    }
    for name, g in graphs.items():
        path = os.path.join(tmp, f"{name}.json")
        with open(path, "w") as f:
            json.dump(g, f)
        variants = [([], []), (["--skip-absent-repos"], ["--skip-absent-repos"])]
        if name == "single":
            variants += [(["--root", w], ["--root", w]), (["--root", bare], ["--root", bare])]
        if name == "ok":
            variants += [(["--repo", f"bare={bare}"], ["--repo", f"bare={bare}"]), (["--repo", "nope=x"], ["--repo", "nope=x"]),
                         (["--repo", "bad"], ["--repo", "bad"]), (["--root", w], ["--root", w])]
        for pre, post in variants:
            for sub, extra in (("plan", []), ("plan", ["--json"]), ("brief", ["a"])):
                py_cmd = ["python3", dpy, *pre, sub, path, *extra]
                rs_cmd = [maestro, *(["--json"] if extra == ["--json"] else []), "delegation", sub, path,
                          *([x for x in extra if x != "--json"]), *post]
                compare_cli(f"cli {name} {sub} {' '.join(pre + extra)}", py_cmd, rs_cmd, w, parse_json=extra == ["--json"])
    assess = [["--kind", "implement", "--paths", "src/a.rs", "deploy/x"],
              ["--kind", "implement", "--paths", "a.rs", "b.rs", "--est-loc", "300"],
              ["--kind", "implement", "--paths", "a.rs", "--est-loc", "300", "--spec", "docs/specs/a.html"],
              ["--kind", "implement", "--paths", "a.rs", "--spec", "docs/specs/none.html"],
              ["--kind", "rerun", "--done", "gh run rerun 1 --failed", "--brief", "--ticket", "BRO-1"],
              ["--kind", "rerun", "--brief"],
              ["--kind", "implement", "--paths", "a.rs", "b.rs", "--est-loc", "300", "--done", "x", "--brief"],
              ["--kind", "review", "--mode", "read-only", "--observed-tier", "apex"],
              ["--kind", "implement", "--paths", "a.rs", "--failed-attempts", "3", "--allow-apex", "--mode", "conserve"]]
    for args in assess:
        for root_args in (["--root", w], ["--root", bare], ["--root", bare, "--no-policy"]):
            compare_cli(f"assess {' '.join(root_args + args)}", ["python3", dpy, *root_args, "assess", *args],
                        [maestro, "delegation", "assess", *root_args, *args], w,
                        parse_json="--brief" not in args)
    compare_cli("ladder", ["python3", dpy, "ladder"], [maestro, "delegation", "ladder"], w, parse_json=True)


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
    case = {"fn": "capacity", "args": {"usage": rows, "samples": samples, "now": E.iso(now)}}
    want = py_eval(None, E, case, None)
    got = rust_eval(maestro, [case])[0]
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
        E, _ = load(ws, a.rev, "fleet_economy", tmp)
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
        cli_scenarios(dpy, maestro, ws, tmp)
        codes = {c: cli_runs.count(c) for c in sorted(set(cli_runs))}
        print(f"cli       {len(cli_runs)} invocations of both CLIs (by exit code {codes}) "
              + ("agree" if len(failures) == before else "DIFFER"))
        live_capacity(E, maestro)
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
