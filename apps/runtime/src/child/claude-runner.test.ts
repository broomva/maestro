// claude-runner integration tests (BRO-1912). The runner spawns a REAL subprocess (the `claude` CLI), so
// these drive it against a FAKE cli (a bash script pointed at via MAESTRO_CLAUDE_BIN) that emits captured
// stream-json shapes and dumps the env it received. They cover the three terminal paths (clean → exit 0,
// crash-without-result → exit 1 + stderr tail, stop → exit 10) AND the P20 BLOCKER mutation-proof: the CLI
// the runner spawns must NOT inherit host secrets, and MUST get the subscription auth channel.

import { describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const RUNNER = new URL("./claude-runner.ts", import.meta.url).pathname;

/** Write an executable fake `claude` at <dir>/fake-claude.sh and return its path. */
function fakeCli(dir: string, body: string): string {
  const p = join(dir, "fake-claude.sh");
  writeFileSync(p, `#!/usr/bin/env bash\n${body}\n`);
  chmodSync(p, 0o755);
  return p;
}

interface RunResult {
  events: Array<{ actor: string; type: string; payload?: Record<string, unknown> }>;
  code: number;
  envDump: Record<string, string> | null;
}

/** Spawn the runner with a fake CLI + a chosen env; collect its emitted events + exit code. */
async function runRunner(opts: {
  cwd: string;
  cliBin: string;
  env: Record<string, string>;
  stopAfterMs?: number;
}): Promise<RunResult> {
  const proc = Bun.spawn(["bun", "run", RUNNER, "--role", "agent", "--session", "cr-test"], {
    cwd: opts.cwd,
    env: { ...opts.env, MAESTRO_CLAUDE_BIN: opts.cliBin, MAESTRO_CLAUDE_MODEL: "fake-model" },
    stdout: "pipe",
    stderr: "pipe",
    stdin: opts.stopAfterMs !== undefined ? "pipe" : "ignore",
  });
  if (opts.stopAfterMs !== undefined) {
    setTimeout(() => {
      const sink = proc.stdin;
      if (sink && typeof sink !== "number") {
        try {
          sink.write('{"type":"stop"}\n');
          sink.flush();
        } catch {
          /* runner already gone */
        }
      }
    }, opts.stopAfterMs);
  }
  const out = await new Response(proc.stdout).text();
  const code = await proc.exited;
  const events = out
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));
  let envDump: Record<string, string> | null = null;
  try {
    envDump = JSON.parse(readFileSync(join(opts.cwd, "env-dump.json"), "utf8"));
  } catch {
    /* the crash-path fake writes no dump */
  }
  return { events, code, envDump };
}

describe("claude-runner — terminal paths", () => {
  test("clean completion → one run.exiting{0} and process exit 0 (→ review / Needs you)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "cr-clean-"));
    try {
      const cli = fakeCli(
        dir,
        `env > "$(pwd)/env-dump.json"
printf '%s\\n' '{"type":"system","subtype":"init","model":"fake"}'
printf '%s\\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"did the work"}]}}'
printf '%s\\n' '{"type":"result","subtype":"success","is_error":false,"result":"done"}'
exit 0`,
      );
      const { events, code } = await runRunner({ cwd: dir, cliBin: cli, env: baseEnv() });
      expect(code).toBe(0);
      const exiting = events.filter((e) => e.type === "run.exiting");
      expect(exiting).toEqual([{ actor: "system", type: "run.exiting", payload: { code: 0 } }]);
      // run.started is emitted exactly once (the runner's guaranteed receipt; the CLI's init does not dup).
      expect(events.filter((e) => e.type === "run.started")).toHaveLength(1);
      expect(events.some((e) => e.type === "agent.said")).toBe(true);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  test("crash without a result → synthesized run.exiting{1} carrying the stderr tail, exit 1", async () => {
    const dir = mkdtempSync(join(tmpdir(), "cr-crash-"));
    try {
      const cli = fakeCli(
        dir,
        `printf '%s\\n' '{"type":"system","subtype":"init","model":"fake"}'
echo "boom: simulated auth failure" >&2
exit 3`,
      );
      const { events, code } = await runRunner({ cwd: dir, cliBin: cli, env: baseEnv() });
      expect(code).toBe(1);
      const exiting = events.filter((e) => e.type === "run.exiting");
      expect(exiting).toHaveLength(1);
      expect(exiting[0]?.payload?.code).toBe(1);
      expect(String(exiting[0]?.payload?.reason)).toContain("boom: simulated auth failure");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  test("stop control → CLI killed, run.exiting{10, user_stop}, exit 10 (→ Stuck, redispatchable)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "cr-stop-"));
    try {
      // A CLI that emits init then hangs (exec sleep so SIGKILL reaps it directly, no orphan).
      const cli = fakeCli(
        dir,
        `printf '%s\\n' '{"type":"system","subtype":"init","model":"fake"}'
exec sleep 20`,
      );
      const { events, code } = await runRunner({
        cwd: dir,
        cliBin: cli,
        env: baseEnv(),
        stopAfterMs: 300,
      });
      expect(code).toBe(10);
      expect(events.filter((e) => e.type === "run.exiting")).toEqual([
        { actor: "system", type: "run.exiting", payload: { code: 10, reason: "user_stop" } },
      ]);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("claude-runner — env confinement (the P20 BLOCKER, end to end)", () => {
  test("the spawned CLI gets NO host secret but DOES get the subscription auth channel", async () => {
    const dir = mkdtempSync(join(tmpdir(), "cr-env-"));
    const dumpPath = join(dir, "env-dump.json");
    try {
      // The fake dumps its received env to an explicit path (passed via the operator passthrough, which
      // this also exercises end-to-end — a host-specific var an operator names must survive confinement).
      const cli = fakeCli(
        dir,
        `env > "$FAKE_ENV_DUMP"
printf '%s\\n' '{"type":"result","subtype":"success","is_error":false}'
exit 0`,
      );
      const { code } = await runRunner({
        cwd: dir,
        cliBin: cli,
        env: {
          ...baseEnv(),
          // a host secret that MUST NOT reach the bypassPermissions CLI (the R5 exfil + billing bug)
          ANTHROPIC_API_KEY: "sk-ant-LEAK-SENTINEL",
          CLOUDFLARE_API_TOKEN: "cf-LEAK-SENTINEL",
          // the auth channel the CLI legitimately needs
          USER: "runner-testuser",
          LOGNAME: "runner-testuser",
          // an operator-named host-specific passthrough (here: where the fake dumps its env)
          MAESTRO_CLAUDE_ENV_PASSTHROUGH: "FAKE_ENV_DUMP",
          FAKE_ENV_DUMP: dumpPath,
        },
      });
      expect(code).toBe(0);
      // `env` prints KEY=value lines (values may themselves contain '='); split on the first '='.
      const dump: Record<string, string> = {};
      for (const line of readFileSync(dumpPath, "utf8").split("\n")) {
        const eq = line.indexOf("=");
        if (eq > 0) dump[line.slice(0, eq)] = line.slice(eq + 1);
      }
      // BLOCKER: no host secret survives into the CLI's env (neither the key nor its value).
      expect(dump.ANTHROPIC_API_KEY).toBeUndefined();
      expect(dump.CLOUDFLARE_API_TOKEN).toBeUndefined();
      expect(Object.values(dump)).not.toContain("sk-ant-LEAK-SENTINEL");
      expect(Object.values(dump)).not.toContain("cf-LEAK-SENTINEL");
      // the CLI must never hold the per-session proxy bearer either.
      expect(dump.BROOMVA_MODEL_TOKEN).toBeUndefined();
      // but the subscription auth channel IS present (else the CLI could not authenticate).
      expect(dump.USER).toBe("runner-testuser");
      expect(dump.LOGNAME).toBe("runner-testuser");
      // and the operator-named host-specific passthrough survived confinement (the escape hatch works).
      expect(dump.FAKE_ENV_DUMP).toBe(dumpPath);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

/** A minimal env with the toolchain the runner needs to run under `bun` (PATH/HOME), no secrets. */
function baseEnv(): Record<string, string> {
  const e: Record<string, string> = {};
  for (const k of ["PATH", "HOME", "SHELL", "TMPDIR", "LANG", "BUN_INSTALL"]) {
    const v = process.env[k];
    if (v !== undefined) e[k] = v;
  }
  return e;
}
