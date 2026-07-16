/// <reference types="bun" />

// Inspector (BRO-1825 M3 stub → BRO-1809 M5) — renderToStaticMarkup (no DOM harness, runs under CI's
// plain bun test; the verb INTERACTION is inspector-m5.pw.ts's concern). Asserts the receipts render, the
// empty state guides selection, the CLAUDE.md §Work-states invariant (NO progress percentage), and — M5 —
// that a gate item renders its verbs (approve / send back / block / escalate) while a non-gate item does not.

import { describe, expect, test } from "bun:test";
import type { Intent, WorkItem } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { Inspector } from "./inspector";

// The verbs need a dispatcher; static markup never fires it, so a no-op suffices for these render tests.
const noop = async (_intent: Intent) => {};
const render = (item: WorkItem | null) =>
  renderToStaticMarkup(<Inspector item={item} onIntent={noop} />);

const base: WorkItem = {
  id: "deploy",
  state: "review",
  kind: "task",
  title: "Approve the deploy",
  gate: "human",
  path: "gate/deploy",
  updatedAt: "2026-07-11T00:00:00.000Z",
};

describe("Inspector — the M5 receipts + verbs", () => {
  test("empty state guides selection (nothing selected)", () => {
    const html = render(null);
    expect(html).toContain('data-testid="inspector-empty"');
    expect(html).toContain("Select work to see its receipts.");
  });

  test("renders the receipts of a selected item — title, branch, verdict, reason, and the gate look", () => {
    const item: WorkItem = {
      ...base,
      gateId: "g1",
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
    const html = render(item);
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
    const html = render(base);
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

  // ── M5: the verbs (rung-3 control) ────────────────────────────────────────────
  test("a review item renders its gate verbs — approve, send back (primary) + block, escalate (secondary)", () => {
    const html = render({ ...base, gateId: "g1" });
    expect(html).toContain('data-testid="inspector-verbs"');
    expect(html).toContain(">Approve<");
    expect(html).toContain("Send back");
    expect(html).toContain(">Block<");
    expect(html).toContain(">Escalate<");
    // "Needs you" verbs are accent-blue, never red (no destructive-red styling on the block verb).
    expect(html).not.toContain("%");
  });

  test("a blocked (Stuck) item renders only Redispatch (no gate row → no verdict verbs)", () => {
    const html = render({ ...base, state: "blocked", reason: "the deploy key expired" });
    expect(html).toContain('data-testid="inspector-verbs"');
    expect(html).toContain("Redispatch");
    expect(html).not.toContain(">Approve<");
    expect(html).not.toContain(">Block<");
  });

  test("a running (non-gate) item renders receipts but NO verbs (nothing to decide)", () => {
    const html = render({ ...base, state: "running", run: "run/live99" });
    expect(html).toContain('data-testid="inspector"');
    expect(html).not.toContain('data-testid="inspector-verbs"');
    expect(html).not.toContain(">Approve<");
  });

  test("never renders engine-room strings — no worktree paths, index.db, or the run's on-disk directory", () => {
    // Even if a hostile projection leaked them onto the fields the inspector reads, it renders only the
    // receipts (run branch, verdict, look) — the disclosure ladder never exposes the engine room.
    const html = render({
      ...base,
      gateId: "g1",
      run: "run/ab12cd",
      verdict: "2 checks passed",
      look: { ran: "e2e", decided: [], ask: "approve?" },
    });
    for (const s of ["worktree", "index.db", ".maestro", "/runs/", "run-deploy/"]) {
      expect(html).not.toContain(s);
    }
  });

  test("the activity-timeline stub is honest + keyed on sessionId (plain voice — no em dash, no 'P1')", () => {
    // A dispatched item (has a session) → the timeline opens when its run events are recorded.
    const dispatched = render({ ...base, sessionId: "s1" });
    // (the apostrophe in "run's" renders as &#x27; in static markup, so match around it)
    expect(dispatched).toContain("activity timeline and diffstat open once");
    expect(dispatched).toContain("events are recorded");
    expect(dispatched).not.toContain("%");
    // A never-run item (no session) → an honest "no run yet" line, never faked activity.
    const neverRun = render(base);
    expect(neverRun).toContain("No run yet");
    expect(neverRun).not.toContain("activity timeline and diffstat");
    // CLAUDE.md §Voice: no em dashes, no internal build-phase names in the user-facing copy.
    for (const h of [dispatched, neverRun]) {
      expect(h).not.toContain("—"); // em dash
      expect(h).not.toContain("P1"); // internal build-phase jargon
    }
  });

  test("omits absent receipts (a bare item renders no branch/verdict/look rows)", () => {
    const html = render(base);
    expect(html).toContain("Approve the deploy");
    expect(html).not.toContain('data-testid="inspector-look"'); // no look on a bare item
    expect(html).not.toContain("Branch"); // no run → no branch receipt
    expect(html).not.toContain("%");
  });
});
