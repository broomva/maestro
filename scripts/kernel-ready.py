#!/usr/bin/env python3
"""kernel-ready@1's machine checks (BRO-3842): one subcommand per observe node of
crates/flow/flows/kernel-ready.yaml.

Each check reads the installation the owner's harnesses actually launch: the binary at
`$HOME/.local/bin/maestro`, Claude Code's and Codex's Stop-hook settings, and Codex's own hook
trust (`$HOME/.local/bin/codex app-server`, `hooks/list`). It prints one JSON object saying what it
measured and exits 0 only when the subject is sound. Any other outcome, a check that cannot read
its subject included, exits 1, so the node's `exit_map` ({0: pass}) refuses it: unread is not
sound. The kernel runs it with only PATH (the system folders), HOME, LANG, LC_ALL and TMPDIR, so
it names every program by absolute path or finds it on that PATH.

Runs on the system's python3 (3.9 on macOS), so it uses nothing newer. `~/.local/bin/codex` must
run with the kernel's PATH (the system folders, /usr/local/bin and /opt/homebrew/bin): Codex's
native standalone install does; a Node shim does only when `node` is in one of those folders (not,
say, under ~/.nvm), and is otherwise refused rather than passed unread.
"""
from __future__ import annotations

import json
import os
import re
import shlex
import subprocess
import sys
import threading
from pathlib import Path

# The `maestro flow` subcommands that have no MCP tool, as `maestro flow --help` says of each:
# `help` is clap's, `driver-skill` is "the one flow command that is no MCP tool's twin", and
# `stop-gate` "needs no MCP twin: only a harness runs it".
NO_MCP_TWIN = {"help", "driver-skill", "stop-gate"}
HARNESSES = ("claude", "codex")
TIMEOUT = 120


class Unsound(Exception):
    """The subject is broken, or could not be read."""


def home() -> Path:
    h = os.environ.get("HOME")
    if not h:
        raise Unsound("HOME is not set")
    return Path(h)


def binary(name: str) -> Path:
    path = home() / ".local" / "bin" / name
    if not (path.is_file() and os.access(path, os.X_OK)):
        raise Unsound(f"{path} is not an executable file")
    return path


def run(argv: list, stdin: str | None = None, timeout: int = TIMEOUT) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            [str(a) for a in argv], input=stdin, capture_output=True, text=True, timeout=timeout
        )
    except (OSError, subprocess.SubprocessError) as e:
        raise Unsound(f"{argv[0]} did not run: {e}") from e


def build_commit(maestro: Path) -> str:
    out = run([maestro, "--version"])
    if out.returncode != 0:
        raise Unsound(f"{maestro} --version exited {out.returncode}")
    m = re.fullmatch(r"maestro \S+ \((\S+)\)", out.stdout.strip())
    if not m:
        raise Unsound(f"{maestro} --version names no build commit: {out.stdout.strip()!r}")
    return m.group(1)


def installed(head: str) -> dict:
    """The installed binary was built from `head`, clean, and its flow verbs run."""
    maestro = binary("maestro")
    commit = build_commit(maestro)
    if commit != head:
        raise Unsound(f"{maestro} was built from {commit}, not {head}")
    out = run([maestro, "flow", "--help"])
    if out.returncode != 0:
        raise Unsound(f"{maestro} flow --help exited {out.returncode}")
    return {"binary": str(maestro), "commit": commit}


def flow_verbs(maestro: Path) -> list:
    """The `maestro flow` subcommands, read from its own help."""
    out = run([maestro, "flow", "--help"])
    if out.returncode != 0:
        raise Unsound(f"{maestro} flow --help exited {out.returncode}")
    verbs, listing = [], False
    for line in out.stdout.splitlines():
        if line.strip() == "Commands:":
            listing = True
            continue
        if listing:
            m = re.match(r"  ([a-z][a-z-]*)(\s|$)", line)
            if m:
                verbs.append(m.group(1))
            elif line.strip() and not line.startswith("    "):
                break
    if not verbs:
        raise Unsound(f"{maestro} flow --help lists no subcommands")
    return verbs


def rpc(argv: list, calls: list, timeout: int = 60) -> dict:
    """Send JSON-RPC lines to a stdio server, then read its replies by id."""
    lines = [json.dumps({"jsonrpc": "2.0", **c}) for c in calls]
    try:
        proc = subprocess.Popen(
            [str(a) for a in argv], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True,
        )
    except OSError as e:
        raise Unsound(f"{argv[0]} did not start: {e}") from e
    wanted = {c["id"] for c in calls if "id" in c}
    replies: dict = {}
    try:
        assert proc.stdin and proc.stdout
        proc.stdin.write("\n".join(lines) + "\n")
        proc.stdin.flush()

        def read() -> None:
            for raw in proc.stdout:  # type: ignore[union-attr]
                try:
                    msg = json.loads(raw)
                except ValueError:
                    continue
                if isinstance(msg, dict) and msg.get("id") in wanted:
                    replies[msg["id"]] = msg
                    if wanted <= replies.keys():
                        return

        reader = threading.Thread(target=read, daemon=True)
        reader.start()
        reader.join(timeout)
    finally:
        proc.kill()
        proc.wait()
    missing = wanted - replies.keys()
    if missing:
        raise Unsound(f"{argv[0]} sent no reply to request(s) {sorted(missing)} within {timeout}s")
    return replies


INIT = {"id": 1, "method": "initialize", "params": {
    "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "kernel-ready", "version": "1"}}}
INITIALIZED = {"method": "notifications/initialized"}


def mcp_tools() -> dict:
    """Every `maestro flow` verb with an MCP twin is a tool of the installed MCP server."""
    maestro = binary("maestro")
    expected = sorted("maestro_flow_" + v.replace("-", "_") for v in flow_verbs(maestro) if v not in NO_MCP_TWIN)
    reply = rpc([maestro, "mcp"], [INIT, INITIALIZED, {"id": 2, "method": "tools/list", "params": {}}])[2]
    tools = (reply.get("result") or {}).get("tools")
    if not isinstance(tools, list):
        raise Unsound(f"tools/list returned no tool list: {json.dumps(reply)[:300]}")
    names = {t.get("name") for t in tools if isinstance(t, dict)}
    missing = [n for n in expected if n not in names]
    if missing:
        raise Unsound(f"the installed MCP server lacks {missing}")
    return {"expected": expected, "tools": len(names)}


def gate_command(command: str, program: Path, harness: str) -> bool:
    """Whether a hook command is exactly `<program> flow stop-gate --harness <harness>`.

    --check-install accepts any program named maestro and further arguments, some of which stop
    the gate gating (`--help`, `--cannot-block`). Only the one form the gate is installed as runs
    the gate as installed, so nothing else passes: an allowlist of one, not a list of bad flags.
    """
    try:
        words = shlex.split(command)
    except ValueError:
        return False
    return (
        len(words) > 0
        and os.path.isabs(words[0])
        and os.path.realpath(words[0]) == os.path.realpath(program)
        and words[1:] == ["flow", "stop-gate", "--harness", harness]
    )


def stop_gate() -> dict:
    """The Stop gate is wired for Claude Code and Codex, and Codex trusts its hook."""
    maestro = binary("maestro")
    commands = {}
    for harness in HARNESSES:
        out = run([maestro, "flow", "stop-gate", "--harness", harness, "--check-install", "--json"])
        try:
            report = json.loads(out.stdout)
        except ValueError:
            raise Unsound(f"stop-gate --check-install for {harness} printed no JSON (exit {out.returncode})")
        if out.returncode != 0 or report.get("wired") is not True:
            raise Unsound(f"the Stop gate is not wired for {harness}: {report.get('detail')!r}")
        commands[harness] = [c for c in report.get("commands") or [] if isinstance(c, str)]
        if not commands[harness]:
            raise Unsound(f"stop-gate --check-install for {harness} names no hook command")
        foreign = [c for c in commands[harness] if not gate_command(c, maestro, harness)]
        if foreign:
            raise Unsound(f"the {harness} Stop hook is not exactly {maestro} flow stop-gate --harness {harness}: {foreign}")
    # Codex runs a hook only once it trusts it, which --check-install does not read.
    codex = binary("codex")
    reply = rpc([codex, "app-server"], [
        {"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "kernel-ready", "version": "1"}}},
        {"method": "initialized"},
        {"id": 2, "method": "hooks/list", "params": {"cwds": [str(home())]}},
    ])[2]
    entries = (reply.get("result") or {}).get("data")
    if not isinstance(entries, list):
        raise Unsound(f"hooks/list returned no data: {json.dumps(reply)[:300]}")
    gates = [h for e in entries if isinstance(e, dict) for h in e.get("hooks") or []
             if isinstance(h, dict) and h.get("eventName") == "stop" and h.get("command") in commands["codex"]]
    if not gates:
        raise Unsound("Codex lists no Stop hook running the gate --check-install names")
    untrusted = [h.get("key") for h in gates if h.get("enabled") is not True or h.get("trustStatus") != "trusted"]
    if untrusted:
        raise Unsound(f"Codex does not run the gate's Stop hook (disabled or untrusted): {untrusted}")
    return {"commands": commands, "codex_trusted": [h.get("key") for h in gates]}


# The folders `maestro agents render` writes (`maestro_flow::agents::RENDER_DIRS`; Codex takes its
# brief inline, so it has none). crates/cli/tests/kernel_ready.rs fails if the two differ.
RENDER_DIRS = (".claude/agents", ".gemini/agents", ".grok/agents")


def skill_name(maestro: Path, flow: Path) -> str:
    """The driver skill's name, as the installed binary derives it from the definition (its flow
    name and version), never from the file name."""
    out = run([maestro, "flow", "driver-skill", "--definition", flow, "--json"])
    try:
        text = json.loads(out.stdout)["skill"]
    except (ValueError, KeyError, TypeError):
        raise Unsound(f"{flow} does not compile (exit {out.returncode}): {(out.stderr or '').strip()[-200:]}")
    # Only the frontmatter names the skill: the body quotes node text (a human node's ask), which
    # may hold a line of its own that reads `name: ...` (P20 round 5).
    lines = text.splitlines()
    end = lines.index("---", 1) if lines[:1] == ["---"] and "---" in lines[1:] else 0
    try:
        names = [json.loads(line[len("name: "):]) for line in lines[1:end] if line.startswith("name: ")]
    except ValueError:
        names = []
    if out.returncode != 0 or len(names) != 1 or not isinstance(names[0], str) or not re.fullmatch(r"flow-[a-z0-9-]+", names[0]):
        raise Unsound(f"{flow}: the generated skill names no single skill (exit {out.returncode})")
    return names[0]


def consumers(roots: list) -> dict:
    """Each consumer's flows compile under the installed binary, and their generated driver skills
    and agent renders are current.

    The check runs this itself, not through the consumer's Makefile: a Makefile can make a target
    that checks nothing pass (an empty recipe, no rule, a file named like the target), so trusting
    it checked only that make exited 0 (P20 rounds 1 and 3). The conventions are the installed
    binary's, not this script's (P20 round 4): each `flows/*.yaml` must have its skill at
    `skills/<name>/SKILL.md`, where the binary names it from the definition. A consumer with no
    flow is refused: there is nothing to check. Its agents are checked with `agents render --check`
    whenever `agents/` or any render folder exists, so an `agents/` that is gone, or not a folder,
    while renders remain is refused rather than read as having no agents.
    """
    maestro = binary("maestro")
    if not roots:
        raise Unsound("no consumer named")
    checked = {}
    for root in roots:
        flows = sorted(Path(root, "flows").glob("*.yaml"))
        if not flows:
            raise Unsound(f"{root} has no flows/*.yaml to check")
        skills = []
        for flow in flows:
            name = skill_name(maestro, flow.resolve())
            skill = Path(root, "skills", name, "SKILL.md")
            out = run([maestro, "flow", "driver-skill", "--definition", flow.resolve(), "--out", skill, "--check"])
            if out.returncode != 0:
                tail = (out.stderr or out.stdout).strip().splitlines()[-2:]
                raise Unsound(f"{flow}'s skill {skill} is stale or missing (exit {out.returncode}): {tail}")
            skills.append(name)
        agents = os.path.lexists(Path(root, "agents")) or any(os.path.lexists(Path(root, d)) for d in RENDER_DIRS)
        if agents:
            out = run([maestro, "agents", "render", "--root", root, "--check"])
            if out.returncode != 0:
                tail = (out.stderr or out.stdout).strip().splitlines()[-2:]
                raise Unsound(f"the agent renders in {root} drift from agents/ (exit {out.returncode}): {tail}")
        checked[root] = {"skills": skills, "agents": agents}
    return {"consumers": checked}


def main(argv: list) -> int:
    checks = {
        "installed": lambda a: installed(a[0]) if len(a) == 1 else None,
        "mcp-tools": lambda a: mcp_tools() if not a else None,
        "stop-gate": lambda a: stop_gate() if not a else None,
        "consumers": lambda a: consumers(a),
    }
    if not argv or argv[0] not in checks:
        print(json.dumps({"ok": False, "error": f"usage: kernel-ready.py {{{','.join(checks)}}} [args]"}))
        return 1
    try:
        measured = checks[argv[0]](argv[1:])
        if measured is None:
            raise Unsound(f"wrong arguments for {argv[0]}: {argv[1:]}")
    except Unsound as e:
        print(json.dumps({"check": argv[0], "ok": False, "error": str(e)}))
        # The kernel records a refused command's standard error with the refusal.
        print(f"kernel-ready {argv[0]}: {e}", file=sys.stderr)
        return 1
    print(json.dumps({"check": argv[0], "ok": True, "measured": measured}))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
