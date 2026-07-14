import { expect, test } from "@playwright/test";

// FS surfaces (BRO-1890 FID-4) integration — the chrome tab strip + file pane + file view in the REAL
// shell, HERMETIC: /api/tree + /api/stream are mocked at the network layer (like board-m3.pw.ts), no
// runtime. Proves the wiring end-to-end: the workspace walks as files, a file opens as a tab + a
// document, the pane marks the open row, and the FS toggle collapses the pane. Named *.pw.ts so bun's
// unit runner skips it — this is the local P11 gate.

const T = 1_760_000_000_000;
const node = (o: {
  id: string;
  path: string;
  state: string;
  title: string;
  kind?: string;
  parentId?: string | null;
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
  updatedAt: T,
});

const NODES = [
  node({ id: "proj1", path: "hawthorne", state: "running", title: "hawthorne", kind: "project" }),
  node({
    id: "spec1",
    path: "hawthorne/spec",
    parentId: "proj1",
    state: "review",
    title: "The spec",
  }),
  node({
    id: "run1",
    path: "hawthorne/build",
    parentId: "proj1",
    state: "running",
    title: "The build",
  }),
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
  await page.goto("/");
  await expect(page.getByTestId("shell-main")).toBeVisible();
});

test("the chrome carries a pinned Maestro tab; the FS pane walks the workspace as files", async ({
  page,
}) => {
  const strip = page.getByTestId("tab-strip");
  // The pinned Maestro tab is present + active on the board (no % anywhere in the chrome).
  const maestro = strip.getByRole("link", { name: "Maestro" });
  await expect(maestro).toBeVisible();
  await expect(maestro).toHaveClass(/is-active/);
  // Active state is exposed to assistive tech, not just via a class (aria-current).
  await expect(maestro).toHaveAttribute("aria-current", "page");
  // The Maestro tab is a static nav tab — it carries a board glyph, NOT a running-state dot
  // (a persistent info dot would read as "always running"; canon §Work states, prototype placement B).
  await expect(maestro.locator(".mc-chip-dot")).toHaveCount(0);
  await expect(strip).not.toContainText("%");

  // The FS pane shows the workspace: the project as a folder, the leaves as files.
  const pane = page.getByTestId("file-pane");
  await expect(pane).toBeVisible();
  await expect(pane.getByText("hawthorne", { exact: true })).toBeVisible();
  await expect(pane.getByText("spec", { exact: true })).toBeVisible();
  // The folder row is inert (disabled); the file rows are openable buttons.
  await expect(pane.locator(".mcc-ftree-row.is-folder")).toBeDisabled();
  await expect(pane).not.toContainText("%");
});

test("opening a file adds a tab + renders it as a document; the pane marks the open row", async ({
  page,
}) => {
  const pane = page.getByTestId("file-pane");
  await pane.getByText("spec", { exact: true }).click();

  // The route carries the path; a file tab appears; the document renders the node's title + crumb.
  await expect(page).toHaveURL(/\/file\/hawthorne\/spec$/);
  const strip = page.getByTestId("tab-strip");
  await expect(strip.getByText("spec", { exact: true })).toBeVisible();
  // The open file's tab is the current one (aria-current); the Maestro tab yields it.
  await expect(strip.getByRole("button", { name: "spec", exact: true })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(strip.getByRole("link", { name: "Maestro" })).not.toHaveAttribute("aria-current");
  const doc = page.getByTestId("file-view");
  await expect(doc).toContainText("The spec"); // real title
  await expect(doc).toContainText("~ / hawthorne / spec"); // crumb from path
  await expect(doc).toContainText("gate: human"); // real frontmatter chip
  await expect(doc).not.toContainText("%"); // receipts, never a percentage

  // The pane marks the open file's row active.
  await expect(pane.locator(".mcc-ftree-row.is-active")).toContainText("spec");

  // Closing the file tab returns to the plane (the pinned Maestro tab is always present).
  await strip.getByRole("button", { name: "Close spec" }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByTestId("board")).toBeVisible();
  await expect(strip.getByText("spec", { exact: true })).toHaveCount(0);
});

// NOTE — the malformed-URL guard (activeFilePath swallowing a URIError so a hand-typed `/file/foo%`
// never blanks the shell) is proven at the UNIT level in src/lib/file-route.test.ts, which is
// mutation-checked (removing the try/catch throws). It is NOT an integration test here on purpose:
// `page.goto("/file/bad%E0%A4")` never reaches the client — vite's own `viteHtmlFallbackMiddleware`
// decodes the request path and 500s server-side before serving the SPA, so the harness can't exercise
// the client-render path the guard protects (production serves the SPA via Hono/static, not vite). An
// integration test through this harness would fail identically with or without the fix — vacuous.

test("the FS toggle collapses the pane (a persisted layout pref)", async ({ page }) => {
  await expect(page.getByTestId("fs-rpane")).toBeVisible();
  await page.getByRole("button", { name: "Hide files" }).click();
  await expect(page.getByTestId("fs-rpane")).toHaveCount(0);
  // Re-show it.
  await page.getByRole("button", { name: "Show files" }).click();
  await expect(page.getByTestId("fs-rpane")).toBeVisible();
});
