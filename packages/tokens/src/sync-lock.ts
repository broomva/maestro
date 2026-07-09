/// <reference types="bun" />
// Re-pin tokens.lock.json to the current handoff token canon. Run this after
// *intentionally* changing a handoff token file (and reviewing the change), so
// the drift check (check-sync.ts) passes again. Run: `bun run --filter
// @maestro/tokens sync:lock`.

import { writeFileSync } from "node:fs";
import { computeLock, LOCK_PATH } from "./sources";

export function writeLock(): void {
  const lock = computeLock();
  writeFileSync(LOCK_PATH, `${JSON.stringify(lock, null, 2)}\n`);
  console.log(`sync:lock — pinned ${Object.keys(lock).length} handoff sources → tokens.lock.json`);
}

if (import.meta.main) {
  writeLock();
}
