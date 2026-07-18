// The orchestrator's decision policy (ORCHESTRATOR §3) — the ordered checklist made EXECUTABLE and
// DETERMINISTIC. Given a Briefing (§2) it produces the tick's decisions + the §7 wake log, with NO LLM.
//
// This function IS the checklist the versioned prompt (`prompts/orchestrator.md`) states in prose, and it
// is the mock-model tick's brain: the real orchestrator agent reasons to the same decisions from the same
// briefing; the mock/deterministic tick runs THIS. It NEVER emits — it decides. Turning a decision into
// intents / chat / a durable wake-log event is the tick's job (slice 2b); keeping decide and emit split
// keeps the policy pure and replay-testable (the done.check drives it over a seeded index, no model).
//
// §3 is evaluated top-to-bottom; earlier rules preempt later ones. "Nothing" is a first-class outcome
// (§3, §7). Rules 3.1–3.3 (safety · surface · dispatch) are deterministic and implemented here. Rules 3.4
// (propose schedule changes) and 3.5 (propose new missions) are discretionary "may" clauses that depend
// on cross-tick history / pattern recognition the briefing does not carry — they are the agent's creative
// latitude, NOT deterministic, so the mock policy emits none (surfaced honestly, not silently dropped).

import { type OrchState, plainVoice, type TickCause } from "@maestro/protocol";
import type { Briefing } from "../tick/briefing";

/** §3.1 — a running session silent beyond this is nudged (then, if still silent after a prior nudge, the
 *  human is asked to look). 30 min (ORCHESTRATOR §3.1). */
export const STALE_RUN_MS = 30 * 60 * 1000;
/** §3.1 — day-budget spent ratio at/above which the tick dispatches nothing. */
export const DAY_BUDGET_HALT_RATIO = 0.9;

export interface OrchestratorOptions {
  /** override the §3.1 staleness threshold (default STALE_RUN_MS). */
  staleRunMs?: number;
  /** override the §3.1 day-budget halt ratio (default DAY_BUDGET_HALT_RATIO). */
  budgetHaltRatio?: number;
  /** session ids ALREADY nudged in their CURRENT stale window. A run stale again despite a prior nudge
   *  escalates to "recommend the human look" (§3.1 — the orchestrator cannot kill without a grant, §5).
   *  The tick supplies this from its wake-log history (slice 2b); absent → none nudged (first nudge).
   *
   *  2b CONTRACT (this slice's escalation is only sound if 2b honors it — flagged so it isn't wired naively):
   *  (a) a nudge must write an event that moves the session's `max(event.ts)`, so a revived run's
   *      `lastEventAgeMs` drops below the threshold and it leaves the stale set naturally; and
   *  (b) this set is scoped to the CURRENT stale window — a run that revived (produced worker activity)
   *      then went stale AGAIN must NOT be in it (it earns a fresh first nudge, not an escalation). "Ever
   *      nudged" is the wrong signal; derive membership from nudges SINCE the run's last worker activity. */
  nudgedSessionIds?: ReadonlySet<string>;
}

/** §3.1 — a running session to nudge (first stale) or to hand to the human (stale after a prior nudge). */
export interface RunAttention {
  sessionId: string;
  nodeId: string;
  /** how long the run has been silent (its last-event age). */
  ageMs: number;
}
/** §3.3 — a node dispatched this tick. */
export interface DispatchDecision {
  nodeId: string;
  state: string;
}
/** §3.3 — a queue node left alone this tick, with the one-line why (§7). */
export interface DeferDecision {
  nodeId: string;
  reason: string;
}
/** §3.2 — an attention-list line surfaced for the human (never cleared by the orchestrator). */
export interface AttentionLine {
  nodeId: string;
  state: string;
  ageMs: number;
}

/** The deterministic outcome of one tick's decision policy (§3) + its rendered wake log (§7). */
export interface OrchestratorDecision {
  cause: TickCause;
  /** §3.1 — non-null when the day budget is ≥ the halt ratio, so the tick dispatched nothing. */
  budgetHalt: { reason: string } | null;
  /** §3.1 — first-stale running sessions to nudge (one chat restating the goal — the task-drift defense). */
  nudges: RunAttention[];
  /** §3.1 — sessions still stale after a prior nudge: recommend the human look (cannot kill, §5). */
  needsHuman: RunAttention[];
  /** §3.2 — the attention list, surfaced verbatim. */
  attention: AttentionLine[];
  /** §3.3 — nodes dispatched (triggered first, then runnable proposed, up to the concurrency cap). */
  dispatches: DispatchDecision[];
  /** §3.3 — queue nodes left alone, with why (day-budget hold / at cap / not runnable). */
  deferrals: DeferDecision[];
  /** §7 — the wake-log narrative: why I woke → needs you → what I did → what I left alone. */
  wakeLog: string;
}

/**
 * Run the §3 decision policy over `briefing`, deterministically. Pure — no db, no clock, no model — so a
 * test replays it over a seeded briefing and asserts each checklist rule fires. The returned decision is
 * what the tick then EMITS (slice 2b); here it is only decided + narrated.
 */
export function decidePolicy(
  briefing: Briefing,
  opts: OrchestratorOptions = {},
): OrchestratorDecision {
  const staleRunMs = opts.staleRunMs ?? STALE_RUN_MS;
  const haltRatio = opts.budgetHaltRatio ?? DAY_BUDGET_HALT_RATIO;
  const nudged = opts.nudgedSessionIds ?? new Set<string>();

  // §3.1 safety first — day budget ≥ ratio → dispatch nothing (noted below; the dispatch loop honors it).
  const { daySpentUsd, dayBudgetUsd } = briefing.ledger;
  const budgetHalt =
    dayBudgetUsd != null && dayBudgetUsd > 0 && daySpentUsd >= haltRatio * dayBudgetUsd
      ? { reason: `day budget ${Math.round((daySpentUsd / dayBudgetUsd) * 100)}% spent` }
      : null;

  // §3.1 stale runs — silent > staleRunMs: first time → nudge; still silent after a prior nudge → recommend
  // the human look. A nudge is itself an event, so a run stale AGAIN despite a prior nudge is one the nudge
  // did not revive (the nudged set is the tick's memory of that, supplied via opts / slice 2b).
  const nudges: RunAttention[] = [];
  const needsHuman: RunAttention[] = [];
  for (const r of briefing.activeRuns) {
    if (r.lastEventAgeMs <= staleRunMs) continue;
    const line: RunAttention = {
      sessionId: r.sessionId,
      nodeId: r.nodeId,
      ageMs: r.lastEventAgeMs,
    };
    (nudged.has(r.sessionId) ? needsHuman : nudges).push(line);
  }

  // §3.2 surface, don't clear — the attention list is for the human; make it visible, never decide it.
  const attention: AttentionLine[] = briefing.attention.map((n) => ({
    nodeId: n.nodeId,
    state: n.state,
    ageMs: n.ageMs,
  }));

  // §3.3 dispatch queued work — while running < cap: triggered first, then runnable proposed. The queue
  // arrives triggered-first, oldest-first (assembleBriefing's attention order). A budget halt dispatches
  // nothing; a non-runnable proposed node stays queued with its reason; the rest defer once slots run out.
  const dispatches: DispatchDecision[] = [];
  const deferrals: DeferDecision[] = [];
  let slots = Math.max(0, briefing.ledger.concurrencyCap - briefing.ledger.activeRuns);
  for (const n of briefing.queue) {
    if (budgetHalt) {
      deferrals.push({ nodeId: n.nodeId, reason: "day budget hold" });
      continue;
    }
    // Runnability gates PROPOSED only (§3.3: "then proposed nodes only if the contract is runnable"; a
    // triggered node was explicitly triggered and dispatches first). Checked BEFORE the slot check so a
    // broken proposed node reports its REAL blocker (no budget / no check) rather than the misleading
    // "waiting for a slot" — it will never dispatch until the contract is fixed, so the contract reason is
    // the actionable one (forward-honesty in the wake log). It never consumed a slot either way.
    if (n.state === "proposed" && !n.runnable) {
      deferrals.push({ nodeId: n.nodeId, reason: n.notRunnableReason ?? "not runnable" });
      continue;
    }
    if (slots <= 0) {
      deferrals.push({ nodeId: n.nodeId, reason: "at concurrency cap" });
      continue;
    }
    dispatches.push({ nodeId: n.nodeId, state: n.state });
    slots--;
  }

  const decision: OrchestratorDecision = {
    cause: briefing.cause,
    budgetHalt,
    nudges,
    needsHuman,
    attention,
    dispatches,
    deferrals,
    wakeLog: "",
  };
  decision.wakeLog = renderWakeLog(decision);
  return decision;
}

// ── §7 wake log ───────────────────────────────────────────────────────────────
// "why I woke → what needs you (if anything) → what I did → what I left alone (and why)". Plain voice, no
// enum names, one line per decision, lead with anything that needs them. A tick that did nothing still
// writes two lines (silence reads as breakage) — the "Woke on…" + "Did nothing…" pair guarantees it.

const CAUSE_PHRASE: Record<TickCause, string> = {
  interval: "a scheduled check",
  worker_return: "a run finishing",
  manual: "a manual tick",
  hook: "a workspace hook",
};

/** §6/§7 plain voice — the wake log is read by a person having coffee, so the policy's technical deferral
 *  reasons (kept precise on the decision object for programmatic use / tests) are mapped to coffee-voice
 *  with no system terms. An unknown reason passes through verbatim (never a crash, never a blank line). */
const PLAIN_REASON: Record<string, string> = {
  "day budget hold": "the day's budget is used up",
  "at concurrency cap": "waiting for a free slot",
  "no budget block": "no spending limit set",
  "no done.check or judge rubric": "no way to check when it's done",
  "not runnable": "not ready to run yet",
};
const plainReason = (reason: string): string => PLAIN_REASON[reason] ?? reason;

/**
 * Every deferral reason `decidePolicy` can emit — the two the dispatch loop writes directly, the two
 * `contractRunnableReason` (protocol §3.3) can return, and the loop's fallback. A guard test asserts each
 * has a `PLAIN_REASON` entry, so a new reason can never reach the human wake log untranslated (the leak
 * class the P20 review caught). If you add a deferral reason, add it here AND to PLAIN_REASON.
 */
export const DEFERRAL_REASONS = [
  "day budget hold",
  "at concurrency cap",
  "no budget block",
  "no done.check or judge rubric",
  "not runnable",
] as const;

/** Soften a technical deferral reason to coffee-voice for the wake log (exported for the coverage guard). */
export const softenReason = (reason: string): string => plainReason(reason);

const minutes = (ms: number): string => {
  const m = Math.round(ms / 60000);
  return m <= 0 ? "under a minute" : m === 1 ? "1 minute" : `${m} minutes`;
};
const indent = (s: string): string => `  ${s}`;
/** A node reference the UI links to its card (§7 "names the node and links its card"). */
const nodeRef = (nodeId: string): string => `[${nodeId}](#node/${nodeId})`;

/** Render the §7 wake log from a decision. Pure + deterministic (drives off the decision alone). */
export function renderWakeLog(d: OrchestratorDecision): string {
  const lines: string[] = [`Woke on ${CAUSE_PHRASE[d.cause]}.`];

  // What needs you — lead with it (§6 "lead with anything that needs them"). Plain voice, no em dashes
  // (CLAUDE.md §Voice — no em dashes in user-facing copy; the wake log is read by a person).
  const needs: string[] = [];
  for (const h of d.needsHuman) {
    needs.push(
      `${nodeRef(h.nodeId)} has been quiet ${minutes(h.ageMs)} even after a nudge, worth a look.`,
    );
  }
  for (const a of d.attention) {
    needs.push(
      `${nodeRef(a.nodeId)} ${plainVoice(a.state as OrchState).toLowerCase()} (${minutes(a.ageMs)}).`,
    );
  }
  if (d.budgetHalt) needs.push(`Holding all new work: ${d.budgetHalt.reason}.`);
  if (needs.length) {
    lines.push("Needs you:");
    lines.push(...needs.map(indent));
  }

  // What I did.
  const did: string[] = [];
  for (const disp of d.dispatches) did.push(`Started ${nodeRef(disp.nodeId)}.`);
  for (const n of d.nudges) did.push(`Nudged ${nodeRef(n.nodeId)} (quiet ${minutes(n.ageMs)}).`);
  if (did.length) {
    lines.push("Did:");
    lines.push(...did.map(indent));
  } else {
    lines.push("Did nothing new this tick.");
  }

  // What I left alone (and why). Plain voice (no system terms), no em dashes (CLAUDE.md §Voice) — a colon
  // carries the reason. The decision object keeps the precise technical reason; only the copy is softened.
  if (d.deferrals.length) {
    lines.push("Left alone:");
    lines.push(
      ...d.deferrals.map((x) => indent(`${nodeRef(x.nodeId)}: ${plainReason(x.reason)}.`)),
    );
  }

  return lines.join("\n");
}
