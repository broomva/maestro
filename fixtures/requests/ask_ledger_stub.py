#!/usr/bin/env python3
"""A stand-in for the workspace's scripts/ask_ledger.py `validate`, for tests and hosted CI
(the workspace repository is private). It speaks the real one's output grammar: one
`  ERROR ...` line per error, then `ask-ledger: N ask(s), 0 decision(s), E error(s)`, exit 1
on any error. An entry whose text holds STUB-INVALID is an error; STUB_SLEEP_S holds the
validator (and so the ledger lock) that long. The live check runs the real validator."""
import os
import re
import sys
import time

if len(sys.argv) < 3 or sys.argv[1] != "validate":
    sys.exit("usage: ask_ledger_stub.py validate <ledger> [--preauth P]")
time.sleep(float(os.environ.get("STUB_SLEEP_S") or 0))
text = open(sys.argv[2], encoding="utf-8").read()
asks = len(re.findall(r"^ {0,2}- id:", text, re.M))
errors = [f"ask {n}: marked STUB-INVALID" for n, line in enumerate(text.splitlines()) if "STUB-INVALID" in line]
for e in errors:
    print(f"  ERROR {e}")
print(f"ask-ledger: {asks} ask(s), 0 decision(s), {len(errors)} error(s)")
sys.exit(1 if errors else 0)
