#!/usr/bin/env python3
"""Differential parity fuzz: Maestro's ask reading against the Paseo plugin's own.

Generates N small ledgers from a grammar of edge values (numbers in every base and
size, keys, tags, quoting, aliases, timestamps, whitespace, BOM, directives),
reads them with the plugin's code (scripts/plugin-golden.ts, run inside the plugin
checkout so its `yaml` package is used) and with `maestro --tree worktree`, and
reports every ledger whose asks (handle, entry hash, state) differ, or that one
side refuses and the other reads.

A divergence that is one of the README's known gaps is counted, not failed:
each gap is recognised by the reason Maestro gives, so a gap that starts reading
silently shows up as a failure.

    python3 scripts/parity-fuzz.py [--n 1500] [--seed 7] [--maestro target/debug/maestro]
                                   [--plugin ~/broomva/apps/maestro-paseo] [--keep DIR]

Exits 0 only when every divergence is a known gap.
"""

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Maestro's own reason for each documented gap, and what the plugin does there.
KNOWN_GAPS = {
    "1.1 rules": "a %YAML 1.1 document is refused",
    "collection used as a map key": "a collection key is refused",
    "lone surrogate": "a lone surrogate reads as U+FFFD (reported)",
    "nested deeper than": "nesting past MAX_DEPTH is refused",
    "tagged map key": "a date or bytes key is refused",
}

NUMBERS = [
    "0", "-0", "+0", "007", "0004", "08", "010", "+5", "-12", "1_000", "0b101", "0o17", "0o", "0x", "0x1F", "0xff",
    "0x400000000000021", "0x10000000000000801", "0o47664030373367731526", "9007199254740993", "18446744073709551616",
    "-9223372036854775809", "20261019123456789012", "1" + "0" * 30, "9" * 320, "1.0", "2.50", ".5", "5.", "-.25",
    "1e3", "1E3", "1.5e-7", "1e21", "1e400", "1e-400", ".5e3", "5.e3", "1e", "1.", "+.5", ".inf", "-.Inf", "+.INF",
    ".nan", ".NaN", ".NAN", "NaN", "Infinity", "- 1", "1.2.3", "0x1.8p3", "123456789012345678",
]
WORDS = ["true", "True", "TRUE", "tRue", "false", "FALSE", "yes", "no", "on", "off", "y", "n", "~", "null", "Null",
         "NULL", "nUll", "", "<<", "=", "-", "?", ":"]
STRINGS = ['"q"', "'s'", '"1e3"', "'0x10'", '"true"', '"\\u00e9"', '"\\N\\_\\L\\P"', '"\\x41"', '"tab\\there"',
           '"\\uD83D\\uDE00"', '"\\uFEFF"', '"\\u0085x"', '" pad "', '"\\u3000x\\u2028"', "|\n      block\n      text",
           ">-\n      folded\n      text", "plain words", "multi\n      line plain", "'it''s'", '"C:\\\\path"']
TAGS = ["!!str 123", "!!int \"42\"", "!!int 0x1F", "!!float 1", "!!float .inf", "!!bool \"true\"", "!!bool maybe",
        "!!null ''", "!!null x", "!!str true", "!custom foo", "! 12", "!!binary aGk=", "!!binary aGk",
        "!!timestamp 2026-10-19", "!!timestamp 2026-02-31", "!!timestamp 2026-13-45", "!!timestamp 0050-01-01",
        "!!timestamp 2026-10-19 23:59:60.1234 +5", "!!timestamp 2026-10-19t10:30:00-03:30",
        "!!timestamp 2026-10-19T24:00:00Z", "!!timestamp 2026-10-19T10:30:00.9995Z", "!!timestamp yesterday",
        "!!timestamp 2026-10-19T10:30:00+05:30", "!!timestamp 2026-1-9", "!!set {a: , b: }", "!!omap [{a: 1}]",
        "!!pairs [{a: 1}, {a: 2}]", "!!map {a: 1}", "!!seq [1, 2]", "!custom {a: 1}"]
KEYS = ["a", "b", "0x10", "1.0", "07", "~", "null", "true", "10", "2", "\"3\"", ".inf", "-.inf", ".nan", "'a'",
        "\"\\uFF61\"", "\"\\U0001F600\"", "B", "id2", "1e3"]
ANSWER_KEYS = ["answer", "resolution", "outcome", "answered_at", "status", "archived_at", "blocking", "preanswerable"]
IDS = ["c", "0004", "42", "1.0", "0x1A", "true", "~", '"\\uFEFF"', '"\\u0085"', '" padded "', "-0", ".inf", "'x'"]


def value(rng: random.Random) -> str:
    pick = rng.random()
    if pick < 0.35:
        return rng.choice(NUMBERS)
    if pick < 0.55:
        return rng.choice(WORDS)
    if pick < 0.75:
        return rng.choice(STRINGS)
    if pick < 0.92:
        return rng.choice(TAGS)
    return "[" + ", ".join(rng.choice(NUMBERS + WORDS[:6]) for _ in range(rng.randint(0, 3))) + "]"


def ledger(rng: random.Random, i: int) -> str:
    lines = []
    if rng.random() < 0.05:
        lines.append("\ufeff")
    if rng.random() < 0.04:
        lines.append(rng.choice(["%YAML 1.2\n---\n", "%YAML 1.1\n---\n", "%TAG ! tag:x,2026:\n%YAML 1.1\n---\n"]))
    if rng.random() < 0.15:
        lines.append(f"arc: {value(rng)}\n")
    alias = rng.random() < 0.2
    if alias:
        lines.append(f"base: &b {rng.choice(['[x]', 'x', '[]', '{k: 1}', '[[a], [b]]'])}\n")
    lines.append("asks:\n")
    for row in range(rng.randint(1, 3)):
        rid = rng.choice(IDS) if rng.random() < 0.4 else f"r{i}x{row}"
        lines.append(f"  - id: {rid}\n")
        for _ in range(rng.randint(0, 4)):
            key = rng.choice(ANSWER_KEYS) if rng.random() < 0.3 else rng.choice(KEYS)
            lines.append(f"    {key}: {value(rng)}\n")
        if alias:
            for n in range(rng.choice([1, 2, 10, 60])):
                lines.append(f"    al{n}: *b\n")
        if rng.random() < 0.15:
            lines.append("    meta:\n")
            for _ in range(rng.randint(1, 4)):
                lines.append(f"      {rng.choice(KEYS)}: {value(rng)}\n")
    text = "".join(lines)
    # Layout noise the two parsers must agree on: comments, document markers, line endings, tabs.
    if rng.random() < 0.15:
        text = text.replace("asks:\n", "asks: # the asks\n", 1)
    if rng.random() < 0.08:
        text = "---\n" + text + rng.choice(["", "...\n", "---\n"])
    if rng.random() < 0.06:
        text = text.replace("\n", "\r\n")
    if rng.random() < 0.03:
        text = text.replace("\n", "\r")
    if rng.random() < 0.05:
        text = text.replace(": ", ":\t", 1)
    if rng.random() < 0.05:
        text += "  - id: dupkey\n    a: 1\n    'a': 2\n"
    return text


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=1500)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--maestro", default=str(ROOT / "target/debug/maestro"))
    ap.add_argument("--plugin", default=os.path.expanduser("~/broomva/apps/maestro-paseo"))
    ap.add_argument("--keep", help="write the corpus here and keep it")
    args = ap.parse_args()

    rng = random.Random(args.seed)
    tmp = Path(args.keep) if args.keep else Path(tempfile.mkdtemp(prefix="maestro-fuzz-"))
    asks = tmp / "ws/.control/asks"
    asks.mkdir(parents=True, exist_ok=True)
    for i in range(args.n):
        (asks / f"f{i:05}.yaml").write_text(ledger(rng, i), encoding="utf-8")

    golden_script = Path(args.plugin) / ".parity-fuzz-golden.ts"
    shutil.copy(ROOT / "scripts/plugin-golden.ts", golden_script)
    try:
        plugin = subprocess.run(["bun", golden_script.name, str(asks)], cwd=args.plugin, capture_output=True, text=True, check=True)
    finally:
        golden_script.unlink(missing_ok=True)
    golden = json.loads(plugin.stdout)
    rust = json.loads(subprocess.run(
        [args.maestro, "--workspace", str(tmp / "ws"), "--tree", "worktree", "--json", "asks", "--state", "all", "--limit", "1000000"],
        capture_output=True, text=True, check=True).stdout)

    errors: dict[str, list[str]] = {}
    for e in rust["errors"]:
        errors.setdefault(e["at"].split("/")[-1].split("#")[0], []).append(e["reason"])
    got: dict[str, list] = {}
    for a in rust["asks"]:
        got.setdefault(a["ledger"], []).append((a["id"], a["hash"], a["state"]))

    gaps: dict[str, int] = {}
    failures = []
    for name, want in sorted(golden.items()):
        reasons = errors.get(name, [])
        gap = next((g for g in KNOWN_GAPS if any(g in r for r in reasons)), None)
        if "error" in want:
            if name in errors:
                continue
            failures.append((name, f"plugin refuses ({want['error'][:70]}); Maestro reads {got.get(name)}"))
            continue
        expected = [(a["id"], a["hash"], a["state"]) for a in want["asks"]]
        if expected == got.get(name, []):
            continue
        if gap:
            gaps[gap] = gaps.get(gap, 0) + 1
        else:
            failures.append((name, f"want {expected} got {got.get(name, [])} errors {[r[:60] for r in reasons]}"))

    print(f"ledgers {len(golden)}  diverging {len(failures) + sum(gaps.values())}  known gaps {sum(gaps.values())} {gaps}")
    for name, why in failures[:40]:
        print(f"  FAIL {name}: {why}")
        print("    " + (asks / name).read_text(encoding="utf-8").replace("\n", "\n    ")[:600])
    if not args.keep:
        shutil.rmtree(tmp, ignore_errors=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
