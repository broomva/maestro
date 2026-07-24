/// <reference types="bun" />
// check:bstack-vendor — drift gate for the bstack scripts this repo vendors (BRO-1973).
//
// maestro does not depend on a bstack install at runtime: the hooks in .claude/settings.json
// invoke `$CLAUDE_PROJECT_DIR/scripts/<hook>`, so every hook must exist inside the repo. That is
// deliberate — it is what makes the hooks work in a fresh clone, in CI, and in a git worktree
// with no user-level `~/.agents/skills/bstack` present. The cost is that ~15 scripts here are
// COPIES of github.com/broomva/bstack, and a copy with no check silently rots.
//
// It already had: `leverage-sensor.py` sat one commit behind a real bug fix (state JSON written
// with no trailing newline → tracked copies go git-dirty every session), and
// `l3-stability-pretool-hook.sh` is still missing bstack's BRO-1926 scope guard.
//
// The SSOT rule this enforces: one fact, one source — every other appearance is either generated
// from the source or asserted against it by a check that goes red on divergence. This is that
// check. It does NOT auto-sync; re-syncing a live PreToolUse gate is a human decision.
//
// Two axes of divergence, two modes:
//
//   default (offline, runs in `bun run check` / CI)
//     Every vendored file hashes to the `local_sha256` recorded in scripts/bstack-vendor.json,
//     and every bstack-sourced script in scripts/ is listed there. Goes red when someone edits a
//     vendored copy in place without recording it. No network, fully deterministic.
//
//   --upstream (opt-in; needs network)
//     Fetches each file from broomva/bstack at the pinned `upstream.ref` and asserts it still
//     hashes to `upstream_sha256`. Goes red when bstack has moved, which is the signal to
//     re-pin and re-sync deliberately.
//
//   --write  regenerate the manifest from the working tree. Pair with --upstream to refresh the
//            upstream hashes too, or --from <path-to-bstack-checkout> to read them locally.
//
// Files whose local copy legitimately differs from upstream carry a `divergence` note. The set of
// noted divergences is itself asserted: a NEW undocumented one goes red, and a note left behind
// after the difference is gone goes red as stale. That is what keeps the notes honest.

import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const REPO_ROOT = join(import.meta.dir, "..");
const SCRIPTS = join(REPO_ROOT, "scripts");
const MANIFEST = join(SCRIPTS, "bstack-vendor.json");

export interface VendoredFile {
  /** Filename under scripts/ in THIS repo. */
  local: string;
  /** Path within the bstack repo. */
  upstream: string;
  local_sha256: string;
  upstream_sha256: string;
  /** Why the local copy differs from upstream. Absent ⟺ the copies are byte-identical. */
  divergence?: string;
}

export interface Manifest {
  upstream: { repo: string; ref: string };
  /** scripts/ entries that are maestro's own — never compared, but listed so the
   *  "is every script accounted for?" check can tell owned from vendored. */
  maestro_only: string[];
  files: VendoredFile[];
}

const sha256 = (buf: Buffer | string) => createHash("sha256").update(buf).digest("hex");

export function readManifest(path = MANIFEST): Manifest {
  return JSON.parse(readFileSync(path, "utf8")) as Manifest;
}

/** Offline integrity: local copies match what the manifest records, and nothing is unaccounted for. */
export function checkLocal(m: Manifest, scriptsDir = SCRIPTS): string[] {
  const problems: string[] = [];

  for (const f of m.files) {
    const p = join(scriptsDir, f.local);
    if (!existsSync(p)) {
      problems.push(`${f.local} — listed in the manifest but missing from scripts/`);
      continue;
    }
    const actual = sha256(readFileSync(p));
    if (actual !== f.local_sha256) {
      problems.push(
        `${f.local} — edited in place without re-recording it.\n` +
          `      manifest: ${f.local_sha256}\n` +
          `      on disk:  ${actual}\n` +
          "      If the edit is intentional, run `bun run scripts/check-bstack-vendor.ts --write`\n" +
          "      and state in the PR why this copy now differs from bstack.",
      );
    }
    // A divergence note with no divergence behind it is stale — it stops describing reality and
    // starts excusing it. Upstream-side equality is only knowable in --upstream mode; here we can
    // still catch the inverse: a file marked identical whose hashes disagree in the manifest.
    if (!f.divergence && f.local_sha256 !== f.upstream_sha256) {
      problems.push(
        `${f.local} — manifest records differing local/upstream hashes but no \`divergence\` note. ` +
          "Every difference from bstack needs a recorded reason.",
      );
    }
    if (f.divergence && f.local_sha256 === f.upstream_sha256) {
      problems.push(
        `${f.local} — carries a \`divergence\` note ("${f.divergence}") but is byte-identical to ` +
          "upstream. Drop the note.",
      );
    }
  }

  const listed = new Set([...m.files.map((f) => f.local), ...m.maestro_only]);
  const onDisk = [...new Bun.Glob("*.{sh,py}").scanSync({ cwd: scriptsDir })];
  for (const name of onDisk.sort()) {
    if (!listed.has(name)) {
      problems.push(
        `${name} — a script in scripts/ that the manifest does not account for. Add it to ` +
          "`files` (if copied from bstack) or `maestro_only` (if this repo owns it).",
      );
    }
  }
  return problems;
}

const rawUrl = (m: Manifest, path: string) =>
  `https://raw.githubusercontent.com/${m.upstream.repo}/${m.upstream.ref}/${path}`;

/** Upstream drift: bstack has moved since the pinned ref was recorded. */
export async function checkUpstream(m: Manifest): Promise<string[]> {
  const problems: string[] = [];
  const results = await Promise.all(
    m.files.map(async (f) => {
      const url = rawUrl(m, f.upstream);
      const res = await fetch(url);
      if (!res.ok) return { f, error: `HTTP ${res.status} fetching ${url}` };
      return { f, hash: sha256(Buffer.from(await res.arrayBuffer())) };
    }),
  );
  for (const r of results) {
    if (r.error) {
      problems.push(`${r.f.local} — ${r.error}`);
    } else if (r.hash !== r.f.upstream_sha256) {
      problems.push(
        `${r.f.local} — bstack has moved at ${m.upstream.ref}.\n` +
          `      pinned:   ${r.f.upstream_sha256}\n` +
          `      upstream: ${r.hash}\n` +
          "      Re-sync the copy deliberately, then re-record with --write --upstream.",
      );
    }
  }
  return problems;
}

async function upstreamHashes(m: Manifest, fromDir: string | null): Promise<Map<string, string>> {
  const out = new Map<string, string>();
  await Promise.all(
    m.files.map(async (f) => {
      if (fromDir) {
        out.set(f.local, sha256(readFileSync(join(fromDir, f.upstream))));
        return;
      }
      const res = await fetch(rawUrl(m, f.upstream));
      if (!res.ok) throw new Error(`HTTP ${res.status} fetching ${rawUrl(m, f.upstream)}`);
      out.set(f.local, sha256(Buffer.from(await res.arrayBuffer())));
    }),
  );
  return out;
}

async function main() {
  const argv = Bun.argv.slice(2);
  const has = (flag: string) => argv.includes(flag);
  const flagValue = (flag: string) => {
    const i = argv.indexOf(flag);
    return i >= 0 ? argv[i + 1] : undefined;
  };

  const m = readManifest();
  const from = flagValue("--from") ?? null;

  if (has("--write")) {
    const refreshUpstream = has("--upstream") || from !== null;
    const upstream = refreshUpstream ? await upstreamHashes(m, from) : null;
    for (const f of m.files) {
      f.local_sha256 = sha256(readFileSync(join(SCRIPTS, f.local)));
      if (upstream) f.upstream_sha256 = upstream.get(f.local) as string;
    }
    writeFileSync(MANIFEST, `${JSON.stringify(m, null, 2)}\n`);
    console.log(
      `check:bstack-vendor — rewrote ${m.files.length} local hashes` +
        `${upstream ? " + upstream hashes" : ""} into scripts/bstack-vendor.json`,
    );
    console.log("Review the diff: a changed hash means a vendored copy moved. Say why in the PR.");
    return;
  }

  const problems = checkLocal(m);
  if (has("--upstream")) problems.push(...(await checkUpstream(m)));

  if (problems.length > 0) {
    console.error("check:bstack-vendor — vendored bstack scripts have drifted:\n");
    for (const p of problems) console.error(`  ✗ ${p}`);
    console.error(
      `\n${problems.length} problem(s). Vendored copies of ${m.upstream.repo}@${m.upstream.ref} ` +
        "must stay accounted for — see the header of scripts/check-bstack-vendor.ts.",
    );
    process.exit(1);
  }

  const diverged = m.files.filter((f) => f.divergence);
  console.log(
    `check:bstack-vendor — ${m.files.length} vendored script(s) verified against ` +
      `${m.upstream.repo}@${m.upstream.ref}${has("--upstream") ? " (upstream fetched)" : " (offline)"}.`,
  );
  if (diverged.length > 0) {
    console.log(`  ${diverged.length} carry a recorded divergence from upstream:`);
    for (const f of diverged) console.log(`    · ${f.local} — ${f.divergence}`);
  }
}

if (import.meta.main) {
  await main();
}
