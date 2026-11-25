#!/usr/bin/env python3
"""Read-only, bounded Maestro subscriber pilot for T3's thread.needs_input event."""

import argparse
from datetime import datetime, timezone
import http.client
import json
from pathlib import Path
import sqlite3
import statistics
import tempfile
from urllib.parse import urlsplit


EXTENSION = "io.modelcontextprotocol/events"
NAME = "thread.needs_input"
VERSION = "2026-07-28"


def client(endpoint, token):
    url = urlsplit(endpoint)
    if (url.scheme, url.hostname, url.path) not in {
        ("http", host, "/mcp") for host in ("127.0.0.1", "::1", "localhost")
    } or any((url.username, url.password, url.query, url.fragment)):
        raise ValueError("pilot requires the T3 loopback MCP endpoint")

    def request(method, **params):
        data = json.dumps({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": {
                **params,
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": VERSION,
                    "io.modelcontextprotocol/clientInfo": {"name": "maestro-events-pilot", "version": "1"},
                    "io.modelcontextprotocol/clientCapabilities": {"extensions": {EXTENSION: {}}},
                },
            },
        })
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Authorization": f"Bearer {token}",
            "Mcp-Protocol-Version": VERSION,
            "Mcp-Method": method,
        }
        if "name" in params:
            headers["Mcp-Name"] = params["name"]
        conn = http.client.HTTPConnection(url.hostname, url.port, timeout=5)
        try:
            conn.request("POST", url.path, data, headers)
            response = conn.getresponse()
            if response.status != 200:
                raise ValueError(f"{method} returned HTTP {response.status}")
            body = response.read(2_000_001)
        finally:
            conn.close()
        if len(body) > 2_000_000:
            raise ValueError(f"{method} response exceeded the pilot limit")
        # A JSON or single-reply SSE response is sufficient for the poll transport.
        if body.startswith(b"event:") or body.startswith(b"data:"):
            body = next((line[6:] for line in body.splitlines() if line.startswith(b"data: ")), b"")
        decoded = json.loads(body)
        if "result" not in decoded:
            raise ValueError(f"{method} returned RPC error {decoded.get('error', {}).get('code')}")
        return decoded["result"]

    return request


def read_thread(request, thread):
    result = request("tools/call", name="t3_thread_read", arguments={"threadId": thread, "limit": 1, "runLimit": 1})
    if result.get("isError"):
        raise ValueError("T3 thread read failed")
    view = result.get("structuredContent")
    if view is None:
        view = json.loads(next(b["text"] for b in result["content"] if b["type"] == "text"))
    return view["thread"]["pendingRequestCount"]


def open_receipts(path):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.touch(mode=0o600, exist_ok=True)
    db = sqlite3.connect(path)
    db.executescript("""
        CREATE TABLE IF NOT EXISTS subscription (
            endpoint TEXT, thread TEXT, cursor TEXT, PRIMARY KEY (endpoint, thread)
        );
        CREATE TABLE IF NOT EXISTS observation (
            endpoint TEXT, thread TEXT, event TEXT, pending INTEGER, delay_ms REAL,
            PRIMARY KEY (endpoint, thread, event)
        );
    """)
    return db


def poll_once(db, request, endpoint, thread, clock):
    scope = (endpoint, thread)
    reads = 0
    with db:
        db.execute("BEGIN IMMEDIATE")
        cursor = db.execute("SELECT cursor FROM subscription WHERE endpoint=? AND thread=?", scope).fetchone()
        batch = request("events/poll", name=NAME,
                        arguments={"threadIds": [thread], "includeSubagentChildren": False},
                        cursor=cursor[0] if cursor else None, limit=40)
        reconcile = cursor is None or batch["truncated"]
        if reconcile:
            read_thread(request, thread)
            reads += 1
        for event in batch["events"]:
            if event["name"] != NAME or event["data"]["threadId"] != thread:
                raise ValueError("received an occurrence outside the subscribed scope")
            key = (*scope, event["eventId"])
            if db.execute("SELECT 1 FROM observation WHERE endpoint=? AND thread=? AND event=?", key).fetchone():
                continue
            timestamp = datetime.fromisoformat(event["timestamp"].replace("Z", "+00:00"))
            pending = read_thread(request, thread)
            reads += 1
            delay = (clock() - timestamp).total_seconds() * 1000
            db.execute("INSERT INTO observation VALUES (?,?,?,?,?)", (*key, pending, delay))
        db.execute("INSERT OR REPLACE INTO subscription VALUES (?,?,?)", (*scope, batch["cursor"]))
    return {"threadReads": reads, "reconciled": reconcile, "hasMore": batch["hasMore"]}


def fixture():
    event_reads = []
    scan_delays = []
    current = {"threadId": "fixture", "pendingRequestCount": 1}
    batch = {"cursor": "epoch.0", "truncated": False, "hasMore": False, "events": []}

    def request(method, **params):
        if method == "tools/call":
            return {"structuredContent": {"thread": current}}
        assert params["arguments"] == {"threadIds": ["fixture"], "includeSubagentChildren": False}
        return batch

    with tempfile.TemporaryDirectory() as root:
        path = Path(root) / "receipts.sqlite"
        db = open_receipts(path)
        run = lambda now: poll_once(db, request, "fixture", "fixture", lambda: datetime.fromtimestamp(now, timezone.utc))
        assert run(0)["threadReads"] == 1
        for n in range(20):
            occurred = n * 10 + 3
            batch.update(cursor=f"epoch.{n + 1}", events=[{
                "name": NAME, "eventId": f"input-{n}", "data": {"threadId": "fixture"},
                "timestamp": datetime.fromtimestamp(occurred, timezone.utc).isoformat(),
            }])
            event_reads.append(run(occurred + 1)["threadReads"])
            scan_at = ((occurred // 10) + 1) * 10
            assert read_thread(request, "fixture") == 1
            scan_delays.append((scan_at - occurred) * 1000)
        db.close()
        db = open_receipts(path)
        assert run(200)["threadReads"] == 0
        batch.update(cursor="new.0", truncated=True, events=[])
        assert run(205) == {"threadReads": 1, "reconciled": True, "hasMore": False}
        batch.update(cursor="new.1", truncated=False, events=[{
            "name": NAME, "eventId": "outside", "data": {"threadId": "another-thread"},
        }])
        try:
            run(207)
        except ValueError as error:
            assert "outside the subscribed scope" in str(error)
        else:
            raise AssertionError("out-of-scope event was accepted")
        assert db.execute("SELECT cursor FROM subscription").fetchone() == ("new.0",)
        observed = db.execute("SELECT delay_ms FROM observation").fetchall()
        assert len(observed) == 20
        median = statistics.median(row[0] for row in observed)
        assert median == 1000 and sum(event_reads) == 20
        print(json.dumps({"fixtureTransitions": len(observed), "eventMedianMs": median,
                          "fixtureScanMedianMs": statistics.median(scan_delays),
                          "eventReadsPerTransition": statistics.mean(event_reads),
                          "fixtureScanReadsPerTransition": 1, "replayReads": 0, "restartReconciled": True}))
        db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", action="store_true")
    parser.add_argument("--mcp-url")
    parser.add_argument("--token-file", type=Path)
    parser.add_argument("--thread")
    parser.add_argument("--receipts", type=Path)
    args = parser.parse_args()
    if args.fixture:
        fixture()
        return
    if not all((args.mcp_url, args.token_file, args.thread, args.receipts)):
        parser.error("live mode requires --mcp-url, --token-file, --thread and --receipts")
    try:
        request = client(args.mcp_url, args.token_file.read_text().strip())
        info = request("server/discover")
        if EXTENSION not in info["capabilities"].get("extensions", {}):
            raise ValueError("the credential and server do not advertise Events")
        if not any(event["name"] == NAME and "poll" in event["delivery"] for event in request("events/list")["events"]):
            raise ValueError("T3 does not expose the input-required event to this client")
        db = open_receipts(args.receipts)
        try:
            print(json.dumps(poll_once(db, request, args.mcp_url, args.thread, lambda: datetime.now(timezone.utc))))
        finally:
            db.close()
    except (OSError, ValueError, KeyError, StopIteration) as error:
        parser.exit(1, f"Events pilot: {error}\n")


if __name__ == "__main__":
    main()
