// @maestro/runtime — the 24/7 supervisor (Bun + Hono, bun build --compile).
// Owns FS + git + sh; schedules agent sessions; libSQL derived index.
// The Hono app, /health route, and single-binary compile land in
// BRO-1790 (p0-runtime-skeleton).

// The wire contract the runtime speaks is imported from @maestro/protocol, never
// redeclared here — the no-codegen-drift guarantee (PATTERNS §10, BRO-1785).
import { type EventEnvelope, type Intent, PROTOCOL_PACKAGE } from "@maestro/protocol";

export const RUNTIME_APP = "@maestro/runtime" as const;

/** The shared wire contract the runtime imports (PATTERNS §10). */
export const RUNTIME_PROTOCOL = PROTOCOL_PACKAGE;

/** Handler signatures the BRO-1790 skeleton will implement — typed by the protocol. */
export type RuntimeIntentHandler = (intent: Intent) => Promise<void>;
export type RuntimeEventSink = (event: EventEnvelope) => void;
