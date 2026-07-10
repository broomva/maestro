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
/** Safety multiplier on the whole ceiling — headroom against price staleness / framing tokens. */
const CEILING_SAFETY = 1.15;
/** Per-image input-token bound. Anthropic bills an image by DIMENSIONS (~W·H/750), and resizes so the
 *  longest edge is <= 1568px and total <= ~1.15 MP → at most ~1600 tokens per image. So this fixed
 *  bound is a SOUND per-image ceiling. It REPLACES the base64 bytes (which are stripped before the
 *  byte-length text bound), never adds to them — counting a dimension-billed blob as text tokens
 *  over-priced a routine screenshot into a false 402 (P20 round-5 finding #4). */
export const MAX_IMAGE_TOKENS = 2000;
/** Per-document input-token bound. A PDF is rendered per page; Anthropic caps a document at 100 pages
 *  (~1600 tok/page) → ~160k tokens is the sound worst case when page count is unknown. This is loose
 *  (a 1-page PDF reserves the 100-page ceiling and can be refused under a tiny per_run cap) — the price
 *  of not knowing page count up front; a guardrail errs high. BRO-1756's live path carries page
 *  metadata and will tighten this to actual pages. Like images, it REPLACES the (stripped) base64. */
export const MAX_DOCUMENT_TOKENS = 160_000;

/** Count image/document content blocks anywhere in the payload (they nest in tool_result content). */
function countModalityBlocks(node: unknown, acc: { images: number; documents: number }): void {
  if (Array.isArray(node)) {
    for (const n of node) countModalityBlocks(n, acc);
    return;
  }
  if (node !== null && typeof node === "object") {
    const t = (node as { type?: unknown }).type;
    if (t === "image") acc.images++;
    else if (t === "document") acc.documents++;
    for (const v of Object.values(node as Record<string, unknown>)) countModalityBlocks(v, acc);
  }
}

/**
 * Deep-clone the payload with image/document `source.data` (the base64 blob) blanked, so the
 * byte-length TEXT bound doesn't price a dimension-billed modality blob as text tokens. The modality
 * cost is added separately via the per-block token bounds. Non-base64 sources (e.g. `type:"url"`) have
 * no `data` and pass through untouched — the per-block bound still covers them.
 */
function stripModalityData(node: unknown): unknown {
  if (Array.isArray(node)) return node.map(stripModalityData);
  if (node !== null && typeof node === "object") {
    const obj = node as Record<string, unknown>;
    const t = obj.type;
    if (
      (t === "image" || t === "document") &&
      obj.source !== null &&
      typeof obj.source === "object"
    ) {
      const src = obj.source as Record<string, unknown>;
      return { ...obj, source: { ...src, data: "" } };
    }
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(obj)) out[k] = stripModalityData(v);
    return out;
  }
  return node;
}

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
 * The per-call cost CEILING in USD — an UPPER BOUND on what this call can cost, used as the budget
 * reservation. Sound (`ceiling >= actual`) across modalities, which is what makes "reserve then
 * reconcile" incapable of overspending, while staying tight enough not to false-refuse real calls:
 *   - output is bounded EXACTLY by `max_tokens` (the API never emits more);
 *   - TEXT input tokens are bounded by the UTF-8 BYTE length of the payload WITH image/document base64
 *     stripped: for a byte-level BPE tokenizer every token is >= 1 byte and merges only reduce the
 *     count, so `tokens <= bytes` for TEXT — including dense CJK a chars/token heuristic under-counts
 *     (the P20 round-3 overspend). Stripping the base64 first is what avoids pricing a dimension-billed
 *     blob as text tokens, which false-refused routine screenshots (P20 round-5 finding #4);
 *   - IMAGE / DOCUMENT blocks are billed by DIMENSIONS, so each adds a fixed per-block token bound
 *     (MAX_IMAGE_TOKENS / MAX_DOCUMENT_TOKENS) INSTEAD OF its stripped bytes — sound for images
 *     (<= ~1600 tok each), conservative for documents (100-page worst case; BRO-1756 tightens it).
 * It over-counts plain text ~4x; erring high is the guardrail's job — a call that could breach a cap
 * is refused, never silently overspent.
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
  // Text bound: byte length of the payload with modality base64 stripped (blobs are priced by the
  // per-block bounds below, not as text tokens).
  const byteTokens = Buffer.byteLength(JSON.stringify(stripModalityData(payload ?? {})), "utf8");
  const modality = { images: 0, documents: 0 };
  countModalityBlocks(payload, modality);
  const inputTokens =
    byteTokens + modality.images * MAX_IMAGE_TOKENS + modality.documents * MAX_DOCUMENT_TOKENS;
  const raw = (inputTokens * price.inputPerMtok + maxTokens * price.outputPerMtok) / 1_000_000;
  return raw * CEILING_SAFETY;
}
