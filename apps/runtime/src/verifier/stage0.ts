// Verifier Stage 0 — the tamper & diff guard (VERIFIER §2 Stage 0). Cheap, NO model, runs FIRST.
//
// The one rule Loop 2 serves: the writer never grades its own homework. Stage 0 is the anti-reward-
// hacking floor — before any check or judge runs, it asks two questions of the run's net diff:
//   1. Did the run touch a PROTECTED path? (its own tests, rubric, _work.md, the check's deps) → the
//      run gamed its success function. verdict FAIL, reason `tampering`, the offending paths as evidence.
//   2. Is the diff over the contract's size limits? verdict FAIL, reason `diff_too_large` — the fix is
//      scoping, not retrying.
// A tamper/oversize verdict here is TERMINAL: `runVerifier` (verifier.ts) never runs a later stage on a
// non-pass Stage 0, so nothing downstream can flip it back to pass. That short-circuit is the invariant.
//
// Stage 0 needs no model and no separate process — it is pure git + glob, run runtime-side after the
// writer/child is already dead (so writer≠judge holds). The separate-PROCESS verifier boundary matters
// for JUDGED work (Stage 2, a model call) and lands with BRO-1786 via the existing `--role verifier` seam.

import { DEFAULT_DIFF_MAX_FILES, DEFAULT_DIFF_MAX_LINES, type Verdict } from "@maestro/protocol";
import { type GitResult, git as realGit } from "../git/git";

/** Files/lines changed by the run's net diff (VERIFIER §4 receipt shape). `plus`/`minus` are summed
 *  added/deleted lines; a binary file contributes to `files` but 0 to `plus`/`minus`. */
export interface DiffStat {
  files: number;
  plus: number;
  minus: number;
}

/** The diff size ceiling a `diff_too_large` verdict was measured against (evidence for the feedback). */
export interface DiffLimit {
  maxFiles: number;
  maxLines: number;
}

/**
 * Stage 0's outcome, aligned with the protocol {@link Verdict} enum:
 * - `pass`  — no protected path touched, diff within limits.
 * - `fail` + `tampering`      — one or more protected paths in the diff (`tampering` lists them).
 * - `fail` + `diff_too_large` — file or line count over `limit`.
 * - `error` — the guard itself could not run (a bad ref, git missing): an INFRA problem, never the
 *   agent's fault. The run parks blocked; an attempt is never burned on a broken harness (VERIFIER §2).
 */
export type Stage0Verdict =
  | { verdict: Extract<Verdict, "pass">; diffstat: DiffStat }
  | {
      verdict: Extract<Verdict, "fail">;
      reason: "tampering";
      tampering: string[];
      diffstat: DiffStat;
    }
  | {
      verdict: Extract<Verdict, "fail">;
      reason: "diff_too_large";
      diffstat: DiffStat;
      limit: DiffLimit;
    }
  | { verdict: Extract<Verdict, "error">; message: string };

/** The git runner Stage 0 calls — the real {@link realGit} by default, a fixture in tests. */
export type GitRunner = (cwd: string, args: string[]) => Promise<GitResult>;

/** Does `glob` match repo-relative `path`? Default {@link defaultGlobMatch} (Bun.Glob); injected in tests
 *  so the match semantics are exercisable without a Bun runtime. */
export type GlobMatch = (glob: string, path: string) => boolean;

export interface Stage0Input {
  /** The workspace git repo root (where `run/<id>` lives). */
  cwd: string;
  /** The commit the run branched from — the left side of the diff (VERIFIER §4 `base`). */
  base: string;
  /** The run branch / tip ref — the right side of the diff (`run/<id>`). */
  branch: string;
  /** The effective protect globs — `effectiveProtect(done)`, the floor ∪ author additions. */
  protect: readonly string[];
  /** Max changed files before `diff_too_large` (default {@link DEFAULT_DIFF_MAX_FILES}). */
  maxFiles?: number;
  /** Max changed lines (added+deleted) before `diff_too_large` (default {@link DEFAULT_DIFF_MAX_LINES}). */
  maxLines?: number;
  /** Injectable git runner (default the real one). */
  git?: GitRunner;
  /** Injectable glob matcher (default Bun.Glob). */
  match?: GlobMatch;
}

/** One parsed `git diff --numstat` row. */
interface NumstatRow {
  added: number;
  deleted: number;
  path: string;
}

/**
 * Parse `git diff --numstat --no-renames` output. Each line is `<added>\t<deleted>\t<path>`; a binary
 * file shows `-\t-\t<path>` (counted as a changed file with 0 lines). `--no-renames` guarantees one path
 * per line (a rename becomes delete(old)+add(new) — SAFER for a tamper guard: a protected file renamed
 * away is still caught as a touched protected path, and no `{old => new}` token can slip past a glob).
 * The path is everything after the second tab, so a path containing spaces is preserved intact.
 */
export function parseNumstat(stdout: string): NumstatRow[] {
  const rows: NumstatRow[] = [];
  for (const line of stdout.split("\n")) {
    if (line === "") continue;
    const firstTab = line.indexOf("\t");
    const secondTab = line.indexOf("\t", firstTab + 1);
    if (firstTab === -1 || secondTab === -1) continue; // not a numstat row — skip defensively
    const addedStr = line.slice(0, firstTab);
    const deletedStr = line.slice(firstTab + 1, secondTab);
    const path = line.slice(secondTab + 1);
    if (path === "") continue;
    rows.push({
      added: addedStr === "-" ? 0 : Number.parseInt(addedStr, 10) || 0,
      deleted: deletedStr === "-" ? 0 : Number.parseInt(deletedStr, 10) || 0,
      path,
    });
  }
  return rows;
}

/** Default glob matcher — Bun.Glob. `**\/*.test.*` etc. must match at the repo ROOT and any depth, so
 *  every floor glob is tried BOTH as written and, when it starts with `**​/`, against the bare basename
 *  pattern too (Bun.Glob's `**\/x` does not always match a top-level `x`). A pattern with no leading
 *  `**​/` (e.g. `package.json`, `.github/**`) is matched literally, as authored. */
export function defaultGlobMatch(glob: string, path: string): boolean {
  const g = new Bun.Glob(glob);
  if (g.match(path)) return true;
  // `**/foo` should also catch a top-level `foo` — retry against the tail after the leading `**/`.
  if (glob.startsWith("**/")) {
    return new Bun.Glob(glob.slice(3)).match(path);
  }
  return false;
}

/**
 * Run Stage 0 against the run's net diff (`git diff --numstat --no-renames <base> <branch>`, which for
 * `diff` equals the spec's `<base>..run/<id>`). Tampering is checked BEFORE size: a run that both edits a
 * protected path AND blows the size limit is reported as `tampering` (the more serious, security-class
 * failure), with the protected paths as evidence. `core.quotePath=false` keeps non-ASCII paths literal so
 * the glob match is not defeated by git's octal escaping.
 */
export async function runStage0(input: Stage0Input): Promise<Stage0Verdict> {
  const {
    cwd,
    base,
    branch,
    protect,
    maxFiles = DEFAULT_DIFF_MAX_FILES,
    maxLines = DEFAULT_DIFF_MAX_LINES,
    git = realGit,
    match = defaultGlobMatch,
  } = input;

  let result: GitResult;
  try {
    result = await git(cwd, [
      "-c",
      "core.quotePath=false",
      "diff",
      "--numstat",
      "--no-renames",
      base,
      branch,
    ]);
  } catch (err) {
    return {
      verdict: "error",
      message: `git diff failed: ${String((err as Error)?.message ?? err)}`,
    };
  }
  if (result.code !== 0) {
    return {
      verdict: "error",
      message: `git diff exited ${result.code}: ${result.stderr.trim() || "(no stderr)"}`,
    };
  }

  const rows = parseNumstat(result.stdout);
  const diffstat: DiffStat = {
    files: rows.length,
    plus: rows.reduce((n, r) => n + r.added, 0),
    minus: rows.reduce((n, r) => n + r.deleted, 0),
  };

  // 1. Tamper guard — any changed path matching any protect glob (checked first: the graver failure).
  const tampering = rows
    .map((r) => r.path)
    .filter((path) => protect.some((glob) => match(glob, path)));
  if (tampering.length > 0) {
    return { verdict: "fail", reason: "tampering", tampering, diffstat };
  }

  // 2. Size guard — files or total churn (added+deleted) over the contract limits.
  if (diffstat.files > maxFiles || diffstat.plus + diffstat.minus > maxLines) {
    return { verdict: "fail", reason: "diff_too_large", diffstat, limit: { maxFiles, maxLines } };
  }

  return { verdict: "pass", diffstat };
}
