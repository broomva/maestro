// Model pinning (HARNESS §3, AUTONOMY §5 "version drift"). The child asks the proxy for its ROLE, not
// a model id; the proxy resolves the pinned model here. Pinning lives supervisor-side so a model
// version bump is one config change, not a child-prompt edit — and so the D8 canary (BRO-1806) can
// later route a fraction of runs to a candidate model without the child knowing.

import type { ChildRole } from "../harness/spawn-contract";

/** The pinned model id for each child role. Opus for the writer/judge (capability), Haiku for the
 *  orchestrator (cheap coordination). The verifier judge is same-vendor by decision, always a
 *  separate session. */
export const DEFAULT_MODEL_PINS: Record<ChildRole, string> = {
  agent: "claude-opus-4-8", // the writer — does the work
  verifier: "claude-opus-4-8", // the judge — scores the work (separate session, temp 0)
  orchestrator: "claude-haiku-4-5-20251001", // coordination — cheap, frequent
};

/** Per-run env override key for a role's pinned model, e.g. MAESTRO_MODEL_AGENT. */
export function modelEnvKey(role: ChildRole): string {
  return `MAESTRO_MODEL_${role.toUpperCase()}`;
}

/**
 * Resolve the pinned model id for a role, honoring an env override (MAESTRO_MODEL_<ROLE>) over the
 * default pin. A blank/whitespace override is ignored (falls through to the default) so an empty env
 * var can never resolve to an empty model id.
 */
export function resolvePinnedModel(
  role: ChildRole,
  env: Record<string, string | undefined> = process.env,
): string {
  const override = env[modelEnvKey(role)]?.trim();
  return override && override.length > 0 ? override : DEFAULT_MODEL_PINS[role];
}

// ── Cost model — the per-call CEILING that makes the budget reservation sound ─────────────────────
//
// The budget guard must reserve at least the true cost of a call before it forwards, or concurrent
// calls overspend (P20 round-1 finding). The ceiling is derived per-call from the model's price and
// the request's `max_tokens`: output is bounded EXACTLY by max_tokens, input is over-estimated from
// the payload size + a safety margin, so the ceiling is >= the actual cost. A call whose ceiling
// exceeds the remaining budget is refused up-front — the safe answer when one call could breach a cap.

/** Per-Mtoken USD price for a model (approximate public Anthropic pricing; env-overridable). */
export interface ModelPrice {
  inputPerMtok: number;
  outputPerMtok: number;
}

/** Pinned-model prices (USD per 1M tokens). Approximate — override via MAESTRO_PRICE_<ID> if needed. */
export const MODEL_PRICING: Record<string, ModelPrice> = {
  "claude-opus-4-8": { inputPerMtok: 15, outputPerMtok: 75 },
  "claude-sonnet-5": { inputPerMtok: 3, outputPerMtok: 15 },
  "claude-haiku-4-5-20251001": { inputPerMtok: 1, outputPerMtok: 5 },
  "claude-fable-5": { inputPerMtok: 5, outputPerMtok: 25 },
};

/** The conservative fallback price for an unknown model id — the most expensive pin, so the ceiling
 *  is never under-estimated for a model we don't have a row for. */
export const FALLBACK_PRICE: ModelPrice = { inputPerMtok: 15, outputPerMtok: 75 };

/** Anthropic requires `max_tokens`; if a request somehow omits it, assume this ceiling for output. */
export const DEFAULT_MAX_OUTPUT_TOKENS = 8192;
/** Chars-per-token for the input estimate. Deliberately LOW (~3.5) so token count is OVER-estimated. */
const CHARS_PER_TOKEN = 3.5;
/** Safety multiplier on the whole ceiling — headroom against estimate error. */
const CEILING_SAFETY = 1.15;

/** The price for a model id, honoring a MAESTRO_PRICE_<canonical> `in/out` override, else the table. */
export function modelPrice(
  model: string,
  env: Record<string, string | undefined> = process.env,
): ModelPrice {
  const raw = env[`MAESTRO_PRICE_${model.toUpperCase().replace(/[^A-Z0-9]/g, "_")}`]?.trim();
  if (raw) {
    const [inP, outP] = raw.split("/").map(Number);
    if (
      Number.isFinite(inP) &&
      Number.isFinite(outP) &&
      (inP as number) >= 0 &&
      (outP as number) >= 0
    ) {
      return { inputPerMtok: inP as number, outputPerMtok: outP as number };
    }
  }
  return MODEL_PRICING[model] ?? FALLBACK_PRICE;
}

/**
 * The per-call cost CEILING in USD — an upper bound on what this call can cost, used as the budget
 * reservation. Output is bounded exactly by `max_tokens`; input is over-estimated from the payload
 * size; the whole thing carries a safety margin — so `ceiling >= actual` with high confidence, which
 * is what makes "reserve then reconcile" incapable of overspending.
 */
export function estimateCallCeilingUsd(
  model: string,
  payload: unknown,
  env: Record<string, string | undefined> = process.env,
): number {
  const price = modelPrice(model, env);
  const p = (payload ?? {}) as { max_tokens?: unknown };
  const maxTokens =
    typeof p.max_tokens === "number" && p.max_tokens > 0 ? p.max_tokens : DEFAULT_MAX_OUTPUT_TOKENS;
  const inputChars = JSON.stringify(payload ?? {}).length;
  const inputTokens = Math.ceil(inputChars / CHARS_PER_TOKEN);
  const raw = (inputTokens * price.inputPerMtok + maxTokens * price.outputPerMtok) / 1_000_000;
  return raw * CEILING_SAFETY;
}
