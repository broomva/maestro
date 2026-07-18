// computeOrchestratorTick — the deterministic "mock-model tick" (ORCHESTRATOR §2-3): assemble the briefing
// (§2), run the decision policy (§3), return the decision + wake log. This is what runs when the
// orchestrator has no live model (MOCK mode) — it reaches the SAME decisions the real orchestrator agent
// would reason to from the same briefing, because the policy IS the checklist the prompt states in prose.
//
// It does NOT emit — no dispatch intents, no nudge chat, no durable wake-log event. That is the tick's
// EMIT half (slice 2b / F6.3-4), deliberately split off: (1) it keeps this pure + replay-testable over a
// seeded index, and (2) live intent-issuing needs the confined orchestrator harness (BRO-1944) before an
// agent identity may drive the write path. This slice ships the brain; the hands come next.

import type { TickCause } from "@maestro/protocol";
import type { IndexDb } from "../db/client";
import { assembleBriefing, type BriefingOptions } from "../tick/briefing";
import { decidePolicy, type OrchestratorDecision, type OrchestratorOptions } from "./policy";

export interface OrchestratorTickOptions extends OrchestratorOptions {
  /** passed through to assembleBriefing (e.g. the day-budget denominator for the §3.1 halt). */
  briefing?: BriefingOptions;
}

/**
 * Compute one orchestrator tick's decisions over the live index at `now`, deterministically. Assembles the
 * §2 briefing then runs the §3 policy; returns the decision + §7 wake log without side effects.
 */
export async function computeOrchestratorTick(
  db: IndexDb,
  cause: TickCause,
  now: number,
  opts: OrchestratorTickOptions = {},
): Promise<OrchestratorDecision> {
  const briefing = await assembleBriefing(db, cause, now, opts.briefing);
  return decidePolicy(briefing, opts);
}
