/// <reference types="bun" />
// child-contract.test.ts — the supervisor→child seam (HARNESS §1, §6) for BRO-1756
// (`bun test apps/runtime --filter child-contract`).
//
// This asserts the seam's four load-bearing guarantees:
//   1. The child argv contract parses/serializes round-trip and REFUSES malformed input loudly
//      (the supervisor builds the argv, so a bad argv is a supervisor bug, not a child default).
//   2. The child env is SECRET-FREE — the security invariant. Host secrets (the Anthropic key, the
//      runtime credential, the relay key) never enter the child env; only the allowlist + the
//      deliberately-set BROOMVA_* contract vars pass. This is the whole point of the seam.
//   3. The contract snapshot (contract.json) round-trips — the child reads a frozen view.
//   4. The §6 SDK→session.jsonl translation maps each occurrence to the right event (or drops it).

import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { WorkContract } from "@maestro/protocol";
import { EVENT_TYPES } from "@maestro/protocol";
import {
  type ContractSnapshot,
  contractPath,
  readContractSnapshot,
  writeContractSnapshot,
} from "./contract-snapshot";
import { type SdkOccurrence, translateSdkOccurrence } from "./runner";
import {
  buildChildEnv,
  type ChildInvocation,
  isSecretEnvName,
  parseChildArgv,
  SpawnContractError,
  serializeChildArgv,
} from "./spawn-contract";

// ── 1. argv contract ─────────────────────────────────────────────────────────

test("argv round-trips for every role", () => {
  for (const role of ["agent", "verifier", "orchestrator"] as const) {
    const inv: ChildInvocation = { role, session: "sess-01J8XZ" };
    expect(parseChildArgv(serializeChildArgv(inv))).toEqual(inv);
  }
});

test("argv parse ignores surrounding args and order", () => {
  // The real child argv is `broomva-child --role agent --session <id>` — extra leading argv
  // (the binary name, a bun runner) must not confuse the parser.
  expect(parseChildArgv(["broomva-child", "--session", "s1", "--role", "verifier"])).toEqual({
    role: "verifier",
    session: "s1",
  });
});

test("argv parse REFUSES a bad role, loudly", () => {
  expect(() => parseChildArgv(["--role", "wizard", "--session", "s1"])).toThrow(SpawnContractError);
  // and a missing role
  expect(() => parseChildArgv(["--session", "s1"])).toThrow(SpawnContractError);
});

test("argv parse REFUSES a missing/empty session, loudly", () => {
  expect(() => parseChildArgv(["--role", "agent"])).toThrow(SpawnContractError);
  expect(() => parseChildArgv(["--role", "agent", "--session", "   "])).toThrow(SpawnContractError);
});

// ── 2. env is secret-free (the security invariant) ───────────────────────────

// A host env that contains EVERY kind of secret HARNESS §1 says the child must never see, plus a
// few legitimate passthrough vars and an option-injection vector.
const HOSTILE_HOST_ENV: Record<string, string> = {
  // secrets — none of these may reach the child
  ANTHROPIC_API_KEY: "sk-ant-must-not-leak",
  OPENAI_API_KEY: "sk-openai-must-not-leak",
  MAESTRO_RELAY_KEY: "relay-must-not-leak",
  RUNTIME_CREDENTIAL: "cred-must-not-leak",
  GITHUB_TOKEN: "ghp_must_not_leak",
  AWS_SECRET_ACCESS_KEY: "aws-must-not-leak",
  SESSION_SECRET: "session-secret-must-not-leak",
  MY_PASSWORD: "hunter2",
  SOME_PAT: "pat-must-not-leak",
  // an option-injection vector — NODE_OPTIONS could `--require` a host module into the child
  NODE_OPTIONS: "--require /tmp/evil.js",
  // legitimate passthrough
  PATH: "/usr/bin:/bin",
  HOME: "/home/agent",
  LANG: "en_US.UTF-8",
  BUN_INSTALL: "/home/agent/.bun",
};

const ENV_SPEC = {
  session: "sess-01J8XZ",
  runDir: "/runs/run-42",
  contractPath: "/runs/run-42/contract.json",
  modelProxyUrl: "http://127.0.0.1:8790",
  modelToken: "proxy-bearer-minted-at-spawn",
} as const;

test("buildChildEnv drops EVERY host secret", () => {
  const env = buildChildEnv(HOSTILE_HOST_ENV, ENV_SPEC);
  for (const leaked of [
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "MAESTRO_RELAY_KEY",
    "RUNTIME_CREDENTIAL",
    "GITHUB_TOKEN",
    "AWS_SECRET_ACCESS_KEY",
    "SESSION_SECRET",
    "MY_PASSWORD",
    "SOME_PAT",
  ]) {
    expect(env[leaked]).toBeUndefined();
  }
  // No leaked VALUE survives under any renamed key, either.
  const values = Object.values(env);
  for (const secret of Object.values(HOSTILE_HOST_ENV)) {
    if (secret.includes("must-not-leak") || secret === "hunter2") {
      expect(values).not.toContain(secret);
    }
  }
});

test("buildChildEnv drops the NODE_OPTIONS injection vector", () => {
  // Not on the allowlist AND a prefix-match would have admitted it — the explicit-name allowlist
  // is what closes this host→child contamination path.
  expect(buildChildEnv(HOSTILE_HOST_ENV, ENV_SPEC).NODE_OPTIONS).toBeUndefined();
});

test("buildChildEnv passes the allowlist through and sets the contract vars", () => {
  const env = buildChildEnv(HOSTILE_HOST_ENV, ENV_SPEC);
  expect(env.PATH).toBe("/usr/bin:/bin");
  expect(env.HOME).toBe("/home/agent");
  expect(env.LANG).toBe("en_US.UTF-8");
  expect(env.BUN_INSTALL).toBe("/home/agent/.bun");
  expect(env.BROOMVA_SESSION).toBe("sess-01J8XZ");
  expect(env.BROOMVA_RUN_DIR).toBe("/runs/run-42");
  expect(env.BROOMVA_CONTRACT).toBe("/runs/run-42/contract.json");
  expect(env.BROOMVA_MODEL_PROXY).toBe("http://127.0.0.1:8790");
  // The model token's NAME matches the secret predicate, so it can ONLY arrive by being set
  // deliberately here — proof the child gets the proxy bearer without any host secret riding along.
  expect(env.BROOMVA_MODEL_TOKEN).toBe("proxy-bearer-minted-at-spawn");
  expect(isSecretEnvName("BROOMVA_MODEL_TOKEN")).toBe(true);
});

test("isSecretEnvName flags secrets and clears the allowlist", () => {
  for (const name of [
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "SESSION_SECRET",
    "MY_PASSWORD",
    "SOME_PAT",
    "A_BEARER",
    "DB_CREDENTIAL",
  ]) {
    expect(isSecretEnvName(name)).toBe(true);
  }
  for (const name of ["PATH", "HOME", "LANG", "BUN_INSTALL", "TZ", "BROOMVA_RUN_DIR"]) {
    expect(isSecretEnvName(name)).toBe(false);
  }
});

// ── 3. contract snapshot round-trips ─────────────────────────────────────────

const NODE: WorkContract = {
  id: "01J8XZ-node",
  kind: "task",
  state: "running",
  owner: "agent:worker",
  gate: "human",
  budget: { per_run_usd: 5, max_iterations: 20 },
  created: "2026-07-10T00:00:00.000Z",
  updated: "2026-07-10T00:00:00.000Z",
};

test("contract snapshot round-trips through the filesystem", async () => {
  const dir = mkdtempSync(join(tmpdir(), "maestro-child-contract-"));
  try {
    const snapshot: ContractSnapshot = {
      session: "sess-01J8XZ",
      node: NODE,
      dispatchedAt: "2026-07-10T12:00:00.000Z",
    };
    const written = await writeContractSnapshot(dir, snapshot);
    expect(written).toBe(contractPath(dir));
    expect(await readContractSnapshot(dir)).toEqual(snapshot);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// ── 4. §6 SDK → session.jsonl translation ────────────────────────────────────

test("§6: an assistant turn becomes agent.said", () => {
  const occ: SdkOccurrence = { kind: "assistant_turn", text: "on it — reading the scanner" };
  expect(translateSdkOccurrence(occ)).toEqual({
    actor: "agent",
    type: EVENT_TYPES.AGENT_SAID,
    payload: { text: "on it — reading the scanner" },
  });
});

test("§6: a tool use becomes an agent-authored tool.call with a summarized input", () => {
  const occ: SdkOccurrence = {
    kind: "tool_use",
    tool: "edit",
    input: { file: "a.ts", replace: "x" },
    path: "apps/runtime/src/a.ts",
  };
  const ev = translateSdkOccurrence(occ);
  expect(ev?.actor).toBe("agent");
  expect(ev?.type).toBe(EVENT_TYPES.TOOL_CALL);
  expect(ev?.payload?.tool).toBe("edit");
  expect(ev?.payload?.path).toBe("apps/runtime/src/a.ts");
  expect(typeof ev?.payload?.input).toBe("string"); // summarized, not the raw object
});

test("§6: a long tool input is truncated in the summary", () => {
  const big = "y".repeat(500);
  const ev = translateSdkOccurrence({ kind: "tool_use", tool: "bash", input: big });
  const summary = ev?.payload?.input as string;
  expect(summary.length).toBeLessThanOrEqual(200);
  expect(summary.endsWith("...")).toBe(true);
});

test("§6: pruneUndefined drops an absent path (no undefined leaks into the payload)", () => {
  const ev = translateSdkOccurrence({ kind: "tool_use", tool: "ls" });
  expect(ev?.payload && "path" in ev.payload).toBe(false);
  expect(ev?.payload && "input" in ev.payload).toBe(false);
});

test("§6: a tool result is authored by the tool, not the agent", () => {
  const ev = translateSdkOccurrence({
    kind: "tool_result",
    tool: "bash",
    ok: false,
    summary: "exit 1",
  });
  expect(ev).toEqual({
    actor: "tool",
    type: EVENT_TYPES.TOOL_RESULT,
    payload: { tool: "bash", ok: false, summary: "exit 1" },
  });
});

test("§6: a model call completing is NOT logged (the proxy meters it)", () => {
  expect(translateSdkOccurrence({ kind: "model_call_completed" })).toBeNull();
});

test("§6: run lifecycle beats are system-authored", () => {
  expect(translateSdkOccurrence({ kind: "run_beat", iteration: 3, diffstat: "+4 -1" })).toEqual({
    actor: "system",
    type: EVENT_TYPES.RUN_BEAT,
    payload: { iteration: 3, diffstat: "+4 -1" },
  });
  expect(translateSdkOccurrence({ kind: "run_started", run: "run-42", branch: "run/42" })).toEqual({
    actor: "system",
    type: EVENT_TYPES.RUN_STARTED,
    payload: { run: "run-42", branch: "run/42" },
  });
  expect(translateSdkOccurrence({ kind: "run_exiting", code: 10, reason: "budget" })).toEqual({
    actor: "system",
    type: EVENT_TYPES.RUN_EXITING,
    payload: { code: 10, reason: "budget" },
  });
});
