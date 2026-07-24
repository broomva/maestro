/// <reference types="bun" />
// Adversarial fixtures for the check:bstack-vendor drift gate (BRO-1973).
//
// Each case below is a way a vendored bstack copy rots WITHOUT anyone noticing — which is exactly
// how `leverage-sensor.py` came to sit one commit behind a real bug fix. A gate that only checks
// "the file exists" would pass every one of them.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { checkLocal, type Manifest, readManifest } from "./check-bstack-vendor.ts";

const sha256 = (s: string) => createHash("sha256").update(s).digest("hex");

const IDENTICAL = "#!/usr/bin/env bash\n# a vendored hook\n";
const DIVERGED_LOCAL = "#!/usr/bin/env bash\n# scripts/a-hook.sh — rewritten header\n";
const DIVERGED_UPSTREAM = "#!/usr/bin/env bash\n# bstack/scripts/a-hook.sh — rewritten header\n";

let dir: string;

/** A manifest + on-disk scripts dir that agree: one identical file, one noted divergence. */
function fixture(): Manifest {
  writeFileSync(join(dir, "identical.sh"), IDENTICAL);
  writeFileSync(join(dir, "diverged.sh"), DIVERGED_LOCAL);
  return {
    upstream: { repo: "broomva/bstack", ref: "v0.37.1" },
    maestro_only: ["owned-by-maestro.sh"],
    files: [
      {
        local: "identical.sh",
        upstream: "scripts/identical.sh",
        local_sha256: sha256(IDENTICAL),
        upstream_sha256: sha256(IDENTICAL),
      },
      {
        local: "diverged.sh",
        upstream: "scripts/diverged.sh",
        local_sha256: sha256(DIVERGED_LOCAL),
        upstream_sha256: sha256(DIVERGED_UPSTREAM),
        divergence: "header comment rewritten at vendor time",
      },
    ],
  };
}

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "vendor-check-"));
});
afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("checkLocal — offline integrity", () => {
  test("a manifest that matches the working tree is clean", () => {
    expect(checkLocal(fixture(), dir)).toEqual([]);
  });

  test("catches a vendored copy edited in place — the leverage-sensor.py failure mode", () => {
    const m = fixture();
    writeFileSync(join(dir, "identical.sh"), `${IDENTICAL}# a local hotfix nobody recorded\n`);
    const problems = checkLocal(m, dir);
    expect(problems).toHaveLength(1);
    expect(problems[0]).toContain("identical.sh");
    expect(problems[0]).toContain("edited in place");
  });

  test("catches a manifest entry whose file has been deleted", () => {
    const m = fixture();
    rmSync(join(dir, "diverged.sh"));
    const problems = checkLocal(m, dir);
    expect(problems).toHaveLength(1);
    expect(problems[0]).toContain("missing from scripts/");
  });

  test("catches a NEW vendored script that nobody listed — the silent-growth failure mode", () => {
    const m = fixture();
    writeFileSync(join(dir, "smuggled-in.sh"), "#!/usr/bin/env bash\n");
    const problems = checkLocal(m, dir);
    expect(problems).toHaveLength(1);
    expect(problems[0]).toContain("smuggled-in.sh");
    expect(problems[0]).toContain("does not account for");
  });

  test("a script declared maestro_only is accounted for without being compared", () => {
    const m = fixture();
    writeFileSync(join(dir, "owned-by-maestro.sh"), "#!/usr/bin/env bash\n# ours\n");
    expect(checkLocal(m, dir)).toEqual([]);
  });

  test("only .sh and .py files are inventoried — a stray .md is not a vendored script", () => {
    const m = fixture();
    writeFileSync(join(dir, "NOTES.md"), "# scratch\n");
    expect(checkLocal(m, dir)).toEqual([]);
  });

  test("catches a difference from upstream with no recorded reason", () => {
    const m = fixture();
    m.files[1].divergence = undefined;
    const problems = checkLocal(m, dir);
    expect(problems).toHaveLength(1);
    expect(problems[0]).toContain("no `divergence` note");
  });

  test("catches a STALE divergence note left behind after the difference is gone", () => {
    const m = fixture();
    m.files[1].upstream_sha256 = m.files[1].local_sha256; // upstream caught up
    const problems = checkLocal(m, dir);
    expect(problems).toHaveLength(1);
    expect(problems[0]).toContain("Drop the note");
  });

  test("reports every problem at once rather than stopping at the first", () => {
    const m = fixture();
    writeFileSync(join(dir, "identical.sh"), "tampered\n");
    writeFileSync(join(dir, "smuggled-in.sh"), "#!/usr/bin/env bash\n");
    expect(checkLocal(m, dir).length).toBe(2);
  });
});

describe("the real manifest", () => {
  const m = readManifest();

  test("is in sync with the scripts/ directory on disk", () => {
    expect(checkLocal(m)).toEqual([]);
  });

  test("pins a concrete bstack ref rather than a moving branch", () => {
    expect(m.upstream.repo).toBe("broomva/bstack");
    expect(m.upstream.ref).toMatch(/^v\d+\.\d+\.\d+$/);
  });

  test("vendors both leverage sensors, byte-identical to upstream (BRO-1973)", () => {
    for (const name of ["leverage-sensor.py", "leverage-ship-sensor.py"]) {
      const f = m.files.find((x) => x.local === name);
      expect(f, `${name} must be a tracked vendored file`).toBeDefined();
      expect(f?.local_sha256, `${name} must match upstream exactly`).toBe(f?.upstream_sha256);
      expect(f?.divergence).toBeUndefined();
    }
  });

  test("every recorded divergence carries a non-empty reason", () => {
    for (const f of m.files.filter((x) => x.divergence !== undefined)) {
      expect(f.divergence?.length, `${f.local} needs a real reason`).toBeGreaterThan(20);
    }
  });
});
