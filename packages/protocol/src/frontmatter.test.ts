/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  parseWorkContract,
  parseWorkFile,
  parseWorkInput,
  reserializeWorkFile,
  resolveWorkContract,
  serializeWorkContract,
  serializeWorkFile,
  setWorkFileFields,
  WorkContractError,
  type WorkContractInput,
} from "./frontmatter";

// ── Fixtures at several depths (DATA-MODEL §A.1: a folder is work at any scale) ──

const INITIATIVE = `---
id: init-01
kind: initiative
state: proposed
owner: "@lead"
gate: human
budget:
  per_run_usd: 5
  per_day_usd: 40
  max_iterations: 30
created: 2026-06-25
updated: 2026-06-25
---

# Growth

The initiative brief.
`;

const TASK = `---
id: 7f3a9c
kind: task
state: running
owner: "@alex"
gate: human
budget:
  per_run_usd: 5
  per_day_usd: 20
  max_iterations: 40
done:
  check: bun test && bun run lint
  stop_on:
    - cap
    - no_progress
    - budget
created: 2026-06-25
updated: 2026-06-25
---

# Fix meta tags

The brief in plain language.
`;

const AUTO_TASK = `---
id: auto-01
kind: task
state: triggered
gate: auto
done:
  check:
    - name: unit
      run: bun test
      required: true
    - name: lint
      run: bun run lint
  diff:
    max_files: 30
    max_lines: 2000
created: 2026-06-26
updated: 2026-06-26
---

# Deterministic task
`;

const ROUTINE = `---
id: nightly-01
kind: routine
state: proposed
gate: human
trigger:
  on: cron
  at: "0 6 * * *"
  idempotency: nightly-triage-{{date}}
created: 2026-06-26
updated: 2026-06-26
---

# Nightly triage
`;

// ── Parsing ──────────────────────────────────────────────────────────────────

describe("parse — valid contracts at several depths", () => {
  test("a task with a string done.check + stop_on", () => {
    const { contract, brief } = parseWorkFile(TASK);
    expect(contract.id).toBe("7f3a9c");
    expect(contract.kind).toBe("task");
    expect(contract.state).toBe("running");
    expect(contract.owner).toBe("@alex");
    expect(contract.gate).toBe("human");
    expect(contract.budget).toEqual({ per_run_usd: 5, per_day_usd: 20, max_iterations: 40 });
    expect(contract.done?.check).toBe("bun test && bun run lint");
    expect(contract.done?.stop_on).toEqual(["cap", "no_progress", "budget"]);
    expect(brief).toContain("# Fix meta tags");
  });

  test("gate:auto with a named-check list + diff limits", () => {
    const c = parseWorkContract(AUTO_TASK);
    expect(c.gate).toBe("auto");
    expect(Array.isArray(c.done?.check)).toBe(true);
    expect(c.done?.check).toEqual([
      { name: "unit", run: "bun test", required: true },
      { name: "lint", run: "bun run lint" },
    ]);
    expect(c.done?.diff).toEqual({ max_files: 30, max_lines: 2000 });
  });

  test("a routine carries its trigger block", () => {
    const c = parseWorkContract(ROUTINE);
    expect(c.kind).toBe("routine");
    expect(c.trigger).toEqual({
      on: "cron",
      at: "0 6 * * *",
      idempotency: "nightly-triage-{{date}}",
    });
  });

  test("gate defaults to human when omitted", () => {
    const src = `---\nid: x\nkind: task\nstate: proposed\ncreated: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n`;
    expect(parseWorkContract(src).gate).toBe("human");
    // ...but the input layer keeps it absent, so resolution can still inherit it.
    expect(parseWorkInput(src).input.gate).toBeUndefined();
  });
});

// ── Round-trip ───────────────────────────────────────────────────────────────

describe("round-trip", () => {
  test("content round-trip is stable (parse → serialize → parse)", () => {
    for (const src of [INITIATIVE, TASK, AUTO_TASK, ROUTINE]) {
      const a = parseWorkFile(src);
      const b = parseWorkFile(serializeWorkFile(a));
      expect(b).toEqual(a);
    }
  });

  test("serialize emits fields in canonical order", () => {
    const out = serializeWorkContract(parseWorkContract(TASK), "body");
    const order = [
      "id:",
      "kind:",
      "state:",
      "owner:",
      "gate:",
      "budget:",
      "done:",
      "created:",
      "updated:",
    ];
    const positions = order.map((k) => out.indexOf(k));
    expect(positions).toEqual([...positions].sort((x, y) => x - y));
    expect(positions.every((p) => p >= 0)).toBe(true);
  });

  test("reserialize preserves comments + key order (where feasible), and is idempotent", () => {
    const commented = `---
id: c-01
kind: task
state: proposed
gate: human   # the human gate is the default for anything irreversible
created: 2026-06-26
updated: 2026-06-26
---

# Commented
`;
    const once = reserializeWorkFile(commented);
    expect(once).toContain("# the human gate is the default");
    // key order preserved: id before gate before created
    expect(once.indexOf("id:")).toBeLessThan(once.indexOf("gate:"));
    expect(once.indexOf("gate:")).toBeLessThan(once.indexOf("created:"));
    expect(reserializeWorkFile(once)).toBe(once); // idempotent
  });
});

// ── Parent-defaults resolution (FLOWS §F1 step 2) ────────────────────────────

describe("resolve — child inherits owner/gate/budget from parent unless overridden", () => {
  const parent = parseWorkContract(INITIATIVE); // owner @lead, gate human, budget

  test("a bare child inherits owner + gate + budget", () => {
    const child: WorkContractInput = {
      id: "child-01",
      kind: "task",
      state: "proposed",
      created: "2026-06-27",
      updated: "2026-06-27",
    };
    const resolved = resolveWorkContract(child, parent);
    expect(resolved.owner).toBe("@lead");
    expect(resolved.gate).toBe("human");
    expect(resolved.budget).toEqual({ per_run_usd: 5, per_day_usd: 40, max_iterations: 30 });
  });

  test("a child's own owner/budget override the parent's", () => {
    const child: WorkContractInput = {
      id: "child-02",
      kind: "task",
      state: "proposed",
      owner: "@alex",
      budget: { per_run_usd: 1 },
      created: "2026-06-27",
      updated: "2026-06-27",
    };
    const resolved = resolveWorkContract(child, parent);
    expect(resolved.owner).toBe("@alex");
    expect(resolved.budget).toEqual({ per_run_usd: 1 });
    expect(resolved.gate).toBe("human"); // still inherited
  });

  test("no parent → materialize with defaults", () => {
    const child: WorkContractInput = {
      id: "root-01",
      kind: "project",
      state: "proposed",
      created: "2026-06-27",
      updated: "2026-06-27",
    };
    expect(resolveWorkContract(child).gate).toBe("human");
  });

  test("done and trigger are never inherited", () => {
    const richParent = parseWorkContract(AUTO_TASK); // has done + gate auto
    const child: WorkContractInput = {
      id: "child-03",
      kind: "task",
      state: "proposed",
      done: { check: "bun test" },
      created: "2026-06-27",
      updated: "2026-06-27",
    };
    // child keeps its own done; inherits gate auto — which is legal because the
    // child supplies its own check.
    const resolved = resolveWorkContract(child, richParent);
    expect(resolved.done?.check).toBe("bun test");
    expect(resolved.trigger).toBeUndefined();
    expect(resolved.gate).toBe("auto");
  });
});

// ── Rejections (typed errors) ────────────────────────────────────────────────

function codeOf(fn: () => unknown): string {
  try {
    fn();
  } catch (err) {
    if (err instanceof WorkContractError) return err.code;
    throw err;
  }
  throw new Error("expected a WorkContractError, none thrown");
}

describe("reject — invalid contracts throw typed errors", () => {
  test("no frontmatter", () => {
    expect(codeOf(() => parseWorkContract("# just markdown, no fence\n"))).toBe("no_frontmatter");
  });

  test("invalid YAML", () => {
    expect(codeOf(() => parseWorkContract("---\nid: [unclosed\n---\n"))).toBe("invalid_yaml");
  });

  test("bad state enum", () => {
    const bad = TASK.replace("state: running", "state: sleeping");
    expect(codeOf(() => parseWorkContract(bad))).toBe("invalid_enum");
  });

  test("bad kind enum", () => {
    const bad = TASK.replace("kind: task", "kind: epic");
    expect(codeOf(() => parseWorkContract(bad))).toBe("invalid_enum");
  });

  test("missing required id", () => {
    const bad = TASK.replace("id: 7f3a9c\n", "");
    expect(codeOf(() => parseWorkContract(bad))).toBe("missing_field");
  });

  test("malformed done — no check", () => {
    const bad = `---\nid: x\nkind: task\nstate: proposed\ngate: human\ndone:\n  judge: rubric.md\ncreated: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n`;
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_done");
  });

  test("malformed done — check list item missing run", () => {
    const bad = `---\nid: x\nkind: task\nstate: proposed\ngate: human\ndone:\n  check:\n    - name: unit\ncreated: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n`;
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_done");
  });

  test("gate:auto with no check is rejected", () => {
    const bad = `---\nid: x\nkind: task\nstate: proposed\ngate: auto\ncreated: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n`;
    expect(codeOf(() => parseWorkContract(bad))).toBe("gate_auto_no_check");
  });

  test("an empty done.check string is malformed", () => {
    const bad = `---\nid: x\nkind: task\nstate: proposed\ngate: human\ndone:\n  check: ""\ncreated: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n`;
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_done");
  });

  test("an empty done.check list is malformed (symmetric with the empty string)", () => {
    const bad = `---\nid: x\nkind: task\nstate: proposed\ngate: human\ndone:\n  check: []\ncreated: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n`;
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_done");
  });

  test("reserializeWorkFile enforces the same gate:auto rule as the read path", () => {
    // The round-trip write path (BRO-1800 scanner) must reject exactly what every
    // read path rejects — not silently re-emit a VERIFIER §1-violating contract.
    const bad = `---\nid: x\nkind: task\nstate: proposed\ngate: auto\ncreated: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n`;
    expect(codeOf(() => reserializeWorkFile(bad))).toBe("gate_auto_no_check");
    expect(codeOf(() => parseWorkContract(bad))).toBe("gate_auto_no_check");
  });

  test("bad stop_on value", () => {
    const bad = TASK.replace("    - cap", "    - explode");
    expect(codeOf(() => parseWorkContract(bad))).toBe("invalid_enum");
  });

  test("wrong type for budget number", () => {
    const bad = TASK.replace("per_run_usd: 5", 'per_run_usd: "lots"');
    expect(codeOf(() => parseWorkContract(bad))).toBe("invalid_type");
  });
});

describe("done-schema — VERIFIER §1 timeout caps + rubric refs + D8 fixture 4 (BRO-1753)", () => {
  const HEAD = "---\nid: x\nkind: task\nstate: proposed\n";
  const TAIL = "created: 2026-06-26\nupdated: 2026-06-26\n---\n# x\n";
  // gate defaults to human unless the case needs auto; `done` is the block under test.
  const doc = (done: string, gate = "gate: human\n") => `${HEAD}${gate}${done}${TAIL}`;

  test("a full valid done: block (in-cap timeout, .md rubric, extended protect, diff) parses clean", () => {
    const ok = doc(
      "done:\n  check:\n    - name: tests\n      run: bun test\n      timeout_s: 600\n  judge: rubric.md\n  protect:\n    - package.json\n  diff:\n    max_files: 30\n    max_lines: 2000\n",
    );
    const c = parseWorkContract(ok);
    expect(c.done?.judge).toBe("rubric.md");
    expect(c.done?.protect).toContain("package.json");
  });

  test("timeout_s over the hard cap (1800) is malformed_done", () => {
    const bad = doc("done:\n  check:\n    - name: t\n      run: bun test\n      timeout_s: 3600\n");
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_done");
  });

  test("a non-positive timeout_s is malformed_done", () => {
    const bad = doc("done:\n  check:\n    - name: t\n      run: bun test\n      timeout_s: 0\n");
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_done");
  });

  test("timeout_s exactly at the cap (1800) is allowed", () => {
    const ok = doc("done:\n  check:\n    - name: t\n      run: bun test\n      timeout_s: 1800\n");
    expect(parseWorkContract(ok).done?.check).toBeDefined();
  });

  test("a non-.md rubric ref is malformed_rubric", () => {
    const bad = doc("done:\n  check: bun test\n  judge: rubric.yaml\n");
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_rubric");
  });

  test("an absolute rubric ref is malformed_rubric", () => {
    const bad = doc("done:\n  check: bun test\n  judge: /etc/rubric.md\n");
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_rubric");
  });

  test("a parent-traversal rubric ref is malformed_rubric (escapes the worktree)", () => {
    const bad = doc("done:\n  check: bun test\n  judge: ../rubric.md\n");
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_rubric");
  });

  test("a valid nested rubric ref is accepted", () => {
    const ok = doc("done:\n  check: bun test\n  judge: checks/rubric.md\n");
    expect(parseWorkContract(ok).done?.judge).toBe("checks/rubric.md");
  });

  // D8 fixture 4 — a judge-only contract trying gate:auto is rejected at contract validation.
  // The parser makes `check` mandatory in any done: block, so it surfaces two ways:
  test("D8 fixture 4a — gate:auto with an empty check list is rejected (malformed_done)", () => {
    const bad = doc("done:\n  check: []\n  judge: rubric.md\n", "gate: auto\n");
    expect(codeOf(() => parseWorkContract(bad))).toBe("malformed_done");
  });

  test("D8 fixture 4b — gate:auto with no deterministic check is rejected (gate_auto_no_check)", () => {
    const bad = doc("", "gate: auto\n");
    expect(codeOf(() => parseWorkContract(bad))).toBe("gate_auto_no_check");
  });
});

// ── setWorkFileFields — comment-preserving frontmatter field patch (BRO-1914) ──
describe("setWorkFileFields", () => {
  // A valid `_work.md` carrying an inline comment + a body — the round-trip must keep BOTH.
  const SOURCE = `---
id: n0
kind: task
state: triggered # the run's current state
gate: human
owner: alice
created: 2026-06-25
updated: 2026-06-25
---

# do the thing
`;

  test("patches state in place, preserving the inline comment, key order, and the brief", () => {
    const out = setWorkFileFields(SOURCE, { state: "review" });
    expect(parseWorkFile(out).contract.state).toBe("review");
    expect(out).toContain("state: review # the run's current state"); // comment preserved
    expect(out).toContain("# do the thing"); // brief preserved
    // key order unchanged: id before kind before state
    expect(out.indexOf("id:")).toBeLessThan(out.indexOf("kind:"));
    expect(out.indexOf("kind:")).toBeLessThan(out.indexOf("state:"));
  });

  test("patches multiple fields at once", () => {
    const out = setWorkFileFields(SOURCE, { state: "blocked", owner: "bob" });
    const c = parseWorkFile(out).contract;
    expect(c.state).toBe("blocked");
    expect(c.owner).toBe("bob");
  });

  test("a null value DELETES the key (clearing an inherited owner)", () => {
    const out = setWorkFileFields(SOURCE, { owner: null });
    expect(out).not.toContain("owner:");
    expect(parseWorkFile(out).contract.owner).toBeUndefined();
  });

  test("is idempotent — patching to the same value round-trips byte-for-byte", () => {
    const once = setWorkFileFields(SOURCE, { state: "review" });
    const twice = setWorkFileFields(once, { state: "review" });
    expect(twice).toBe(once);
  });

  test("throws on a malformed SOURCE (rejects exactly what the read path rejects)", () => {
    expect(() => setWorkFileFields("no frontmatter here", { state: "review" })).toThrow(
      WorkContractError,
    );
  });

  test("throws when the PATCH would make the contract invalid (bad state enum)", () => {
    expect(() => setWorkFileFields(SOURCE, { state: "bogus" })).toThrow(WorkContractError);
  });
});
