import { type ChildProcess, spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

// board-live (BRO-1780) — the P1 exit behavior, end to end: a hand-edit to a `_work.md` on
// disk propagates to the board WITHOUT a reload. Boots a real runtime over a temp fixture
// workspace on the port the vite-preview `/api` proxy targets (default 4319), loads /app, and
// drives the full seam: fs edit → watcher → reconcile → node.updated → SSE → store → board.
// Named *.pw.ts so bun's runner skips it; Playwright-only. Serial (owns a shared runtime port).

const RUNTIME_PORT = 4319; // matches the vite-preview default `/api` proxy target
const RUNTIME_ENTRY = fileURLToPath(new URL("../../runtime/src/index.ts", import.meta.url));

let runtime: ChildProcess | undefined;
let workspace = "";

const wm = (o: { id: string; state: string; title?: string }): string =>
  [
    "---",
    `id: ${o.id}`,
    "kind: task",
    `state: ${o.state}`,
    "created: 2026-06-25",
    "updated: 2026-06-25",
    "---",
    "",
    `# ${o.title ?? o.id}`,
    "",
  ].join("\n");

const write = (rel: string, content: string): void => {
  const abs = join(workspace, rel);
  mkdirSync(dirname(abs), { recursive: true });
  writeFileSync(abs, content);
};

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  workspace = mkdtempSync(join(tmpdir(), "maestro-board-live-"));
  write("_work.md", wm({ id: "root", state: "running", title: "Root" }));
  write("gate/_work.md", wm({ id: "gate", state: "review", title: "Approve the deploy" }));
  write("stuck/_work.md", wm({ id: "stuck", state: "blocked", title: "Stuck task" }));
  write("later/_work.md", wm({ id: "later", state: "proposed", title: "Queued task" }));

  runtime = spawn("bun", ["run", RUNTIME_ENTRY], {
    env: {
      ...process.env,
      MAESTRO_WORKSPACE: workspace,
      MAESTRO_PORT: String(RUNTIME_PORT),
      MAESTRO_STREAM_POLL_MS: "50",
    },
    stdio: "inherit",
  });

  const deadline = Date.now() + 15_000;
  for (;;) {
    try {
      const r = await fetch(`http://localhost:${RUNTIME_PORT}/health`);
      if (r.ok) break;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) throw new Error("runtime did not become healthy in time");
    await new Promise((res) => setTimeout(res, 200));
  }
});

test.afterAll(() => {
  runtime?.kill("SIGTERM");
  if (workspace) rmSync(workspace, { recursive: true, force: true });
});

test("board shows real nodes review-first, and a disk edit propagates live with no reload", async ({
  page,
}) => {
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible();

  // Attention-first: the FIRST board group is review ("Needs you").
  const groups = page.locator('[data-testid^="board-group-"]');
  await expect(groups.first()).toHaveAttribute("data-testid", "board-group-review");

  // The review card renders with its plain-voice label.
  await expect(page.getByText("Approve the deploy")).toBeVisible();
  await expect(page.getByText("Needs you").first()).toBeVisible();

  // LIVE: flip `later` proposed → review on disk. It must move into the review group with NO
  // reload — the whole point of the watcher + SSE + store seam. A page reload preserves the URL,
  // so a URL check can't prove "no reload"; plant a window sentinel a reload would wipe.
  await page.evaluate(() => {
    (window as unknown as { __boardAlive?: boolean }).__boardAlive = true;
  });
  write("later/_work.md", wm({ id: "later", state: "review", title: "Queued task" }));

  await expect(page.getByTestId("board-group-review").getByText("Queued task")).toBeVisible({
    timeout: 8_000,
  });
  const survived = await page.evaluate(
    () => (window as unknown as { __boardAlive?: boolean }).__boardAlive === true,
  );
  expect(survived, "the page must NOT have reloaded — the update is live over SSE").toBe(true);
});
