import { expect, test } from "@playwright/test";

// Account / user page (BRO-1893 FID-6 slice 4, `MccUser`) integration — the account page in the REAL
// shell, HERMETIC: /api/tree + /api/stream mocked. Proves the wiring: /account renders the Overview with
// the identity + autonomy-score hero + the honest "sample" affordance, REAL run rows come from the mocked
// tree (selectHistory), the Overview↔Account segmented swaps views, the editable Account view is honest
// (says "not saved", never the false "syncs to your profile"), destructive red is confined to
// revoke/sign out, and — the one LIVE control — the Personal-preferences Theme radio writes through to
// <html data-theme> and stays in sync with the top-bar toggle. Named *.pw.ts so bun's unit runner skips
// it; this is the local P11 gate.

const T = 1_760_000_000_000;
// Nodes that selectHistory turns into run rows: a mix of you-owned (@handle) and loop-owned (autonomous),
// across run states. Container / backlog nodes would be excluded, so all are leaf tasks in run states.
const NODES = [
  {
    id: "n1",
    path: "work/store",
    parentId: null,
    kind: "task",
    state: "running",
    owner: null,
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: "Refactor the store",
    createdAt: T,
    updatedAt: T + 3000,
  },
  {
    id: "n2",
    path: "work/note",
    parentId: null,
    kind: "task",
    state: "review",
    owner: "@ana",
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: "Draft the release note",
    createdAt: T,
    updatedAt: T + 2000,
  },
  {
    id: "n3",
    path: "work/sweep",
    parentId: null,
    kind: "task",
    state: "done",
    owner: null,
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: "Nightly dependency sweep",
    createdAt: T,
    updatedAt: T + 1000,
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
  await page.goto("/account");
  await expect(page.getByTestId("view-account")).toBeVisible();
});

test("the Overview renders identity + the autonomy-score hero + the honest 'sample' affordance", async ({
  page,
}) => {
  const view = page.getByTestId("view-account");
  await expect(view.locator(".usr-id-name")).toHaveText("Ana Diaz");
  await expect(view.locator(".usr-score")).toContainText("Your autonomy score");
  await expect(view.locator(".usr-score")).toContainText("Unsupervised this week");
  // the autonomy number is sample; the affordance labels it so.
  await expect(view.getByText("sample", { exact: true })).toBeVisible();
  // receipts, never a work-progress percentage.
  await expect(view).not.toContainText("%");
});

test("the 'Your sessions' card shows REAL runs from the tree (not a fabricated list)", async ({
  page,
}) => {
  const view = page.getByTestId("view-account");
  const rows = view.locator(".usr-sess");
  await expect(rows).toHaveCount(3); // the three run-state leaves in the mocked tree
  await expect(view.locator(".usr-sess", { hasText: "Refactor the store" })).toBeVisible();
  await expect(view.locator(".usr-sess", { hasText: "Draft the release note" })).toBeVisible();
  // the you/loop kind badge is a projection of who ran it (owner @handle vs autonomous).
  await expect(
    view.locator(".usr-sess", { hasText: "Draft the release note" }).locator(".usr-sess-kind--you"),
  ).toBeVisible();
  // destructive red does NOT appear on the Overview (revoke/sign out live in the Account view only).
  await expect(view.locator(".usr-danger")).toHaveCount(0);
});

test("the Overview↔Account segmented swaps to the editable Account view (honest, no false sync claim)", async ({
  page,
}) => {
  const view = page.getByTestId("view-account");
  await view
    .getByRole("radiogroup", { name: "Account view" })
    .getByRole("radio", {
      name: "Account",
    })
    .click();
  // the editable identity form appears...
  await expect(view.getByRole("textbox", { name: "Full name" })).toBeVisible();
  // ...and it is HONEST: it says "not saved", never the prototype's false "syncs to your profile".
  await expect(view).toContainText("not saved yet");
  await expect(view).not.toContainText("syncs to your profile");
  // security section + the sanctioned destructive reds (revoke a session, sign out).
  await expect(view.locator(".usr-danger")).toHaveCount(2); // two device revokes
  await expect(view.getByRole("button", { name: "Sign out" })).toBeVisible();
});

test("Personal preferences > Theme is LIVE — it writes through to <html data-theme>", async ({
  page,
}) => {
  const view = page.getByTestId("view-account");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await view
    .getByRole("radiogroup", { name: "Account view" })
    .getByRole("radio", {
      name: "Account",
    })
    .click();
  const theme = view.getByRole("radiogroup", { name: "Theme" });
  await theme.getByRole("radio", { name: "Dark" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark"); // real write-through
  await theme.getByRole("radio", { name: "Light" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("the Theme selection stays in sync with the top-bar toggle (shared reactive state)", async ({
  page,
}) => {
  const view = page.getByTestId("view-account");
  await view
    .getByRole("radiogroup", { name: "Account view" })
    .getByRole("radio", {
      name: "Account",
    })
    .click();
  const theme = view.getByRole("radiogroup", { name: "Theme" });
  await expect(theme.getByRole("radio", { name: "Light" })).toHaveAttribute("aria-checked", "true");
  // flip via the always-visible top-bar toggle (chrome), NOT the segmented.
  await page.getByRole("button", { name: /^Switch to/ }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(theme.getByRole("radio", { name: "Dark" })).toHaveAttribute("aria-checked", "true");
});
