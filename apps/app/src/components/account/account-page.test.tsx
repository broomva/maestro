/// <reference types="bun" />

// Account page (BRO-1893 FID-6 slice 4, `MccUser`) — renderToStaticMarkup on the PURE `AccountView`,
// seeded with a session fixture by prop (zustand's useStore reads the pinned initial snapshot under
// renderToStaticMarkup, so the store read lives in the container and the view is tested by props — the
// History/Inspector lesson). The default view is Overview; the editable Account view + the live theme
// write-through are account.pw.ts's concern (they need a real DOM + the segmented toggle).
//
// Asserts: the page + identity + autonomy-score hero render, the "sample" honesty affordance is present,
// REAL sessions render as rows (and the empty state is honest), and the copy holds §Work-states (no
// progress %) + §Voice (no em dash, no build-phase jargon).

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { HistorySession } from "@/store";
import { AccountView } from "./account-page";

const SESSIONS: HistorySession[] = [
  {
    id: "s1",
    title: "Wire the approve path",
    state: "running",
    agent: "claude",
    kind: "loop",
    folder: "maestro / runtime",
    at: "2026-07-14T12:00:00.000Z",
  },
  {
    id: "s2",
    title: "Draft the launch note",
    state: "review",
    agent: "@ana",
    kind: "you",
    folder: "maestro / marketing",
    run: "run/abc123",
    at: "2026-07-14T09:00:00.000Z",
  },
  {
    id: "s3",
    title: "Nightly dependency sweep",
    state: "done",
    agent: "maestro",
    kind: "loop",
    folder: "maestro / ops",
    at: "2026-07-13T22:00:00.000Z",
  },
];

describe("AccountView — overview (default)", () => {
  const html = renderToStaticMarkup(<AccountView sessions={SESSIONS} />);

  test("renders the page + identity", () => {
    expect(html).toContain('data-testid="view-account"');
    expect(html).toContain("Ana Diaz");
    expect(html).toContain("Operator · Owner");
  });

  test("the autonomy score hero — the number this product is really about", () => {
    expect(html).toContain("Your autonomy score");
    expect(html).toContain("Unsupervised this week");
    expect(html).toContain("Times you had to look");
    expect(html).toContain("Longest single run");
  });

  test("the honest 'sample' affordance (autonomy score is sample; sessions + theme are real)", () => {
    expect(html).toContain("sample");
    expect(html).toContain("autonomy ledger");
  });

  test("REAL sessions render as rows with their receipts (title + folder)", () => {
    for (const s of SESSIONS) {
      expect(html).toContain(s.title);
      expect(html).toContain(s.folder);
    }
    // the you / loop kind badge (a projection of who ran it)
    expect(html).toContain("usr-sess-kind--you");
    expect(html).toContain("usr-sess-kind--loop");
  });

  test("§Work-states — no progress percentage anywhere on the page", () => {
    expect(html).not.toContain("%");
  });

  test("§Voice — no em dash, no build-phase jargon in user-facing copy", () => {
    expect(html).not.toContain("—"); // em dash (U+2014)
    expect(html).not.toContain("P1");
    expect(html).not.toContain("primitive");
    expect(html).not.toContain("engine room"); // disclosure ladder — do not name the substrate
  });

  test("does not make the false 'syncs to your profile' claim (that card is account-view + honest)", () => {
    expect(html).not.toContain("syncs to your profile");
  });
});

describe("AccountView — overview with no runs", () => {
  const html = renderToStaticMarkup(<AccountView sessions={[]} />);

  test("the sessions card shows an honest empty state, not a fabricated list", () => {
    expect(html).toContain("No runs yet");
    expect(html).not.toContain("usr-sess-kind"); // no session rows
  });
});
