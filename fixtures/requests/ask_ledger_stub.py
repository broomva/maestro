#!/usr/bin/env python3
"""A stand-in for the workspace's scripts/ask_ledger.py, for tests and hosted CI (the workspace
repository is private). Maestro calls the validator of record's own `load`, `validate(ledger,
preauth, path=...)` and `asks_of`; this module has the same three, reading YAML with PyYAML as
the real one does. An ask whose any value holds STUB-INVALID is an error, and so is any ask in a
ledger named `ideas-*.yaml` whose class is not `idea` (a rule judged on the ledger's real name).
STUB_SLEEP_S holds each validation (and so the ledger lock) that long. The live check runs the
real validator.

`file` stands in for `ask_ledger.py file` (BRO-3406), the writer Maestro files through: the same
arguments, refusals, §5.5 lock (the plugin's key, pid grammar, token and stale reclaim) and exit
codes (0 filed, 2 refused, 75 lock busy). Its emitter is a simple one: the real writer's
line-shape checks are the real writer's to prove, in the workspace suite."""
import hashlib
import json
import os
import re
import secrets
import subprocess
import sys
import time
from pathlib import Path

import yaml


def load(path: Path) -> dict:
    return yaml.safe_load(Path(path).read_text(encoding="utf-8")) or {}


def asks_of(ledger: dict):
    raw = ledger.get("asks")
    if isinstance(raw, list):
        return raw
    if isinstance(ledger.get("open_asks"), list):
        return ledger["open_asks"]
    return [] if raw is None else None


def validate(ledger: dict, preauth=None, require_decisions=False, require_decision=False, path=None) -> list:
    time.sleep(float(os.environ.get("STUB_SLEEP_S") or 0))
    errs = []
    ideas = path is not None and re.match(r"^ideas-[a-z0-9-]+\.yaml$", Path(path).name)
    for ask in asks_of(ledger) or []:
        if "STUB-INVALID" in repr(ask):
            errs.append(f"ask {ask.get('id')}: marked STUB-INVALID")
        if ideas and ask.get("class") != "idea":
            errs.append(f"ask {ask.get('id')}: {Path(path).name} holds ideas only")
    return errs


ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*\Z")
REFUSED = ("answer", "answered", "resolution", "outcome", "answered_at", "defaulted_at", "answered_by",
           "status", "archived_at", "archived_reason", "origin")
LIMIT = 1 << 20


class Refused(Exception):
    pass


class Busy(Exception):
    pass


def _lock_dir(ledger: Path) -> Path:
    root = Path(os.environ.get("MAESTRO_PASEO_ASK_LOCKS") or Path.home() / ".local/state/control-asks/locks")
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    key = hashlib.sha256(f"{os.path.realpath(ledger.parent)}/{ledger.name}".encode()).hexdigest()[:16]
    return root / f"{key}.d"


def _dead(d: Path) -> bool:
    try:
        if time.time() - d.stat().st_mtime < 120:
            return False
        pid = int((d / "pid").read_text().split()[0])
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True
    except (OSError, ValueError, IndexError):
        return True


def _start() -> int:
    out = subprocess.run(["ps", "-o", "etime=", "-p", str(os.getpid())], capture_output=True, text=True).stdout
    parts = [int(x) for x in out.strip().replace("-", ":").split(":") if x.isdigit()]
    return int(time.time()) - sum(u * n for u, n in zip([86400, 3600, 60, 1][-len(parts):], parts))


def _key(k: str) -> str:
    return k if re.match(r"^[A-Za-z_][A-Za-z0-9_]*\Z", k) else json.dumps(k)


def _render(v, indent: int) -> list:
    pad = " " * indent
    out = []
    items = v.items() if isinstance(v, dict) else ((None, x) for x in v)
    for k, item in items:
        lead = f"{pad}{_key(k)}: " if k is not None else f"{pad}- "
        if isinstance(item, (dict, list)) and item:
            if k is not None:
                out.append(f"{pad}{_key(k)}:\n")
                out.extend(_render(item, indent + 2))
            else:
                inner = _render(item, indent + 2)
                out.append(f"{pad}- {inner[0][indent + 2:]}")
                out.extend(inner[1:])
        else:
            out.append(lead + json.dumps(item) + "\n")
    return out


def file(ledger: Path, ask: dict, session, wait: float) -> str:
    if not isinstance(ask, dict) or not isinstance(ask.get("id"), str):
        raise Refused("the ask must be an object with a string id")
    for name in ("id", "uid"):
        if name in ask and not (isinstance(ask[name], str) and ID_RE.match(ask[name])):
            raise Refused(f"the ask's {name} must be a string matching {ID_RE.pattern}")
    bad = sorted(k for k in ask if k in REFUSED or k.startswith("answer_"))
    if bad:
        raise Refused(f"an agent never files {', '.join(bad)}")
    entry = dict(ask)
    if session is not None:
        entry["origin"] = {"via": "maestro", "session": session, "verified": False}
    d = _lock_dir(ledger)
    deadline = time.time() + wait
    while True:
        try:
            d.mkdir()
            break
        except FileExistsError:
            if _dead(d):
                aside = Path(f"{d}.stale-{secrets.token_hex(4)}")
                try:
                    d.rename(aside)
                    for c in aside.glob("*"):
                        c.unlink()
                    aside.rmdir()
                except OSError:
                    pass
                continue
            if time.time() >= deadline:
                raise Busy(f"another writer holds the lock on {ledger}; nothing was written")
            time.sleep(0.05)
    token = secrets.token_hex(16)
    try:
        (d / "pid").write_text(f"{os.getpid()} {_start()}\n")
        (d / "token").write_text(token + "\n")
        text = ledger.read_text(encoding="utf-8")
        before = load(ledger)
        was = asks_of(before)
        if "asks" not in before or was is None:
            raise Refused("the ledger has no `asks` list to append to")
        taken = {str(v) for a in was for v in (a.get("id"), a.get("uid")) if v is not None}
        for name in (entry["id"], entry.get("uid")):
            if name is not None and name in taken:
                raise Refused(f"the ledger already has an ask {name!r}; nothing was written")
        lines = text.splitlines(keepends=True)
        at = next(i for i, line in enumerate(lines) if line.startswith("asks:"))
        end = next((i for i in range(at + 1, len(lines)) if lines[i][:1] not in ("", " ", "\n", "#", "-")), len(lines))
        while end > at + 1 and not lines[end - 1].strip():
            end -= 1
        body = _render({"id": entry["id"]}, 4)[0].replace("    ", "  - ", 1)
        rest = _render({k: v for k, v in entry.items() if k != "id"}, 4)
        new = "".join(lines[:end]) + body + "".join(rest) + "".join(lines[end:])
        if len(new.encode()) > LIMIT:
            raise Refused(f"the ledger would be over {LIMIT} bytes; nothing was written")
        after = yaml.safe_load(new)
        if asks_of(after) != [*was, entry]:
            raise Refused("the appended ledger does not read as the old one plus this ask")
        old_errs, new_errs = validate(before, path=ledger), validate(after, path=ledger)
        fresh = [e for e in new_errs if e not in old_errs]
        if fresh:
            raise Refused("the ledger would gain error(s): " + "; ".join(fresh))
        if (d / "token").read_text().strip() != token:
            raise Busy(f"the lock on {ledger} was taken over; nothing was written")
        tmp = ledger.with_name(f".{ledger.name}.tmp-{token}")
        tmp.write_text(new, encoding="utf-8")
        os.replace(tmp, ledger)
    finally:
        try:
            if (d / "token").read_text().strip() == token:
                for c in d.glob("*"):
                    c.unlink()
                d.rmdir()
        except OSError:
            pass
    return entry["id"]


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "file":
        args = sys.argv[3:]
        opt = lambda name, default=None: args[args.index(name) + 1] if name in args else default  # noqa: E731
        try:
            raw = sys.stdin.read() if opt("--json") == "-" else Path(opt("--json")).read_text()
            filed = file(Path(sys.argv[2]), json.loads(raw), opt("--maestro-session"), float(opt("--wait", "30")))
        except Busy as e:
            print(f"error: {e}", file=sys.stderr)
            sys.exit(75)
        except (Refused, OSError, ValueError) as e:
            print(f"error: {e}", file=sys.stderr)
            sys.exit(2)
        print(f"filed {filed} into {sys.argv[2]}")
        sys.exit(0)
    if len(sys.argv) < 3 or sys.argv[1] != "validate":
        sys.exit("usage: ask_ledger_stub.py validate|file <ledger>")
    ledger = load(Path(sys.argv[2]))
    errs = validate(ledger, path=Path(sys.argv[2]))
    for e in errs:
        print(f"  ERROR {e}")
    print(f"ask-ledger: {len(asks_of(ledger) or [])} ask(s), 0 decision(s), {len(errs)} error(s)")
    sys.exit(1 if errs else 0)
