/// <reference types="bun" />

import { describe, expect, test } from "bun:test";
import { MAESTRO_PROTOCOL_VERSION } from "@maestro/protocol";
import { createApp, type HealthReport, RUNTIME_VERSION } from "./app";
import { DEFAULT_PORT, loadConfig, type RuntimeConfig } from "./config";

const testConfig: RuntimeConfig = {
  port: DEFAULT_PORT,
  workspace: "/tmp/ws",
  indexPath: "/tmp/ws/.maestro/index.db",
  lockPath: "/tmp/ws/.maestro/runtime.lock",
};

describe("GET /health", () => {
  test("returns 200 with ok, protocol version, workspace, and index stub", async () => {
    const app = createApp(testConfig, Date.now());
    const res = await app.request("/health");
    expect(res.status).toBe(200);
    const body = (await res.json()) as HealthReport;
    expect(body.ok).toBe(true);
    expect(body.service).toBe("@maestro/runtime");
    expect(body.version).toBe(RUNTIME_VERSION);
    expect(body.protocol).toBe(MAESTRO_PROTOCOL_VERSION);
    expect(body.workspace).toBe("/tmp/ws");
    expect(body.index).toEqual({ path: "/tmp/ws/.maestro/index.db", status: "stub" });
    expect(Number.isInteger(body.uptime_s)).toBe(true);
    expect(body.uptime_s).toBeGreaterThanOrEqual(0);
  });

  test('the response carries the literal "ok" the P0-exit check greps for', async () => {
    const app = createApp(testConfig, Date.now());
    const text = await (await app.request("/health")).text();
    expect(text).toContain('"ok"');
  });

  test("echoes the protocol version header", async () => {
    const app = createApp(testConfig, Date.now());
    const res = await app.request("/health");
    expect(res.headers.get("x-maestro-protocol")).toBe(String(MAESTRO_PROTOCOL_VERSION));
  });

  test("an unknown route 404s", async () => {
    const app = createApp(testConfig, Date.now());
    expect((await app.request("/nope")).status).toBe(404);
  });
});

describe("loadConfig", () => {
  test("defaults: cwd workspace, default port, derived index + lock paths", () => {
    const cfg = loadConfig({});
    expect(cfg.port).toBe(DEFAULT_PORT);
    expect(cfg.workspace).toBe(process.cwd());
    expect(cfg.indexPath.endsWith(".maestro/index.db")).toBe(true);
    expect(cfg.lockPath.endsWith(".maestro/runtime.lock")).toBe(true);
  });

  test("env overrides port, workspace, and index path (absolute)", () => {
    const cfg = loadConfig({
      MAESTRO_PORT: "5000",
      MAESTRO_WORKSPACE: "/tmp/ws",
      MAESTRO_INDEX: "/custom/i.db",
    });
    expect(cfg.port).toBe(5000);
    expect(cfg.workspace).toBe("/tmp/ws");
    expect(cfg.indexPath).toBe("/custom/i.db");
  });

  test("a relative MAESTRO_INDEX resolves against the workspace, not cwd", () => {
    const cfg = loadConfig({ MAESTRO_WORKSPACE: "/tmp/ws", MAESTRO_INDEX: "data/i.db" });
    expect(cfg.indexPath).toBe("/tmp/ws/data/i.db");
  });

  test("an invalid port falls back to the default", () => {
    expect(loadConfig({ MAESTRO_PORT: "abc" }).port).toBe(DEFAULT_PORT);
    expect(loadConfig({ MAESTRO_PORT: "-5" }).port).toBe(DEFAULT_PORT);
    expect(loadConfig({ MAESTRO_PORT: "0" }).port).toBe(DEFAULT_PORT);
    // above the TCP u16 ceiling → fall back, never silently clamp to 65535
    expect(loadConfig({ MAESTRO_PORT: "70000" }).port).toBe(DEFAULT_PORT);
    expect(loadConfig({ MAESTRO_PORT: "65536" }).port).toBe(DEFAULT_PORT);
    // the boundary itself is valid
    expect(loadConfig({ MAESTRO_PORT: "65535" }).port).toBe(65535);
  });
});
