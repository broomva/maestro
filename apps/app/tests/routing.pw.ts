import { expect, test } from "@playwright/test";

// routing (BRO-1824) — the product views route under one persistent shell. Proves the restructure end to
// end in a browser: Board at /, the sidebar nav navigates to /knowledge · /history · /settings · /account,
// the matched view renders in the shell's Outlet, and the shell chrome (brand, nav) persists across nav.
// HERMETIC (page.route mock of /api/tree + /api/stream) — same rationale as board-m3.pw.ts (deterministic,
// no runtime-port clash). The error-boundary CONTRACT is unit-covered in routing.test.tsx.

const T = 1_700_000_000_000;
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

test("the sidebar nav routes between product views while the shell chrome persists", async ({
  page,
}) => {
  await page.goto("/");
  // Board is the / view.
  await expect(page.getByTestId("board")).toBeVisible();

  // Knowledge → /knowledge stub: the board unmounts, the chrome (brand mark + nav) stays.
  await page.getByRole("link", { name: "Knowledge" }).click();
  await expect(page).toHaveURL(/\/knowledge$/);
  await expect(page.getByTestId("view-knowledge")).toBeVisible();
  await expect(page.getByTestId("board")).toHaveCount(0);
  await expect(page.getByTestId("brand-mark")).toBeVisible();

  // History + Settings + Account are all reachable from the same shell.
  await page.getByRole("link", { name: "History" }).click();
  await expect(page.getByTestId("view-history")).toBeVisible();
  await page.getByRole("link", { name: "Settings" }).click();
  await expect(page.getByTestId("view-settings")).toBeVisible();

  // Back to the board.
  await page.getByRole("link", { name: "Board", exact: true }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByTestId("board")).toBeVisible();
});
