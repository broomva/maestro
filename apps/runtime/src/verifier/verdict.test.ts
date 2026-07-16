import { afterAll, describe, expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { VerdictReceipt } from "@maestro/protocol";
import { parse as parseYaml } from "yaml";
import {
  appendVerdictFeedback,
  decideVerdictOutcome,
  readVerdict,
  renderVerdictBody,
  renderVerdictFeedback,
  renderVerdictMd,
  toVerdictReceipt,
  type VerdictIo,
  verdictPath,
  verdictSignature,
  writeVerdict,
} from "./verdict";
import type { VerifierResult } from "./verifier";

const readVerdictTmps: string[] = [];
afterAll(async () => {
  await Promise.all(
    readVerdictTmps.map((d) => rm(d, { recursive: true, force: true }).catch(() => {})),
  );
});

describe("verifier-verdict — readVerdict (the inverse of renderVerdictMd, for F5 approve)", () => {
  const receipt: VerdictReceipt = {
    verdict: "pass",
    attempt: 3,
    base: "abc1234",
    diffstat: { files: 2, plus: 9, minus: 1 },
    tampering: [],
    checks: [{ name: "tests", ok: true, exit: 0, duration_s: 1.2, log: "checks/tests.log" }],
    judge: { score: 1, model: "claude" },
  };

  async function runDirWith(content: string | null): Promise<string> {
    const dir = await mkdtemp(join(tmpdir(), "maestro-readverdict-"));
    readVerdictTmps.push(dir);
    if (content !== null) await writeFile(verdictPath(dir), content, "utf8");
    return dir;
  }

  test("round-trips a rendered receipt back into the wire shape", async () => {
    const dir = await runDirWith(renderVerdictMd(receipt, "looks good"));
    expect(await readVerdict(dir)).toEqual(receipt);
  });

  test("absent verdict.md (the run never verified) → null, not a throw", async () => {
    expect(await readVerdict(await runDirWith(null))).toBeNull();
  });

  test("no frontmatter fence → null (approve must refuse, never merge a phantom base)", async () => {
    expect(await readVerdict(await runDirWith("just a plain body, no fences\n"))).toBeNull();
  });

  test("frontmatter missing a load-bearing field (base) → null", async () => {
    expect(await readVerdict(await runDirWith("---\nverdict: pass\nattempt: 1\n---\n"))).toBeNull();
  });

  test("malformed YAML frontmatter → null (not a throw)", async () => {
    expect(await readVerdict(await runDirWith("---\nverdict: : : bad\n---\n"))).toBeNull();
  });
});

// ── fixtures ─────────────────────────────────────────────────────────────────────────────────────

/** A clean Stage-0-through-Stage-2 pass. */
function passResult(over: Partial<VerifierResult> = {}): VerifierResult {
  return {
    verdict: "pass",
    tampering: [],
    diffstat: { files: 4, plus: 122, minus: 8 },
    base: "3f1c9e0",
    checks: [
      { name: "tests", ok: true, exit: 0, duration_s: 41, log: "checks/tests.log", required: true },
      { name: "lint", ok: true, exit: 0, duration_s: 6, log: "checks/lint.log", required: true },
    ],
    judge: { score: 0.85, model: "claude-opus-4-8", detail: "judge.json" },
    ...over,
  };
}

/** A Stage-1 check failure (one required check red). */
function checkFailResult(over: Partial<VerifierResult> = {}): VerifierResult {
  return {
    verdict: "fail",
    tampering: [],
    diffstat: { files: 3, plus: 40, minus: 2 },
    base: "3f1c9e0",
    checks: [
      {
        name: "tests",
        ok: false,
        exit: 1,
        duration_s: 41,
        log: "checks/tests.log",
        required: true,
        reason: "fail",
      },
      { name: "lint", ok: true, exit: 0, duration_s: 6, log: "checks/lint.log", required: true },
      {
        name: "types",
        ok: false,
        exit: 2,
        duration_s: 9,
        log: "checks/types.log",
        required: false,
        reason: "fail",
      },
    ],
    ...over,
  };
}

/** An in-memory {@link VerdictIo} for deterministic, filesystem-free tests. `read` returns null for a
 *  missing file (the seam's not-found contract), never throws. */
function memIo(seed: Record<string, string> = {}): VerdictIo & { files: Map<string, string> } {
  const files = new Map<string, string>(Object.entries(seed));
  return {
    files,
    read: async (path) => files.get(path) ?? null,
    write: async (path, data) => {
      files.set(path, data);
    },
    mkdirp: async () => {},
  };
}

// ── toVerdictReceipt ───────────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — toVerdictReceipt", () => {
  test("projects a pass result into the wire receipt, dropping runtime-only fields", () => {
    const r = toVerdictReceipt(passResult(), 2);
    expect(r.verdict).toBe("pass");
    expect(r.attempt).toBe(2);
    expect(r.base).toBe("3f1c9e0");
    expect(r.diffstat).toEqual({ files: 4, plus: 122, minus: 8 });
    expect(r.tampering).toEqual([]);
    // `required`/`reason` are internal to CheckResult — the receipt row is exactly {name,ok,exit,duration_s,log}.
    expect(r.checks[0]).toEqual({
      name: "tests",
      ok: true,
      exit: 0,
      duration_s: 41,
      log: "checks/tests.log",
    });
    expect(Object.keys(r.checks[0] as object)).not.toContain("required");
    expect(r.judge).toEqual({ score: 0.85, model: "claude-opus-4-8", detail: "judge.json" });
  });

  test("absent judge projects to { score: null }", () => {
    const r = toVerdictReceipt(checkFailResult(), 1);
    expect(r.judge).toEqual({ score: null });
  });

  test("absent checks projects to an empty array", () => {
    const r = toVerdictReceipt(
      { verdict: "error", tampering: [], diffstat: { files: 0, plus: 0, minus: 0 }, base: "abc" },
      1,
    );
    expect(r.checks).toEqual([]);
  });
});

// ── renderVerdictBody ──────────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — renderVerdictBody", () => {
  test("pass body summarizes checks + judge + the review hand-off", () => {
    const body = renderVerdictBody(passResult());
    expect(body).toContain("2 of 2 checks passed.");
    expect(body).toContain("Judge scored 0.85.");
    expect(body).toContain("Ready for you to review.");
  });

  test("check-fail body counts passed vs total and names each failing check + evidence", () => {
    const body = renderVerdictBody(checkFailResult());
    expect(body).toContain("1 of 3 checks passed.");
    expect(body).toContain("`tests` failed (exit 1). See checks/tests.log.");
    expect(body).toContain("`types` failed (exit 2) (advisory). See checks/types.log.");
    expect(body).not.toContain("Ready for you to review.");
  });

  test("timeout reads as 'timed out', not an exit code", () => {
    const body = renderVerdictBody(
      checkFailResult({
        checks: [
          {
            name: "tests",
            ok: false,
            exit: 137,
            duration_s: 600,
            log: "checks/tests.log",
            required: true,
            reason: "timeout",
          },
        ],
      }),
    );
    expect(body).toContain("`tests` timed out. See checks/tests.log.");
  });

  test("tamper body lists the protected paths", () => {
    const body = renderVerdictBody(
      checkFailResult({ reason: "tampering", tampering: ["src/x.test.ts", "package.json"] }),
    );
    expect(body).toContain("Failed the tamper guard.");
    expect(body).toContain("src/x.test.ts, package.json");
  });

  test("diff-too-large body states the size and says scope, not retry", () => {
    const body = renderVerdictBody(
      checkFailResult({ reason: "diff_too_large", diffstat: { files: 44, plus: 3000, minus: 10 } }),
    );
    expect(body).toContain("too large (44 files, +3000/-10)");
    expect(body).toContain("Scope it down");
  });

  test("error body says parked, no attempt spent", () => {
    const body = renderVerdictBody({
      verdict: "error",
      tampering: [],
      diffstat: { files: 0, plus: 0, minus: 0 },
      base: "abc",
      message: "git exited 128",
    });
    expect(body).toContain("Verification could not run: git exited 128.");
    expect(body).toContain("no attempt was spent");
  });

  test("body carries NO em dash (CLAUDE.md voice rule)", () => {
    for (const r of [
      passResult(),
      checkFailResult(),
      checkFailResult({ reason: "tampering", tampering: ["a.test.ts"] }),
      checkFailResult({ reason: "diff_too_large" }),
      {
        verdict: "error",
        tampering: [],
        diffstat: { files: 0, plus: 0, minus: 0 },
        base: "x",
      } as VerifierResult,
    ]) {
      expect(renderVerdictBody(r)).not.toContain("—");
    }
  });
});

// ── renderVerdictMd ────────────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — renderVerdictMd", () => {
  test("emits --- fenced frontmatter + body, and the frontmatter round-trips to the receipt", () => {
    const receipt = toVerdictReceipt(checkFailResult(), 2);
    const md = renderVerdictMd(receipt, renderVerdictBody(checkFailResult()));
    expect(md.startsWith("---\n")).toBe(true);
    const fmText = md.slice(4, md.indexOf("\n---\n", 4));
    const parsed = parseYaml(fmText) as Record<string, unknown>;
    expect(parsed.verdict).toBe("fail");
    expect(parsed.attempt).toBe(2);
    expect(parsed.base).toBe("3f1c9e0");
    expect(parsed.diffstat).toEqual({ files: 3, plus: 40, minus: 2 });
    expect(parsed.judge).toEqual({ score: null });
    expect((parsed.checks as unknown[]).length).toBe(3);
    // body present after the closing fence
    expect(md).toContain("1 of 3 checks passed.");
  });

  test("frontmatter keys are in VERIFIER §4 order", () => {
    const md = renderVerdictMd(toVerdictReceipt(passResult(), 1), "");
    const fmText = md.slice(4, md.indexOf("\n---\n", 4));
    const keyOrder = fmText
      .split("\n")
      .map((l) => l.match(/^([a-z_]+):/)?.[1])
      .filter((k): k is string => k !== undefined && k !== null);
    expect(keyOrder).toEqual([
      "verdict",
      "attempt",
      "base",
      "diffstat",
      "tampering",
      "checks",
      "judge",
    ]);
  });

  test("an empty body renders a fenced frontmatter with no trailing body", () => {
    const md = renderVerdictMd(toVerdictReceipt(passResult(), 1), "   ");
    expect(md.endsWith("---\n")).toBe(true);
  });
});

// ── renderVerdictFeedback ──────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — renderVerdictFeedback", () => {
  test("a check fail appends one checkbox per failing check, naming the evidence log", () => {
    const block = renderVerdictFeedback(checkFailResult(), 2, "2026-07-12T06:14Z");
    expect(block.startsWith("## Verifier — attempt 2 failed (2026-07-12T06:14Z)\n")).toBe(true);
    expect(block).toContain("- [ ] tests: failed (exit 1) (see checks/tests.log)");
    expect(block).toContain("- [ ] types: failed (exit 2) (see checks/types.log)");
    // lint passed → no item for it
    expect(block).not.toContain("lint");
  });

  test("the judge item appears only when NO required check failed (the judge is the gate)", () => {
    // required check failed → judge is not the gating reason → no judge item
    const withRequiredFail = renderVerdictFeedback(
      checkFailResult({ judge: { score: 0.4, detail: "judge.json" } }),
      1,
      "t",
    );
    expect(withRequiredFail).not.toContain("judge:");

    // all required checks pass but verdict fail → the judge gated → judge item present
    const judgeGated = renderVerdictFeedback(
      {
        verdict: "fail",
        tampering: [],
        diffstat: { files: 1, plus: 1, minus: 0 },
        base: "b",
        checks: [
          {
            name: "tests",
            ok: true,
            exit: 0,
            duration_s: 1,
            log: "checks/tests.log",
            required: true,
          },
        ],
        judge: { score: 0.4, detail: "judge.json" },
      },
      3,
      "t",
    );
    expect(judgeGated).toContain(
      "- [ ] judge: scored 0.4 below the rubric threshold (see judge.json)",
    );
  });

  test("a tamper fail lists one item per protected path", () => {
    const block = renderVerdictFeedback(
      checkFailResult({ reason: "tampering", tampering: ["a.test.ts", "package.json"] }),
      1,
      "t",
    );
    expect(block).toContain("- [ ] tamper: a.test.ts is protected");
    expect(block).toContain("- [ ] tamper: package.json is protected");
  });

  test("a diff-too-large fail says split, not retry", () => {
    const block = renderVerdictFeedback(
      checkFailResult({ reason: "diff_too_large", diffstat: { files: 44, plus: 9, minus: 9 } }),
      1,
      "t",
    );
    expect(block).toContain("- [ ] scope: the diff is too large (44 files, +9/-9)");
    expect(block).toContain("split the work, do not retry");
  });

  test("a pass or error yields NO feedback block (errors are not feedback)", () => {
    expect(renderVerdictFeedback(passResult(), 1, "t")).toBe("");
    expect(
      renderVerdictFeedback(
        { verdict: "error", tampering: [], diffstat: { files: 0, plus: 0, minus: 0 }, base: "b" },
        1,
        "t",
      ),
    ).toBe("");
  });

  test("a fail with no enumerable item still records the attempt", () => {
    const block = renderVerdictFeedback(
      {
        verdict: "fail",
        tampering: [],
        diffstat: { files: 0, plus: 0, minus: 0 },
        base: "b",
        checks: [],
      },
      4,
      "t",
    );
    expect(block).toContain("## Verifier — attempt 4 failed (t)");
    expect(block).toContain("- [ ] verification failed (see verdict.md)");
  });
});

// ── appendVerdictFeedback ──────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — appendVerdictFeedback", () => {
  test("seeds a fix_plan header when none exists, then appends the block", async () => {
    const io = memIo();
    const ok = await appendVerdictFeedback(
      "/run",
      "## Verifier — attempt 1 failed (t)\n- [ ] x\n",
      io,
    );
    expect(ok).toBe(true);
    const written = io.files.get("/run/fix_plan.md") ?? "";
    expect(written).toContain("# Fix plan");
    expect(written).toContain("## Verifier — attempt 1 failed (t)");
    expect(written).toContain("- [ ] x");
    // exactly one blank-line separator between the seed header and the first block (no double blank)
    expect(written.startsWith("# Fix plan\n\n## Verifier")).toBe(true);
  });

  test("APPENDS to an existing fix_plan without rewriting prior content (VERIFIER §5 history)", async () => {
    const prior =
      "# Fix plan\n\n- [x] original task\n\n## Verifier — attempt 1 failed (t1)\n- [ ] a\n";
    const io = memIo({ "/run/fix_plan.md": prior });
    await appendVerdictFeedback("/run", "## Verifier — attempt 2 failed (t2)\n- [ ] b\n", io);
    const written = io.files.get("/run/fix_plan.md") ?? "";
    // prior content preserved verbatim
    expect(written).toContain("- [x] original task");
    expect(written).toContain("## Verifier — attempt 1 failed (t1)");
    // new block appended AFTER it
    expect(written.indexOf("attempt 2")).toBeGreaterThan(written.indexOf("attempt 1"));
    expect(written).toContain("- [ ] b");
  });

  test("an empty block is a no-op (a non-fail verdict leaves the file untouched)", async () => {
    const io = memIo({ "/run/fix_plan.md": "# Fix plan\n" });
    const ok = await appendVerdictFeedback("/run", "", io);
    expect(ok).toBe(false);
    expect(io.files.get("/run/fix_plan.md")).toBe("# Fix plan\n");
  });

  test("a REAL read fault propagates and does NOT clobber the existing file (history-loss guard)", async () => {
    // CodeRabbit R1: a transient read failure (permission / disk I/O) on an EXISTING file must reject,
    // not be swallowed as "absent" and then overwrite prior append-only history with a fresh header.
    const prior = "# Fix plan\n\n## Verifier — attempt 1 failed (t1)\n- [ ] a\n";
    const files = new Map<string, string>([["/run/fix_plan.md", prior]]);
    let wrote = false;
    const io: VerdictIo = {
      read: async () => {
        throw Object.assign(new Error("EACCES: permission denied"), { code: "EACCES" });
      },
      write: async () => {
        wrote = true;
      },
      mkdirp: async () => {},
    };
    await expect(
      appendVerdictFeedback("/run", "## Verifier — attempt 2 failed (t2)\n- [ ] b\n", io),
    ).rejects.toThrow(/EACCES/);
    expect(wrote).toBe(false); // never wrote → prior history intact
    expect(files.get("/run/fix_plan.md")).toBe(prior);
  });

  test("a genuine not-found (read → null) still seeds a fresh fix_plan", async () => {
    const io = memIo(); // read returns null for the missing file
    const ok = await appendVerdictFeedback(
      "/run",
      "## Verifier — attempt 1 failed (t)\n- [ ] x\n",
      io,
    );
    expect(ok).toBe(true);
    expect(io.files.get("/run/fix_plan.md")).toContain("# Fix plan");
  });
});

// ── writeVerdict ───────────────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — writeVerdict", () => {
  test("writes verdict.md into the run dir and returns the receipt verbatim", async () => {
    const io = memIo();
    const receipt = toVerdictReceipt(passResult(), 1);
    const returned = await writeVerdict("/run", receipt, "body text", io);
    expect(returned).toBe(receipt);
    const md = io.files.get(verdictPath("/run"));
    expect(md).toBeDefined();
    expect(md).toContain("verdict: pass");
    expect(md).toContain("body text");
  });
});

// ── decideVerdictOutcome ───────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — decideVerdictOutcome", () => {
  const base = { attempt: 1, maxAttempts: 5, iterations: 3, maxIterations: 30, signature: "sig-A" };

  test("error parks blocked and does NOT burn an attempt (broken harness is not the agent's fault)", () => {
    const out = decideVerdictOutcome({ ...base, verdict: "error" });
    expect(out).toEqual({ action: "park_blocked", reason: "verify_error", burnsAttempt: false });
  });

  test("pass goes to the gate (park review)", () => {
    const out = decideVerdictOutcome({ ...base, verdict: "pass" });
    expect(out).toEqual({ action: "park_review", burnsAttempt: true });
  });

  test("fail under all caps respawns and burns an attempt", () => {
    const out = decideVerdictOutcome({ ...base, verdict: "fail" });
    expect(out).toEqual({ action: "respawn", burnsAttempt: true });
  });

  test("fail with an IDENTICAL prior verdict parks no_progress", () => {
    const out = decideVerdictOutcome({
      ...base,
      verdict: "fail",
      signature: "sig-A",
      priorSignature: "sig-A",
    });
    expect(out).toEqual({ action: "park_blocked", reason: "no_progress", burnsAttempt: true });
  });

  test("fail with a DIFFERENT prior verdict still respawns (progress was made)", () => {
    const out = decideVerdictOutcome({
      ...base,
      verdict: "fail",
      signature: "sig-B",
      priorSignature: "sig-A",
    });
    expect(out.action).toBe("respawn");
  });

  test("fail at the consecutive-fail cap parks verifier_exhausted", () => {
    const out = decideVerdictOutcome({
      ...base,
      verdict: "fail",
      attempt: 5,
      priorSignature: "sig-Z",
    });
    expect(out).toEqual({
      action: "park_blocked",
      reason: "verifier_exhausted",
      burnsAttempt: true,
    });
  });

  test("fail at the iteration cap parks iteration_cap", () => {
    const out = decideVerdictOutcome({
      ...base,
      verdict: "fail",
      iterations: 30,
      priorSignature: "sig-Z",
    });
    expect(out).toEqual({ action: "park_blocked", reason: "iteration_cap", burnsAttempt: true });
  });

  test("precedence when several fire: no_progress > verifier_exhausted > iteration_cap", () => {
    // identical verdict AND at the attempt cap AND over the iteration cap → no_progress wins
    const allThree = decideVerdictOutcome({
      verdict: "fail",
      attempt: 5,
      maxAttempts: 5,
      iterations: 40,
      maxIterations: 30,
      signature: "s",
      priorSignature: "s",
    });
    expect(allThree.action === "park_blocked" && allThree.reason).toBe("no_progress");

    // at the attempt cap AND over the iteration cap, distinct verdict → verifier_exhausted wins
    const twoLeft = decideVerdictOutcome({
      verdict: "fail",
      attempt: 5,
      maxAttempts: 5,
      iterations: 40,
      maxIterations: 30,
      signature: "s2",
      priorSignature: "s1",
    });
    expect(twoLeft.action === "park_blocked" && twoLeft.reason).toBe("verifier_exhausted");
  });
});

// ── verdictSignature ───────────────────────────────────────────────────────────────────────────────

describe("verifier-verdict — verdictSignature", () => {
  test("identical failing sets produce identical signatures (the no-progress key)", () => {
    expect(verdictSignature(checkFailResult())).toBe(verdictSignature(checkFailResult()));
  });

  test("a different failing check changes the signature", () => {
    const a = verdictSignature(checkFailResult());
    const b = verdictSignature(
      checkFailResult({
        checks: [
          {
            name: "lint",
            ok: false,
            exit: 1,
            duration_s: 6,
            log: "checks/lint.log",
            required: true,
            reason: "fail",
          },
        ],
      }),
    );
    expect(a).not.toBe(b);
  });

  test("check ORDER does not change the signature (order-independent)", () => {
    const forward = checkFailResult();
    const reversed = checkFailResult({ checks: [...(checkFailResult().checks ?? [])].reverse() });
    expect(verdictSignature(forward)).toBe(verdictSignature(reversed));
  });

  test("a changed judge score changes the signature", () => {
    const a = verdictSignature(checkFailResult({ judge: { score: 0.4 } }));
    const b = verdictSignature(checkFailResult({ judge: { score: 0.7 } }));
    expect(a).not.toBe(b);
  });

  test("a SHRINKING diff_too_large is NOT identical (progress detected, no false no_progress)", () => {
    // CodeRabbit R1: two over-cap diffs must not collide just because both are diff_too_large — a run that
    // shrank 200 files → 50 (still over cap) made progress and must not park no_progress.
    const big = verdictSignature(
      checkFailResult({
        reason: "diff_too_large",
        diffstat: { files: 200, plus: 9000, minus: 10 },
      }),
    );
    const smaller = verdictSignature(
      checkFailResult({ reason: "diff_too_large", diffstat: { files: 50, plus: 2000, minus: 10 } }),
    );
    expect(big).not.toBe(smaller);
    // but an unchanged over-cap diff IS identical (genuine no-progress)
    const same = verdictSignature(
      checkFailResult({
        reason: "diff_too_large",
        diffstat: { files: 200, plus: 9000, minus: 10 },
      }),
    );
    expect(big).toBe(same);
  });
});
