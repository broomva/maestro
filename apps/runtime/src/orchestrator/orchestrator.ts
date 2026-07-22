// computeOrchestratorTick — the deterministic "mock-model tick" (ORCHESTRATOR §2-3): assemble the briefing
// (§2), run the decision policy (§3), return the decision + wake log. This is what runs when the
// orchestrator has no live model (MOCK mode) — it reaches the SAME decisions the real orchestrator agent
// would reason to from the same briefing, because the policy IS the checklist the prompt states in prose.
//
// It does NOT emit — no dispatch intents, no nudge chat, no durable wake-log event. That is the tick's
// EMIT half (slice 2b / F6.3-4), deliberately split off: (1) it keeps this pure + replay-testable over a
// seeded index, and (2) live intent-issuing needs the confined orchestrator harness (BRO-1944) before an
// agent identity may drive the write path. This slice ships the brain; the hands come next.
//
// BRO-1945 (s2b-ii): it now READS one more signal before deciding — which active runs were already
// nudged in their CURRENT stale window (derived from the index per the §3.1 contract, see nudge.ts).
// That makes the policy's `afterNudge` escalation reachable in production; it stays a read, so this
// function is still side-effect-free.

import type { TickCause } from "@maestro/protocol";
import type { IndexDb } from "../db/client";
import { assembleBriefing, type BriefingOptions } from "../tick/briefing";
import { deriveNudgedSessionIds } from "../tick/nudge";
import { decidePolicy, type OrchestratorDecision, type OrchestratorOptions } from "./policy";

export interface OrchestratorTickOptions extends OrchestratorOptions {
  /** passed through to assembleBriefing (e.g. the day-budget denominator for the §3.1 halt). */
  briefing?: BriefingOptions;
}

/**
 * Compute one orchestrator tick's decisions over the live index at `now`, deterministically. Assembles the
 * §2 briefing then runs the §3 policy; returns the decision + §7 wake log without side effects.
 *
 * `nudgedSessionIds` is DERIVED from the index (BRO-1945) unless the caller supplies it — the derivation
 * is scoped to each run's current stale window (nudge.ts contract (b)), so a run the nudge revived is not
 * escalated on its next quiet spell. An explicit `opts.nudgedSessionIds` still wins (tests inject it).
 */
export async function computeOrchestratorTick(
  db: IndexDb,
  cause: TickCause,
  now: number,
  opts: OrchestratorTickOptions = {},
): Promise<OrchestratorDecision> {
  const briefing = await assembleBriefing(db, cause, now, opts.briefing);
  const nudgedSessionIds =
    opts.nudgedSessionIds ??
    (await deriveNudgedSessionIds(
      db,
      briefing.activeRuns.map((r) => r.sessionId),
    ));
  return decidePolicy(briefing, { ...opts, nudgedSessionIds });
}
