// @maestro/protocol — the single language: events, intents, work items.
// Imported by BOTH the runtime and the client so the wire contract is the same
// code on both sides, not a codegen seam that drifts.
// Real content (8-state OrchState machine, Intent union, event envelope,
// work-contract types) lands in BRO-1785 (p0-protocol-package).
export const PROTOCOL_PACKAGE = "@maestro/protocol" as const;

/** Protocol header constant (D-NAME). */
export const X_MAESTRO_PROTOCOL = "x-maestro-protocol" as const;
