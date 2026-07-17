import { type ChildProcess, spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

// p3-exit (BRO-1821) — the ROADMAP §P3 phase gate, end to end. Runs the exit criteria verbatim:
//   ① A CLEAN run dispatched FROM THE APP drives a real child to a completion gate ("Needs you"), and
//      an `approve` squash-merges the run branch → the node lands `done` and the work lands in the tree.
//   ② A `revise` (send-back) redispatches the run: the gate is decided `revise` and the node → `triggered`.
//
// Boots a REAL runtime WITH the mock model (`MAESTRO_MOCK_MODEL=1`, the only mode that mounts dispatch —
// no real Anthropic upstream exists) AND a scripted upstream via `MAESTRO_MOCK_SCRIPT` (loadMockScriptFromEnv,
// dispatch.ts) on the port the vite `/api` proxy targets (4319), over a temp GIT workspace. The mock script's
// FIRST turn is a `shell` tool_use that EDITS + COMMITS on the run branch (a real agent commits its work —
// without a commit, approve refuses `empty_run`); the fallback ends the turn. The child's edit satisfies the
// node's `done.check` (`test -f feature.txt`), so a completion gate opens and the node parks at review
// (plain-voice "Needs you"). This is the "one live gated run end-to-end" evidence gate-slice.test.ts flagged
// as missing — here the gate is genuinely LIVE, opened by a real dispatched run, not hand-seeded.
//
// CRITICAL (P20, [[maestro-build-arc]]): the mock-script JSON file lives in a SEPARATE temp dir OUTSIDE the git
// workspace. A script file written inside the workspace would be an untracked file that dirties the tree and
// wedges `approveMerge` (dirty_workspace). Same reason the runtime's own transient dirs (.maestro/, runs/) are
// gitignored.
//
// The verdict is driven via the WRITE API (POST /api/intents), not a browser click, for the SAME structural
// reason gate-slice.test.ts documents: the SPA's gate-queue learns a gate's id + its full row ONLY by hydrating
// the gate-queue (a session + gate-row read), whereas the live SSE `gate.opened` payload the shell subscribes to
// is minimal ({ gateId, kind }) — it does not carry the gate row + session the approve/revise verb interaction
// renders from. So a browser-driven approve would need gate-queue-hydration plumbing out of proportion to this
// exit gate. The NEW evidence this spec adds over gate-slice.test.ts (a runtime integration test) is that the run
// is genuinely LIVE: dispatched from the app, driven by a real child, reaching a real gate — and the browser
// carries the live-dispatch + "Needs you" ⇄ cleared evidence, while the intents write API carries the decision.
//
// Video is captured as evidence; a round-trip summary is written to test-results/p3-exit-approve.txt. Named
// *.pw.ts so bun's `bun test` runner skips it; Playwright-only. Serial — the two tests share the one runtime
// port 4319 (the single shared vite-preview `/api` target), so they must never run concurrently.

const RUNTIME_PORT = 4319; // the vite-preview `/api` proxy target (see apps/app/vite.config.ts)
const RUNTIME_ENTRY = fileURLToPath(new URL("../../runtime/src/index.ts", import.meta.url));
const EVIDENCE_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "test-results");

// The scripted upstream (mock-model.ts `MockModelOptions` shape, loaded via MAESTRO_MOCK_SCRIPT): the first
// call returns a `shell` tool_use that writes `feature.txt` and COMMITS it on the run branch (the done.check
// `test -f feature.txt` then passes → a completion gate opens); once exhausted the fallback ends the turn.
const MOCK_SCRIPT = {
  script: [
    {
      body: {
        id: "m",
        type: "message",
        role: "assistant",
        content: [
          {
            type: "tool_use",
            id: "e",
            name: "shell",
            input: {
              command: "echo feature > feature.txt && git add -A && git commit -q -m 'add feature'",
            },
          },
        ],
        stop_reason: "tool_use",
        usage: {},
      },
    },
  ],
  fallback: {
    body: {
      id: "m2",
      type: "message",
      role: "assistant",
      content: [{ type: "text", text: "done" }],
      stop_reason: "end_turn",
      usage: {},
    },
  },
} as const;

let runtime: ChildProcess | undefined;
const tmps: string[] = [];

// ── Fixtures + process helpers (harness reused verbatim from p2-exit.pw.ts) ─────────────────────

// A work-contract `_work.md` carrying a `done.check` + `gate: human` — the completion contract the run must
// satisfy to open a gate (DATA-MODEL §work-contract). The base `wm` in p2-exit has no gate/done; this node
// needs both so a clean run parks at review instead of auto-completing.
const wmDone = (o: { id: string; check: string; title?: string }): string =>
  [
    "---",
    `id: ${o.id}`,
    "kind: task",
    "state: proposed",
    "gate: human",
    "done:",
    `  check: ${o.check}`,
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

// A canonical (realpath'd) temp GIT workspace — the worktree sandbox factory branches `run/<id>` worktrees off
// it, so it MUST be a git repo with a commit. realpath matters on macOS (/tmp → /private/tmp) so worktree paths
// match. Mirrors p2-exit.pw.ts / dispatch.test.ts.
function makeGitWorkspace(prefix: string, files: Record<string, string>): string {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), prefix)));
  tmps.push(dir);
  git(dir, ["init", "-q"]);
  git(dir, ["config", "user.email", "t@t.co"]);
  git(dir, ["config", "user.name", "t"]);
  git(dir, ["config", "commit.gpgsign", "false"]);
  writeFileSync(join(dir, ".gitignore"), "/.maestro/\n/runs/\n");
  for (const [rel, content] of Object.entries(files)) write(dir, rel, content);
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-qm", "init"]);
  return dir;
}

// Write the mock-script JSON to a SEPARATE temp dir OUTSIDE the git workspace (see the file header — a script
// file inside the workspace dirties the tree and wedges approve). Pushed to the cleanup list.
function writeMockScript(): string {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "maestro-p3-script-")));
  tmps.push(dir);
  const scriptPath = join(dir, "mock-script.json");
  writeFileSync(scriptPath, JSON.stringify(MOCK_SCRIPT));
  return scriptPath;
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
  // Resolve immediately if the process ALREADY exited — the `exit` event has fired and would never fire again,
  // so a bare listener would hang until Playwright's timeout (CodeRabbit BRO-1824). signalCode is set on a kill.
  if (p.exitCode !== null) return Promise.resolve(p.exitCode);
  if (p.signalCode !== null) return Promise.resolve(0);
  return new Promise((res) => p.on("exit", (code) => res(code ?? 0)));
}

// Assign the module-level `runtime` BEFORE awaiting health so a waitHealthy() timeout still leaves the process
// reapable by afterEach (a boot that never comes up would otherwise leak, holding port 4319). Kill-safe, mirrors
// p2-exit.pw.ts. When `scriptPath` is set, MAESTRO_MOCK_SCRIPT drives the scripted-to-gate dispatch (BRO-1821).
async function boot(workspace: string, scriptPath?: string): Promise<void> {
  runtime = spawn("bun", ["run", RUNTIME_ENTRY], {
    env: {
      ...process.env,
      MAESTRO_WORKSPACE: workspace,
      MAESTRO_PORT: String(RUNTIME_PORT),
      MAESTRO_MOCK_MODEL: "1", // the ONLY mode that mounts dispatch (no real upstream) — chat 501s otherwise
      MAESTRO_STREAM_POLL_MS: "50",
      ...(scriptPath ? { MAESTRO_MOCK_SCRIPT: scriptPath } : {}),
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

// ── Read + write API accessors (the source the UI consumes) ─────────────────────────────────────

interface GateRow {
  id: string;
  verdict: string | null;
  sessionId: string;
}
interface NodeDetailResp {
  node: { id: string; state: string };
  gates: GateRow[];
}

async function api<T>(path: string): Promise<T> {
  const r = await fetch(`http://localhost:${RUNTIME_PORT}${path}`);
  if (!r.ok) throw new Error(`GET ${path} → ${r.status}`);
  return (await r.json()) as T;
}
const fetchNode = (id: string) => api<NodeDetailResp>(`/api/node/${id}`);

// POST an intent on the ONE write surface. Every intent requires an `Idempotency-Key` header (API §1); a fresh
// uuid per call means each is a distinct decision (no accidental dedupe).
async function postIntent(body: Record<string, unknown>): Promise<Response> {
  return fetch(`http://localhost:${RUNTIME_PORT}/api/intents`, {
    method: "POST",
    headers: { "Content-Type": "application/json", "Idempotency-Key": randomUUID() },
    body: JSON.stringify(body),
  });
}

// Poll the read API until the node is at `review` with an OPEN gate (verdict null) — a real child dispatch +
// verify can take up to ~30s, so the deadline is generous. Tolerates transient 404s (the file may not be
// indexed the instant the run starts) by retrying. Returns the gate + session ids the verbs act on.
async function waitForOpenGate(
  nodeId: string,
  deadlineMs: number,
): Promise<{ gateId: string; sessionId: string }> {
  const started = Date.now();
  const deadline = started + deadlineMs;
  for (;;) {
    try {
      const detail = await fetchNode(nodeId);
      if (detail.node.state === "review") {
        const open = detail.gates.find((g) => g.verdict === null);
        if (open) {
          console.log(`[p3-exit] ${nodeId} reached review+open-gate in ${Date.now() - started}ms`);
          return { gateId: open.id, sessionId: open.sessionId };
        }
      }
    } catch {
      // node not yet indexed / a mid-scan race — retry until the deadline
    }
    if (Date.now() > deadline) {
      throw new Error(
        `node ${nodeId} did not reach review with an open gate within ${deadlineMs}ms`,
      );
    }
    await new Promise((res) => setTimeout(res, 500));
  }
}

function branchList(workspace: string): string {
  const r = spawnSync("git", ["branch", "--list"], { cwd: workspace, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git branch --list failed (${r.status})`);
  return r.stdout;
}

function writeEvidence(name: string, lines: string[]): void {
  mkdirSync(EVIDENCE_DIR, { recursive: true });
  writeFileSync(join(EVIDENCE_DIR, name), `${lines.join("\n")}\n`);
}

test.use({ video: "on" }); // evidence: a screen recording of the exit gate
test.describe.configure({ mode: "serial" });

test.afterEach(async () => {
  // SIGKILL any surviving runtime, AWAIT its exit before removing the workspace so libSQL's WAL handles close
  // (a bare kill+rm races ENOTEMPTY on .maestro/). Mirrors p2-exit.pw.ts.
  await stop(runtime, "SIGKILL");
  runtime = undefined;
  for (const d of tmps.splice(0)) rmSync(d, { recursive: true, force: true });
});

// ① The clean-run path — dispatch from the app, park at "Needs you", approve → squash-merge → done.
test("P3 exit ①: a clean run dispatched from the app parks at Needs you; approve squash-merges the branch", async ({
  page,
}) => {
  test.setTimeout(120_000); // a real child dispatch + verify + squash-merge
  const workspace = makeGitWorkspace("maestro-p3-approve-", {
    "feat/_work.md": wmDone({ id: "feat", check: "test -f feature.txt", title: "Ship" }),
  });
  const scriptPath = writeMockScript();
  await boot(workspace, scriptPath);

  // DISPATCH FROM THE APP: open the session view for the idle node and send a turn. The real
  // RuntimeChatTransport POSTs /api/sessions/feat/chat → dispatch-then-chat → supervisor + mock proxy + a real
  // spawned child that runs the scripted `shell` edit+commit.
  await page.goto("/session/feat");
  await page.getByPlaceholder("Message Maestro").fill("ship the feature");
  await page.getByRole("button", { name: "Send" }).click();

  // Confirm the composer submitted (the client registered the turn → the POST fired) before we depend on the run.
  const thread = page.locator('section[aria-label="Thread"] [data-testid="chat-feed"]');
  await expect(thread.getByTestId("chat-user").first()).toContainText("ship the feature");

  // Poll the LIVE read API (independent of the browser render — the run continues even if the SSE render ends,
  // FLOWS §F10.4) until the node parks at review with an open completion gate.
  const { gateId, sessionId } = await waitForOpenGate("feat", 40_000);

  // BROWSER live-UI evidence: the board is up, and the sidebar's adaptive lens shows "Needs you" (needsYou > 0 →
  // the Inbox lens + count badge, sidebar.tsx; selectNeedsYouCount counts the leaf `review` node). `.first()`
  // because "Needs you" also appears in the top-bar attention pill (case-insensitive substring → 2 matches).
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();
  await expect(page.getByText("Needs you").first()).toBeVisible({ timeout: 10_000 });

  // APPROVE via the write API (see the file header for WHY the verdict is driven here, not by a browser click).
  const approve = await postIntent({ type: "approve", gateId });
  expect(approve.status, "approve is accepted (202)").toBe(202);

  // THE REAL MERGE: the node lands `done`, the run's work (feature.txt) lands in the workspace root, and the
  // run branch is archived — `run/<id>` → `archive/run-<id>` (the branch is the receipt). Poll `done` (the merge
  // + durable _work.md write follow the 202 on the stream, not in the body).
  const deadline = Date.now() + 30_000;
  for (;;) {
    const detail = await fetchNode("feat");
    if (detail.node.state === "done") break;
    if (Date.now() > deadline)
      throw new Error(`node did not reach done after approve (last: ${detail.node.state})`);
    await new Promise((res) => setTimeout(res, 300));
  }
  expect(
    existsSync(join(workspace, "feature.txt")),
    "the run's work landed in the workspace root",
  ).toBe(true);
  const branches = branchList(workspace);
  expect(branches, "the run branch was archived on merge").toContain(`archive/run-${sessionId}`);
  expect(branches, "no live run/<id> branch survives the merge").not.toContain(`run/${sessionId}`);

  // BROWSER: the "Needs you" indicator has CLEARED — the queue is empty (node done), so the sidebar lens falls
  // back to "Maestro" and the top-bar pill is gone. A fresh load hydrates from /api/tree (feat now `done`), so
  // no "Needs you" text remains on the board route.
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();
  await expect(page.getByText("Needs you")).toHaveCount(0);

  writeEvidence("p3-exit-approve.txt", [
    "P3 exit ① — a clean run dispatched from the app parks at Needs you; approve squash-merges (BRO-1821)",
    "(a REAL runtime + mock model + MAESTRO_MOCK_SCRIPT: the child edits+commits, done.check passes, a gate opens)",
    `dispatched from the app: POST /api/sessions/feat/chat → run session ${sessionId}`,
    `run parked at review (Needs you) with open gate ${gateId}`,
    "approve → POST /api/intents → 202 → squash-merge",
    `node feat: proposed → review → done; feature.txt landed in the workspace root`,
    `run/${sessionId} → archive/run-${sessionId} (the branch is the receipt)`,
    'browser: "Needs you" lens shown at the gate, cleared after done',
    "",
  ]);
});

// ② The send-back path — a `revise` redispatches the run: the gate is decided `revise`, the node → `triggered`.
test("P3 exit ②: send-back decides the gate revise → node returns to triggered (re-queued for dispatch) with feedback", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const workspace = makeGitWorkspace("maestro-p3-revise-", {
    "feat/_work.md": wmDone({ id: "feat", check: "test -f feature.txt", title: "Ship" }),
  });
  const scriptPath = writeMockScript();
  await boot(workspace, scriptPath);

  // Dispatch from the app; reach review + an open gate (same live path as ①).
  await page.goto("/session/feat");
  await page.getByPlaceholder("Message Maestro").fill("ship the feature");
  await page.getByRole("button", { name: "Send" }).click();
  const thread = page.locator('section[aria-label="Thread"] [data-testid="chat-feed"]');
  await expect(thread.getByTestId("chat-user").first()).toContainText("ship the feature");
  const { gateId } = await waitForOpenGate("feat", 40_000);

  // REVISE (send-back): decide the gate `revise` with feedback → the node returns to `triggered` for a fresh
  // dispatch (the feedback rides the gate.decided payload the redispatched run picks up). Events on the stream,
  // not this 202. No background scan auto-redispatches `triggered`, so the state is stable to assert.
  const revise = await postIntent({ type: "revise", gateId, feedback: "tighten the migration" });
  expect(revise.status, "revise is accepted (202)").toBe(202);

  const detail = await fetchNode("feat");
  expect(detail.node.state, "send-back returns the node to triggered (redispatched)").toBe(
    "triggered",
  );
  expect(detail.gates.find((g) => g.id === gateId)?.verdict, "the gate was decided revise").toBe(
    "revise",
  );

  // Light browser evidence: the board is still up over the redispatched node.
  await page.goto("/");
  await expect(page.getByTestId("board")).toBeVisible();
});
