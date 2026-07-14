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

test("Esc closes the feedback drawer and returns focus to the trigger (WAI-ARIA)", async ({
  page,
}) => {
  await page.goto("/");
  const trigger = page.locator("aside").getByRole("button", { name: "Feedback", exact: true });
  await trigger.click();
  await expect(page.getByTestId("feedback-drawer")).toBeVisible();
  // Focus must actually MOVE into the drawer first (the textarea auto-focuses), otherwise the
  // return-to-trigger assertion below would pass vacuously (focus never having left the trigger).
  await expect(page.getByRole("textbox", { name: "Your feedback" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("feedback-drawer")).toHaveCount(0);
  // The modal-dialog focus-return leg the palette also upholds (P20 BRO-1894).
  await expect(trigger).toBeFocused();
});

test("in-progress feedback survives a background store update (no text-wipe on re-render)", async ({
  page,
}) => {
  // Reproduce the exact real-world trigger: a background run emits an SSE event WHILE the drawer is
  // open → Shell re-renders → OverlayHost re-renders. Before the fix, the drawer's open effect (keyed
  // on the fresh inline onClose) re-ran on that render and wiped the user's text. We HOLD the stream
  // open, then fire one node.updated event mid-typing. (Test-scoped route added before goto; Playwright
  // runs the most-recently-registered handler first, so it shadows the beforeEach stream mock.)
  const deferred: { fire: () => void } = { fire: () => {} };
  const armed = new Promise<void>((resolve) => {
    deferred.fire = resolve;
  });
  const EVENT = {
    seq: 999_999,
    type: "node.updated",
    payload: {
      id: "n2",
      path: "later",
      parentId: null,
      kind: "task",
      state: "running",
      owner: null,
      gate: "human",
      budgetJson: null,
      doneJson: null,
      title: "A late arrival",
      createdAt: T,
      updatedAt: T,
    },
    sessionId: null,
    ts: "2026-07-14T00:00:00.000Z",
    actor: "system",
  };
  await page.route("**/api/stream*", async (route) => {
    await armed;
    await route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      headers: { "cache-control": "no-cache" },
      body: `retry: 3600000\n\ndata: ${JSON.stringify(EVENT)}\n\n`,
    });
  });

  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();
  await page.locator("aside").getByRole("button", { name: "Feedback", exact: true }).click();
  await expect(page.getByTestId("feedback-drawer")).toBeVisible();

  const REPORT = "the gate queue lost my scroll position";
  const field = page.getByRole("textbox", { name: "Your feedback" });
  await field.fill(REPORT);
  await expect(field).toHaveValue(REPORT);

  // Fire the event → the store updates → Shell + OverlayHost re-render while the drawer is open.
  deferred.fire();
  await page.waitForTimeout(250); // let the store update + re-render flush

  // The fix: the text must still be there (the reset effect keys on [open] + reads onClose via a ref).
  await expect(field).toHaveValue(REPORT);
  await expect(page.getByTestId("feedback-drawer")).toBeVisible();

  // Non-vacuity guard: prove the event actually applied (so a re-render DID happen while open — else
  // the assertion above would pass trivially). The new node shows on the board once the drawer closes.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("feedback-drawer")).toHaveCount(0);
  await expect(page.getByText("A late arrival")).toBeVisible();
});
