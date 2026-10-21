#!/usr/bin/env python3
"""Check that the spec covers the plan (BRO-3262 acceptance).

Two claims of the spec's acceptance, made checkable:

1. Every node of the plan graph has exactly one row in the spec's node index, and the section that row
   links to exists in the spec.
2. Every ledger in the plugin inventory (the needles below, from the BRO-3262 ticket and the 2026-10-21
   read of maestro-paseo) is named by exactly one row of the retirement table.

Standard library only. Exit 0 when both hold, 1 with one line per failure, 2 on a bad invocation.

    python3 scripts/spec-coverage.py [--graph PATH] [--spec PATH]
"""

import argparse
import os
import re
import sys
from html.parser import HTMLParser

DEFAULT_GRAPH = os.path.expanduser("~/broomva/docs/plans/2026-10-21-maestro.graph.yaml")
DEFAULT_SPEC = os.path.join(os.path.dirname(__file__), "..", "docs", "specs", "2026-10-18-maestro-rust.html")

# Each ledger the plugin writes, as a string its retirement row must contain.
INVENTORY = [
    ".control/asks/*.yaml",
    "maestro/answers",
    "answers-landing.json",
    "asks.jsonl",
    "_work.md",
    "_events.jsonl",
    "verbs.jsonl",
    "approvals.jsonl",
    "notice-snoozes.jsonl",
    "proposal-reviews.jsonl",
    "session labels",
    "maestro.sock",
    "config.json",
    "drift.jsonl",
    "wake-log.jsonl",
    "inbox.json",
    "timeline.append",
    "Answer delivery",
    "respondToPermission",
]


class Tables(HTMLParser):
    """Collects every element id, and the rows of the tables that follow the headings with given ids."""

    def __init__(self, wanted):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.wanted = set(wanted)
        self.section = None
        self.rows = {key: [] for key in wanted}
        self.row = None
        self.cell = None

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        if tag == "h2":
            self.section = attrs.get("id") if attrs.get("id") in self.wanted else None
        elif self.section and tag == "tr":
            self.row = []
        elif self.row is not None and tag in ("td", "th"):
            self.cell = {"text": "", "hrefs": [], "header": tag == "th"}
        elif self.cell is not None and tag == "a" and "href" in attrs:
            self.cell["hrefs"].append(attrs["href"])

    def handle_endtag(self, tag):
        if tag in ("td", "th") and self.cell is not None:
            self.row.append(self.cell)
            self.cell = None
        elif tag == "tr" and self.row is not None:
            if self.row and not any(cell["header"] for cell in self.row):
                self.rows[self.section].append(self.row)
            self.row = None

    def handle_data(self, data):
        if self.cell is not None:
            self.cell["text"] += data


def graph_nodes(text):
    """Node ids under `nodes:` in the plan graph: lines `  - id: <name>`."""
    return re.findall(r"^\s*-\s+id:\s*([A-Za-z0-9_.-]+)\s*$", text, re.M)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--graph", default=DEFAULT_GRAPH)
    parser.add_argument("--spec", default=DEFAULT_SPEC)
    args = parser.parse_args()
    try:
        graph = open(args.graph, encoding="utf-8").read()
        spec = open(args.spec, encoding="utf-8").read()
    except OSError as error:
        print(f"spec-coverage: {error}", file=sys.stderr)
        return 2

    tables = Tables(["node-index", "retirement"])
    tables.feed(spec)
    failures = []

    nodes = graph_nodes(graph)
    if not nodes:
        failures.append(f"no nodes read from {args.graph}")
    index = {}
    for row in tables.rows["node-index"]:
        index.setdefault(row[0]["text"].strip(), []).append(row)
    for node in nodes:
        rows = index.get(node, [])
        if len(rows) != 1:
            failures.append(f"node {node}: {len(rows)} rows in the node index, want 1")
            continue
        hrefs = [href for cell in rows[0] for href in cell["hrefs"]]
        if not hrefs:
            failures.append(f"node {node}: its row links to no section")
        for href in hrefs:
            if not href.startswith("#") or href[1:] not in tables.ids:
                failures.append(f"node {node}: links to {href}, which is not a section of the spec")
    for name in sorted(set(index) - set(nodes)):
        failures.append(f"node index row {name}: not a node of the graph")

    retirement = [" ".join(cell["text"] for cell in row) for row in tables.rows["retirement"]]
    if not retirement:
        failures.append("the retirement table has no rows")
    for needle in INVENTORY:
        hits = sum(needle in row for row in retirement)
        if hits != 1:
            failures.append(f"ledger {needle}: named by {hits} retirement rows, want 1")

    for line in failures:
        print(line)
    if not failures:
        print(f"spec covers {len(nodes)} graph nodes and {len(INVENTORY)} plugin ledgers in {len(retirement)} rows")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
