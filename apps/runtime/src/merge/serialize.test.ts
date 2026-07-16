/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import { serializeWorkspaceGit } from "./serialize";

describe("serializeWorkspaceGit — per-workspace git critical section (BRO-1802/BRO-1914)", () => {
  test("same cwd: critical sections do NOT interleave (deterministic mutex proof)", async () => {
    const events: string[] = [];
    const cs = (tag: string) => async () => {
      events.push(`${tag}:enter`);
      await new Promise((r) => setTimeout(r, 5)); // yield — an unlocked body would let the other enter here
      events.push(`${tag}:exit`);
      return tag;
    };
    const [a, b] = await Promise.all([
      serializeWorkspaceGit("/ws", cs("A")),
      serializeWorkspaceGit("/ws", cs("B")),
    ]);
    expect([a, b]).toEqual(["A", "B"]);
    // A fully enters AND exits before B enters — no A:enter,B:enter,A:exit interleave.
    expect(events).toEqual(["A:enter", "A:exit", "B:enter", "B:exit"]);
  });

  test("distinct cwds run CONCURRENTLY (the lock is per-workspace, not global)", async () => {
    const order: string[] = [];
    const cs = (tag: string, ms: number) => async () => {
      order.push(`${tag}:enter`);
      await new Promise((r) => setTimeout(r, ms));
      order.push(`${tag}:exit`);
    };
    // X sleeps longer than Y; on distinct cwds Y must finish while X is still running (interleaved).
    await Promise.all([
      serializeWorkspaceGit("/wsX", cs("X", 20)),
      serializeWorkspaceGit("/wsY", cs("Y", 1)),
    ]);
    expect(order.indexOf("Y:exit")).toBeLessThan(order.indexOf("X:exit"));
    expect(order.slice(0, 2)).toEqual(["X:enter", "Y:enter"]); // both entered before either exited
  });

  test("a rejecting critical section does NOT wedge the next caller on the same cwd", async () => {
    const failing = serializeWorkspaceGit("/ws2", async () => {
      throw new Error("boom");
    });
    await expect(failing).rejects.toThrow("boom");
    // the chain must still accept + run the next holder (the tail never-rejects)
    const after = await serializeWorkspaceGit("/ws2", async () => "recovered");
    expect(after).toBe("recovered");
  });
});
