/// <reference types="bun" />
// mock-model.test.ts — BRO-1806 (part of `bun test:loops`). The scripted upstream in isolation: the
// loops.test.ts integration proves it drives real F2→F3 flows; this pins the fixture's own contract.
// Anti-vacuity [[self-hosting-vacuous-pass]]: exact status/usage/body/order assertions.

import { describe, expect, test } from "bun:test";
import { createMockModel, DEFAULT_MOCK_USAGE_USD } from "./mock-model";

const req = (n = 0) => ({
  model: "claude-opus-4-8",
  role: "agent" as const,
  payload: { beat: n },
  apiKey: "sk-not-used",
});

describe("mock-model — scripted upstream", () => {
  test("consumes the script in order, then repeats the fallback", async () => {
    const mock = createMockModel({
      script: [{ usage: { usd: 0.1 } }, { status: 402, usage: undefined }],
      fallback: { usage: { usd: 0.9 } },
    });
    const r1 = await mock.forward(req(1));
    const r2 = await mock.forward(req(2));
    const r3 = await mock.forward(req(3));
    const r4 = await mock.forward(req(4));
    expect(r1.status).toBe(200);
    expect(r1.usage).toEqual({ usd: 0.1 });
    expect(r2.status).toBe(402);
    expect(r2.usage).toBeUndefined(); // explicitly non-billable
    expect(r3.usage).toEqual({ usd: 0.9 }); // fallback
    expect(r4.usage).toEqual({ usd: 0.9 }); // fallback repeats
  });

  test("a bare {} response is a 200 with the default per-call usage", async () => {
    const mock = createMockModel();
    const r = await mock.forward(req());
    expect(r.status).toBe(200);
    expect(r.usage).toEqual({ usd: DEFAULT_MOCK_USAGE_USD });
    // body is Anthropic-shaped
    expect(r.body).toMatchObject({ type: "message", role: "assistant" });
  });

  test("usagePerCallUsd overrides the default cost", async () => {
    const mock = createMockModel({ usagePerCallUsd: 0.25 });
    const r = await mock.forward(req());
    expect(r.usage).toEqual({ usd: 0.25 });
  });

  test("records every call in order", async () => {
    const mock = createMockModel();
    await mock.forward(req(1));
    await mock.forward({ ...req(2), role: "verifier" });
    expect(mock.calls).toEqual([
      { model: "claude-opus-4-8", role: "agent", payload: { beat: 1 } },
      { model: "claude-opus-4-8", role: "verifier", payload: { beat: 2 } },
    ]);
  });

  test("delayMs awaits the injected sleep before answering (kill-mid-call hook)", async () => {
    const slept: number[] = [];
    const mock = createMockModel({
      script: [{ delayMs: 5000 }],
      sleep: async (ms) => {
        slept.push(ms);
      },
    });
    const r = await mock.forward(req());
    expect(slept).toEqual([5000]);
    expect(r.status).toBe(200);
  });

  test("forward never throws (an upstream throw would be the proxy's 502, not a scripted path)", async () => {
    const mock = createMockModel({ script: [{ status: 500, body: { error: "boom" } }] });
    const r = await mock.forward(req());
    expect(r.status).toBe(500);
    expect(r.body).toEqual({ error: "boom" });
  });
});
