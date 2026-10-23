#!/usr/bin/env python3
"""A stand-in for the workspace's scripts/ask_ledger.py, for tests and hosted CI (the workspace
repository is private). Maestro calls the validator of record's own `load`, `validate(ledger,
preauth, path=...)` and `asks_of`; this module has the same three, reading YAML with PyYAML as
the real one does. An ask whose any value holds STUB-INVALID is an error, and so is any ask in a
ledger named `ideas-*.yaml` whose class is not `idea` (a rule judged on the ledger's real name).
STUB_SLEEP_S holds each validation (and so the ledger lock) that long. The live check runs the
real validator."""
import os
import re
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


if __name__ == "__main__":
    if len(sys.argv) < 3 or sys.argv[1] != "validate":
        sys.exit("usage: ask_ledger_stub.py validate <ledger>")
    ledger = load(Path(sys.argv[2]))
    errs = validate(ledger, path=Path(sys.argv[2]))
    for e in errs:
        print(f"  ERROR {e}")
    print(f"ask-ledger: {len(asks_of(ledger) or [])} ask(s), 0 decision(s), {len(errs)} error(s)")
    sys.exit(1 if errs else 0)
