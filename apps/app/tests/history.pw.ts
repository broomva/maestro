import { expect, test } from "@playwright/test";

// History page (BRO-1893 FID-6, `MccHistory`) integration — the run list in the REAL shell, HERMETIC:
// /api/tree + /api/stream mocked at the network layer (like routing.pw.ts / fs.pw.ts), no runtime.
// Proves the wiring end to end: History derives from WORK STATE (the honest, hydrated spine — the
// client store has no session-row read path yet), so run-state leaves become rows, backlog + containers
// do not, the axis toggle re-groups the same rows, the you/autonomous filter and search narrow them,
// and — the canon §Work-states invariant — NO progress percentage appears. Named *.pw.ts so bun's unit
// runner skips it; this is the local P11 gate.

const T = 1_760_000_000_000;
const node = (o: {
  id: string;
  path: string;
  state: string;
  title: string;
  kind?: string;
  owner?: string | null;
  parentId?: string | null;
  updatedAt?: number;
}) => ({
  id: o.id,
  path: o.path,
  parentId: o.parentId ?? null,
  kind: o.kind ?? "task",
  state: o.state,
  owner: o.owner ?? null,
  gate: "human",
  budgetJson: null,
  doneJson: null,
  title: o.title,
  createdAt: T,
  updatedAt: o.updatedAt ?? T,
});

// Two containers (excluded), three leaves that have RUN (rows), one backlog leaf (excluded).
const NODES = [
  node({ id: "i", path: "hawthorne", state: "running", title: "hawthorne", kind: "initiative" }),
  node({
    id: "p",
    path: "hawthorne/core",
    parentId: "i",
    state: "running",
    title: "core",
    kind: "project",
  }),
  node({
    id: "w1",
    path: "hawthorne/core/persist",
    parentId: "p",
    state: "review",
    owner: "@ana",
    title: "Persist run transcripts",
    updatedAt: T + 3000,
  }),
  node({
    id: "w2",
    path: "hawthorne/core/resume",
    parentId: "p",
    state: "running",
    owner: "agent:claude",
    title: "Resume sessions",
    updatedAt: T + 2000,
  }),
  node({
    id: "w3",
    path: "genesis/ship",
    state: "done",
    title: "Ship genesis",
    updatedAt: T + 1000,
  }),
  // Not yet run — backlog. Must NOT appear (it is not a run).
  node({ id: "w4", path: "genesis/draft", state: "proposed", title: "Backlog draft idea" }),
];

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
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
  await page.goto("/history");
  await expect(page.getByTestId("view-history")).toBeVisible();
});

test("History lists a row per run — backlog + container folders are not rows", async ({ page }) => {
  const view = page.getByTestId("view-history");
  // The three leaves that have run, each with its title.
  await expect(view.getByText("Persist run transcripts")).toBeVisible();
  await expect(view.getByText("Resume sessions")).toBeVisible();
  await expect(view.getByText("Ship genesis")).toBeVisible();
  // Exactly three run rows (no folder rows, no backlog row).
  await expect(view.locator(".mcc-hrow")).toHaveCount(3);
  // The backlog leaf (proposed) is not a run — never rendered.
  await expect(view.getByText("Backlog draft idea")).toHaveCount(0);
  // The disclosure ladder + §Work-states invariant: receipts, never a progress percentage.
  await expect(view).not.toContainText("%");
});

test("the you/autonomous filter narrows to who owns the run", async ({ page }) => {
  const view = page.getByTestId("view-history");

  // You → only the @handle-owned run (Persist, owner @ana).
  await view.getByRole("button", { name: "You", exact: true }).click();
  await expect(view.getByText("Persist run transcripts")).toBeVisible();
  await expect(view.getByText("Resume sessions")).toHaveCount(0);
  await expect(view.locator(".mcc-hrow")).toHaveCount(1);

  // Autonomous → only the loop-owned runs (Resume = agent, Ship = the loop).
  await view.getByRole("button", { name: "Autonomous", exact: true }).click();
  await expect(view.getByText("Resume sessions")).toBeVisible();
  await expect(view.getByText("Ship genesis")).toBeVisible();
  await expect(view.getByText("Persist run transcripts")).toHaveCount(0);
  await expect(view.locator(".mcc-hrow")).toHaveCount(2);
});

test("the axis toggle re-groups the same rows (by work → folder headers)", async ({ page }) => {
  const view = page.getByTestId("view-history");
  // By work → the group headers become folder crumbs (one per distinct folder); rows unchanged.
  await view.getByRole("tab", { name: "By work" }).click();
  await expect(view.locator(".mcc-hgroup").filter({ hasText: "hawthorne / core" })).toHaveCount(1);
  await expect(view.locator(".mcc-hrow")).toHaveCount(3);

  // By agent → group headers carry the agent identity; still the same three runs.
  await view.getByRole("tab", { name: "By agent" }).click();
  await expect(view.locator(".mcc-hgroup").first()).toBeVisible();
  await expect(view.locator(".mcc-hrow")).toHaveCount(3);
});

test("search narrows the runs by title / folder / agent", async ({ page }) => {
  const view = page.getByTestId("view-history");
  await view.getByPlaceholder("Search sessions").fill("resume");
  await expect(view.getByText("Resume sessions")).toBeVisible();
  await expect(view.getByText("Persist run transcripts")).toHaveCount(0);
  await expect(view.locator(".mcc-hrow")).toHaveCount(1);
});

test("a filtered-empty list reads 'filters', not 'search' (honest end note)", async ({ page }) => {
  const view = page.getByTestId("view-history");
  await view.getByPlaceholder("Search sessions").fill("zzz-no-such-run");
  await expect(view.locator(".mcc-hrow")).toHaveCount(0);
  // The store is non-empty (it is the filters, not an empty workspace), so the copy says "filters".
  await expect(view.locator(".mcc-hist-end")).toHaveText("No runs match these filters.");
  await expect(view.getByTestId("history-empty")).toHaveCount(0); // not the zero-state screen
});

test("the axis toggle is a WAI-ARIA tab list — arrows move selection AND focus, roving tabindex", async ({
  page,
}) => {
  const view = page.getByTestId("view-history");
  const byDay = view.getByRole("tab", { name: "By day" });
  const byWork = view.getByRole("tab", { name: "By work" });

  // Roving tabindex: the selected tab is the single Tab stop; the others are removed from the order.
  await expect(byDay).toHaveAttribute("tabindex", "0");
  await expect(byWork).toHaveAttribute("tabindex", "-1");

  // Automatic activation: ArrowRight moves BOTH aria-selected and DOM focus to the next tab.
  await byDay.focus();
  await page.keyboard.press("ArrowRight");
  await expect(byWork).toBeFocused();
  await expect(byWork).toHaveAttribute("aria-selected", "true");
  await expect(byDay).toHaveAttribute("aria-selected", "false");
  await expect(byWork).toHaveAttribute("tabindex", "0");
  await expect(byDay).toHaveAttribute("tabindex", "-1");
  // Selection moving re-grouped the list (by work → folder header), same rows.
  await expect(view.locator(".mcc-hgroup").filter({ hasText: "hawthorne / core" })).toHaveCount(1);
});
