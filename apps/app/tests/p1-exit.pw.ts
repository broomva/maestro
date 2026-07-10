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
// The index dump runs in a bun subprocess (Playwright's loader can't import the runtime's bun-only
// `.sql` modules that openIndex pulls in — same reason the runtime itself is spawned, not imported).
const DUMP_SCRIPT = fileURLToPath(new URL("../../runtime/scripts/dump-index.ts", import.meta.url));
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

// The canonical content of an index db file (every node column except the volatile `updatedAt` scan
// clock, id-sorted), read DIRECTLY from the file via the bun dump helper. Reading the file directly
// is deliberate: a RESTARTED runtime unconditionally rescans the FS on boot (index.ts
// `scanIntoIndex`), which would repopulate the index from disk and MASK whatever `--rebuild` actually
// wrote — so comparing /api/tree across a restart proves nothing about `--rebuild`. Dumping the db
// file makes `--rebuild`'s output the load-bearing artifact.
async function dumpIndexFile(indexPath: string): Promise<unknown[]> {
  const proc = spawn("bun", ["run", DUMP_SCRIPT, indexPath], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  const chunks: Buffer[] = [];
  proc.stdout?.on("data", (chunk: Buffer) => chunks.push(Buffer.from(chunk)));
  // Wait for "close", not "exit": "exit" can fire before stdout is fully drained, so reading the
  // buffer on "exit" risks a truncated read. "close" fires only after all stdio streams are closed.
  const code = await new Promise<number>((res) => proc.on("close", (c) => res(c ?? 0)));
  if (code !== 0) throw new Error(`dump-index exited ${code}`);
  return JSON.parse(Buffer.concat(chunks).toString("utf8")) as unknown[];
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

// ② The rebuild seam — kill the index file; `--rebuild` rebuilds it identical, and the app comes
// back up over it. The identity assertion is load-bearing on `--rebuild`'s OUTPUT: both snapshots
// are dumped from the db FILE directly, so a broken rebuild (0 rows / wrong states) fails here — a
// restarted runtime's boot rescan cannot mask it (that was the P20 finding this test now closes).
test("P1 exit ②: killing the index rebuilds it identical (from --rebuild's own output)", async ({
  page,
}) => {
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible(); // the app is live over the built index

  const indexPath = join(workspace, ".maestro/index.db");

  // Kill the runtime so nothing holds or rewrites the index while we read + rebuild it.
  runtime?.kill("SIGTERM");
  if (runtime) await onExit(runtime);
  runtime = undefined;

  // The index the running runtime built — dumped from the db FILE (the pre-kill truth).
  const before = await dumpIndexFile(indexPath);
  expect(before.length, "the built index has nodes to compare").toBeGreaterThan(0);

  // Kill the index file (+ WAL/SHM), rebuild it from the FS (truth) via the `--rebuild` CLI, exit 0.
  for (const p of [indexPath, `${indexPath}-wal`, `${indexPath}-shm`]) rmSync(p, { force: true });
  expect(existsSync(indexPath), "the index file is gone before rebuild").toBe(false);

  const rebuild = spawn("bun", ["run", RUNTIME_ENTRY, "--rebuild"], {
    env: runtimeEnv(),
    stdio: "inherit",
  });
  expect(await onExit(rebuild), "`--rebuild` exits 0").toBe(0);
  expect(existsSync(indexPath), "the index rebuilt from the FS").toBe(true);

  // The REBUILT index — dumped from the db FILE, BEFORE any runtime boots and rescans it. This is
  // `--rebuild`'s own output; if it wrote nothing or the wrong content, this comparison fails.
  const after = await dumpIndexFile(indexPath);
  expect(after, "`--rebuild` produced an identical index").toEqual(before);

  // Evidence: the rebuild diff (node counts + verdict + the compared canonical dumps) next to the video.
  const identical = JSON.stringify(before) === JSON.stringify(after);
  mkdirSync(EVIDENCE_DIR, { recursive: true });
  writeFileSync(
    join(EVIDENCE_DIR, "p1-exit-rebuild-diff.txt"),
    [
      "P1 exit ② — kill the index file; --rebuild rebuilds it identical (BRO-1823)",
      "(both snapshots dumped from the db file directly, so --rebuild's output is load-bearing)",
      `nodes before kill: ${before.length}`,
      `nodes after rebuild: ${after.length}`,
      `identical (updatedAt-stripped canonical dump): ${identical}`,
      "",
      `before: ${JSON.stringify(before, null, 2)}`,
      "",
      `after:  ${JSON.stringify(after, null, 2)}`,
      "",
    ].join("\n"),
  );
  console.log(`P1 exit ②: --rebuild produced an identical index — ${before.length} nodes`);

  // INTEGRATION: the app comes back up over the REBUILT index and serves the board.
  runtime = spawn("bun", ["run", RUNTIME_ENTRY], { env: runtimeEnv(), stdio: "inherit" });
  await waitHealthy();
  await page.goto("/app");
  await expect(page.getByTestId("board")).toBeVisible();
});
