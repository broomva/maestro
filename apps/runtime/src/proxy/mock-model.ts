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

/** A recorded forward — what the proxy asked the upstream for (model resolved proxy-side, role, beat). */
export interface MockCall {
  model: string;
  role: ChildRole;
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
      calls.push({ model: req.model, role: req.role });
      const r = i < script.length ? (script[i] as MockResponse) : fallback;
      i++;
      if (r.delayMs) await sleep(r.delayMs);
      const status = r.status ?? 200;
      const body = r.body ?? defaultBody(status >= 200 && status < 300 ? "ok" : "error");
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
