// Model pinning (HARNESS §3, AUTONOMY §5 "version drift"). The child asks the proxy for its ROLE, not
// a model id; the proxy resolves the pinned model here. Pinning lives supervisor-side so a model
// version bump is one config change, not a child-prompt edit — and so the D8 canary (BRO-1806) can
// later route a fraction of runs to a candidate model without the child knowing.

import type { ChildRole } from "../harness/spawn-contract";

/** The pinned model id for each child role. Opus for the writer/judge (capability), Haiku for the
 *  orchestrator (cheap coordination). The verifier judge is same-vendor by decision, always a
 *  separate session. */
export const DEFAULT_MODEL_PINS: Record<ChildRole, string> = {
  agent: "claude-opus-4-8", // the writer — does the work
  verifier: "claude-opus-4-8", // the judge — scores the work (separate session, temp 0)
  orchestrator: "claude-haiku-4-5-20251001", // coordination — cheap, frequent
};

/** Per-run env override key for a role's pinned model, e.g. MAESTRO_MODEL_AGENT. */
export function modelEnvKey(role: ChildRole): string {
  return `MAESTRO_MODEL_${role.toUpperCase()}`;
}

/**
 * Resolve the pinned model id for a role, honoring an env override (MAESTRO_MODEL_<ROLE>) over the
 * default pin. A blank/whitespace override is ignored (falls through to the default) so an empty env
 * var can never resolve to an empty model id.
 */
export function resolvePinnedModel(
  role: ChildRole,
  env: Record<string, string | undefined> = process.env,
): string {
  const override = env[modelEnvKey(role)]?.trim();
  return override && override.length > 0 ? override : DEFAULT_MODEL_PINS[role];
}
