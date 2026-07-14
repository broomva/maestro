// The intent write client (BRO-1888 FID-3) — the ONE write path (POST /api/intents, PATTERNS §3:
// intents in, events out). A verb the human presses at the gate (approve / send back / redispatch)
// becomes an `Intent` posted here; the RESULT never comes back in the response body — it arrives on
// the event stream and re-projects the store (the same SSE that feeds the mission plane). The 202 ack
// only says "accepted". Mirrors RuntimeChatTransport: same-origin by default (the vite proxy forwards
// /api), an injectable `fetch` for tests, and an Idempotency-Key so a retried POST is a no-op (the
// storm killer, FLOWS F7) — the gate grace window resends under a STABLE key so no verb double-applies.

import {
  IDEMPOTENCY_KEY_HEADER,
  INTENTS_ENDPOINT,
  type Intent,
  type IntentAccepted,
} from "@maestro/protocol";

/** The API §4 error envelope subset this client reads (mirrors ChatTransportError's source). */
interface ErrorEnvelope {
  error?: { message?: string; code?: string; retryable?: boolean };
}

/** A typed intent-POST failure (non-2xx) — carries the API §4 code + status so the caller can
 *  re-queue the gate card with an honest chip (gate.ts grace `failed` phase), never a silent drop. */
export class IntentError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly retryable: boolean;
  constructor(message: string, status: number, code?: string, retryable = false) {
    super(message);
    this.name = "IntentError";
    this.status = status;
    this.code = code;
    this.retryable = retryable;
  }
}

/** The subset of `fetch` the client uses — a plain function is a valid test double (the global satisfies it). */
export type FetchLike = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

export interface PostIntentOptions {
  /** Runtime origin; default "" (same origin — the vite proxy forwards /api). */
  baseUrl?: string;
  /** Injected `fetch` (default the global) — the test seam. */
  fetchImpl?: FetchLike;
  /**
   * The Idempotency-Key. A retried POST at the SAME key is a no-op (API.md §Intents), so pass a STABLE
   * key per human decision — the gate grace window resends the chosen verdict under one key so a retry
   * can never double-apply. Defaults to a fresh uuid per call (fine for one-shot, non-retried sends).
   */
  idempotencyKey?: string;
  signal?: AbortSignal;
}

/** Best-effort parse of the API §4 error envelope from a non-OK response. */
async function intentHttpError(res: Response): Promise<IntentError> {
  let message = `intent rejected (${res.status})`;
  let code: string | undefined;
  let retryable = false;
  try {
    const body = (await res.json()) as ErrorEnvelope;
    if (body?.error) {
      if (typeof body.error.message === "string") message = body.error.message;
      code = body.error.code;
      retryable = body.error.retryable ?? false;
    }
  } catch {
    // non-JSON error body — keep the status-derived message
  }
  return new IntentError(message, res.status, code, retryable);
}

/**
 * POST one intent to the runtime. Resolves with the 202 ack (`{accepted:true}`) — the RESULT lands on
 * the event stream, not here (PATTERNS §3: a gate approve re-projects the node review→done via the SSE,
 * which is what dequeues the card, not this call). Throws `IntentError` on a non-2xx so the gate grace
 * window can move to `failed` and re-queue the card. `fetch` is bound to the global (the illegal-invocation
 * guard RuntimeChatTransport documents — a private-field call would pass `this === undefined`).
 */
export async function postIntent(
  intent: Intent,
  opts: PostIntentOptions = {},
): Promise<IntentAccepted> {
  const fetchImpl = opts.fetchImpl ?? globalThis.fetch.bind(globalThis);
  const key = opts.idempotencyKey ?? crypto.randomUUID();
  const res = await fetchImpl(`${opts.baseUrl ?? ""}${INTENTS_ENDPOINT}`, {
    method: "POST",
    headers: { "content-type": "application/json", [IDEMPOTENCY_KEY_HEADER]: key },
    body: JSON.stringify(intent),
    signal: opts.signal,
  });
  if (!res.ok) throw await intentHttpError(res);
  // The body is the 202 ack; tolerate an empty / non-JSON 2xx (the ack is advisory — the stream is truth).
  try {
    return (await res.json()) as IntentAccepted;
  } catch {
    return { accepted: true };
  }
}
