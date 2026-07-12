/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  DEFAULT_DIFF_MAX_FILES,
  DEFAULT_DIFF_MAX_LINES,
  DEFAULT_PROTECT_GLOBS,
  effectiveProtect,
} from "@maestro/protocol";
import type { GitRunner } from "./stage0";
import { defaultGlobMatch, parseNumstat, runStage0 } from "./stage0";

/** A numstat fixture line — `[added, deleted, path]`; a number or "-" (binary). */
type Row = [number | "-", number | "-", string];
const numstat = (rows: Row[]): string => rows.map(([a, d, p]) => `${a}\t${d}\t${p}`).join("\n");

/** An injected git runner returning a scripted numstat blob (exit 0 by default). */
const fakeGit =
  (stdout: string, code = 0, stderr = ""): GitRunner =>
  async () => ({ code, stdout, stderr });

const BASE = "3f1c9e0";
const BRANCH = "run/abcd1234";

describe("verifier-stage0 — parseNumstat", () => {
  test("parses added/deleted/path rows", () => {
    expect(parseNumstat(numstat([[12, 3, "src/a.ts"]]))).toEqual([
      { added: 12, deleted: 3, path: "src/a.ts" },
    ]);
  });
  test("a binary file is 0 lines but still a changed file", () => {
    expect(parseNumstat(numstat([["-", "-", "logo.png"]]))).toEqual([
      { added: 0, deleted: 0, path: "logo.png" },
    ]);
  });
  test("a path containing spaces is preserved intact", () => {
    expect(parseNumstat("1\t0\tsrc/my file.ts")).toEqual([
      { added: 1, deleted: 0, path: "src/my file.ts" },
    ]);
  });
  test("blank lines and malformed rows are skipped", () => {
    expect(parseNumstat("\n5\t2\tok.ts\ngarbage-no-tabs\n")).toEqual([
      { added: 5, deleted: 2, path: "ok.ts" },
    ]);
  });
});

describe("verifier-stage0 — defaultGlobMatch (the protect floor matches at root AND any depth)", () => {
  test("**/*.test.* matches a test file at the repo root and nested", () => {
    expect(defaultGlobMatch("**/*.test.*", "head.test.tsx")).toBe(true);
    expect(defaultGlobMatch("**/*.test.*", "src/a/head.test.tsx")).toBe(true);
  });
  test("**/rubric.md and **/_work.md match at root and nested", () => {
    expect(defaultGlobMatch("**/rubric.md", "rubric.md")).toBe(true);
    expect(defaultGlobMatch("**/rubric.md", "checks/rubric.md")).toBe(true);
    expect(defaultGlobMatch("**/_work.md", "_work.md")).toBe(true);
    expect(defaultGlobMatch("**/_work.md", "q/task/_work.md")).toBe(true);
  });
  test("a directory glob (.github/**) matches contents, a literal (package.json) matches only itself", () => {
    expect(defaultGlobMatch(".github/**", ".github/workflows/ci.yml")).toBe(true);
    expect(defaultGlobMatch("package.json", "package.json")).toBe(true);
    expect(defaultGlobMatch("package.json", "sub/package.json")).toBe(false);
  });
  test("a non-protected source file matches none of the floor globs", () => {
    for (const g of DEFAULT_PROTECT_GLOBS) {
      expect(defaultGlobMatch(g, "src/server.ts")).toBe(false);
    }
  });
  test("a leading-! protect entry is NEUTRALIZED — it never inverts the guard (matched literally)", () => {
    // Bun.Glob reads a leading `!` as negation; a positive protect entry must never mean "everything
    // except X". A `!`-prefixed entry matches only the file literally NAMED that, so a clean file is
    // never falsely flagged, and the guard can never be inverted by author input.
    expect(defaultGlobMatch("!secrets.env", "src/server.ts")).toBe(false);
    expect(defaultGlobMatch("!secrets.env", "README.md")).toBe(false);
    expect(defaultGlobMatch("!secrets.env", "!secrets.env")).toBe(true); // the literal file
    expect(defaultGlobMatch("!secrets.env", "secrets.env")).toBe(false); // NOT gitignore re-include
    // ...even when the negation is the tail of a **/ pattern (the .slice(3) retry must escape too).
    expect(defaultGlobMatch("**/!keep.env", "src/server.ts")).toBe(false);
    expect(defaultGlobMatch("**/!keep.env", "!keep.env")).toBe(true);
  });
});

describe("verifier-stage0 — runStage0 (tamper + diff guard)", () => {
  const protect = effectiveProtect(undefined); // the floor

  test("a clean, in-bounds diff passes", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      git: fakeGit(
        numstat([
          [10, 2, "src/server.ts"],
          [4, 0, "README.md"],
        ]),
      ),
    });
    expect(v).toEqual({ verdict: "pass", diffstat: { files: 2, plus: 14, minus: 2 } });
  });

  test("touching a floor-protected test file fails with reason tampering + the path as evidence", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      git: fakeGit(
        numstat([
          [3, 1, "src/server.ts"],
          [8, 0, "src/head.test.tsx"],
        ]),
      ),
    });
    expect(v.verdict).toBe("fail");
    if (v.verdict !== "fail" || v.reason !== "tampering") throw new Error("expected tampering");
    expect(v.tampering).toEqual(["src/head.test.tsx"]);
    expect(v.diffstat).toEqual({ files: 2, plus: 11, minus: 1 });
  });

  test("an author-added protect path (package.json) is enforced too", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect: effectiveProtect({ check: "bun test", protect: ["package.json"] }),
      git: fakeGit(numstat([[2, 2, "package.json"]])),
    });
    expect(v.verdict).toBe("fail");
    if (v.verdict !== "fail" || v.reason !== "tampering") throw new Error("expected tampering");
    expect(v.tampering).toEqual(["package.json"]);
  });

  test("a !-prefixed author protect entry does NOT invert the guard — a clean diff still passes", async () => {
    // The footgun: `!secrets.env` under raw Bun.Glob matched every path but that one, flagging a clean
    // run as tampering. Neutralized — a clean, in-bounds diff passes despite the malformed protect entry.
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect: effectiveProtect({ check: "bun test", protect: ["!secrets.env"] }),
      git: fakeGit(
        numstat([
          [10, 2, "src/server.ts"],
          [4, 0, "README.md"],
        ]),
      ),
    });
    expect(v).toEqual({ verdict: "pass", diffstat: { files: 2, plus: 14, minus: 2 } });
  });

  test("over the file limit → diff_too_large with the limit as evidence", async () => {
    const rows: Row[] = Array.from({ length: 4 }, (_, i) => [1, 0, `src/f${i}.ts`] as Row);
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      maxFiles: 3,
      maxLines: 1000,
      git: fakeGit(numstat(rows)),
    });
    expect(v.verdict).toBe("fail");
    if (v.verdict !== "fail" || v.reason !== "diff_too_large")
      throw new Error("expected diff_too_large");
    expect(v.diffstat.files).toBe(4);
    expect(v.limit).toEqual({ maxFiles: 3, maxLines: 1000 });
  });

  test("over the line limit (added+deleted) → diff_too_large", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      maxFiles: 100,
      maxLines: 10,
      git: fakeGit(numstat([[6, 6, "src/big.ts"]])),
    });
    expect(v.verdict).toBe("fail");
    if (v.verdict !== "fail" || v.reason !== "diff_too_large")
      throw new Error("expected diff_too_large");
    expect(v.diffstat).toEqual({ files: 1, plus: 6, minus: 6 });
  });

  test("tampering wins over an oversize diff (the graver, security-class failure)", async () => {
    const rows: Row[] = [
      [50, 50, "src/head.test.tsx"], // protected AND large
      [50, 50, "src/a.ts"],
    ];
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      maxFiles: 1,
      maxLines: 10,
      git: fakeGit(numstat(rows)),
    });
    expect(v.verdict).toBe("fail");
    if (v.verdict !== "fail") throw new Error("expected fail");
    expect(v.reason).toBe("tampering");
  });

  test("a binary-only change counts as a file, contributes 0 lines, and passes when in bounds", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      maxFiles: 5,
      maxLines: 5,
      git: fakeGit(numstat([["-", "-", "assets/logo.png"]])),
    });
    expect(v).toEqual({ verdict: "pass", diffstat: { files: 1, plus: 0, minus: 0 } });
  });

  test("an empty diff (no changes) passes", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      git: fakeGit(""),
    });
    expect(v).toEqual({ verdict: "pass", diffstat: { files: 0, plus: 0, minus: 0 } });
  });

  test("a non-zero git exit → verdict error (infra, never the agent's fault)", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: "run/nope",
      protect,
      git: fakeGit("", 128, "fatal: bad revision 'run/nope'"),
    });
    expect(v.verdict).toBe("error");
    if (v.verdict !== "error") throw new Error("expected error");
    expect(v.message).toContain("bad revision");
  });

  test("a thrown git runner → verdict error (guard never rejects)", async () => {
    const v = await runStage0({
      cwd: "/repo",
      base: BASE,
      branch: BRANCH,
      protect,
      git: async () => {
        throw new Error("spawn git ENOENT");
      },
    });
    expect(v.verdict).toBe("error");
    if (v.verdict !== "error") throw new Error("expected error");
    expect(v.message).toContain("ENOENT");
  });

  test("the numeric defaults are the canon diff limits", () => {
    expect(DEFAULT_DIFF_MAX_FILES).toBe(30);
    expect(DEFAULT_DIFF_MAX_LINES).toBe(2000);
  });
});
