// Contract snapshot (HARNESS §1) — at dispatch the supervisor serializes the node's RESOLVED work
// contract (frontmatter + inherited defaults) to runs/run-<id>/contract.json. The child reads the
// SNAPSHOT, never the live `_work.md`: a mid-run contract edit takes effect on the *next* attempt,
// atomically. The snapshot is the child's frozen view of "what am I working on."

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { WorkContract } from "@maestro/protocol";

export interface ContractSnapshot {
  /** The session this run belongs to (matches BROOMVA_SESSION / the child argv). */
  session: string;
  /** The node's fully-resolved work contract — defaults (owner/gate/budget) already inherited, so
   *  the child never re-derives them. */
  node: WorkContract;
  /** When the supervisor froze this snapshot (ISO-8601). Passed in — the runtime has no ambient clock. */
  dispatchedAt: string;
}

/** The canonical path of a run's contract snapshot. */
export function contractPath(runDir: string): string {
  return join(runDir, "contract.json");
}

/** Freeze a contract snapshot to runs/run-<id>/contract.json (creating the run dir if needed).
 *  Returns the path written (= BROOMVA_CONTRACT for the spawned child). */
export async function writeContractSnapshot(
  runDir: string,
  snapshot: ContractSnapshot,
): Promise<string> {
  const path = contractPath(runDir);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, `${JSON.stringify(snapshot, null, 2)}\n`, "utf8");
  return path;
}

/** Read a run's frozen contract snapshot (the child's first act, HARNESS §5). Validates the shape
 *  rather than blind-casting — a wrong-shape or older-schema snapshot fails loudly HERE, at the
 *  child's first act, not deep inside the run where the cause is unrecoverable. */
export async function readContractSnapshot(runDir: string): Promise<ContractSnapshot> {
  const path = contractPath(runDir);
  const raw = await readFile(path, "utf8");
  return assertContractSnapshot(JSON.parse(raw), path);
}

/** Runtime guard: the on-disk snapshot must carry the fields the child depends on. */
function assertContractSnapshot(v: unknown, path: string): ContractSnapshot {
  const bad = (why: string): never => {
    throw new Error(`contract snapshot at ${path} is malformed: ${why}`);
  };
  if (typeof v !== "object" || v === null) return bad("not an object");
  const o = v as Record<string, unknown>;
  if (typeof o.session !== "string" || o.session === "") bad("session must be a non-empty string");
  if (typeof o.dispatchedAt !== "string") bad("dispatchedAt must be a string");
  if (typeof o.node !== "object" || o.node === null) bad("node must be an object");
  const node = o.node as Record<string, unknown>;
  for (const key of ["id", "kind", "state", "gate"]) {
    if (typeof node[key] !== "string") bad(`node.${key} must be a string`);
  }
  return v as ContractSnapshot;
}
