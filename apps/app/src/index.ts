// @maestro/app — one Vite + React + TypeScript SPA delivered as web, PWA, and
// Tauri shell. Chat is a projection; the shell never scrolls, inner panels do.
// The real Vite scaffold + token wiring + theme switching land in
// BRO-1782 (p0-app-scaffold-m0).

// The client renders plain voice from the shared contract — imported, never
// redeclared (the no-codegen-drift guarantee, PATTERNS §10, BRO-1785).
import { type OrchState, PROTOCOL_PACKAGE, plainVoice } from "@maestro/protocol";

export const APP = "@maestro/app" as const;

/** The shared wire contract the client imports (PATTERNS §10). */
export const APP_PROTOCOL = PROTOCOL_PACKAGE;

/** The UI shows plain voice, never the system enum (CLAUDE.md §Work states). */
export const renderState = (state: OrchState): string => plainVoice(state);
