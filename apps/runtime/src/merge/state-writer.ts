// Durable node-state writer (BRO-1914) — persist a node's runtime `state`/`owner` advance to its
// `_work.md` so it SURVIVES A RESTART.
//
// The problem it fixes: a node's runtime state is written DB-only (supervisor `park`, intent verdicts).
// But the FS is authoritative — the scanner re-derives `state` and `owner` from `_work.md` on an
// `--rebuild` rescan (its `CONTENT_KEYS`), so a DB-only advance is LOST on rebuild. A run parked at
// `review` ("Needs you") silently reverts to its dispatch-time state after a restart.
//
// Two earlier naive prototypes were REVERTED (BRO-1913); this writer is built to avoid BOTH:
//   1. A bare uncommitted `_work.md` write DIRTIES the tree → the clean-tree-gated `approveMerge`
//      refuses `dirty_workspace` and WEDGES the F5 gate. → We COMMIT the write (path-scoped), so the
//      tree stays clean.
//   2. A non-atomic `writeFile` of a SCANNED file can be read half-written → `syncNodes` sees a torn
//      file and TOMBSTONES the node (it vanishes from the board). → We write atomically (temp+rename),
//      so a concurrent scanner read sees old-full or new-full, never torn, and rename never removes the
//      file.
//
// All of it runs under the SHARED per-workspace git lock ({@link serializeWorkspaceGit}) so it can
// never interleave with an approve's squash+commit on the same `.git/index`. The commit is a no-pre-`add`
// `git commit -- <path>` ({@link gitCommitPaths}), so a commit fault leaves the INDEX untouched — the ONLY
// undo is the worktree write, rolled back to the exact pre-call bytes. A failed persist thus leaves NOTHING
// half-written and the path clean — then reports `failed` (never a false success). The commit is the transaction.

import { randomBytes } from "node:crypto";
import { readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { setWorkFileFields } from "@maestro/protocol";
import { gitCommitPaths as defaultGitCommitPaths } from "../git/git";
import { WORK_FILE } from "../scanner/scanner";
import { serializeWorkspaceGit } from "./serialize";

/** Injectable git seam (tests drive commit failure to exercise the rollback). Defaults to the real helper. */
export interface PersistNodeStateDeps {
  git?: {
    /** Commit the `_work.md` worktree content, no pre-`add` — a failure leaves the index untouched. */
    commit?: typeof defaultGitCommitPaths;
  };
}

export type PersistNodeStateOutcome =
  /** The field(s) changed → atomic write + committed path-scoped; the tree is CLEAN. */
  | { kind: "written" }
  /** The field(s) already held the target value → byte-unchanged, no write, no empty commit (idempotent). */
  | { kind: "unchanged" }
  /** No `_work.md` at `nodePath` (ENOENT) — the node is not FS-backed (deleted / mid-move), so there is
   *  NOTHING to keep consistent: the next reconcile TOMBSTONES it, it never reverts to a stale `review`.
   *  Benign, distinct from `failed` (a real fault the caller should surface). */
  | { kind: "absent" }
  /** Could not patch/write/commit (a real fault) → NOTHING half-written, tree rolled back to clean. */
  | { kind: "failed"; reason: string };

/** Atomic file replace: full content to a unique temp, then `rename` onto `path` (atomic overwrite — a
 *  concurrent reader sees old-full or new-full, never torn; `rename` never leaves the path absent). */
async function atomicWrite(path: string, content: string): Promise<void> {
  const tmp = `${path}.${randomBytes(6).toString("hex")}.tmp`;
  try {
    await writeFile(tmp, content, "utf8");
    await rename(tmp, path);
  } catch (err) {
    await rm(tmp, { force: true }).catch(() => {}); // best-effort temp cleanup on a failed write/rename
    throw err;
  }
}

/** The path-scoped commit subject — an agent-facing receipt, not UI copy. Values are single-lined so a
 *  patched value carrying a newline cannot inject extra commit-body lines. */
function commitMessage(patches: Record<string, string | null>): string {
  const fields = Object.entries(patches)
    .map(([k, v]) => `${k}=${v === null ? "(cleared)" : (v.split("\n")[0] ?? "")}`)
    .join(" ");
  return `orchestrator: node ${fields}`;
}

/**
 * Durably persist a node's frontmatter `state`/`owner` advance to `<nodePath>/_work.md` as a git
 * transaction under the shared workspace lock. Returns `written` (committed, tree clean), `unchanged`
 * (already at the target — no empty commit), or `failed` (rolled back to the exact pre-call bytes,
 * tree clean — never a false success). Idempotent and safe to call best-effort from a hot path.
 *
 * @param cwd       the WORKSPACE ROOT (`RuntimeConfig.workspace`), NOT a run worktree
 * @param nodePath  the node's workspace-relative folder (`""` for the root) — where its `_work.md` lives
 * @param patches   frontmatter fields to set (`{ state: "review" }`); a `null` value clears the key
 */
export async function persistNodeState(
  cwd: string,
  nodePath: string,
  patches: Record<string, string | null>,
  deps: PersistNodeStateDeps = {},
): Promise<PersistNodeStateOutcome> {
  const commit = deps.git?.commit ?? defaultGitCommitPaths;
  const pathspec = nodePath === "" ? WORK_FILE : `${nodePath}/${WORK_FILE}`;
  const filePath = join(cwd, pathspec);

  return serializeWorkspaceGit(cwd, async () => {
    let orig: string;
    try {
      orig = await readFile(filePath, "utf8");
    } catch (err) {
      // A missing file is NOT a fault — the node isn't FS-backed (deleted / mid-move), so there's nothing
      // to persist and the reconcile will tombstone it, not revert it. Any other read error IS a fault.
      if ((err as NodeJS.ErrnoException).code === "ENOENT") return { kind: "absent" };
      return { kind: "failed", reason: `read _work.md: ${(err as Error).message}` };
    }

    let patched: string;
    try {
      patched = setWorkFileFields(orig, patches);
    } catch (err) {
      return { kind: "failed", reason: `patch _work.md: ${(err as Error).message}` };
    }

    if (patched === orig) return { kind: "unchanged" }; // idempotent — never an empty commit

    try {
      await atomicWrite(filePath, patched);
    } catch (err) {
      return { kind: "failed", reason: `write _work.md: ${(err as Error).message}` };
    }

    // Commit the `_work.md` worktree content (no pre-`add`, so a commit fault leaves the index
    // UNTOUCHED — no staged residual). On failure the ONLY thing to undo is the worktree write, so
    // roll it back to the exact pre-call bytes atomically (a scanner read during rollback also never
    // sees a torn file). Index needs no reset; `_work.md`'s path returns to its exact pre-call state.
    // (The commit is path-scoped, so unrelated pre-existing worktree dirt is left as-is — the tree is
    // clean AT THIS PATH, which is what `approveMerge`'s global `isClean` gate needs to not wedge.)
    try {
      await commit(cwd, [pathspec], commitMessage(patches));
      return { kind: "written" };
    } catch (err) {
      await atomicWrite(filePath, orig).catch(() => {}); // restore the exact pre-call working-tree bytes
      return { kind: "failed", reason: `commit _work.md: ${(err as Error).message}` };
    }
  });
}
