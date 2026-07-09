/// <reference types="bun" />
// Drift check: verify the handoff token canon still matches the committed pin in
// tokens.lock.json (TOKENS-INTEGRATION §1 — "token edits happen in one place").
//
// A mismatch means someone changed a handoff token file without acknowledging it
// here. That is not an error to paper over — it is a review checkpoint: inspect
// the change, then `bun run --filter @maestro/tokens sync:lock` to re-pin (and
// rebuild). Exit 1 on drift or a missing lock so CI blocks the un-reviewed edit.
//
// Run: `bun run --filter @maestro/tokens check:sync`.

import { existsSync, readFileSync } from "node:fs";
import { computeLock, LOCK_PATH, type Lockfile } from "./sources";

/**
 * Parse lock text into a Lockfile, or null if it is missing / unparseable /
 * not a plain object. A merge-conflicted or truncated lock (plausible in a
 * fan-out repo) is treated the same as a missing lock — fail closed with the
 * "run sync:lock" guidance, never an uncaught stack trace.
 */
export function parseLock(text: string | null): Lockfile | null {
  if (text === null) return null;
  try {
    const parsed: unknown = JSON.parse(text);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as Lockfile;
  } catch {
    return null;
  }
}

/** Compare the current handoff hashes to the pinned lock. Returns the drift set. */
export function findDrift(): { missing: boolean; drifted: string[]; added: string[] } {
  const pinned = existsSync(LOCK_PATH) ? parseLock(readFileSync(LOCK_PATH, "utf8")) : null;
  if (!pinned) return { missing: true, drifted: [], added: [] };
  const current = computeLock();
  const drifted: string[] = [];
  const added: string[] = [];
  for (const [rel, hash] of Object.entries(current)) {
    if (!(rel in pinned)) added.push(rel);
    else if (pinned[rel] !== hash) drifted.push(rel);
  }
  const removed = Object.keys(pinned).filter((rel) => !(rel in current));
  return { missing: false, drifted: [...drifted, ...removed], added };
}

if (import.meta.main) {
  const { missing, drifted, added } = findDrift();
  if (missing) {
    console.error(
      "check:sync — tokens.lock.json is missing or corrupt. Run `bun run --filter @maestro/tokens sync:lock`.",
    );
    process.exit(1);
  }
  if (drifted.length || added.length) {
    console.error("check:sync — the handoff token canon has drifted from the pinned lock:");
    for (const f of drifted) console.error(`  changed/removed: ${f}`);
    for (const f of added) console.error(`  new source:      ${f}`);
    console.error(
      "Review the change, then re-pin: `bun run --filter @maestro/tokens sync:lock` (and rebuild).",
    );
    process.exit(1);
  }
  console.log("check:sync — tokens in sync with the handoff canon.");
}
