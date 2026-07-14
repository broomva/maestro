import { expect, test } from "@playwright/test";

// Knowledge page (BRO-1893 FID-6 slice 2a, `MccKnowledge`) integration — the scope graph in the REAL
// shell, HERMETIC: /api/tree + /api/stream mocked (the shell's sidebar reads the tree; Knowledge itself
// renders SAMPLE data, no store). Proves the wiring: /knowledge renders the graph, the graph⇄list toggle
// swaps views, type-filter chips narrow, an entity opens the detail drawer, a folder descends the scope
// + the breadcrumb navigates back, and the data is honestly labelled "sample". Named *.pw.ts so bun's
// unit runner skips it; this is the local P11 gate.

const T = 1_760_000_000_000;
const NODES = [
  {
    id: "n1",
    path: "work",
    parentId: null,
    kind: "task",
    state: "running",
    owner: null,
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: "A running task",
    createdAt: T,
    updatedAt: T,
  },
];

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1360, height: 900 });
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
  await page.goto("/knowledge");
  await expect(page.getByTestId("view-knowledge")).toBeVisible();
});

test("the graph renders the sample scope, honestly labelled, with a rail + legend", async ({
  page,
}) => {
  const view = page.getByTestId("view-knowledge");
  await expect(page.getByTestId("kg-graph")).toBeVisible();
  // the breadcrumb + scope kind/count + the honest "sample" affordance (never faked as live).
  await expect(view.getByText("Broomva", { exact: true })).toBeVisible();
  await expect(view.getByText("vault · 11")).toBeVisible();
  await expect(view.getByText("sample", { exact: true })).toBeVisible();
  // real sample entities render as graph nodes (accessible names).
  await expect(
    page.getByTestId("kg-graph").getByRole("button", { name: /Bookkeeping/ }),
  ).toBeVisible();
  // the rail + legend.
  await expect(page.getByTestId("kg-rail")).toBeVisible();
  await expect(view.locator(".kg-legend")).toBeVisible();
  // the graph/list surface shows no work-progress percentage.
  await expect(page.getByTestId("kg-graph")).not.toContainText("%");
});

test("the graph⇄list toggle swaps the view; a type chip narrows it", async ({ page }) => {
  const view = page.getByTestId("view-knowledge");
  await view.getByRole("tab", { name: "List" }).click();
  await expect(page.getByTestId("kg-list")).toBeVisible();
  await expect(page.getByTestId("kg-graph")).toHaveCount(0);
  await expect(page.getByTestId("kg-list")).not.toContainText("%");

  // "concept" chip (a category present at the root scope) → only concepts remain; a primitive drops.
  const beforeRows = await view.locator(".kg-list-row").count();
  await view.getByRole("button", { name: "concept", exact: true }).click();
  await expect(view.locator(".kg-list-row").filter({ hasText: "Bookkeeping" })).toHaveCount(0);
  expect(await view.locator(".kg-list-row").count()).toBeLessThan(beforeRows);
});

test("selecting an entity opens the detail drawer (inspector + neighbourhood)", async ({
  page,
}) => {
  const view = page.getByTestId("view-knowledge");
  await view.getByRole("tab", { name: "List" }).click();
  await view.locator(".kg-list-row").filter({ hasText: "Bookkeeping" }).click();
  const drawer = page.getByTestId("kg-drawer");
  await expect(drawer).toBeVisible();
  await expect(drawer.getByTestId("kg-inspect")).toBeVisible();
  await expect(drawer).toContainText("Bookkeeping");
  await expect(drawer).toContainText("Nous score"); // a scored entity's receipt
  await expect(drawer.locator(".kg-mini")).toBeVisible(); // the neighbourhood mini-graph
  // close it.
  await drawer.getByRole("button", { name: "Close detail" }).click();
  await expect(page.getByTestId("kg-drawer")).toHaveCount(0);
});

test("descending a folder re-scopes the graph; the breadcrumb navigates back", async ({ page }) => {
  const view = page.getByTestId("view-knowledge");
  // Descend via the graph folder node (hawthorne is a sub-scope).
  await page
    .getByTestId("kg-graph")
    .getByRole("button", { name: /hawthorne · folder/ })
    .click();
  await expect(view.getByText("initiative · 8")).toBeVisible(); // hawthorne scope kind · count
  // the breadcrumb now has an active hawthorne crumb; click Broomva to go back up.
  await view.getByRole("button", { name: "Broomva", exact: true }).click();
  await expect(view.getByText("vault · 11")).toBeVisible();
});
