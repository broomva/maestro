// The mock-model server (DECISIONS §D8 layer 1, BRO-1806) — a scripted `ModelUpstream` that sits behind
// the model proxy in place of Anthropic, so CI drives full F2→F3 loops with ZERO tokens and NO API key.
// The proxy already injects its upstream (`createModelProxy({ upstream })`, HARNESS §3), so the mock
// plugs into the exact production seam — the child dials the real proxy over real HTTP; only the far
// side is scripted. Deterministic by construction: responses are consumed from a queue in order, then a
// fallback repeats, so a loop that keeps calling gets a stable answer.
//
// The mock returns the proxy's `UpstreamResult` shape ({ status, body, usage? }): `usage.usd` is what the
// budget guard METERS (so a scripted per-call cost drives the budget-refusal scenario deterministically),
// `body` is a minimal Anthropic Messages response (the child's SDK speaks that shape; the fixture child
// only needs a 200 vs the proxy's 402), and `delayMs` lets a call hang so a kill can land mid-call.

import { readFileSync } from "node:fs";
import type { ChildRole } from "../harness/spawn-contract";
import type { ModelUpstream, UpstreamResult } from "./proxy";

/** One scripted upstream answer. Every field defaults so a bare `{}` is a valid 200 with default usage. */
export interface MockResponse {
  /** HTTP status the proxy passes back (default 200). A non-2xx body is passed through verbatim. */
  status?: number;
  /** The response body (default a minimal Anthropic-shaped assistant message). */
  body?: unknown;
  /** Metered usage — `usage.usd` is booked against the budget. Omit → the reservation is released
   *  (nothing billed). Default: `{ usd: usagePerCallUsd }`. */
  usage?: { usd: number; tokens?: number };
  /** Await this many ms before answering — lets a kill land mid-call (default 0). */
  delayMs?: number;
}

export interface MockModelOptions {
  /** Per-call answers consumed in order; once exhausted, `fallback` repeats for every further call. */
  script?: readonly MockResponse[];
  /** The answer returned after the script is exhausted (default: 200 + `usagePerCallUsd`). */
  fallback?: MockResponse;
  /** Default `usage.usd` per call when a response sets none (drives the budget scenario). Default 0.5. */
  usagePerCallUsd?: number;
  /** Injected sleep for `delayMs` — default a real timer; tests can pin it. */
  sleep?: (ms: number) => Promise<void>;
}

/** A recorded forward — what the proxy asked the upstream for (model resolved proxy-side, role, and the
 *  forwarded request payload so a test can assert the outbound prompt reflects the contract). */
export interface MockCall {
  model: string;
  role: ChildRole;
  /** The request body the proxy forwarded (`{ max_tokens, messages, … }`) — verbatim from the child. */
  payload: unknown;
}

export interface MockModel extends ModelUpstream {
  /** Every forwarded call in order — for assertions (how many beats reached the model). */
  readonly calls: readonly MockCall[];
}

/** The default per-call cost when a response/opts sets none — chosen so a handful of calls trips a
 *  modest per_run cap (the budget-refusal scenario) without a single call refusing up-front. */
export const DEFAULT_MOCK_USAGE_USD = 0.5;

/** A minimal Anthropic Messages response body — real-shaped so a future SDK child parses it, though the
 *  fixture child only distinguishes the proxy's 200 vs 402. */
function defaultBody(text: string): unknown {
  return {
    id: "msg_mock",
    type: "message",
    role: "assistant",
    content: [{ type: "text", text }],
    stop_reason: "end_turn",
    usage: { input_tokens: 8, output_tokens: 4 },
  };
}

const realSleep = (ms: number): Promise<void> =>
  ms > 0 ? new Promise((r) => setTimeout(r, ms)) : Promise.resolve();

/** True if the response body contains any `tool_use` content block. */
function hasToolUse(body: unknown): boolean {
  if (body && typeof body === "object") {
    const c = (body as { content?: unknown }).content;
    if (Array.isArray(c)) {
      return c.some(
        (b) => b && typeof b === "object" && (b as { type?: unknown }).type === "tool_use",
      );
    }
  }
  return false;
}

/** True if the forwarded request advertised a non-empty `tools` array — the Anthropic precondition for a
 *  model to be ALLOWED to return `tool_use`. */
function advertisesTools(payload: unknown): boolean {
  if (payload && typeof payload === "object") {
    const t = (payload as { tools?: unknown }).tools;
    return Array.isArray(t) && t.length > 0;
  }
  return false;
}

/** The body a real Anthropic model returns when it wants to act but the request advertised NO tools: it
 *  cannot emit tool_use, so it falls back to text + end_turn. This is the fidelity that catches a child
 *  which forgot to send `tools` ([[mock-fidelity-gap-false-green]]). */
function degradedNoTools(): unknown {
  return {
    id: "msg_no_tools",
    type: "message",
    role: "assistant",
    content: [{ type: "text", text: "(no tools were provided, so I cannot call one)" }],
    stop_reason: "end_turn",
    usage: { input_tokens: 8, output_tokens: 12 },
  };
}

/**
 * Build a scripted mock upstream. Responses come from `script` in order; after it is exhausted every
 * further call returns `fallback` (default: a 200 with `usagePerCallUsd` billed), so a loop that keeps
 * calling gets a deterministic steady state. `forward` never throws — an upstream throw would surface as
 * the proxy's 502, which is not what the fixtures exercise.
 */
export function createMockModel(opts: MockModelOptions = {}): MockModel {
  const usagePerCall = opts.usagePerCallUsd ?? DEFAULT_MOCK_USAGE_USD;
  const script = [...(opts.script ?? [])];
  const fallback = opts.fallback ?? {};
  const sleep = opts.sleep ?? realSleep;
  const calls: MockCall[] = [];
  let i = 0;

  const upstream: ModelUpstream = {
    async forward(req): Promise<UpstreamResult> {
      calls.push({ model: req.model, role: req.role, payload: req.payload });
      const r = i < script.length ? (script[i] as MockResponse) : fallback;
      i++;
      if (r.delayMs) await sleep(r.delayMs);
      const status = r.status ?? 200;
      let body = r.body ?? defaultBody(status >= 200 && status < 300 ? "ok" : "error");
      // FIDELITY: a real model cannot return tool_use unless the request advertised `tools`. If a scripted
      // 2xx body wants to call a tool but the child forgot to send `tools`, degrade to text — so a child
      // that drops the tools schema turns the tool tests RED instead of silently passing on the mock.
      if (status >= 200 && status < 300 && hasToolUse(body) && !advertisesTools(req.payload)) {
        body = degradedNoTools();
      }
      // `usage` is honored as given (incl. explicitly omitted for a non-billable call); when the field is
      // absent entirely, default to the per-call cost so the budget scenario accrues spend.
      const usage = "usage" in r ? r.usage : { usd: usagePerCall };
      return usage === undefined ? { status, body } : { status, body, usage };
    },
  };

  return {
    forward: upstream.forward,
    get calls() {
      return calls;
    },
  };
}

/**
 * Load a mock script for the DISPATCH-mounted mock (the spawned runtime's token-free upstream) from
 * `MAESTRO_MOCK_SCRIPT` — a JSON file path holding `{ script?, fallback?, usagePerCallUsd? }`, the same
 * shape `createMockModel` takes. Returns `undefined` when the env is unset, so the DEFAULT dispatch mock
 * (a bare "ok" that ends a turn WITHOUT reaching a gate — the behavior p2-exit relies on) is unchanged;
 * the script is strictly opt-in.
 *
 * This is the mock-dispatch-to-gate seam the P3-exit E2E needs (BRO-1821): a script whose child EDITS a
 * file then completes drives a REAL run to a completion gate with a mergeable diff — the "one live gated
 * run end-to-end" evidence gate-slice.test.ts flagged as missing. Only ever consulted in mock mode
 * (`createMockModel` is dispatch's upstream only there). A malformed/missing file throws — a misconfigured
 * test setup should fail loudly (index.ts's mount-catch then degrades the runtime to reads).
 */
export function loadMockScriptFromEnv(
  env: Record<string, string | undefined> = process.env,
): MockModelOptions | undefined {
  const path = env.MAESTRO_MOCK_SCRIPT;
  if (!path) return undefined;
  const parsed = JSON.parse(readFileSync(path, "utf8")) as {
    script?: readonly MockResponse[];
    fallback?: MockResponse;
    usagePerCallUsd?: number;
  };
  return {
    script: parsed.script,
    fallback: parsed.fallback,
    usagePerCallUsd: parsed.usagePerCallUsd,
  };
}
