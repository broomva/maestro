// @maestro/runtime workspace scanner (BRO-1800) — the read-side rebuild of the
// `node` table from `_work.md` frontmatter (FLOWS §F9 step 1).
//
// `scanWorkspace` is the pure, deterministic derivation (walk + parse + resolve);
// `syncNodes` reconciles it into the index (idempotent upsert + tombstone);
// `scanIntoIndex` is the one-call startup entry point.

export {
  createdToEpochMs,
  findWorkDirs,
  firstHeading,
  type ScanError,
  type ScannedNode,
  type ScanResult,
  SKIP_DIRS,
  scanWorkspace,
  WORK_FILE,
} from "./scanner";
export { type ScanIntoIndexResult, type SyncSummary, scanIntoIndex, syncNodes } from "./sync";
