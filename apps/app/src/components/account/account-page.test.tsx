/// <reference types="bun" />

// Account page (BRO-1893 FID-6 slice 4, `MccUser`) — renderToStaticMarkup on the PURE panels, seeded by
// prop (zustand's useStore reads the pinned initial snapshot under renderToStaticMarkup, so the store
// read lives in the container and the panels are tested by props — the History/Inspector lesson). The
// two view branches are module-level components (OverviewPanel / AccountPanel), so BOTH halves are
// rendered in isolation here and the §Voice / §Work-states guards cover the whole page — not just the
// default Overview (P20 slice-4 coverage-gap finding).
//
// The Overview's "Your sessions" nav is TanStack <Link> (client-side, never a hard-reload <a href>), so
// those renders need router context — a loaded memory router provides it (the shell.test.tsx pattern).
//
// Asserts, per panel: it renders, honest labelling holds (real vs sample/preview clearly marked, no
// faked persistence, no false "syncs to your profile"), REAL sessions render (+ honest empty state), and
// the copy holds §Work-states (no progress %) + §Voice (no em dash, no build-phase jargon).

import { beforeAll, describe, expect, test } from "bun:test";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { HistorySession } from "@/store";
import { AccountPanel, AccountView, OverviewPanel } from "./account-page";

const noop = () => {};

/** Render a node inside a loaded memory router so its <Link>s resolve (the shell.test.tsx pattern). */
async function renderInRouter(node: ReactNode): Promise<string> {
  const rootRoute = createRootRoute({ component: () => node });
  const routes = ["/", "/history", "/account"].map((path) =>
    createRoute({ getParentRoute: () => rootRoute, path, component: () => null }),
  );
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: ["/account"] }),
  });
  await router.load();
  return renderToStaticMarkup(<RouterProvider router={router} />);
}

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

/** §Voice + §Work-states guards applied to any rendered markup — reused for both panels. */
function assertVoice(html: string) {
  expect(html).not.toContain("%"); // §Work-states — no progress percentage
  expect(html).not.toContain("—"); // §Voice — em dash (U+2014)
  expect(html).not.toContain("P1");
  expect(html).not.toContain("primitive");
  expect(html).not.toContain("engine room"); // disclosure ladder — do not name the substrate
}

describe("AccountView — shell + top bar", () => {
  let html = "";
  beforeAll(async () => {
    html = await renderInRouter(<AccountView sessions={SESSIONS} />);
  });

  test("renders the page shell with the testid + the honest 'sample' scope chip", () => {
    expect(html).toContain('data-testid="view-account"');
    expect(html).toContain("Ana Diaz");
    expect(html).toContain("sample");
    // the chip scope now names identity + security as sample too (not only the score) — no over-scope.
    expect(html).toContain("your profile, autonomy score, and security are sample");
    // the disclosure is screen-reader accessible (a visually-hidden companion), not only a hover title.
    expect(html).toContain('class="sr-only"');
  });
});

describe("OverviewPanel", () => {
  let html = "";
  beforeAll(async () => {
    html = await renderInRouter(<OverviewPanel sessions={SESSIONS} onEdit={noop} />);
  });

  test("the autonomy score hero — receipts, the number this product is really about", () => {
    expect(html).toContain("Your autonomy score");
    expect(html).toContain("Unsupervised this week");
    expect(html).toContain("Times you had to look");
    expect(html).toContain("Longest single run");
  });

  test("REAL sessions render as rows with their receipts (title + folder + you/loop kind)", () => {
    for (const s of SESSIONS) {
      expect(html).toContain(s.title);
      expect(html).toContain(s.folder);
    }
    expect(html).toContain("usr-sess-kind--you");
    expect(html).toContain("usr-sess-kind--loop");
  });

  test("internal nav is client-side <Link> targeting /history (never a hard-reload)", () => {
    // TanStack <Link> renders <a href="/history">; the no-reload behavior is asserted in the pw test.
    expect(html).toContain('href="/history"');
  });

  test("Preferences is honestly a 'preview' (persists nothing) — NOT a 'this device' persistence claim", () => {
    expect(html).toContain("preview");
    expect(html).not.toContain("this device"); // the fake-persistence receipt was removed (P20 major)
  });

  test("honest empty state when there are no runs", async () => {
    const empty = await renderInRouter(<OverviewPanel sessions={[]} onEdit={noop} />);
    expect(empty).toContain("No runs yet");
    expect(empty).not.toContain("usr-sess-kind"); // no fabricated rows
  });

  test("§Voice + §Work-states hold across the Overview markup", () => {
    assertVoice(html);
  });
});

describe("AccountPanel (the editable account view)", () => {
  let html = "";
  beforeAll(async () => {
    html = await renderInRouter(<AccountPanel onEdit={noop} />);
  });

  test("the identity form is honest — 'not saved yet', never the false 'syncs to your profile'", () => {
    expect(html).toContain("not saved yet");
    expect(html).not.toContain("syncs to your profile");
  });

  test("the two inert preference rows carry a 'preview' affordance (distinct from the live Theme)", () => {
    expect(html).toContain("Theme");
    expect(html).toContain("Writes live");
    expect(html).toContain("preview"); // on Keyboard shortcuts + Reduced motion
  });

  test("destructive red is confined to genuine destructive actions (revoke + sign out)", () => {
    expect(html).toContain("usr-danger"); // the device revokes
    expect(html).toContain("Sign out");
    // Sign out sits outside the sample-labelled cards, so it carries its own sample affordance (no no-op
    // destructive action presented as if it were wired).
    expect(html).toContain("usr-signout");
  });

  test("§Voice + §Work-states hold across the Account markup (the copy-denser half)", () => {
    assertVoice(html);
  });
});
