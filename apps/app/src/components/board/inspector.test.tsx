/// <reference types="bun" />

// Inspector (BRO-1825, M3 stub) — renderToStaticMarkup (no DOM harness, runs under CI's plain bun
// test; the selection interaction is board-m3.pw.ts's concern). Asserts the receipts render, the empty
// state guides selection, and — the CLAUDE.md §Work-states invariant — NO progress percentage anywhere.

import { describe, expect, test } from "bun:test";
import type { WorkItem } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { Inspector } from "./inspector";

const base: WorkItem = {
  id: "deploy",
  state: "review",
  kind: "task",
  title: "Approve the deploy",
  gate: "human",
  path: "gate/deploy",
  updatedAt: "2026-07-11T00:00:00.000Z",
};

describe("Inspector — the M5 receipts stub", () => {
  test("empty state guides selection (nothing selected)", () => {
    const html = renderToStaticMarkup(<Inspector item={null} />);
    expect(html).toContain('data-testid="inspector-empty"');
    expect(html).toContain("Select work to see its receipts.");
  });

  test("renders the receipts of a selected item — title, branch, verdict, reason, and the gate look", () => {
    const item: WorkItem = {
      ...base,
      run: "run/ab12cd",
      verdict: "2 checks passed",
      reason: "waiting on your approval",
      initiative: "Platform",
      project: "Deploy pipeline",
      look: {
        ran: "e2e + typecheck",
        decided: ["kept the migration", "skipped the cache warm"],
        ask: "approve the deploy?",
      },
    };
    const html = renderToStaticMarkup(<Inspector item={item} />);
    expect(html).toContain('data-testid="inspector"');
    expect(html).toContain("Approve the deploy");
    expect(html).toContain("run/ab12cd"); // the branch receipt
    expect(html).toContain("2 checks passed"); // verdict
    expect(html).toContain("Platform › Deploy pipeline"); // crumb
    // The gate look — what ran · decided · asks
    expect(html).toContain('data-testid="inspector-look"');
    expect(html).toContain("e2e + typecheck");
    expect(html).toContain("kept the migration");
    expect(html).toContain("approve the deploy?");
    // The invariant: receipts, never a progress percentage.
    expect(html).not.toContain("%");
  });

  test("renders the lifecycle rail (read-only progression) + the 'Done is earned' note", () => {
    const html = renderToStaticMarkup(<Inspector item={base} />);
    expect(html).toContain('data-testid="inspector-rail"');
    expect(html).toContain("mc-rail"); // the ported rail
    // The four plain-voice stages (the app collapses proposed/reviewing/triggered → Queued).
    expect(html).toContain("Queued");
    expect(html).toContain("Running");
    expect(html).toContain("Needs you"); // review label
    expect(html).toContain("Done");
    // review → the "Needs you" stage is the current one (the exact rendered pairing).
    expect(html).toContain(
      'mc-rail-stage is-current" aria-current="step"><span class="mc-rail-dot"></span><span class="mc-rail-name">Needs you',
    );
    expect(html).toContain("Done is earned"); // the rail note (canon copy)
    expect(html).not.toContain("%");
  });

  test("the activity-timeline stub is honest + keyed on sessionId", () => {
    // A dispatched item (has a session) → the timeline lands with the session read path.
    const dispatched = renderToStaticMarkup(<Inspector item={{ ...base, sessionId: "s1" }} />);
    expect(dispatched).toContain("activity timeline and diffstat");
    expect(dispatched).toContain("lands in P1");
    expect(dispatched).not.toContain("%");
    // A never-run item (no session) → an honest "no run yet" line, never faked activity.
    const neverRun = renderToStaticMarkup(<Inspector item={base} />);
    expect(neverRun).toContain("No run yet");
    expect(neverRun).not.toContain("activity timeline and diffstat");
  });

  test("omits absent receipts (a bare item renders no branch/verdict/look rows)", () => {
    const html = renderToStaticMarkup(<Inspector item={base} />);
    expect(html).toContain("Approve the deploy");
    expect(html).not.toContain('data-testid="inspector-look"'); // no look on a bare item
    expect(html).not.toContain("Branch"); // no run → no branch receipt
    expect(html).not.toContain("%");
  });
});
