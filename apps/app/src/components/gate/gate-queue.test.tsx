/// <reference types="bun" />

// Gate queue structure (BRO-1888) — renderToStaticMarkup (no DOM harness; the interactive grace window
// is gate-queue.pw.ts's concern). Asserts: the empty "all clear" state, that review + blocked leaves
// render as cards with a title + the ran receipt, the "Needs you" dot is accent-blue, and — the
// CLAUDE.md §Work-states invariant — NO progress percentage anywhere.

import { describe, expect, test } from "bun:test";
import type { WorkItem } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { GateQueue } from "./gate-queue";

const noop = async () => {};

const review: WorkItem = {
  id: "g1",
  state: "review",
  kind: "task",
  title: "Approve the deploy",
  gate: "human",
  path: "hawthorne/gate",
  updatedAt: "2026-07-14T00:00:00.000Z",
  gateId: "gate-1",
  run: "run/7c2f1a",
  look: {
    ran: "2h 14m unsupervised · judge passed",
    decided: ["persist transcripts"],
    ask: "merge it",
  },
};
const blocked: WorkItem = {
  id: "b1",
  state: "blocked",
  kind: "task",
  title: "Waiting on a token",
  gate: "human",
  path: "hawthorne/stuck",
  updatedAt: "2026-07-14T00:00:00.000Z",
  reason: "the deploy key expired",
};

describe("GateQueue — structure", () => {
  test("empty → the all-clear line, not a card", () => {
    const html = renderToStaticMarkup(<GateQueue items={[]} onIntent={noop} />);
    expect(html).toContain('data-testid="gate-allclear"');
    expect(html).toContain("Nothing at your gate");
    expect(html).not.toContain('data-testid="gate-card"');
  });

  test("renders a card per item with its title + the ran receipt; no progress percentage", () => {
    const html = renderToStaticMarkup(<GateQueue items={[review, blocked]} onIntent={noop} />);
    expect(html).toContain('data-testid="gate-queue"');
    expect(html).toContain('data-state="review"');
    expect(html).toContain('data-state="blocked"');
    expect(html).toContain("Approve the deploy");
    expect(html).toContain("Waiting on a token");
    expect(html).toContain("2h 14m unsupervised · judge passed"); // look.ran on the card
    // Receipts, never a percentage (CLAUDE.md §Work states — the single loudest canon rule).
    expect(html).not.toContain("%");
  });

  test('the "Needs you" (review) dot is accent-blue, never red', () => {
    const html = renderToStaticMarkup(<GateQueue items={[review]} onIntent={noop} />);
    // The dot's inline background is the accent tone var (STATUS_DOT_VAR.accent = --bv-blue-accent).
    expect(html).toMatch(/bv-blue-accent/);
    expect(html).not.toMatch(/\bred\b|#f00|crimson/i);
  });
});
