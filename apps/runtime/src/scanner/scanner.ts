// Workspace scanner — the pure derivation half of BRO-1800 (FLOWS §F9 step 1).
//
// Walk a workspace git repo, parse every `_work.md` frontmatter, resolve each
// contract against its parent (folder depth = the work tree; a child inherits
// owner/gate/budget — FLOWS §F1 step 2), and produce the `node` row set. This
// module is PURE apart from reading files: no db, no clock, no randomness, so
// `scanWorkspace` is deterministic — the same workspace yields the byte-identical
// node set (the property the seam guarantees, DATA-MODEL §B intro). The stateful
// half (upsert seen rows, tombstone vanished ones) lives in ./sync.ts.
//
// `createdAt` is FS-derived (frontmatter `created`) so it is part of the
// deterministic node set; `updatedAt`/`deletedAt` are the index's clock and are
// assigned by ./sync.ts (fs-index.md §4), never here.

import { readdir, readFile } from "node:fs/promises";
import { join, sep } from "node:path";
import {
  type GateMode,
  type Kind,
  type OrchState,
  parseWorkInput,
  resolveWorkContract,
  type WorkContract,
  WorkContractError,
} from "@maestro/protocol";

/** The name that marks a folder as a unit of work (DATA-MODEL §A.1). */
export const WORK_FILE = "_work.md";

/**
 * Directories the scan never descends into: the git store, dependency trees, and
 * the runtime's own index/worktree dirs. `runs/` is NOT skipped — it holds session
 * receipts (never a `_work.md`), so it produces no nodes, but a folder legitimately
 * named `runs` would still be scanned.
 */
export const SKIP_DIRS: ReadonlySet<string> = new Set([".git", "node_modules", ".maestro", "dist"]);

/** A single derived `node` row, minus the index-assigned SyncFields (./sync.ts adds those). */
export interface ScannedNode {
  id: string;
  /** workspace-relative folder path with `/` separators; `""` for the workspace root. */
  path: string;
  parentId: string | null;
  kind: Kind;
  state: OrchState;
  owner: string | null;
  gate: GateMode;
  budgetJson: string | null;
  doneJson: string | null;
  title: string | null;
  createdAt: number;
}

/** A `_work.md` that could not be turned into a node — surfaced, never silently dropped. */
export interface ScanError {
  /** workspace-relative folder path of the offending `_work.md`. */
  path: string;
  code: string;
  message: string;
}

export interface ScanResult {
  /** The derived nodes, sorted by path — a stable, comparable set. */
  nodes: ScannedNode[];
  /** Files that failed to parse/derive, sorted by path. One bad file never aborts the scan. */
  errors: ScanError[];
  /**
   * True when every directory was read. False when a dir was unreadable (permissions,
   * a race) so the scan may be MISSING nodes — the reconcile then skips tombstoning, so
   * an unreadable dir cannot mass-soft-delete a subtree it simply failed to see.
   */
  complete: boolean;
}

/** First markdown heading (`# … ` through `###### … `) of the brief, or null. */
export function firstHeading(brief: string): string | null {
  const m = /^#{1,6}[ \t]+(.+?)[ \t]*$/m.exec(brief);
  return m?.[1] ?? null;
}

/**
 * `created` (frontmatter ISO date) → epoch ms. Only a date-only `YYYY-MM-DD` is
 * accepted: it parses as UTC midnight, so `createdAt` is deterministic across
 * machines (part of the rebuild-identity dump, fs-index.md §4). A zone-less datetime
 * (`2026-06-25T10:00:00`) or any other string is rejected — `Date.parse` would read
 * it in the host's LOCAL timezone, making the derived node set machine-dependent.
 */
export function createdToEpochMs(created: string): number {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(created)) {
    throw new WorkContractError(
      "invalid_type",
      `created must be an ISO date (YYYY-MM-DD): ${created}`,
      "created",
    );
  }
  const ms = Date.parse(created);
  if (!Number.isFinite(ms)) {
    throw new WorkContractError(
      "invalid_type",
      `created is not a parseable date: ${created}`,
      "created",
    );
  }
  return ms;
}

/**
 * Normalize an OS-relative path to workspace-canonical form: `/` separators and a
 * single Unicode normalization form (NFC), so a non-ASCII folder yields the same
 * path bytes on macOS (NFD-native) and Linux (NFC) — the byte-canonical `sourcePath`
 * the journal + rebuild identity require (fs-index.md §6).
 */
function toPosix(rel: string): string {
  return (sep === "/" ? rel : rel.split(sep).join("/")).normalize("NFC");
}

/**
 * The nearest proper-ancestor of `dir` that RESOLVED successfully (a key of
 * `resolved`), or null. Walks segments up from `dir`; the tree is by nesting, and a
 * malformed/duplicate ancestor is skipped so its child re-attaches to the nearest
 * VALID ancestor (and inherits that ancestor's defaults) instead of orphaning to null.
 */
function parentDirOf(dir: string, resolved: ReadonlyMap<string, unknown>): string | null {
  if (dir === "") return null;
  const parts = dir.split("/");
  for (let i = parts.length - 1; i > 0; i--) {
    const anc = parts.slice(0, i).join("/");
    if (resolved.has(anc)) return anc;
  }
  // The root work folder ("") is the parent of any top-level work folder.
  return resolved.has("") ? "" : null;
}

/** The dirs a walk found, plus any it could not read. */
export interface WorkDirsResult {
  /** workspace-relative dirs containing a `_work.md`, shallowest-first then lexicographic. */
  dirs: string[];
  /** dirs whose contents could not be read — the scan is incomplete under these. */
  unreadable: string[];
}

/**
 * Recursively collect the workspace-relative dirs that contain a `_work.md`, sorted
 * (shallowest first, then lexicographic) so a parent is always visited before its
 * children and the walk is order-deterministic. Symlinked dirs are NOT followed
 * (`Dirent.isDirectory()` is false for a symlink), which also rules out walk cycles.
 * A dir that cannot be read is recorded in `unreadable`, never silently dropped.
 */
export async function findWorkDirs(root: string): Promise<WorkDirsResult> {
  const found: string[] = [];
  const unreadable: string[] = [];

  async function walk(absDir: string, relDir: string): Promise<void> {
    const entries = await readdir(absDir, { withFileTypes: true }).catch(() => null);
    if (entries === null) {
      unreadable.push(relDir);
      return;
    }
    let hasWork = false;
    const subdirs: string[] = [];
    for (const e of entries) {
      if (e.isDirectory()) {
        if (!SKIP_DIRS.has(e.name)) subdirs.push(e.name);
      } else if (e.isFile() && e.name === WORK_FILE) {
        hasWork = true;
      }
    }
    if (hasWork) found.push(relDir);
    subdirs.sort();
    for (const name of subdirs) {
      await walk(join(absDir, name), relDir === "" ? name : `${relDir}/${name}`);
    }
  }

  await walk(root, "");
  const byDepthThenName = (a: string, b: string) => {
    const da = a === "" ? 0 : a.split("/").length;
    const db = b === "" ? 0 : b.split("/").length;
    return da - db || (a < b ? -1 : a > b ? 1 : 0);
  };
  return { dirs: found.sort(byDepthThenName), unreadable: unreadable.sort() };
}

/**
 * Scan a workspace to its `node` set. Parses each `_work.md` at the input layer
 * (owner/gate/budget optional) so parent-defaults resolve (F1 step 2) against the
 * parent's already-resolved contract, walking top-down. A parse failure, a
 * duplicate frontmatter `id`, or an unparseable `created` is recorded in `errors`
 * and the offending file is skipped — the rest of the workspace still scans.
 */
export async function scanWorkspace(root: string): Promise<ScanResult> {
  const { dirs, unreadable } = await findWorkDirs(root);
  const resolvedByDir = new Map<string, WorkContract>();
  const idToDir = new Map<string, string>();
  const nodes: ScannedNode[] = [];
  const errors: ScanError[] = [];
  for (const d of unreadable) {
    errors.push({ path: d, code: "unreadable_dir", message: "directory could not be read" });
  }

  for (const dir of dirs) {
    const absFile = join(root, dir === "" ? WORK_FILE : join(dir, WORK_FILE));
    let source: string;
    try {
      source = await readFile(absFile, "utf8");
    } catch (err) {
      errors.push({ path: dir, code: "unreadable", message: (err as Error).message });
      continue;
    }

    try {
      const { input, brief } = parseWorkInput(source);
      // Resolve against the nearest ALREADY-RESOLVED ancestor (top-down guarantees it
      // is present) so a malformed ancestor does not orphan its subtree.
      const parentDir = parentDirOf(dir, resolvedByDir);
      const parent = parentDir === null ? undefined : resolvedByDir.get(parentDir);
      const contract = resolveWorkContract(input, parent);
      const createdAt = createdToEpochMs(contract.created);

      // A duplicate id would collide on the `node` PK and make the tree ambiguous;
      // keep the first (path-order) occurrence and record the rest.
      const firstDir = idToDir.get(contract.id);
      if (firstDir !== undefined) {
        errors.push({
          path: dir,
          code: "duplicate_id",
          message: `id ${contract.id} already used by ${firstDir === "" ? "<root>" : firstDir}`,
        });
        continue;
      }
      idToDir.set(contract.id, dir);
      resolvedByDir.set(dir, contract);

      const parentId = parentDir === null ? null : (resolvedByDir.get(parentDir)?.id ?? null);
      nodes.push({
        id: contract.id,
        path: toPosix(dir),
        parentId,
        kind: contract.kind,
        state: contract.state,
        owner: contract.owner ?? null,
        gate: contract.gate,
        budgetJson: contract.budget ? JSON.stringify(contract.budget) : null,
        doneJson: contract.done ? JSON.stringify(contract.done) : null,
        title: firstHeading(brief),
        createdAt,
      });
    } catch (err) {
      const code = err instanceof WorkContractError ? err.code : "scan_error";
      errors.push({ path: dir, code, message: (err as Error).message });
    }
  }

  nodes.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  errors.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  return { nodes, errors, complete: unreadable.length === 0 };
}
