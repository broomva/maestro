import { expect, test } from "@playwright/test";

// Settings page (BRO-1893 FID-6 slice 3, `MccSettings`) integration — the config page in the REAL shell,
// HERMETIC: /api/tree + /api/stream mocked (the shell sidebar reads the tree; Settings itself is
// store-free). Proves the wiring: /settings renders the two-pane nav, the section nav swaps sections, the
// layout toggle switches to one-scroll (+ ToC), the honest "preview" affordance is present, and — the one
// LIVE control — the Appearance theme radio writes through to <html data-theme>. Named *.pw.ts so bun's
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
  // Start from a known theme so the write-through assertion is unambiguous.
  await page.addInitScript(() => {
    try {
      localStorage.setItem("bv-theme", "light");
    } catch {}
  });
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
  await page.goto("/settings");
  await expect(page.getByTestId("view-settings")).toBeVisible();
});

test("the two-pane page renders with the nav + the honest 'preview' affordance", async ({
  page,
}) => {
  const view = page.getByTestId("view-settings");
  await expect(view.getByText("preview", { exact: true })).toBeVisible();
  // the section nav lists every section (the credentials badge is present).
  await expect(view.locator(".set-secnav-btn", { hasText: "Runners and worktrees" })).toBeVisible();
  await expect(view.locator(".set-secnav-badge")).toHaveText("1");
  // the default (runners) section content shows; no work-progress percentage.
  await expect(view.locator(".set-content")).toContainText("Default runner");
  await expect(view).not.toContainText("%");
});

test("the section nav swaps the visible section", async ({ page }) => {
  const view = page.getByTestId("view-settings");
  await view.locator(".set-secnav-btn", { hasText: "Notifications" }).click();
  await expect(view.locator(".set-content")).toContainText("When and where the loop pings you");
  await expect(view.locator(".set-content")).not.toContainText("Default runner");
});

test("the layout toggle switches to the one-scroll editorial with a table of contents", async ({
  page,
}) => {
  const view = page.getByTestId("view-settings");
  await view.getByRole("radio", { name: "One scroll" }).click();
  await expect(view.locator(".set-toc")).toBeVisible(); // the sticky ToC
  await expect(view.locator(".set-toc")).toContainText("On this page");
  // one scroll renders ALL sections at once (two-pane showed only the active one).
  await expect(view.locator("#set-appearance")).toBeVisible();
  await expect(view.locator("#set-members")).toBeVisible();
});

test("Appearance > Theme is LIVE — it writes through to <html data-theme>", async ({ page }) => {
  const view = page.getByTestId("view-settings");
  // baseline: light (set in beforeEach).
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await view.locator(".set-secnav-btn", { hasText: "Appearance" }).click();
  const theme = view.getByRole("radiogroup", { name: "Theme" });
  await theme.getByRole("radio", { name: "Dark" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark"); // real write-through
  await theme.getByRole("radio", { name: "Light" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});
