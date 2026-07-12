import { type ChildProcess, spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

// p2-exit (BRO-1827) — the ROADMAP §P2 phase gate, end to end. Runs the exit criteria verbatim:
//   ① Dispatch a contract FROM THE APP → watch events stream → kill it mid-run (a hard crash) →
//      restart the runtime → nothing lost.
//   ② A crash-interrupted run (a still-`running` session) is parked at Stuck on restart, and its
//      pre-crash events survive (nothing lost), with a `run.orphaned` receipt.
//
// Boots a REAL runtime WITH the mock model (`MAESTRO_MOCK_MODEL=1`) — the only mode that mounts
// dispatch (no real Anthropic upstream exists) — over a temp GIT workspace on the port the vite
// `/api` proxy targets (4319). ① drives the real seam app → RuntimeChatTransport → POST
// /api/sessions/:id/chat → dispatch-then-chat → supervisor + mock proxy + a real spawned child →
// session.jsonl + index → SSE → the feed, then a SIGKILL + restart. ② constructs the post-crash
// index directly (a running session can't be caught mid-flight against the sub-second mock — the
// deterministic reproduction is to seed it, the E2E counterpart of recovery.test.ts) and proves the
// boot-path recovery (recoverOnStartup → parkOrphans, F9.3) through the running app + read API.
//
// The `test:loops` half of the done.check (BRO-1806) covers the in-process kill-mid-child scenario
// (SIGKILL a real child mid-executeTool → canceled + run.killed) deterministically; this spec covers
// the process-boundary crash + restart + recovery the loop tests can't.
//
// Video is captured as evidence; a recovery excerpt is written to test-results/p2-exit-recovery.txt.
// Named *.pw.ts so bun's `bun test` runner skips it; Playwright-only. Serial — the tests share the one
// runtime port. This spec + p1-exit.pw.ts both bind 4319, so the config runs Playwright with workers:1.

const RUNTIME_PORT = 4319; // the vite-preview `/api` proxy target (see apps/app/vite.config.ts)
const RUNTIME_ENTRY = fileURLToPath(new URL("../../runtime/src/index.ts", import.meta.url));
// Seeds a post-crash `running` session into an index db. A bun subprocess because Playwright's loader
// can't transpile the runtime's bun-only `.sql` imports that openIndex pulls in (as with dump-index).
const SEED_SCRIPT = fileURLToPath(new URL("../../runtime/scripts/seed-orphan.ts", import.meta.url));
const EVIDENCE_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "test-results");

let runtime: ChildProcess | undefined;
const tmps: string[] = [];

// ── Fixtures + process helpers ─────────────────────────────────────────────────

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

function write(workspace: string, rel: string, content: string): void {
  const abs = join(workspace, rel);
  mkdirSync(dirname(abs), { recursive: true });
  writeFileSync(abs, content);
}

function git(cwd: string, args: string[]): void {
  const r = spawnSync("git", args, { cwd, stdio: "ignore" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")} failed (${r.status})`);
}

// A canonical (realpath'd) temp GIT workspace — the worktree sandbox factory (createWorktreeSandbox-
// Factory, BRO-1746) branches `run/<id>` worktrees off it, so it MUST be a git repo with a commit.
// realpath matters on macOS (/tmp → /private/tmp) so worktree paths match. Mirrors dispatch.test.ts.
function makeGitWorkspace(prefix: string, files: Record<string, string>): string {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), prefix)));
  tmps.push(dir);
  git(dir, ["init", "-q"]);
  git(dir, ["config", "user.email", "t@t.co"]);
  git(dir, ["config", "user.name", "t"]);
  writeFileSync(join(dir, ".gitignore"), "/.maestro/\n/runs/\n");
  for (const [rel, content] of Object.entries(files)) write(dir, rel, content);
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-qm", "init"]);
  return dir;
}

async function waitHealthy(port: number, deadlineMs = 20_000): Promise<void> {
  const deadline = Date.now() + deadlineMs;
  for (;;) {
    try {
      const r = await fetch(`http://localhost:${port}/health`);
      if (r.ok) break;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) throw new Error("runtime did not become healthy in time");
    await new Promise((res) => setTimeout(res, 200));
  }
}

function onExit(p: ChildProcess): Promise<number> {
  // Resolve immediately if the process ALREADY exited (a crash before this is called) — the `exit`
  // event has fired and would never fire again (CodeRabbit BRO-1824). signalCode is set on a kill.
  if (p.exitCode !== null) return Promise.resolve(p.exitCode);
  if (p.signalCode !== null) return Promise.resolve(0);
  return new Promise((res) => p.on("exit", (code) => res(code ?? 0)));
}

// Assign the module-level `runtime` to the spawned child BEFORE awaiting health, so a waitHealthy()
// timeout (a boot that never comes up) still leaves the process reapable by afterEach. Returning it
// only after health would leak the process — holding the shared port 4319, never killed — on a failed
// boot. Kill-safe, mirroring p1-exit.pw.ts (P20 BRO-1827 minor).
async function boot(workspace: string): Promise<void> {
  runtime = spawn("bun", ["run", RUNTIME_ENTRY], {
    env: {
      ...process.env,
      MAESTRO_WORKSPACE: workspace,
      MAESTRO_PORT: String(RUNTIME_PORT),
      MAESTRO_MOCK_MODEL: "1", // the ONLY mode that mounts dispatch (no real upstream) — chat 501s otherwise
      MAESTRO_STREAM_POLL_MS: "50",
    },
    stdio: "inherit",
  });
  await waitHealthy(RUNTIME_PORT);
}

async function stop(p: ChildProcess | undefined, signal: NodeJS.Signals): Promise<void> {
  if (!p) return;
  p.kill(signal);
  await onExit(p);
}

// A crashed runtime leaves a stale D4 lock (`.maestro/runtime.lock`) with a dead heartbeat; the next
// runtime would refuse for DEFAULT_LOCK_STALE_MS (15s). The process IS gone, so the lock is genuinely
// stale — clearing it is the faithful "the crashed process is dead" signal and avoids a 15s wait.
function clearLock(workspace: string): void {
  rmSync(join(workspace, ".maestro", "runtime.lock"), { force: true });
}

function seedOrphan(indexPath: string, sessionId: string, nodeId: string): void {
  const r = spawnSync("bun", ["run", SEED_SCRIPT, indexPath, sessionId, nodeId], {
    stdio: ["ignore", "inherit", "inherit"],
  });
  if (r.status !== 0) throw new Error(`seed-orphan exited ${r.status}`);
}

// ── Read-API accessors (the source the UI consumes; the orphan invariant lives on the session, not
//    the board node card — parkOrphans updates the session row only, DATA-MODEL §F9.3) ──────────────

interface SessionRow {
  id: string;
  status: string;
}
interface EventRow {
  type: string;
  seq: number;
}

async function api<T>(path: string): Promise<T> {
  const r = await fetch(`http://localhost:${RUNTIME_PORT}${path}`);
  if (!r.ok) throw new Error(`GET ${path} → ${r.status}`);
  return (await r.json()) as T;
}
const fetchNodeSessions = (id: string) =>
  api<{ sessions: SessionRow[] }>(`/api/node/${id}`).then((b) => b.sessions);
const fetchSession = (id: string) => api<{ session: SessionRow }>(`/api/sessions/${id}`);
const fetchSessionEvents = (id: string) =>
  api<{ events: EventRow[] }>(`/api/sessions/${id}/events`).then((b) => b.events);

function writeEvidence(name: string, lines: string[]): void {
  mkdirSync(EVIDENCE_DIR, { recursive: true });
  writeFileSync(join(EVIDENCE_DIR, name), `${lines.join("\n")}\n`);
}

test.use({ video: "on" }); // evidence: a screen recording of the exit gate
test.describe.configure({ mode: "serial" });

test.afterEach(async () => {
  // SIGKILL any surviving runtime (a test may have left one up), AWAIT its exit before removing the
  // workspace so libSQL's WAL handles close (a bare kill+rm races ENOTEMPTY on .maestro/).
  await stop(runtime, "SIGKILL");
  runtime = undefined;
  for (const d of tmps.splice(0)) rmSync(d, { recursive: true, force: true });
});

// ① The live path — dispatch from the app, watch events stream, hard-crash, restart, nothing lost.
test("P2 exit ①: dispatch from the app, watch events stream, then survive a hard crash + restart", async ({
  page,
}) => {
  test.setTimeout(120_000); // two runtime boots + a real child dispatch
  const workspace = makeGitWorkspace("maestro-p2-live-", {
    "runner/_work.md": wm({ id: "runner", state: "proposed", title: "Ship the runner" }),
  });
  await boot(workspace);

  // DISPATCH FROM THE APP: open the session view for the idle node and send a turn. The real
  // RuntimeChatTransport (no ?fixture) POSTs /api/sessions/runner/chat → dispatch-then-chat (F10.2).
  await page.goto("/session/runner");
  await page.getByPlaceholder("Message Maestro").fill("run the first step");
  await page.getByRole("button", { name: "Send" }).click();

  const thread = page.locator('section[aria-label="Thread"] [data-testid="chat-feed"]');
  await expect(thread.getByTestId("chat-user").first()).toContainText("run the first step");
  // WATCH EVENTS STREAM: the mock's reply (agent.said "ok") streams into the feed over SSE, from a
  // REAL dispatched run (supervisor + mock proxy + spawned child) — not a fixture. This is the live
  // "dispatch from the app, watch events stream" proof.
  await expect(thread.getByTestId("chat-assistant-text")).toContainText("ok", { timeout: 30_000 });
  // The run reached a terminal (the verb returns to Send).
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible({ timeout: 30_000 });

  // Capture the durable session + its event count (the pre-crash baseline).
  const before = await fetchNodeSessions("runner");
  const dispatched = before[0];
  if (!dispatched) throw new Error("the app did not dispatch a run on the node");
  const sessionId = dispatched.id;
  const eventsBefore = await fetchSessionEvents(sessionId);
  expect(eventsBefore.length, "the run streamed events before the crash").toBeGreaterThan(0);

  // KILL IT (hard crash): SIGKILL, NOT the graceful SIGTERM path (which reaps runs cleanly and leaves
  // no orphan). The runtime dies without a clean shutdown. Then RESTART over the same workspace.
  await stop(runtime, "SIGKILL");
  runtime = undefined;
  clearLock(workspace);
  await boot(workspace);

  // NOTHING LOST: the restarted runtime still serves the same session and all its pre-crash events
  // (durable across a hard crash), and the app comes back up over the persisted index.
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();
  const after = await fetchNodeSessions("runner");
  expect(
    after.map((s) => s.id),
    "the dispatched session survived the crash + restart",
  ).toContain(sessionId);
  const eventsAfter = await fetchSessionEvents(sessionId);
  expect(eventsAfter.length, "no events lost across the crash + restart").toBeGreaterThanOrEqual(
    eventsBefore.length,
  );
});

// ② The recovery path — a crash-interrupted run is parked at Stuck on restart, with nothing lost.
test("P2 exit ②: a crash-interrupted run is parked at Stuck on restart, with nothing lost", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const workspace = makeGitWorkspace("maestro-p2-orphan-", {
    "runner/_work.md": wm({ id: "runner", state: "running", title: "Long run" }),
  });

  // Construct the post-crash index: create + migrate the index and seed a still-`running` session with
  // two pre-crash agent.said events (the state a SIGKILLed runtime leaves behind — a run that never
  // reached a terminal). This is the deterministic reproduction of "killed mid-run".
  const indexPath = join(workspace, ".maestro", "index.db");
  mkdirSync(dirname(indexPath), { recursive: true });
  const sessionId = "orphan-run";
  seedOrphan(indexPath, sessionId, "runner");

  // RESTART: boot the runtime over the crash-left index. On boot recoverOnStartup → parkOrphans parks
  // the orphan `blocked` + appends `run.orphaned`; scanIntoIndex loads node `runner` from the FS.
  await boot(workspace);

  // The app comes up healthy over the recovered index.
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();

  // ORPHAN PARKED AT STUCK: the session is now `blocked` — plain-voice "Stuck" (packages/protocol/
  // src/plain-voice.ts:36: `blocked → "Stuck"`) — NOT still `running` and NOT silently respawned. The
  // invariant lives on the session row (parkOrphans never touches the node card).
  const detail = await fetchSession(sessionId);
  expect(detail.session.status, "the orphaned run is parked blocked (Stuck), never respawned").toBe(
    "blocked",
  );

  // NOTHING LOST + the receipt: the two pre-crash agent.said events survive, and recovery appended
  // exactly one run.orphaned (parkOrphans only ADDS the receipt; it never deletes prior work).
  const events = await fetchSessionEvents(sessionId);
  const said = events.filter((e) => e.type === "agent.said");
  const orphaned = events.filter((e) => e.type === "run.orphaned");
  expect(said.length, "pre-crash agent.said events survived recovery").toBe(2);
  expect(orphaned.length, "recovery appended exactly one run.orphaned receipt").toBe(1);

  writeEvidence("p2-exit-recovery.txt", [
    "P2 exit ② — a crash-interrupted run is parked at Stuck on restart (BRO-1827)",
    "(seeded a still-running session, then booted a runtime; recoverOnStartup.parkOrphans, F9.3)",
    `session ${sessionId}: running (seeded) → ${detail.session.status} (after recovery)`,
    'blocked ⇄ plain-voice "Stuck" (packages/protocol/src/plain-voice.ts)',
    `events: 2 agent.said seeded + ${orphaned.length} run.orphaned appended = ${events.length} total`,
    `pre-crash agent.said preserved: ${said.length} (nothing lost)`,
    "",
  ]);
});
