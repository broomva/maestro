#!/usr/bin/env python3
"""Exercise the built Maestro CLI and current MCP wire against an isolated fixture."""

import argparse
import base64
from contextlib import contextmanager
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
from urllib.parse import urlsplit
import uuid


REPO = Path(__file__).resolve().parents[2]
VERSION = "2026-07-28"
TASKS = "io.modelcontextprotocol/tasks"
FILES = "io.modelcontextprotocol/files"
SKILLS = "io.modelcontextprotocol/skills"


@contextmanager
def listener(binary, root, **options):
    workspace = root / "machine/workspace"
    env = dict(os.environ, **{
        "BROOMVA_HOME": str(root / "machine/broomva-home"),
        "MAESTRO_WORK_ROOT": str(root / "machine/work"),
        "MAESTRO_FLEET_STATE": str(root / "machine/fleet-reconcile"),
        "MAESTRO_FLEET_META": str(root / "health"),
        "MAESTRO_DIRECT_REGISTRY": str(root / "direct.json"),
        "MAESTRO_T3_HOME": str(root / "no-t3"),
        "MAESTRO_PASEO_AGENTS_DIR": str(root / "no-paseo"),
        "MAESTRO_OWNER_DIR": str(root / "owner"),
        "MAESTRO_ANSWERS_CHECKOUT": str(root / "answers"),
        "MAESTRO_WEB_ORIGIN": "https://maestro-fixture.invalid",
    })
    flags = [flag for key, value in options.items() if value is not False
             for flag in ([f"--{key.replace('_', '-')}"] if value is True else [f"--{key.replace('_', '-')}", str(value)])]
    proc = subprocess.Popen([binary, "--workspace", workspace, "--tree", "worktree",
                             "serve", "--http", "--bind", "127.0.0.1:0", *flags],
                            stderr=subprocess.PIPE, text=True, env=env)
    try:
        ready = proc.stderr.readline().strip()
        endpoint = urlsplit(ready.rsplit(" ", 1)[-1])
        assert endpoint.hostname == "127.0.0.1", ready
        yield endpoint
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def rpc(endpoint, method, params=None, caps=None, identity=None, error=False):
    params = {**(params or {}), "_meta": {
        "io.modelcontextprotocol/protocolVersion": VERSION,
        "io.modelcontextprotocol/clientCapabilities": caps or {},
        "io.modelcontextprotocol/clientInfo": {"name": "maestro-fixture", "version": "1"},
    }}
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
               "Mcp-Protocol-Version": VERSION, "Mcp-Method": method, **(identity or {})}
    resource = params.get("name") or params.get("uri") or params.get("taskId")
    if resource is not None:
        headers["Mcp-Name"] = resource
    conn = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=15)
    try:
        conn.request("POST", endpoint.path, body, headers)
        response = conn.getresponse()
        decoded = json.loads(response.read())
    finally:
        conn.close()
    field = "error" if error else "result"
    assert field in decoded, (method, response.status, decoded)
    return decoded[field]


def retry_key(nonce):
    return str(uuid.UUID(int=(int(time.time() * 1000) << 80) | 0x70008000000000000000 | nonce))


def verify(binary, root):
    workspace = root / "machine/workspace"
    shutil.copytree(REPO / "fixtures/machine", root / "machine")
    shutil.copytree(REPO / "fixtures/health", root / "health")
    registry = root / "direct.json"
    registry.write_text(json.dumps([{"id": "fixture", "cwd": str(workspace), "liveness": "live",
                                     "token": "synthetic-local-fixture-credential"}]))
    registry.chmod(0o600)
    identity = {"Authorization": "Bearer synthetic-local-fixture-credential",
                "Maestro-Session": "direct:-/fixture", "Maestro-Cwd": str(workspace)}
    caps = {"extensions": {TASKS: {}}, "elicitation": {"url": {}}}
    ledger, = workspace.rglob("demo-arc.yaml")
    before = ledger.read_bytes()
    handle = {"ledger": "demo-arc.yaml", "id": "merge-now"}
    scope = dict(skills_dir=REPO / "skills/mcp", task_db=root / "tasks.sqlite",
                 task_decisions=True, fixture_file_transfer=True)
    with listener(binary, root, **scope) as endpoint:
        discovered = rpc(endpoint, "server/discover")["capabilities"]["extensions"]
        assert {SKILLS, TASKS, FILES} <= discovered.keys()
        entries = rpc(endpoint, "skills/list")["skills"]
        count = 0
        for entry in entries:
            uri = entry["uri"]
            name = entry["frontmatter"]["name"]
            assert rpc(endpoint, "skills/get", {"uri": uri})["skill"] == entry
            for asset in entry["resources"]:
                remote = rpc(endpoint, "resources/read", {"uri": asset["uri"]})["contents"][0]["text"].encode()
                relative = asset["uri"].split(f"/{name}/", 1)[1]
                assert remote == (REPO / "skills/mcp" / name / relative).read_bytes()
                assert len(remote) == asset["size"]
                assert "sha256:" + hashlib.sha256(remote).hexdigest() == asset["digest"]
                count += 1
        assert len(entries) == 5
        print(f"Skills: {len(entries)} entries and {count} verified files")
        context = rpc(endpoint, "tools/call", {"name": "maestro_context", "arguments": {"project": "demo"}})
        assert not context.get("isError")

        report = {"name": "maestro_health_report", "arguments": {"section": "loops", "retry_key": retry_key(1)}}
        plain = rpc(endpoint, "tools/call", report, identity=identity)["structuredContent"]
        assert plain["loops"]["data"]["loops"]["census"]["status"] == "ok"
        task = rpc(endpoint, "tools/call", report, caps, identity)
        assert task["resultType"] == "task"

        for _ in range(20):
            initial = rpc(endpoint, "tasks/get", {"taskId": task["taskId"]}, caps, identity)
            if initial["status"] == "completed":
                break
            time.sleep(0.1)
        assert initial["status"] == "completed"

        follow = {"name": "maestro_wait_for_decision", "arguments": {**handle, "interactive": True, "retry_key": retry_key(2)}}
        fallback = rpc(endpoint, "tools/call", follow, {"extensions": {TASKS: {}}, "elicitation": {}}, identity)
        assert fallback["structuredContent"]["nextTool"] == "maestro_ask_get"
        observer = rpc(endpoint, "tools/call", follow, caps, identity)
        assert observer["resultType"] == "task"
        observed = {"taskId": observer["taskId"]}
        missing = rpc(endpoint, "tasks/get", observed, {"extensions": {TASKS: {}}}, identity, error=True)
        assert missing["code"] == -32021 and "url" in missing["data"]["requiredCapabilities"]["elicitation"]
        pending = rpc(endpoint, "tasks/get", observed, caps, identity)
        prompt = pending["inputRequests"]["open-decisions"]["params"]
        assert pending["status"] == "input_required" and prompt["mode"] == "url"
        assert prompt["url"] == "https://maestro-fixture.invalid/decisions/demo-arc.yaml/merge-now"
        update = {**observed, "inputResponses": {"open-decisions": {"action": "accept", "content": {"answer": "merge"}}}}
        assert rpc(endpoint, "tasks/update", update, caps, identity, error=True)["code"] == -32602
        update["inputResponses"]["open-decisions"].pop("content")
        rpc(endpoint, "tasks/update", update, caps, identity)
        assert rpc(endpoint, "tasks/get", observed, caps, identity)["status"] == "working"
        assert ledger.read_bytes() == before
        print("Elicitation: URL consent stayed working, injected answer refused, ledger byte-identical")

        download = {"name": "maestro_download_health_fixture", "arguments": {}}
        assert rpc(endpoint, "tools/call", download).get("isError")
        result = rpc(endpoint, "tools/call", download, {"extensions": {FILES: {"download": {}}}})
        target = result["_meta"][FILES]["downloads"][0]
        assert "structuredContent" not in result and target["credentials"] == "omit"
        conn = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=5)
        try:
            conn.request("GET", target["path"])
            response = conn.getresponse()
            content = response.read()
            assert response.status == 200 and len(content) == target["size"]
        finally:
            conn.close()
        for headers in ({"Host": "untrusted.invalid"}, {"Origin": "https://untrusted.invalid"}):
            conn = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=5)
            try:
                conn.request("GET", target["path"], headers=headers)
                response = conn.getresponse()
                assert response.status == 403, (headers, response.status)
            finally:
                conn.close()
        assert content == (REPO / "fixtures/health/loop-health.json").read_bytes()
        assert base64.urlsafe_b64encode(hashlib.sha256(content).digest()).rstrip(b"=").decode() == target["digest"]["value"]
        print(f"Files: {len(content)} redacted fixture bytes verified by SHA-256")
        health = root / "health/loop-health.json"
        original = health.read_bytes()
        health.write_text(json.dumps({"loops": {"census": {"status": "warn", "why": "x" * 150_000}}}))
        oversized = {**report, "arguments": {**report["arguments"], "retry_key": retry_key(4)}}
        assert rpc(endpoint, "tools/call", oversized, identity=identity, error=True)["code"] == -32603
        oversized_task = rpc(endpoint, "tools/call", oversized, caps, identity)
        for _ in range(20):
            failed = rpc(endpoint, "tasks/get", {"taskId": oversized_task["taskId"]}, caps, identity)
            if failed["status"] == "failed":
                break
            time.sleep(0.1)
        assert failed["status"] == "failed" and failed["error"]["code"] == -32603
        health.write_bytes(original)
        verified = registry.read_text()
        registry.write_text("[]")
        revoked = rpc(endpoint, "tasks/get", {"taskId": task["taskId"]}, caps, identity, error=True)
        assert revoked["code"] == -32602
        registry.write_text(verified)

    with listener(binary, root, **scope, drain_tasks=True) as endpoint:
        resumed = rpc(endpoint, "tools/call", report, caps, identity)
        assert resumed["taskId"] == task["taskId"]
        changed = {**report, "arguments": {**report["arguments"], "section": "capacity"}}
        assert rpc(endpoint, "tools/call", changed, caps, identity, error=True)["code"] == -32602
        fresh = {**report, "arguments": {**report["arguments"], "retry_key": retry_key(3)}}
        assert rpc(endpoint, "tools/call", fresh, caps, identity)["resultType"] == "complete"
        for _ in range(20):
            current = rpc(endpoint, "tasks/get", {"taskId": task["taskId"]}, caps, identity)
            if current["status"] == "completed":
                break
            time.sleep(0.1)
        assert current["status"] == "completed" and current["result"]["structuredContent"] == plain
        rpc(endpoint, "tasks/cancel", observed, caps, identity)
        cancelled = rpc(endpoint, "tasks/get", observed, caps, identity)
        assert cancelled["status"] == "cancelled" and "result" not in cancelled
        assert ledger.read_bytes() == before
        print("Tasks: caller revocation took effect, report reconnect survived restart and drain")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug/maestro")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="maestro-mcp-fixture-") as tmp:
        verify(args.binary.resolve(), Path(tmp).resolve())


if __name__ == "__main__":
    main()
