// The deterministic loop fixture child (BRO-1806, DECISIONS §D8 layer 1) — a MINIMAL stand-in for the
// real Agent-SDK child (`broomva-child`, which lands with the runner/P2-exit BRO-1827). It drives the
// F3 beat loop against the REAL model proxy (→ the scripted mock upstream) and the REAL BRO-1795
// stop-condition engine, so `bun test:loops` exercises full F2→F3 flows end-to-end with ZERO tokens and
// NO API key. It is NOT wired into the runtime — the supervisor's injectable `spawnChild` seam runs it
// only from the loop tests, via `bun <this file> --scenario <s> --role agent --session <id>`.
//
// Its behavior is loop MECHANICS, not model intelligence: each beat it calls the proxy (a real 200, or
// the proxy's in-path 402 when the budget guard refuses), then feeds a scenario-scripted per-beat effect
// (empty diff / growing context) into `evaluateBeat` and acts on the decision — exactly what the shipped
// child's loop will do. The four scenarios map to the four guardrails the ticket must prove:
//   budget        → per_run cap trips → proxy 402 mid-run → halt budget       (tests BRO-1788)
//   no_progress   → 3 consecutive empty diffs → engine halt                    (tests BRO-1795)
//   fresh_context → context ceiling → checkpoint + restart → respawn resumes   (tests BRO-1795 + 1779)
//   kill          → announce a tool call, then hang mid-call → SIGKILL         (tests BRO-1801)

import type { ChildEmittedEvent } from "../harness/runner";
import {
  beatExitEvents,
  evaluateBeat,
  prepareRestart,
  readProgress,
} from "../harness/stop-conditions";

const proxyUrl = process.env.BROOMVA_MODEL_PROXY ?? "";
const token = process.env.BROOMVA_MODEL_TOKEN ?? "";
const runDir = process.env.BROOMVA_RUN_DIR ?? "";
const session = process.env.BROOMVA_SESSION ?? "session";

/** A tiny argv flag reader (`--scenario budget`). */
function flag(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}
const scenario = flag("--scenario") ?? "no_progress";

/** Emit one NDJSON event to stdout. `Bun.write(Bun.stdout, …)` (not process.stdout.write) so the line
 *  FLUSHES before exit — a child killed before an OS flush loses un-flushed events (the BRO-1767 lesson). */
async function emit(ev: ChildEmittedEvent): Promise<void> {
  await Bun.write(Bun.stdout, `${JSON.stringify(ev)}\n`);
}

/** POST one beat to the real proxy; return the HTTP status (200 = served, 402 = budget refused in-path). */
async function callProxy(beat: number): Promise<number> {
  const resp = await fetch(`${proxyUrl}/v1/messages`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    // No model id — the proxy resolves the pinned model by the session's role. max_tokens is required
    // for the budget reservation estimate.
    body: JSON.stringify({ max_tokens: 64, messages: [{ role: "user", content: `beat ${beat}` }] }),
  });
  return resp.status;
}

await emit({ actor: "system", type: "run.started" });

// ── kill: announce a tool call, then hang mid-call so a SIGKILL lands on a LIVE child (F8). ──
if (scenario === "kill") {
  await emit({
    actor: "agent",
    type: "tool.call",
    payload: { tool: "sh", input: "sleep infinity" },
  });
  await new Promise<never>(() => {}); // hang forever — the test kills us
}

// ── fresh_context respawn: on attempt ≥2 the checkpoint exists → resume, finish, complete (exit 0). ──
const checkpoint = await readProgress(runDir);
if (scenario === "fresh_context" && checkpoint) {
  // A fresh context (low tokens again) does one finishing beat through the real proxy, then completes —
  // proving the restart was lossless (the checkpoint told us what was left) and the proxy still serves.
  await callProxy(checkpoint.iteration + 1);
  await emit({
    actor: "agent",
    type: "agent.said",
    payload: {
      text: `resumed from checkpoint @${checkpoint.iteration}; finished ${JSON.stringify(checkpoint.whatsLeft)}`,
    },
  });
  await emit({ actor: "system", type: "run.exiting", payload: { code: 0 } });
  process.exit(0);
}

// ── the beat loop (F3) ──
const CEILING = scenario === "fresh_context" ? 100 : Number.MAX_SAFE_INTEGER;
const state = {
  iterations: 0,
  budget: { max_iterations: 100 }, // safety net; the SCENARIO condition (or the proxy 402) fires first
  spentUsd: 0,
  dayUsd: 0,
  recentDiffs: [] as string[],
  recentErrors: [] as string[],
  contextTokens: 0,
  ceiling: CEILING,
  noProgressN: 3,
};

for (let beat = 1; beat <= 60; beat++) {
  const status = await callProxy(beat);
  if (status === 402) {
    // The proxy refused in-path (BRO-1788): the child observes it and halts budget (F3 §1).
    for (const ev of beatExitEvents({ action: "halt", reason: "budget" }, { iteration: beat })) {
      await emit(ev);
    }
    process.exit(10);
  }

  state.iterations = beat;
  state.recentDiffs = [...state.recentDiffs, scenario === "no_progress" ? "" : `diff-${beat}`];
  if (scenario === "fresh_context") state.contextTokens = beat * 40; // → exceeds ceiling 100 at beat 3

  const decision = evaluateBeat(state);
  if (decision.action === "halt") {
    for (const ev of beatExitEvents(decision, { iteration: beat })) await emit(ev);
    process.exit(10);
  }
  if (decision.action === "restart") {
    const events = await prepareRestart(runDir, {
      progress: {
        session,
        iteration: beat,
        updated: "2026-07-11T00:00:00.000Z",
        stateOfTheWorld: `hit the context ceiling at beat ${beat}`,
        whatsLeft: ["finish the remaining work"],
      },
    });
    for (const ev of events) await emit(ev);
    process.exit(10);
  }
  // continue — next beat
}

// Safety terminal — a scenario that never tripped its condition completes cleanly rather than spinning.
await emit({ actor: "system", type: "run.exiting", payload: { code: 0 } });
process.exit(0);
