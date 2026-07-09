// @maestro/runtime — the 24/7 supervisor (Bun + Hono, bun build --compile).
// Owns FS + git + sh; schedules agent sessions; libSQL derived index.
// The Hono app, /health route, and single-binary compile land in
// BRO-1790 (p0-runtime-skeleton).
export const RUNTIME_APP = "@maestro/runtime" as const;
