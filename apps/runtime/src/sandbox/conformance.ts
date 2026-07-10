// The Sandbox interface-conformance suite (BRO-1746 done.check). Any Sandbox implementation must pass
// it — the phase-1 worktree adapter here, AND the phase-2 container adapter later (ARCHITECTURE §5:
// "the same suite phase-2 containers must pass"). It asserts only the PORTABLE contract: the paths a
// handle exposes, that exec runs a command inside and captures it, and the load-bearing RECEIPT
// invariant — `runDir` survives teardown regardless of `preserve`. Implementation-specific facts (the
// worktree lives under .maestro/, the branch persists) are asserted in the adapter's own test file.
//
// Not a `.test.ts` file: it registers describe/test blocks when imported by a test file, so a phase-2
// suite reuses it with one `registerSandboxConformance("container", provisionContainer)` call.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { stat } from "node:fs/promises";
import { basename } from "node:path";
import type { SandboxFactory } from "./sandbox";

async function exists(p: string): Promise<boolean> {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

/** What a conformance run needs: a factory to exercise, and teardown of whatever provisioned it. */
export interface ConformanceHarness {
  factory: SandboxFactory;
  cleanup(): Promise<void>;
}

/** Register the conformance suite for a Sandbox implementation. `provision` builds a fresh factory
 *  (and its backing workspace) once for the suite; `name` labels it and seeds unique run ids. */
export function registerSandboxConformance(
  name: string,
  provision: () => Promise<ConformanceHarness>,
): void {
  describe(`Sandbox conformance: ${name}`, () => {
    let h: ConformanceHarness;
    let seq = 0;
    const slug = name.replace(/[^A-Za-z0-9]/g, "").toLowerCase() || "impl";
    const nextId = (): string => `cf${slug}${seq++}`;

    beforeAll(async () => {
      h = await provision();
    });
    afterAll(async () => {
      await h.cleanup();
    });

    test("create provisions a handle: runId, non-empty workdir/branch, eager runDir, recorded resources", async () => {
      const id = nextId();
      const sb = await h.factory.create(id, { resources: { cpuCount: 2, memoryMb: 512 } });
      expect(sb.runId).toBe(id);
      expect(typeof sb.workdir).toBe("string");
      expect(sb.workdir.length).toBeGreaterThan(0);
      expect(sb.branch.length).toBeGreaterThan(0);
      expect(await exists(sb.runDir)).toBe(true); // receipts dir created eagerly (snapshot writes into it)
      expect(sb.resources.cpuCount).toBe(2);
      expect(sb.resources.memoryMb).toBe(512);
      await sb.teardown({ preserve: false });
    });

    test("spawnContext returns a valid enter descriptor", async () => {
      const sb = await h.factory.create(nextId());
      const ctx = sb.spawnContext();
      expect(typeof ctx.cwd).toBe("string");
      expect(ctx.cwd.length).toBeGreaterThan(0);
      expect(Array.isArray(ctx.commandPrefix)).toBe(true);
      expect(typeof ctx.env).toBe("object");
      await sb.teardown({ preserve: false });
    });

    test("exec runs a command inside and captures stdout + exit code", async () => {
      const sb = await h.factory.create(nextId());
      const ok = await sb.exec(["git", "--version"]);
      expect(ok.code).toBe(0);
      expect(ok.stdout).toContain("git version");
      // a failing command's non-zero code is captured, not thrown
      const bad = await sb.exec(["git", "rev-parse", "--verify", "refs/heads/no-such-ref-xyz"]);
      expect(bad.code).not.toBe(0);
      await sb.teardown({ preserve: false });
    });

    test("exec runs INSIDE the sandbox, not on the host (the containment property this suite exists for)", async () => {
      // This is the load-bearing conformance assertion: `git --version` above is cwd-agnostic and would
      // pass from anywhere, so on its own the suite is VACUOUS for a phase-2 container adapter that
      // forgot its exec prefix and leaked onto the host. `pwd` reports the dir exec actually ran in —
      // its leaf must be the sandbox's own workdir leaf (portable: phase-1 run-<id>, phase-2 whatever
      // the container's workdir is), which a host-leaking impl (running in the supervisor's cwd) fails.
      const sb = await h.factory.create(nextId());
      const where = await sb.exec(["pwd"]);
      expect(where.code).toBe(0);
      expect(basename(where.stdout.trim())).toBe(basename(sb.workdir));
      await sb.teardown({ preserve: false });
    });

    test("exec rejects an empty command (caller bug, not a spawn)", async () => {
      const sb = await h.factory.create(nextId());
      await expect(sb.exec([])).rejects.toThrow();
      await sb.teardown({ preserve: false });
    });

    test("teardown(preserve=true) keeps the sandbox — runDir survives, no throw", async () => {
      const sb = await h.factory.create(nextId());
      await sb.teardown({ preserve: true });
      expect(await exists(sb.runDir)).toBe(true);
      await sb.teardown({ preserve: false }); // now actually free it
    });

    test("RECEIPT INVARIANT: teardown(preserve=false) preserves runDir", async () => {
      const sb = await h.factory.create(nextId());
      await sb.teardown({ preserve: false });
      expect(await exists(sb.runDir)).toBe(true); // the receipt is never destroyed by teardown
    });

    test("create is idempotent for the same runId (fresh-context respawn)", async () => {
      const id = nextId();
      const a = await h.factory.create(id);
      const b = await h.factory.create(id); // respawn — must not throw
      expect(b.workdir).toBe(a.workdir);
      expect(b.branch).toBe(a.branch);
      await a.teardown({ preserve: false });
    });
  });
}
