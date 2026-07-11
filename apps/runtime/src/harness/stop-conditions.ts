// The stop-condition engine (FLOWS §F3 steps 3–5, HARNESS §5, AUTONOMY §3–4, BRO-1795) — the CHILD
// side of the Loop-1 guardrail seam. A running agent child evaluates this at the END of every beat to
// decide whether to keep going, HALT (park blocked, a human should look), or RESTART with a fresh
// context (respawn, keep going). It is a PURE engine over the loop's live numbers plus two disk-memory
// helpers (progress.md rewrite + fix_plan.md tick) — no ambient clock, no config reads, no live child.
// The child binary (`broomva-child`, P2 exit BRO-1827) consumes it; the supervisor (BRO-1779) already
// consumes the OTHER side — it reads `run.exiting {code:10, reason}` and either parks (halt reasons) or
// respawns (fresh_context), same session/worktree/run_budget.
//
// The three HALT conditions are the AUTONOMY §4 guardrail ("loops don't get tired"): a loop on a vague
// goal is a token furnace, and the common incident is retry-retry-retry, not a wrong answer. Each halt
// ends the loop with exit 10 + a pinned `ExitReason`:
//   • iteration_cap — iterations >= (contract budget.max_iterations ?? runtime default ?? 30)
//   • no_progress   — N consecutive empty diffs OR N identical non-empty errors ("agreeing with itself")
//   • budget        — run/day dollar caps reached (the in-path budget guard BRO-1788 is the hard
//                     pre-call 402; this is the end-of-beat backstop for "already exhausted")
// FRESH-CONTEXT RESTART is NOT a halt — it is a continuation: at the context ceiling the child rewrites
// its disk memory (so no work is lost), emits `run.restart_requested` + exit 10 `fresh_context`, and the
// supervisor respawns it. Memory-on-disk is what makes the restart lossless (AUTONOMY §3: filesystem
// memory beats context, 74% vs 68.5%; the 80%-reliability time-horizon is far shorter than the 50% one,
// so durable work must cross that cliff via restarts).
//
// Why the engine is a pure function of numeric state (not coupled to the proxy): the in-path budget
// guard (BRO-1788) enforces the SAME thresholds before each model call and refuses 402; this engine
// re-derives the same conclusion from the same numbers at end-of-beat. They agree because they check
// the same caps — defense-in-depth, no shared mutable coupling.

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { Budget, ExitReason, StopCondition } from "@maestro/protocol";
import { DEFAULT_STOP_ON, EVENT_TYPES } from "@maestro/protocol";
import { DEFAULT_MAX_ITERATIONS, DEFAULT_NO_PROGRESS_N } from "../config";
import type { ChildEmittedEvent } from "./runner";

// ── State + decisions ──────────────────────────────────────────────────────────

/**
 * The loop's live numbers at end-of-beat — everything the engine reasons over (F3 §5). All values are
 * cumulative for the RUN (which spans fresh-context respawns; budgets span attempts, not processes).
 * `recentDiffs`/`recentErrors` are the child's rolling per-beat signatures, oldest→newest; the caller
 * keeps at least the last N. The config knobs are threaded in so the engine stays pure (no ambient reads).
 */
export interface BeatState {
  /** Beats completed so far this run (accumulates across respawns). */
  iterations: number;
  /** The contract's budget block — `max_iterations` overrides the runtime default (frontmatter wins). */
  budget: Budget;
  /** Dollars spent this run so far (per_run scope). */
  spentUsd: number;
  /** Dollars spent workspace-wide today (per_day scope). */
  dayUsd: number;
  /** Per-beat diff signatures, oldest→newest. `""` = the beat produced NO change (an empty diff). */
  recentDiffs: readonly string[];
  /** Per-beat terminal-error signatures, oldest→newest. `""` = the beat had no error. */
  recentErrors: readonly string[];
  /** Current context size (tokens) the child is carrying. */
  contextTokens: number;
  /** Context-size ceiling (tokens) past which the child restarts fresh. `<= 0` disables the check. */
  ceiling: number;
  /** Which halt conditions are active (Done.stop_on). Default all three (DEFAULT_STOP_ON). */
  stopOn?: readonly StopCondition[];
  /** Consecutive-beats window for the no-progress halt. Default DEFAULT_NO_PROGRESS_N. */
  noProgressN?: number;
  /** Runtime iteration-cap default when the contract sets none. Default DEFAULT_MAX_ITERATIONS. */
  maxIterationsDefault?: number;
}

/** The halt reasons the stop-condition engine can raise (a subset of the pinned `ExitReason` enum). */
export type HaltReason = Extract<ExitReason, "iteration_cap" | "no_progress" | "budget">;

/** The stop-condition verdict — `halt:true` ends the loop (exit 10 + `reason`); `halt:false` continues. */
export interface StopDecision {
  halt: boolean;
  reason?: HaltReason;
}

/** The full end-of-beat decision: keep going, HALT (park blocked), or RESTART fresh (respawn). */
export type BeatDecision =
  | { action: "continue" }
  | { action: "halt"; reason: HaltReason }
  | { action: "restart"; reason: "fresh_context" };

/** Map a `Done.stop_on` StopCondition to the exit-10 ExitReason the child declares ("cap"→"iteration_cap"). */
const CONDITION_REASON: Record<StopCondition, HaltReason> = {
  cap: "iteration_cap",
  no_progress: "no_progress",
  budget: "budget",
};

/** The effective iteration cap: contract `budget.max_iterations` (frontmatter) wins, else the runtime
 *  default threaded via state, else the engine's hard default (30). */
function effectiveMaxIterations(state: BeatState): number {
  return state.budget.max_iterations ?? state.maxIterationsDefault ?? DEFAULT_MAX_ITERATIONS;
}

/** True when the last `n` diffs all exist AND are all empty (no change for n consecutive beats). A
 *  window of `n <= 0` DISABLES the check (returns false) rather than tripping it — `slice(-0)` is the
 *  whole array and `[].every()` is vacuously true, so without this guard a zero window would raise a
 *  spurious no_progress halt on a fresh run (P20 correctness nit). */
function stalledOnDiffs(recentDiffs: readonly string[], n: number): boolean {
  if (n <= 0 || recentDiffs.length < n) return false;
  return recentDiffs.slice(-n).every((d) => d === "");
}

/** True when the last `n` errors all exist, are all non-empty, AND are all identical (agreeing with
 *  itself — the same failure n times). Empty entries (`""` = no error) never count; `n <= 0` disables. */
function stalledOnErrors(recentErrors: readonly string[], n: number): boolean {
  if (n <= 0 || recentErrors.length < n) return false;
  const window = recentErrors.slice(-n);
  const first = window[0];
  if (first === undefined || first === "") return false;
  return window.every((e) => e === first);
}

// ── Evaluators (pure) ────────────────────────────────────────────────────────

/**
 * The three independent HALT conditions (AUTONOMY §4, F3 §5). Evaluated in canon order
 * (DEFAULT_STOP_ON: cap → no_progress → budget) so a beat that trips more than one halts on the FIRST
 * in that order — deterministic. Only conditions listed in `stopOn` (default all three) are checked;
 * a contract can narrow them, and the in-path budget guard (BRO-1788) remains the hard backstop that
 * cannot be disabled, so narrowing here never yields an unbounded loop.
 */
export function evaluateStopConditions(state: BeatState): StopDecision {
  const active = new Set<StopCondition>(state.stopOn ?? DEFAULT_STOP_ON);
  const n = state.noProgressN ?? DEFAULT_NO_PROGRESS_N;

  if (active.has("cap") && state.iterations >= effectiveMaxIterations(state)) {
    return { halt: true, reason: CONDITION_REASON.cap };
  }
  if (
    active.has("no_progress") &&
    (stalledOnDiffs(state.recentDiffs, n) || stalledOnErrors(state.recentErrors, n))
  ) {
    return { halt: true, reason: CONDITION_REASON.no_progress };
  }
  if (active.has("budget") && budgetExhausted(state)) {
    return { halt: true, reason: CONDITION_REASON.budget };
  }
  return { halt: false };
}

/** True when a dollar cap has been reached — run spend >= per_run OR day spend >= per_day (a cap that
 *  is `undefined` never fires). The reserve-then-reconcile guard (BRO-1788) refuses the NEXT call 402;
 *  this end-of-beat check catches an already-at/over-cap run so the child exits cleanly with reason
 *  `budget` even if it never issued another call. */
function budgetExhausted(state: BeatState): boolean {
  const { per_run_usd, per_day_usd } = state.budget;
  if (per_run_usd !== undefined && state.spentUsd >= per_run_usd) return true;
  if (per_day_usd !== undefined && state.dayUsd >= per_day_usd) return true;
  return false;
}

/** True when the child has reached the context ceiling and should restart fresh (HARNESS §5). A ceiling
 *  of `<= 0` disables the check (never restart). */
export function needsFreshContext(state: BeatState): boolean {
  return state.ceiling > 0 && state.contextTokens >= state.ceiling;
}

/**
 * The full end-of-beat decision. HALT is checked BEFORE restart: if the run is out of iterations /
 * budget / progress, a fresh context cannot help (iterations + budget span attempts, and no_progress is
 * a "a human should look" signal), so the halt wins. Only when nothing halts does the context ceiling
 * trigger a (lossless, memory-on-disk) restart.
 */
export function evaluateBeat(state: BeatState): BeatDecision {
  const stop = evaluateStopConditions(state);
  if (stop.halt && stop.reason !== undefined) return { action: "halt", reason: stop.reason };
  if (needsFreshContext(state)) return { action: "restart", reason: "fresh_context" };
  return { action: "continue" };
}

// ── The child's terminal event sequence for a beat decision (HARNESS §4/§5, F3 §5) ──

/** Context a decision's events carry (kept narrow so the producer is decoupled from BeatState). */
export interface ExitEventContext {
  /** The beat index the decision fired on — the receipt's "why it stopped, and when". */
  iteration: number;
}

/**
 * The `ChildEmittedEvent[]` the child writes to stdout for a beat decision — the child's LAST events
 * before it exits (the supervisor's tee stamps `seq`/`ts` and reaps the exit code). `continue` emits
 * nothing.
 *
 * • halt `budget`        → `budget.exhausted` (the child's loop-halt marker — the proxy owns `refused`/
 *                           `metered`, the child owns `exhausted`, per events.ts) THEN `run.exiting`.
 * • halt cap/no_progress → just `run.exiting {code:10, reason}` (the reason IS the record; F3 §5).
 * • restart              → `run.restart_requested` THEN `run.exiting {code:10, reason:fresh_context}`.
 *
 * Every `run.exiting` carries `{code:10, reason}` — the terminal child event the supervisor cross-checks
 * against the real exit code (HARNESS §4; a disagreement is `run.exit_mismatch`).
 */
export function beatExitEvents(decision: BeatDecision, ctx: ExitEventContext): ChildEmittedEvent[] {
  if (decision.action === "continue") return [];

  if (decision.action === "restart") {
    return [
      {
        actor: "system",
        type: EVENT_TYPES.RUN_RESTART_REQUESTED,
        payload: { iteration: ctx.iteration, reason: "context_ceiling" },
      },
      {
        actor: "system",
        type: EVENT_TYPES.RUN_EXITING,
        payload: { code: 10, reason: "fresh_context" },
      },
    ];
  }

  const events: ChildEmittedEvent[] = [];
  if (decision.reason === "budget") {
    events.push({
      actor: "system",
      type: EVENT_TYPES.BUDGET_EXHAUSTED,
      payload: { iteration: ctx.iteration },
    });
  }
  events.push({
    actor: "system",
    type: EVENT_TYPES.RUN_EXITING,
    payload: { code: 10, reason: decision.reason },
  });
  return events;
}

// ── Disk memory — progress.md (rewritten every beat) + fix_plan.md (a ticked task list) ──
// The run dir is `runs/run-<id>/` (DATA-MODEL §A.1) — progress.md is disk memory rewritten every
// iteration; fix_plan.md is the task list the loop reads + ticks. On a fresh-context respawn the new
// child reads both and skips done work (HARNESS §5). No ambient clock: `updated` is passed in, like the
// contract snapshot's `dispatchedAt`.

/** The canonical run-dir path of the disk-memory files. */
export function progressPath(runDir: string): string {
  return join(runDir, "progress.md");
}
export function fixPlanPath(runDir: string): string {
  return join(runDir, "fix_plan.md");
}

/** The child's disk memory (progress.md) — rewritten in full every beat. `whatsLeft` is what a respawn
 *  reads to know what remains; `stateOfTheWorld` is prose for the human + the next context. */
export interface ProgressDoc {
  session: string;
  iteration: number;
  /** ISO-8601, passed in (no ambient clock). */
  updated: string;
  stateOfTheWorld: string;
  whatsLeft: string[];
}

const PROGRESS_META_OPEN = "<!-- maestro:progress";
const PROGRESS_META_CLOSE = "-->";

const SW_HEADING = /^##[ \t]+State of the world[ \t]*$/m;
const WL_HEADING = /^##[ \t]+What's left[ \t]*$/gm;

/** Render a ProgressDoc to markdown — agent- AND human-readable (P18): a machine block in an HTML
 *  comment + prose sections. Full-rewrite content (never appended). An empty `whatsLeft` renders an
 *  empty section (no bullets) rather than a `- (none)` sentinel — so a real item whose text is
 *  literally "(none)" round-trips instead of colliding with the placeholder (P20 io-robustness nit). */
export function renderProgress(doc: ProgressDoc): string {
  const lines = [
    `# Progress — ${doc.session}`,
    "",
    PROGRESS_META_OPEN,
    `session: ${doc.session}`,
    `iteration: ${doc.iteration}`,
    `updated: ${doc.updated}`,
    PROGRESS_META_CLOSE,
    "",
    "## State of the world",
    "",
    doc.stateOfTheWorld,
    "",
    "## What's left",
    "",
  ];
  for (const item of doc.whatsLeft) lines.push(`- ${item}`);
  lines.push("");
  return lines.join("\n");
}

/** Parse progress.md back to a ProgressDoc, or `null` if the machine block is absent/malformed (the
 *  respawn then treats it as "no checkpoint" and starts from the contract). Tolerant: a missing section
 *  yields an empty value rather than throwing, so a hand-edited file never wedges the child. CRLF is
 *  normalized first so a Windows-saved hand edit parses identically to LF (P20 io-robustness). */
export function parseProgress(md: string): ProgressDoc | null {
  const text = md.replace(/\r\n/g, "\n");
  const open = text.indexOf(PROGRESS_META_OPEN);
  if (open === -1) return null;
  const close = text.indexOf(PROGRESS_META_CLOSE, open);
  if (close === -1) return null;
  const meta = text.slice(open + PROGRESS_META_OPEN.length, close);
  const field = (name: string): string | undefined => {
    const m = meta.match(new RegExp(`^\\s*${name}:\\s*(.*)$`, "m"));
    return m?.[1]?.trim();
  };
  const session = field("session");
  const iterationRaw = field("iteration");
  const updated = field("updated");
  // All three machine fields are REQUIRED and non-empty — a blank `iteration:` must NOT slip through
  // as `Number("") === 0` (a bogus iteration-0 checkpoint); a blank `updated:` is equally malformed.
  if (
    session === undefined ||
    session === "" ||
    updated === undefined ||
    updated === "" ||
    iterationRaw === undefined ||
    iterationRaw === ""
  ) {
    return null;
  }
  const iteration = Number(iterationRaw);
  if (!Number.isInteger(iteration)) return null;
  const body = text.slice(close + PROGRESS_META_CLOSE.length);
  const { stateOfTheWorld, whatsLeft } = splitProgressBody(body);
  return { session, iteration, updated, stateOfTheWorld, whatsLeft };
}

/**
 * Split the post-meta body into the two known sections. `stateOfTheWorld` runs from its heading to the
 * LAST `## What's left` heading (render always emits What's left as the FINAL section, so binding to
 * the last occurrence keeps an embedded `## …` line inside the prose from truncating the field — the
 * P20 silent-data-loss finding). `whatsLeft` is the bullets after that heading.
 */
function splitProgressBody(body: string): { stateOfTheWorld: string; whatsLeft: string[] } {
  const sw = SW_HEADING.exec(body);
  WL_HEADING.lastIndex = 0;
  let wlHeadingStart = -1;
  let wlBodyStart = -1;
  for (let m = WL_HEADING.exec(body); m !== null; m = WL_HEADING.exec(body)) {
    wlHeadingStart = m.index;
    wlBodyStart = m.index + m[0].length;
  }
  const stateStart = sw ? sw.index + sw[0].length : 0;
  const stateEnd = wlHeadingStart === -1 ? body.length : wlHeadingStart;
  const stateOfTheWorld = sw ? body.slice(stateStart, stateEnd).trim() : "";
  const whatsLeft = wlBodyStart === -1 ? [] : extractBullets(body.slice(wlBodyStart));
  return { stateOfTheWorld, whatsLeft };
}

/** Bullet items (`- item`) inside a section body, skipping only blank bullets (no magic sentinel). */
function extractBullets(section: string): string[] {
  const out: string[] = [];
  for (const line of section.split("\n")) {
    const m = line.match(/^\s*-\s+(.*)$/);
    if (!m) continue;
    const text = (m[1] ?? "").trim();
    if (text === "") continue;
    out.push(text);
  }
  return out;
}

/** Rewrite progress.md in full (disk memory, every beat — DATA-MODEL §A.1). Creates the run dir if needed. */
export async function writeProgress(runDir: string, doc: ProgressDoc): Promise<void> {
  await mkdir(runDir, { recursive: true });
  await writeFile(progressPath(runDir), renderProgress(doc), "utf8");
}

/** Read progress.md back (a respawn's checkpoint). `null` if the file is absent or unparseable. */
export async function readProgress(runDir: string): Promise<ProgressDoc | null> {
  let raw: string;
  try {
    raw = await readFile(progressPath(runDir), "utf8");
  } catch {
    return null; // no checkpoint yet — the respawn starts from the contract
  }
  return parseProgress(raw);
}

/** One fix_plan.md task-list item — a GitHub-style checkbox line. `done` = a respawn SKIPS it. */
export interface FixPlanItem {
  text: string;
  done: boolean;
}

const FIX_PLAN_ITEM = /^(\s*)-\s+\[( |x|X)\]\s+(.*)$/;

/** Parse ALL checkbox items from fix_plan.md, across every `## attempt` section (order preserved). Non-
 *  checkbox lines — headings, prose, evidence links — are ignored, so an append-only history parses fine. */
export function parseFixPlan(md: string): FixPlanItem[] {
  const out: FixPlanItem[] = [];
  for (const line of md.replace(/\r\n/g, "\n").split("\n")) {
    const m = line.match(FIX_PLAN_ITEM);
    if (!m) continue;
    out.push({ text: (m[3] ?? "").trim(), done: (m[2] ?? " ").toLowerCase() === "x" });
  }
  return out;
}

/** The items a respawn must still do — the "skip done work" filter (HARNESS §5). */
export function pendingItems(items: readonly FixPlanItem[]): FixPlanItem[] {
  return items.filter((i) => !i.done);
}

/** Render a flat fix_plan.md checklist — used to SEED the file (F2 creates runs/run-<id>/ with an empty
 *  fix_plan.md; the child writes its plan). In-run ticking uses `tickFixPlan`, which preserves any
 *  appended verifier history rather than rewriting it. */
export function renderFixPlan(items: readonly FixPlanItem[]): string {
  const lines = items.map((i) => `- [${i.done ? "x" : " "}] ${i.text}`);
  return `# Fix plan\n\n${lines.join("\n")}\n`;
}

/** Seed / overwrite fix_plan.md with a flat checklist. Creates the run dir if needed. */
export async function writeFixPlan(runDir: string, items: readonly FixPlanItem[]): Promise<void> {
  await mkdir(runDir, { recursive: true });
  await writeFile(fixPlanPath(runDir), renderFixPlan(items), "utf8");
}

/** Read fix_plan.md back to its items. `[]` if the file is absent. */
export async function readFixPlan(runDir: string): Promise<FixPlanItem[]> {
  let raw: string;
  try {
    raw = await readFile(fixPlanPath(runDir), "utf8");
  } catch {
    return [];
  }
  return parseFixPlan(raw);
}

/**
 * TICK matching items done, IN PLACE (`- [ ]` → `- [x]`) — the F3 §5 / HARNESS §5 "tick fix_plan.md".
 * An in-place line rewrite (not a full re-render) so append-only verifier history — the `## attempt N`
 * sections VERIFIER §5 relies on for Loop 4 — is preserved; only the matched checkboxes flip. Line
 * endings are NORMALIZED to LF on rewrite (CRLF is split on read — `.` never matches `\r`, so without
 * this a CRLF-saved plan would tick nothing; the writers all emit LF, so LF-normalizing is consistent).
 * Matching is by trimmed item text (exact). Returns how many items it ticked; a no-op (no file / no
 * match) returns 0. Idempotent: an already-ticked item is left as-is.
 */
export async function tickFixPlan(runDir: string, doneTexts: Iterable<string>): Promise<number> {
  const wanted = new Set<string>();
  for (const t of doneTexts) wanted.add(t.trim());
  if (wanted.size === 0) return 0;
  let raw: string;
  try {
    raw = await readFile(fixPlanPath(runDir), "utf8");
  } catch {
    return 0; // nothing to tick
  }
  let ticked = 0;
  const lines = raw.split(/\r?\n/).map((line) => {
    const m = line.match(FIX_PLAN_ITEM);
    if (!m) return line;
    const already = (m[2] ?? " ").toLowerCase() === "x";
    const text = (m[3] ?? "").trim();
    if (already || !wanted.has(text)) return line;
    ticked++;
    return `${m[1] ?? ""}- [x] ${m[3] ?? ""}`;
  });
  if (ticked > 0) await writeFile(fixPlanPath(runDir), lines.join("\n"), "utf8");
  return ticked;
}

/**
 * The full fresh-context restart preparation the child runs when `evaluateBeat` returns `restart`
 * (HARNESS §5): rewrite progress.md (state + what's left — so nothing is lost), tick any items the beat
 * finished (so the respawn skips them), and return the `run.restart_requested` + `run.exiting` events
 * for the child's stdio to emit. Memory-on-disk BEFORE the signal is what makes the respawn lossless.
 */
export async function prepareRestart(
  runDir: string,
  args: { progress: ProgressDoc; doneTexts?: Iterable<string> },
): Promise<ChildEmittedEvent[]> {
  await writeProgress(runDir, args.progress);
  if (args.doneTexts) await tickFixPlan(runDir, args.doneTexts);
  return beatExitEvents(
    { action: "restart", reason: "fresh_context" },
    {
      iteration: args.progress.iteration,
    },
  );
}
