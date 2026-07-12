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
 * Parse `git diff --numstat -z --no-renames` output. Records are NUL-terminated (`-z`): each is
 * `<added>\t<deleted>\t<path>\0`; a binary file shows `-\t-\t<path>` (a changed file with 0 lines).
 * `-z` is load-bearing: it emits the path RAW (no C-quoting), so a path containing a `"`, `\`, tab, or
 * newline is matched against the protect globs literally — WITHOUT `-z`, git wraps such paths in
 * `"…"` with C-escapes (and `core.quotePath=false` suppresses only the *non-ASCII* case), which would
 * let a protected file with such a name slip past the glob — a tamper false-negative in the floor.
 * `--no-renames` keeps one path per record (a rename → delete(old)+add(new): a protected file renamed
 * away is still caught, and there is no `-z` two-NUL rename record to special-case). The path is
 * everything after the second tab, so an embedded tab/space/newline in the path is preserved intact.
 */
export function parseNumstat(stdout: string): NumstatRow[] {
  const rows: NumstatRow[] = [];
  for (const record of stdout.split("\0")) {
    if (record === "") continue; // the trailing NUL yields a final empty record — skip it
    const firstTab = record.indexOf("\t");
    const secondTab = record.indexOf("\t", firstTab + 1);
    if (firstTab === -1 || secondTab === -1) continue; // not a numstat record — skip defensively
    const addedStr = record.slice(0, firstTab);
    const deletedStr = record.slice(firstTab + 1, secondTab);
    const path = record.slice(secondTab + 1);
    if (path === "") continue;
    rows.push({
      added: addedStr === "-" ? 0 : Number.parseInt(addedStr, 10) || 0,
      deleted: deletedStr === "-" ? 0 : Number.parseInt(deletedStr, 10) || 0,
      path,
    });
  }
  return rows;
}

/** Build a Bun.Glob from a PROTECT pattern, neutralizing a leading `!`. Bun.Glob reads a leading `!` as
 *  micromatch-style NEGATION — meaningless for a positive protect entry (protect is an OR of inclusion
 *  patterns; a path is protected if it matches ANY) and a footgun: `!secrets.env` would match EVERY path
 *  except `secrets.env`, inverting the guard (clean diffs flagged `tampering`, the named file waved
 *  through). Escaping the `!` makes it match a file literally NAMED `!…`; to protect `secrets.env` an
 *  author writes `secrets.env` / `**​/secrets.env`, not a gitignore-style negation. (`!` is only special
 *  at position 0, so this is applied to the sliced tail below too, where the `!` becomes leading.) */
function protectGlob(pattern: string): Bun.Glob {
  return new Bun.Glob(pattern.startsWith("!") ? `\\${pattern}` : pattern);
}

/** Default glob matcher — Bun.Glob, with negation neutralized ({@link protectGlob}). `**\/*.test.*` etc.
 *  must match at the repo ROOT and any depth, so every floor glob is tried BOTH as written and, when it
 *  starts with `**​/`, against the bare basename pattern too (Bun.Glob's `**\/x` does not always match a
 *  top-level `x`). A pattern with no leading `**​/` (e.g. `package.json`, `.github/**`) is matched as
 *  authored. Extglob syntax (`(a|b)`) is unsupported by Bun.Glob → silently no-matches (an author-intent
 *  miss, never an inversion); protect patterns use `*` / `**` / `?` / `[…]` and plain literals. */
export function defaultGlobMatch(glob: string, path: string): boolean {
  if (protectGlob(glob).match(path)) return true;
  // `**/foo` should also catch a top-level `foo` — retry against the tail after the leading `**/`.
  if (glob.startsWith("**/")) {
    return protectGlob(glob.slice(3)).match(path);
  }
  return false;
}

/**
 * Run Stage 0 against the run's net diff (`git diff --numstat -z --no-renames <base> <branch>`, which for
 * `diff` equals the spec's `<base>..run/<id>`). Tampering is checked BEFORE size: a run that both edits a
 * protected path AND blows the size limit is reported as `tampering` (the more serious, security-class
 * failure), with the protected paths as evidence. `-z` emits every path RAW (NUL-terminated, no C-quoting
 * of `"`/`\`/tab/newline nor non-ASCII octal escaping), so no protected path can be hidden from the glob
 * by git's quoting — the soundness the floor depends on.
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
    // `--no-ext-diff --no-textconv`: --numstat does not itself invoke an external diff driver, but pass
    // them anyway so every diff the runtime runs in an agent worktree is uniformly driver-free (the git
    // env is already scrubbed of secrets — see git.ts key-confinement note — this is defense-in-depth).
    result = await git(cwd, [
      "diff",
      "--no-ext-diff",
      "--no-textconv",
      "--numstat",
      "-z",
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
