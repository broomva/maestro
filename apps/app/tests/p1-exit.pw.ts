import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

// p1-exit (BRO-1823) — the ROADMAP §P1 phase gate, end to end. Runs the two exit criteria verbatim:
//   ① Edit a `_work.md` by hand → the board updates over the stream WITHOUT a reload.
//   ② Kill the index file → it rebuilds identical, and the app serves the same board.
// Boots a real runtime over a temp fixture workspace on the port the vite `/api` proxy targets
// (4319). ① drives the full READ seam (fs → watcher → reconcile → node.updated → SSE → store →
// board). ② drives the REBUILD seam (rm index → `--rebuild` rescans the FS, the source of truth →
// identical). Video is captured as evidence; the rebuild diff is written to
// test-results/p1-exit-rebuild-diff.txt. Named *.pw.ts so bun's `bun test` runner skips it;
// Playwright-only. Serial — the two tests share one runtime port, and ② kills + restarts it.
// This is the canonical P1 exit E2E; it subsumes the BRO-1780 board-live proof (①).

const RUNTIME_PORT = 4319; // matches the vite-preview default `/api` proxy target
const RUNTIME_ENTRY = fileURLToPath(new URL("../../runtime/src/index.ts", import.meta.url));
const EVIDENCE_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "test-results");

let runtime: ChildProcess | undefined;
let workspace = "";

const runtimeEnv = () => ({
  ...process.env,
  MAESTRO_WORKSPACE: workspace,
  MAESTRO_PORT: String(RUNTIME_PORT),
  MAESTRO_STREAM_POLL_MS: "50",
});

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

async function waitHealthy(deadlineMs = 15_000): Promise<void> {
  const deadline = Date.now() + deadlineMs;
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
}

function onExit(p: ChildProcess): Promise<number> {
  return new Promise((res) => p.on("exit", (code) => res(code ?? 0)));
}

// The work tree the app serves, minus the ONE volatile column (`updatedAt` — the scan clock, which
// a rebuild legitimately re-stamps; exactly what rebuild.ts's dumpIndex strips), sorted by id. A
// stable, rebuild-invariant snapshot: if this is equal before and after a kill+rebuild, the index
// rebuilt identical.
async function stableTree(): Promise<Array<Record<string, unknown>>> {
  const r = await fetch(`http://localhost:${RUNTIME_PORT}/api/tree`);
  const body = (await r.json()) as { nodes: Array<Record<string, unknown>> };
  return body.nodes
    .map(({ updatedAt: _updatedAt, ...rest }) => rest)
    .sort((a, b) => String(a.id).localeCompare(String(b.id)));
}

test.use({ video: "on" }); // evidence: a screen recording of the exit gate
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  workspace = mkdtempSync(join(tmpdir(), "maestro-p1-exit-"));
  write("_work.md", wm({ id: "root", state: "running", title: "Root" }));
  write("gate/_work.md", wm({ id: "gate", state: "review", title: "Approve the deploy" }));
  write("stuck/_work.md", wm({ id: "stuck", state: "blocked", title: "Stuck task" }));
  write("later/_work.md", wm({ id: "later", state: "proposed", title: "Queued task" }));

  runtime = spawn("bun", ["run", RUNTIME_ENTRY], { env: runtimeEnv(), stdio: "inherit" });
  await waitHealthy();
});

test.afterAll(() => {
  runtime?.kill("SIGTERM");
  if (workspace) rmSync(workspace, { recursive: true, force: true });
});

// ① The read seam — a hand-edit propagates to the board live, with no reload.
test("P1 exit ①: a hand-edit to a _work.md propagates to the board live, with no reload", async ({
  page,
}) => {
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible();

  // Attention-first: the FIRST board group is review ("Needs you").
  const groups = page.locator('[data-testid^="board-group-"]');
  await expect(groups.first()).toHaveAttribute("data-testid", "board-group-review");
  await expect(page.getByText("Approve the deploy")).toBeVisible();
  await expect(page.getByText("Needs you").first()).toBeVisible();

  // LIVE: flip `later` proposed → review on disk. It must move into the review group with NO reload.
  // A reload preserves the URL, so a URL check can't prove "no reload" — plant a window sentinel a
  // reload would wipe (the BRO-1780 no-reload proof).
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

// ② The rebuild seam — kill the index file; it rebuilds identical and the app serves the same board.
test("P1 exit ②: killing the index rebuilds it identical and the app serves the same board", async ({
  page,
}) => {
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible();

  const before = await stableTree();
  expect(before.length, "fixture has nodes to compare").toBeGreaterThan(0);

  // Kill the runtime, delete the index file (+ its WAL/SHM), rebuild it from the FS (truth), restart.
  runtime?.kill("SIGTERM");
  if (runtime) await onExit(runtime);
  const indexPath = join(workspace, ".maestro/index.db");
  for (const p of [indexPath, `${indexPath}-wal`, `${indexPath}-shm`]) rmSync(p, { force: true });
  expect(existsSync(indexPath), "the index file is gone before rebuild").toBe(false);

  const rebuild = spawn("bun", ["run", RUNTIME_ENTRY, "--rebuild"], {
    env: runtimeEnv(),
    stdio: "inherit",
  });
  const code = await onExit(rebuild);
  expect(code, "`--rebuild` exits 0").toBe(0);
  expect(existsSync(indexPath), "the index rebuilt from the FS").toBe(true);

  runtime = spawn("bun", ["run", RUNTIME_ENTRY], { env: runtimeEnv(), stdio: "inherit" });
  await waitHealthy();

  const after = await stableTree();
  expect(after, "the app serves an identical work tree after a kill + rebuild").toEqual(before);

  // Evidence: the rebuild diff (node counts + verdict + the compared trees) next to the video.
  const identical = JSON.stringify(before) === JSON.stringify(after);
  mkdirSync(EVIDENCE_DIR, { recursive: true });
  writeFileSync(
    join(EVIDENCE_DIR, "p1-exit-rebuild-diff.txt"),
    [
      "P1 exit ② — kill the index file; it rebuilds identical (BRO-1823)",
      `nodes before kill: ${before.length}`,
      `nodes after rebuild: ${after.length}`,
      `identical (updatedAt-stripped): ${identical}`,
      "",
      `before: ${JSON.stringify(before, null, 2)}`,
      "",
      `after:  ${JSON.stringify(after, null, 2)}`,
      "",
    ].join("\n"),
  );
  console.log(`P1 exit ②: rebuilt index identical — ${before.length} nodes, before === after`);

  // The app is live again over the REBUILT index — the board still renders.
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible();
});
