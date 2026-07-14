import { expect, test } from "@playwright/test";

// The overlay layer (BRO-1894 FID-7) — the ⌘K command palette + the feedback drawer, proven end to end
// in a real browser (open, keyboard nav, Enter-navigates, Esc-closes-and-returns-focus, scrim-closes,
// theme toggle, feedback from the footer AND from the palette). The pure markup + §Voice are unit-covered
// (command-palette.test.tsx / feedback-drawer.test.tsx); this is the behaviour + composition proof.
// HERMETIC (page.route mock of /api/tree + /api/stream) — same rationale as routing.pw.ts.

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

test("⌘K opens the palette and Enter navigates to the matched page", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();

  await page.keyboard.press("ControlOrMeta+k");
  const combo = page.getByRole("dialog", { name: "Command palette" });
  await expect(combo).toBeVisible();
  await expect(page.getByRole("combobox")).toBeFocused();

  // Type to filter, then Enter runs the top match → real navigation (never a raw <a> reload).
  await page.getByRole("combobox").fill("history");
  await expect(page.getByRole("option", { name: /History/ })).toBeVisible();
  await page.getByRole("combobox").press("Enter");

  await expect(page).toHaveURL(/\/history$/);
  await expect(combo).toHaveCount(0); // the palette closed on select
  await expect(page.getByTestId("view-history")).toBeVisible();
});

test("arrow keys move the active option; Enter runs it", async ({ page }) => {
  await page.goto("/");
  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByRole("dialog", { name: "Command palette" })).toBeVisible();

  // Order is Maestro(0) · History(1) · Knowledge(2) · … — ArrowDown once selects History.
  await page.getByRole("combobox").press("ArrowDown");
  await expect(page.getByRole("option", { name: /History/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.getByRole("combobox").press("Enter");
  await expect(page).toHaveURL(/\/history$/);
});

test("Esc closes the palette and returns focus to the trigger", async ({ page }) => {
  await page.goto("/");
  // Open by clicking the top-bar command field, so the field is the focus-return target.
  await page.getByTestId("cmd-field").click();
  const combo = page.getByRole("dialog", { name: "Command palette" });
  await expect(combo).toBeVisible();

  await page.getByRole("combobox").press("Escape");
  await expect(combo).toHaveCount(0);
  // WAI-ARIA dialog: focus returns to the element that opened it.
  await expect(page.getByTestId("cmd-field")).toBeFocused();
});

test("clicking the scrim closes the palette", async ({ page }) => {
  await page.goto("/");
  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByRole("dialog", { name: "Command palette" })).toBeVisible();
  await page.locator(".cmdk-scrim").click({ position: { x: 5, y: 5 } });
  await expect(page.getByRole("dialog", { name: "Command palette" })).toHaveCount(0);
});

test("the Toggle theme command flips the applied theme", async ({ page }) => {
  await page.goto("/");
  // Default is light (data-theme absent or 'light').
  await expect(page.locator("html")).not.toHaveAttribute("data-theme", "dark");
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByRole("combobox").fill("toggle");
  await page.getByRole("combobox").press("Enter");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("the feedback drawer opens from the sidebar footer and Esc closes it", async ({ page }) => {
  await page.goto("/");
  await page.locator("aside").getByRole("button", { name: "Feedback", exact: true }).click();
  const drawer = page.getByTestId("feedback-drawer");
  await expect(drawer).toBeVisible();
  await expect(drawer).toHaveAttribute("aria-label", "Feedback");
  // Honest surface: a 'preview' chip on delivery + a 'sample' chip on the thread history.
  await expect(drawer.getByText("preview", { exact: true })).toBeVisible();
  await expect(drawer.getByText("sample", { exact: true })).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(drawer).toHaveCount(0);
});

test("the Send feedback command opens the drawer from the palette", async ({ page }) => {
  await page.goto("/");
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByRole("combobox").fill("feedback");
  await page.getByRole("combobox").press("Enter");
  // The palette closes and the drawer opens.
  await expect(page.getByRole("dialog", { name: "Command palette" })).toHaveCount(0);
  await expect(page.getByTestId("feedback-drawer")).toBeVisible();
});
