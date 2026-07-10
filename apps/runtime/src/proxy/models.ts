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
/** Per (binary) document input-token bound — the flat reserve for a page-rendered PDF whose page count
 *  is not in the request (`source.type` base64/url). Anthropic caps a document at 100 pages and bills
 *  ~1,500-3,000 tokens/page (rendered image + extracted text), so ~300k tokens is the SOUND worst case.
 *  This is loose (a 1-page PDF reserves the 100-page ceiling and can be refused under a tiny per_run
 *  cap) — the price of not knowing page count up front; a guardrail errs high. BRO-1756's live path
 *  carries page metadata and will tighten this. Applies ONLY to binary documents — a TEXT-source
 *  document's `data` IS billable text and is priced by the byte-length bound, not this flat floor
 *  (blanking it there under-priced unboundedly, P20 round-6). */
export const MAX_DOCUMENT_TOKENS = 300_000;

/** What one `priceModality` pass accumulates: the per-block modality floors, and the max prompt-cache
 *  WRITE multiplier any block opts into (input is billed 1.25x for the default 5-min ephemeral cache,
 *  2x for `ttl:"1h"`; 1 = no cache_control). */
interface ModalityAcc {
  images: number;
  documents: number;
  cacheMult: number;
}

/**
 * One pass that both SANITIZES the payload (blanks dimension-billed binary blobs so they are not priced
 * as text tokens) and ACCUMULATES the per-block token floors that replace them + the max cache-write
 * multiplier. The critical rule (P20 round-6) is that stripping is gated on the SOURCE type, not the
 * block type:
 *   - `image`: always dimension-billed → +MAX_IMAGE_TOKENS, blank `source.data` if present (base64);
 *     a url-source image has no data, still floored.
 *   - `document` with `source.type: "text"`: the `data` is RAW TEXT billed as tokens → keep it, add NO
 *     floor (the byte-length bound prices it soundly). Blanking it under-priced unboundedly.
 *   - `document` with `source.type: "content"`: its `source.content` is an array of text/image blocks
 *     billed individually → recurse (inner images get stripped + floored, inner text counts), add NO
 *     flat floor for the container.
 *   - `document` otherwise (base64/url/unknown): page-billed, unknown pages → +MAX_DOCUMENT_TOKENS,
 *     blank `source.data` if present.
 * Any block carrying `cache_control` raises `acc.cacheMult` (P20 round-7: input is billed at the cache
 * WRITE premium, which the base rate under-reserved). Returns a deep clone — never mutates the input.
 */
function priceModality(node: unknown, acc: ModalityAcc): unknown {
  if (Array.isArray(node)) return node.map((n) => priceModality(n, acc));
  if (node === null || typeof node !== "object") return node;
  const obj = node as Record<string, unknown>;

  // Prompt-cache WRITE premium — any block (text/image/document/tool) can opt in via cache_control.
  const cc = obj.cache_control;
  if (cc !== null && typeof cc === "object") {
    acc.cacheMult = Math.max(acc.cacheMult, (cc as { ttl?: unknown }).ttl === "1h" ? 2 : 1.25);
  }

  const source =
    obj.source !== null && typeof obj.source === "object"
      ? (obj.source as Record<string, unknown>)
      : undefined;
  const srcType = source?.type;

  if (obj.type === "image") {
    acc.images++; // always dimension-billed
    if (source && "data" in source) return { ...obj, source: { ...source, data: "" } };
    return mapChildren(obj, acc); // url-source image: no data to blank; recurse defensively
  }
  if (obj.type === "document") {
    if (srcType === "text" || srcType === "content") {
      return mapChildren(obj, acc); // text/content billed as its own tokens → keep data, no flat floor
    }
    acc.documents++; // base64 / url / unknown source: page-billed, unknown page count → flat floor
    if (source && "data" in source) return { ...obj, source: { ...source, data: "" } };
    return mapChildren(obj, acc);
  }
  return mapChildren(obj, acc);
}

/** Deep-clone an object, recursing every value through `priceModality`. */
function mapChildren(obj: Record<string, unknown>, acc: ModalityAcc): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(obj)) out[k] = priceModality(v, acc);
  return out;
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
 *   - TEXT input tokens are bounded by the UTF-8 BYTE length of the payload with only DIMENSION-BILLED
 *     binary blobs stripped: for a byte-level BPE tokenizer every token is >= 1 byte and merges only
 *     reduce the count, so `tokens <= bytes` for TEXT — including dense CJK a chars/token heuristic
 *     under-counts (P20 round-3), AND a document's raw TEXT `data`, which stays in the byte count
 *     (blanking it under-priced unboundedly, P20 round-6);
 *   - only DIMENSION-BILLED blobs (base64 images, base64/url PDFs) are stripped and replaced by a fixed
 *     per-block token bound (MAX_IMAGE_TOKENS / MAX_DOCUMENT_TOKENS) — sound for images (<= ~1600 tok
 *     each), conservative for binary documents (100-page worst case; BRO-1756 tightens it). Pricing a
 *     screenshot's base64 as text tokens is what false-refused it (P20 round-5 finding #4);
 *   - a `cache_control` breakpoint makes the WHOLE input bound bill at the prompt-cache WRITE premium
 *     (1.25x for the 5-min ephemeral default, 2x for `ttl:"1h"`) — the base rate under-reserved a
 *     low-compressibility cached block (P20 round-7). Applying it to the whole input over-reserves
 *     uncached blocks; the ~4x byte over-count on natural language absorbs that.
 * It over-counts plain text ~4x; erring high is the guardrail's job — a call that could breach a cap
 * is refused, never silently overspent. LIMITATION: server-tool surcharges (web_search / code_execution
 * billed per-call on top of tokens) are NOT modeled here — the meter() reconcile is the backstop, and
 * BRO-1756's live path prices them; a run that leans on server tools should carry headroom in per_run.
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
  // One pass: sanitize (blank dimension-billed blobs), count the per-block floors, and pick up the max
  // cache-write multiplier any block opts into.
  const modality: ModalityAcc = { images: 0, documents: 0, cacheMult: 1 };
  const sanitized = priceModality(payload ?? {}, modality);
  const byteTokens = Buffer.byteLength(JSON.stringify(sanitized), "utf8"); // text bound, blobs excluded
  const inputTokens =
    byteTokens + modality.images * MAX_IMAGE_TOKENS + modality.documents * MAX_DOCUMENT_TOKENS;
  // Cache write premium applies to INPUT only (output is billed normally, never cached).
  const raw =
    (inputTokens * price.inputPerMtok * modality.cacheMult + maxTokens * price.outputPerMtok) /
    1_000_000;
  return raw * CEILING_SAFETY;
}
