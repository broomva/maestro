/// <reference types="bun" />

// HistoryView (BRO-1893 FID-6, `MccHistory`) — renderToStaticMarkup (no DOM harness; the axis/filter
// interaction is history.pw.ts's concern). The pure view takes the already-derived `sessions` (the
// container `HistoryPage` reads the store; the projector's `selectHistory` is unit-tested in
// project.test.ts). Asserts: rows render from real receipts, the empty state guides the first run,
// the kind pill + axis/filter chrome render, and — the CLAUDE.md §Work-states invariant — NO progress
// percentage. Sessions here are built through the SAME projector the app uses (selectHistory over a
// fixture ServerTruth), so the test data can never drift from the real shape.

import { describe, expect, test } from "bun:test";
import type { LiveNode, LiveSession } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { emptyServerTruth, type HistorySession, selectHistory } from "@/store";
import { HistoryView } from "./history-page";

function node(p: Partial<LiveNode> & Pick<LiveNode, "id" | "kind" | "state">): LiveNode {
  return {
    id: p.id,
    path: p.path ?? p.id,
    parentId: p.parentId ?? null,
    kind: p.kind,
    state: p.state,
    owner: p.owner ?? null,
    gate: p.gate ?? "human",
    budgetJson: null,
    doneJson: null,
    title: p.title ?? null,
    createdAt: 1,
    updatedAt: p.updatedAt ?? 1,
  };
}

function session(id: string, nodeId: string): LiveSession {
  return {
    id,
    nodeId,
    branch: `run/${id}`,
    status: "review",
    startedAt: 1,
    endedAt: null,
    diffstatJson: null,
    updatedAt: 1,
  };
}

/** Real sessions — projected through selectHistory over a fixture ServerTruth (never hand-faked). */
function project(nodes: LiveNode[], sessions: LiveSession[]): HistorySession[] {
  const s = emptyServerTruth();
  for (const n of nodes) s.nodes[n.id] = n;
  for (const sess of sessions) s.sessions[sess.id] = sess;
  return selectHistory(s);
}

describe("HistoryView — the session list", () => {
  test("empty guides the first run (no rows manufactured)", () => {
    const html = renderToStaticMarkup(<HistoryView sessions={[]} />);
    expect(html).toContain('data-testid="view-history"');
    expect(html).toContain('data-testid="history-empty"');
    expect(html).toContain("No runs yet");
    // §Type: empty-state titles are the ONE place weight 600 is used (not 500). Lock it here.
    expect(html).toMatch(/font-semibold[^>]*>No runs yet/);
    expect(html).not.toContain('class="mcc-hrow"'); // no rows
  });

  test("renders a real row per dispatched leaf — title, agent, folder, run branch, kind pill", () => {
    const sessions = project(
      [
        node({ id: "i", kind: "initiative", state: "running", title: "hawthorne" }),
        node({ id: "p", parentId: "i", kind: "project", state: "running", title: "core" }),
        node({
          id: "w1",
          parentId: "p",
          kind: "task",
          state: "review",
          owner: "@ana",
          title: "Persist run transcripts",
        }),
      ],
      [session("s1", "w1")],
    );
    const html = renderToStaticMarkup(<HistoryView sessions={sessions} />);
    expect(html).toContain("Persist run transcripts"); // title
    expect(html).toContain("@ana"); // agent (owner, no worker on fixture)
    expect(html).toContain("hawthorne / core"); // folder crumb
    expect(html).toContain("run/s1"); // the branch receipt
    expect(html).toContain("mcc-hrow-kind--you"); // human @handle → "you"
  });

  test("an agent owner renders the 'loop' kind (autonomous, not 'you')", () => {
    const sessions = project(
      [node({ id: "w1", kind: "task", state: "running", owner: "agent:x", title: "Ran" })],
      [session("s1", "w1")],
    );
    const html = renderToStaticMarkup(<HistoryView sessions={sessions} />);
    expect(html).toContain("Ran");
    expect(html).toContain("mcc-hrow-kind--loop");
  });

  test("the §Work-states invariant — receipts, never a progress percentage", () => {
    const sessions = project(
      [node({ id: "w1", kind: "task", state: "running", owner: "agent:x", title: "Live" })],
      [session("s1", "w1")],
    );
    const html = renderToStaticMarkup(<HistoryView sessions={sessions} />);
    expect(html).not.toContain("%");
  });

  test("renders the organizing-axis toggle + you/autonomous filter chrome", () => {
    const sessions = project(
      [node({ id: "w1", kind: "task", state: "review", owner: "@ana", title: "One" })],
      [session("s1", "w1")],
    );
    const html = renderToStaticMarkup(<HistoryView sessions={sessions} />);
    expect(html).toContain('role="tablist"'); // the axis segmented control
    expect(html).toContain("By day");
    expect(html).toContain("Autonomous"); // the loop filter (plain voice, not "auto")
    expect(html).toContain("Search sessions");
  });

  test("groups render a header with a count (the default 'by day' axis)", () => {
    const sessions = project(
      [
        node({ id: "w1", kind: "task", state: "review", owner: "@ana", title: "A" }),
        node({ id: "w2", kind: "task", state: "running", owner: "agent:x", title: "B" }),
      ],
      [session("s1", "w1"), session("s2", "w2")],
    );
    const html = renderToStaticMarkup(<HistoryView sessions={sessions} />);
    expect(html).toContain("mcc-hgroup"); // a group header renders
    expect(html).toContain("mcc-hgroup-count"); // with its count
  });
});
