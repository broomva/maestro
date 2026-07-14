import { expect, test } from "@playwright/test";

// Mission plane (BRO-1825 M3 board → BRO-1886 fidelity), in a real browser. Proves the plane: the
// default FEED renders LEAF work attention-first ("Needs you"/review FIRST in accent-blue, never
// red), a RUNNING card wears the Undertow, container folders are NOT shown as cards, selection drives
// the inspector, the view toggle switches feed/board/list, and there is NO progress percentage.
//
// HERMETIC: the data is mocked at the network layer (page.route on /api/tree + /api/stream), NOT a
// real runtime — deterministic visuals + no port clash with p1-exit.pw.ts. The live-SSE seam itself is
// proven end-to-end by p1-exit ①; this spec exercises the real Board / store / plane / WorkCard /
// Inspector tree over a controlled data source.

const T = 1_700_000_000_000;

// A /api/tree LiveNode (NodeRow minus deletedAt) — the shape the store hydrates + deriveWorkItem projects.
const node = (o: {
  id: string;
  path: string;
  state: string;
  title: string;
  kind?: string;
  parentId?: string | null;
  updatedAt?: number;
}) => ({
  id: o.id,
  path: o.path,
  parentId: o.parentId ?? null,
  kind: o.kind ?? "task",
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
  // A container folder — it must NOT surface as a plane card (leaf-only; the sidebar tree owns folders).
  node({
    id: "proj1",
    path: "hawthorne",
    state: "running",
    title: "hawthorne-core",
    kind: "project",
  }),
  node({
    id: "run1",
    path: "hawthorne/build",
    parentId: "proj1",
    state: "running",
    title: "Building the runner",
    updatedAt: T + 2,
  }),
  node({
    id: "review1",
    path: "hawthorne/gate",
    parentId: "proj1",
    state: "review",
    title: "Approve the deploy",
    updatedAt: T + 3,
  }),
  node({
    id: "queue1",
    path: "hawthorne/later",
    parentId: "proj1",
    state: "proposed",
    title: "Queued task",
    updatedAt: T + 1,
  }),
];

test.beforeEach(async ({ page }) => {
  await page.route("**/api/tree*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ nodes: NODES }) }),
  );
  await page.route("**/api/stream*", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      headers: { "cache-control": "no-cache" },
      body: "retry: 3600000\n\n",
    }),
  );
});

test("the feed shows leaf work attention-first; Needs you is accent-blue; running wears the Undertow", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();
  // Default view is the feed.
  await expect(page.getByTestId("plane-feed")).toBeVisible();

  // Attention-first: the FIRST group is review ("Needs you"), never a failure tone.
  const groups = page.locator('[data-testid^="board-group-"]');
  await expect(groups.first()).toHaveAttribute("data-testid", "board-group-review");
  await expect(
    page.getByTestId("board-group-review").getByText("Approve the deploy"),
  ).toBeVisible();

  // The "Needs you" group dot is accent-blue, never red (the tone → --bv-blue-accent join).
  const needsYouDot = page.getByTestId("board-group-review").locator(".mc-chip-dot").first();
  await expect(needsYouDot).toHaveAttribute("style", /bv-blue-accent/);

  // The triage headline is plain voice — "1 piece of work needs you".
  await expect(page.getByTestId("plane-feed")).toContainText("1 piece of work needs you");

  // LEAF-ONLY: the mock has 1 container + 3 leaf tasks, and exactly 3 cards render — the container
  // folder is NOT a card (it lives in the sidebar tree). Its title still appears as the leaf cards'
  // crumb ("hawthorne › hawthorne-core"), which is correct; the count is the leaf-only proof.
  const cards = page.locator('[data-testid="work-card"]');
  await expect(cards).toHaveCount(3);

  // Exactly the ONE running card wears the Undertow — the two non-running cards must NOT be haloed.
  const running = page.locator('[data-testid="work-card"][data-running]');
  await expect(running).toHaveCount(1);
  await expect(running).toContainText("Building the runner");
  await expect(page.locator(".bv-undertow")).toHaveCount(1);
  await expect(page.locator('[data-testid="work-card"]:not([data-running])')).toHaveCount(2);

  // No progress percentages anywhere (receipts, not progress).
  await expect(page.getByTestId("board")).not.toContainText("%");
});

test("the view toggle switches feed → board → list", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("plane-feed")).toBeVisible();

  // Board: four fixed columns, still leaf-only (3 cards), no percentage.
  await page.getByRole("tab", { name: "Board" }).click();
  await expect(page.getByTestId("plane-board")).toBeVisible();
  await expect(page.getByTestId("plane-feed")).toHaveCount(0);
  await expect(page.locator('[data-testid^="board-col-"]')).toHaveCount(4);
  await expect(page.locator('[data-testid="work-card"]')).toHaveCount(3);

  // List: compact rows (work-row), still leaf-only.
  await page.getByRole("tab", { name: "List" }).click();
  await expect(page.getByTestId("plane-list")).toBeVisible();
  await expect(page.locator('[data-testid="work-row"]')).toHaveCount(3); // leaf-only (container excluded)

  // Back to feed.
  await page.getByRole("tab", { name: "Feed" }).click();
  await expect(page.getByTestId("plane-feed")).toBeVisible();
});

test("selection drives the inspector — a selected card opens the receipts panel", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();

  // No inspector until something is selected (the plane is the primary surface).
  await expect(page.getByTestId("inspector-panel")).toHaveCount(0);

  // Click the running card (NOT the review group that renders first) so the assertion distinguishes
  // "reflects the selection" from "always shows the first item".
  await page.getByTestId("board-group-running").getByText("Building the runner").click();
  const panel = page.getByTestId("inspector-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByTestId("inspector")).toContainText("Building the runner");
  await expect(panel.getByTestId("inspector")).not.toContainText("Approve the deploy");
  await expect(panel).not.toContainText("%"); // receipts, never a percentage
  // The lifecycle rail renders read-only progression: a running item lights the Running stage.
  await expect(panel.getByTestId("inspector-rail")).toBeVisible();
  await expect(panel.locator(".mc-rail-stage.is-current")).toContainText("Running");

  // Escape dismisses it and the plane returns to full width.
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
