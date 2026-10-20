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

# A refusal for YAML outside the subset is loud and named: counted, not failed. Anything Maestro
# reads must match the plugin exactly, and anything the plugin refuses Maestro must refuse too.
OUTSIDE = "outside the YAML subset Maestro reads exactly"

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
    # Syntax the two parsers must accept and refuse alike.
    for _ in range(rng.choice([0, 0, 1, 2, 3])):
        text = syntax_noise(rng, text)
    return text


SYNTAX_ROWS = [
    "  - id: s{n}\n    v: |+\n      keep\n\n",
    "  - id: s{n}\n    v: |-\n      strip\n",
    "  - id: s{n}\n    v: |2\n       indented\n",
    "  - id: s{n}\n    v: >\n      folded\n\n      para\n",
    "  - id: s{n}\n    v: plain\n      continued\n      again\n",
    "  - id: s{n}\n    v: plain\n      with: colon\n",
    "  - id: s{n}\n    v: plain # comment\n",
    "  - id: s{n}\n    v: plain#nocomment\n",
    "  - id: s{n}\n    v: [a,\n      b, c]\n",
    "  - id: s{n}\n    v: {a: 1,\n      b: 2}\n",
    "  - id: s{n}\n    v: \"multi\n      line quoted\"\n",
    "  - id: s{n}\n    v: 'multi\n\n      single'\n",
    "  - id: s{n}\n    ? explicit\n    : value\n",
    "  - id: s{n}\n    \"quoted key\": 1\n",
    "  - id: s{n}\n    v: &anc scalar\n    w: *anc\n",
    "  - id: s{n}\n    v:\n    - unindented\n    - seq\n",
    "  - id: s{n}\n    v: -dash\n    w: - not a seq\n",
    "  - id: s{n}\n    v: a: b\n",
    "  - id: s{n}\n    v: x # c\n    # full line\n    w: y\n",
    "  - id: s{n}\n    v: ''\n    w: \"\"\n    x:\n",
    "  - id: s{n}\n    v: @reserved\n",
    "  - id: s{n}\n    v: `tick\n",
    "  - id: s{n}\n    v: %percent\n",
    "  - id: s{n}\n    v: !\n",
    "  - id: s{n}\n    v: [a, b,]\n",
    "  - id: s{n}\n    v: {a, b: 1}\n",
    "  - id: s{n}\n    v: [a: 1, b]\n",
    "  - id: s{n}\n    v: \"esc \\a \\e \\0 \\/ \\  x\"\n",
    "  - id: s{n}\n    v: \"bad \\q\"\n",
    "  - id: s{n}\n    v: 'it''s'\n",
    "  - id: s{n}\n    v: >-\n\n      leading blank\n",
    "  - id: s{n}\n    v: |\n    under-indented\n",
    "  - id: s{n}\n    v:   spaced   \n",
    "  - id: s{n}\n    v: x\n     w: misaligned\n",
    "  - id: s{n}\n    v: ? a\n",
    "  - id: s{n}\n    v: [[a, [b]], {c: [d]}]\n",
    "  - id: s{n}\n    v: a\u00a0b\n",
    "  - id: s{n}\n    v: \u2028x\n",
]


def syntax_noise(rng: random.Random, text: str) -> str:
    pick = rng.random()
    if pick < 0.45:
        return text + rng.choice(SYNTAX_ROWS).replace("{n}", str(rng.randint(0, 999)))
    if pick < 0.6:  # a tab somewhere after a separator
        idx = [i for i, c in enumerate(text) if c in ":-" and i + 1 < len(text) and text[i + 1] == " "]
        if idx:
            i = rng.choice(idx)
            return text[: i + 1] + rng.choice(["\t", " \t", "\t "]) + text[i + 2 :]
    if pick < 0.7:  # a tab at the start of a line
        lines = text.split("\n")
        j = rng.randrange(len(lines))
        lines[j] = "\t" + lines[j]
        return "\n".join(lines)
    if pick < 0.8:  # a lone CR inside a line
        idx = [i for i, c in enumerate(text) if c == " "]
        if idx:
            i = rng.choice(idx)
            return text[:i] + "\r" + text[i:]
    if pick < 0.9:  # trailing spaces or tabs at line ends
        return text.replace("\n", rng.choice([" \n", "\t\n"]), rng.randint(1, 3))
    return text.replace("    ", "   ", 1)  # one under-indented line


SUBSET_BLOCKS = ["|", "|-", ">", ">-"]
SUBSET_TEXT = ["plain words", "Ünïcödé ¿sí? 日本語 😀", "with: colon inside quotes", "a - dash", "trailing.", "#not-a-comment", "x#y"]


def subset_value(rng: random.Random, indent: int) -> str:
    """A value ledger writers produce: within the subset, but stressing its edges."""
    pad = " " * (indent + 2)
    pick = rng.random()
    if pick < 0.30:  # a block scalar, with blank lines and deeper lines inside
        lines = []
        for _ in range(rng.randint(0, 4)):
            r = rng.random()
            lines.append("" if r < 0.2 else ("  " + rng.choice(SUBSET_TEXT) if r < 0.35 else rng.choice(SUBSET_TEXT)))
        if rng.random() < 0.3:
            lines += [""] * rng.randint(1, 2)  # trailing empty lines: chomping decides them
        body = "".join(("\n" if not l else "\n" + pad + l) for l in lines)
        return rng.choice(SUBSET_BLOCKS) + body
    if pick < 0.45:  # a multi-line plain scalar
        return rng.choice(["first", "a long", "words"]) + "".join("\n" + pad + rng.choice(["second", "third line", "more: no", "x"]) for _ in range(rng.randint(1, 3)))
    if pick < 0.58:  # a multi-line double-quoted scalar, with an escaped line break sometimes
        parts = [rng.choice(["one", "two words", "é"]) for _ in range(rng.randint(2, 3))]
        sep = rng.choice(["\n" + pad, "\\\n" + pad, "\n\n" + pad])
        return '"' + sep.join(parts) + '"'
    if pick < 0.66:  # single-quoted, multi-line, with '' escapes
        return "'" + ("it''s" if rng.random() < 0.5 else "plain") + "\n" + pad + "next'"
    if pick < 0.76:  # flow
        return rng.choice(["[a, b]", "[]", "{}", "{k: v, n: 1}", "[1, [2, 3], {x: y}]", "[a,\n" + pad + "b]", "{a: 1,\n" + pad + "b: 2}"])
    if pick < 0.86:
        return rng.choice(NUMBERS[:20] + WORDS[:10])
    return rng.choice(['"q"', "'s'", '"\\u00e9"', '"\\N"', '"tab\\there"', '"\\U0001F600"'])


def subset_ledger(rng: random.Random, i: int) -> str:
    out = ["\ufeff" if rng.random() < 0.05 else "", "---\n" if rng.random() < 0.1 else "", "asks:\n"]
    anchored = False
    for row in range(rng.randint(1, 3)):
        out.append(f"  - id: {rng.choice(IDS[:6]) if rng.random() < 0.3 else f'k{i}x{row}'}\n")
        for _ in range(rng.randint(1, 5)):
            key = rng.choice(ANSWER_KEYS) if rng.random() < 0.25 else rng.choice(["note", "body", "q", "meta2", "ask", "default"])
            out.append(f"    {key}: {subset_value(rng, 4)}\n")
        if rng.random() < 0.15 and not anchored:
            out.append("    origin: &o {agent_id: z, cwd: /x}\n")
            anchored = True
        elif anchored and rng.random() < 0.4:
            out.append("    origin2: *o\n")
        if rng.random() < 0.1:
            out.append("    # a comment line\n")
    text = "".join(out)
    if rng.random() < 0.1:
        text = text.replace("\n", "\r\n")
    # Keys must be unique per row: drop repeats so the case tests reading, not duplicate refusal.
    rows, seen = [], set()
    for line in text.split("\n"):
        if line.startswith("  - id:"):
            seen = set()
        key = line.strip().split(":")[0] if line.startswith("    ") and not line.startswith("     ") and ":" in line else None
        if key and key in seen:
            continue
        if key:
            seen.add(key)
        rows.append(line)
    return "\n".join(rows)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=1500)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--maestro", default=str(ROOT / "target/debug/maestro"))
    ap.add_argument("--plugin", default=os.path.expanduser("~/broomva/apps/maestro-paseo"))
    ap.add_argument("--keep", help="write the corpus here and keep it")
    ap.add_argument("--subset-share", type=float, default=0.6, help="share of ledgers drawn inside the subset")
    args = ap.parse_args()

    rng = random.Random(args.seed)
    tmp = Path(args.keep) if args.keep else Path(tempfile.mkdtemp(prefix="maestro-fuzz-"))
    asks = tmp / "ws/.control/asks"
    asks.mkdir(parents=True, exist_ok=True)
    for i in range(args.n):
        doc = subset_ledger(rng, i) if rng.random() < args.subset_share else ledger(rng, i)
        (asks / f"f{i:05}.yaml").write_bytes(doc.encode("utf-8"))

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

    # Ledger-level errors only: an entry the plugin drops silently (no id) Maestro also drops, and
    # reports as `<ledger>#<index>`; the asks list compares those, not the report.
    errors: dict[str, list[str]] = {}
    for e in rust["errors"]:
        if "#" in e["at"].split("/")[-1]:
            continue
        errors.setdefault(e["at"].split("/")[-1], []).append(e["reason"])
    got: dict[str, list] = {}
    for a in rust["asks"]:
        got.setdefault(a["ledger"], []).append((a["id"], a["hash"], a["state"]))

    gaps: dict[str, int] = {}
    failures = []
    read_same = 0
    for name, want in sorted(golden.items()):
        reasons = errors.get(name, [])
        read = got.get(name, [])
        if any(OUTSIDE in r for r in reasons):
            if read:
                failures.append((name, f"refused as outside the subset and read at once: {read}"))
            else:
                gaps["outside the subset"] = gaps.get("outside the subset", 0) + 1
            continue
        if "error" in want:
            # The plugin refuses it: Maestro must read nothing from it.
            if not read and name in errors:
                continue
            failures.append((name, f"plugin refuses ({want['error'][:70]}); Maestro reads {read} errors {[r[:50] for r in reasons]}"))
            continue
        expected = [(a["id"], a["hash"], a["state"]) for a in want["asks"]]
        if expected == read and not reasons:
            read_same += 1
            continue
        failures.append((name, f"want {expected} got {read} errors {[r[:60] for r in reasons]}"))

    print(f"ledgers {len(golden)}  read identically {read_same}  refused loudly {gaps}  FAILURES {len(failures)}")
    for name, why in failures[:40]:
        print(f"  FAIL {name}: {why}")
        print("    " + (asks / name).read_text(encoding="utf-8").replace("\n", "\n    ")[:600])
    if not args.keep:
        shutil.rmtree(tmp, ignore_errors=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
