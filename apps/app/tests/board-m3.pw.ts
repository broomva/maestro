import { expect, test } from "@playwright/test";

// board-m3 (BRO-1825) — the M3 board acceptance gate, in a real browser. Proves the M3 upgrade of the
// read board: a RUNNING card wears the Undertow live, "Needs you" (review) is surfaced FIRST in
// accent-blue (never red), selection drives the inspector, and there is NO progress percentage anywhere.
//
// HERMETIC: the board data is mocked at the network layer (page.route on /api/tree + /api/stream), NOT a
// real runtime. Two reasons: (1) it makes the M3 VISUALS deterministic — the exact review/running/queued
// mix, no dispatch timing; (2) it avoids booting a second runtime on the shared 4319 vite-preview proxy
// port that p1-exit.pw.ts already owns (a parallel-worker port clash). The live-SSE seam itself
// (fs → watcher → node.updated → SSE → store → board, with no reload) is proven end-to-end by p1-exit ①;
// this spec exercises the real Board / store / WorkCard / Inspector tree over a controlled data source.

const T = 1_700_000_000_000;

// A /api/tree LiveNode (NodeRow minus deletedAt) — the shape the store hydrates + deriveWorkItem projects.
const node = (o: {
  id: string;
  path: string;
  state: string;
  title: string;
  updatedAt?: number;
}) => ({
  id: o.id,
  path: o.path,
  parentId: null,
  kind: "task",
  state: o.state,
  owner: null,
  gate: "human",
  budgetJson: null,
  doneJson: null,
  title: o.title,
  createdAt: T,
  updatedAt: o.updatedAt ?? T,
});

const NODES = [
  node({
    id: "run1",
    path: "build",
    state: "running",
    title: "Building the runner",
    updatedAt: T + 2,
  }),
  node({
    id: "review1",
    path: "gate",
    state: "review",
    title: "Approve the deploy",
    updatedAt: T + 3,
  }),
  node({ id: "queue1", path: "later", state: "proposed", title: "Queued task", updatedAt: T + 1 }),
];

test.beforeEach(async ({ page }) => {
  // Hydrate the board from a controlled work tree.
  await page.route("**/api/tree*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ nodes: NODES }) }),
  );
  // EventSource connects, reads a far-future retry (so it does not reconnect-loop during the test), then
  // the fulfilled response closes — the board stays hydrated from /api/tree either way.
  await page.route("**/api/stream*", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      headers: { "cache-control": "no-cache" },
      body: "retry: 3600000\n\n",
    }),
  );
});

test("Needs you is surfaced first in accent-blue; a running card wears the Undertow", async ({
  page,
}) => {
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible();

  // Attention-first: the FIRST board group is review ("Needs you"), never a failure tone.
  const groups = page.locator('[data-testid^="board-group-"]');
  await expect(groups.first()).toHaveAttribute("data-testid", "board-group-review");
  await expect(
    page.getByTestId("board-group-review").getByText("Approve the deploy"),
  ).toBeVisible();

  // Accent-blue, never red: the headline count wears the accent-blue token.
  const needsYou = page.getByTestId("needs-you");
  await expect(needsYou).toBeVisible();
  await expect(needsYou).toHaveClass(/bv-blue-accent/);

  // Exactly the ONE running card wears the Undertow — the two non-running cards (review + queued) must
  // NOT be haloed. Asserting the total halo count (not a has-filter) discriminates: if every card were
  // haloed this fails, so it proves the signal is exclusive to running.
  const cards = page.locator('[data-testid="work-card"]');
  await expect(cards).toHaveCount(3);
  const running = page.locator('[data-testid="work-card"][data-running]');
  await expect(running).toHaveCount(1);
  await expect(running).toContainText("Building the runner");
  await expect(page.locator(".bv-undertow")).toHaveCount(1); // ONLY the running card is haloed
  await expect(page.locator('[data-testid="work-card"]:not([data-running])')).toHaveCount(2);

  // No progress percentages anywhere on the board (receipts, not progress).
  await expect(page.getByTestId("board")).not.toContainText("%");
});

test("selection drives the inspector — a selected card opens the receipts panel", async ({
  page,
}) => {
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible();

  // No inspector until something is selected (board is the primary surface).
  await expect(page.getByTestId("inspector-panel")).toHaveCount(0);

  // Click a NON-first card (the running card, which is NOT the review group that renders first) so the
  // assertion distinguishes "reflects the selection" from "always shows the first item".
  await page.getByTestId("board-group-running").getByText("Building the runner").click();
  const panel = page.getByTestId("inspector-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByTestId("inspector")).toContainText("Building the runner");
  await expect(panel.getByTestId("inspector")).not.toContainText("Approve the deploy");
  await expect(panel).not.toContainText("%"); // receipts, never a percentage

  // The inspector is DISMISSIBLE (P20 #2): Escape closes it and the board returns to full width.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("inspector-panel")).toHaveCount(0);

  // Re-clicking the same card TOGGLES: open, then click again to close (honest aria-pressed).
  const runningCard = page
    .locator('[data-testid="work-card"]', { hasText: "Building the runner" })
    .first();
  await runningCard.click();
  await expect(page.getByTestId("inspector-panel")).toBeVisible();
  await runningCard.click();
  await expect(page.getByTestId("inspector-panel")).toHaveCount(0);

  // The close button also dismisses it.
  await runningCard.click();
  await page.getByTestId("inspector-close").click();
  await expect(page.getByTestId("inspector-panel")).toHaveCount(0);
});
