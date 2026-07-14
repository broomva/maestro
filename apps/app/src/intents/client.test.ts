/// <reference types="bun" />

// Intent write client (BRO-1888 FID-3) — the one write path. Anti-vacuity [[self-hosting-vacuous-pass]]:
// every case asserts a concrete wire fact (URL, method, the Idempotency-Key, the JSON body) or a concrete
// failure (a non-2xx becomes a typed IntentError carrying the API §4 code). A fetch double is the seam.

import { describe, expect, test } from "bun:test";
import { IDEMPOTENCY_KEY_HEADER, INTENTS_ENDPOINT, type Intent } from "@maestro/protocol";
import { type FetchLike, IntentError, postIntent } from "./client";

/** A fetch double that records the call and returns a scripted response. */
function recorder(res: () => Response) {
  const calls: { url: string; init?: RequestInit }[] = [];
  const fetchImpl: FetchLike = async (url, init) => {
    calls.push({ url: String(url), init });
    return res();
  };
  return { calls, fetchImpl };
}

const approve: Intent = { type: "approve", gateId: "g1" };

describe("postIntent — the intent write path", () => {
  test("POSTs the intent to /api/intents with the JSON body + the Idempotency-Key header", async () => {
    const { calls, fetchImpl } = recorder(
      () => new Response(JSON.stringify({ accepted: true }), { status: 202 }),
    );
    const ack = await postIntent(approve, { fetchImpl, idempotencyKey: "key-123" });

    expect(ack).toEqual({ accepted: true });
    expect(calls).toHaveLength(1);
    const call = calls[0];
    expect(call?.url).toBe(INTENTS_ENDPOINT);
    expect(call?.init?.method).toBe("POST");
    const headers = call?.init?.headers as Record<string, string>;
    expect(headers[IDEMPOTENCY_KEY_HEADER]).toBe("key-123");
    expect(headers["content-type"]).toBe("application/json");
    expect(JSON.parse(String(call?.init?.body))).toEqual(approve);
  });

  test("honours the baseUrl prefix (a non-same-origin runtime)", async () => {
    const { calls, fetchImpl } = recorder(() => new Response(null, { status: 202 }));
    await postIntent(approve, { fetchImpl, baseUrl: "http://localhost:4319" });
    expect(calls[0]?.url).toBe(`http://localhost:4319${INTENTS_ENDPOINT}`);
  });

  test("mints an Idempotency-Key when none is given (a retried POST can be de-duped)", async () => {
    const { calls, fetchImpl } = recorder(() => new Response(null, { status: 202 }));
    await postIntent(approve, { fetchImpl });
    const headers = calls[0]?.init?.headers as Record<string, string>;
    const key = headers[IDEMPOTENCY_KEY_HEADER];
    expect(key).toBeTruthy();
    expect(key?.length ?? 0).toBeGreaterThan(8);
  });

  test("tolerates an empty / non-JSON 2xx body — the ack is advisory, the stream is truth", async () => {
    const { fetchImpl } = recorder(() => new Response("", { status: 202 }));
    expect(await postIntent(approve, { fetchImpl })).toEqual({ accepted: true });
  });

  test("a non-2xx becomes a typed IntentError carrying the API §4 code + status", async () => {
    const { fetchImpl } = recorder(
      () =>
        new Response(
          JSON.stringify({
            error: { message: "gate already resolved", code: "gate_closed", retryable: false },
          }),
          { status: 409 },
        ),
    );
    let caught: unknown;
    try {
      await postIntent(approve, { fetchImpl });
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(IntentError);
    const err = caught as IntentError;
    expect(err.status).toBe(409);
    expect(err.code).toBe("gate_closed");
    expect(err.message).toBe("gate already resolved");
    expect(err.retryable).toBe(false);
  });

  test("a non-2xx with a non-JSON body keeps a status-derived message (never throws while parsing)", async () => {
    const { fetchImpl } = recorder(() => new Response("<html>502</html>", { status: 502 }));
    let caught: unknown;
    try {
      await postIntent(approve, { fetchImpl });
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(IntentError);
    expect((caught as IntentError).status).toBe(502);
  });
});
