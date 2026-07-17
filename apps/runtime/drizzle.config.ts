// drizzle-kit config — generates the control-plane index migrations (BRO-1796).
//
// `generate` runs offline (no DB connection): it diffs src/db/schema.ts against
// the last snapshot and emits SQLite DDL into src/db/migrations/. The runtime
// applies that folder via the compiled-safe embedded migrator (src/db/embedded-migrations.ts).
//
// dialect: "sqlite" — the index driver is `bun:sqlite` (drizzle-orm/bun-sqlite,
// BRO-1841), so the emitted SQLite DDL runs verbatim on the embedded file. The
// migration journal format is dialect-agnostic (fs-index §7: "types are the
// contract, the ORM binding is an implementation detail").
//
// Regenerate after any schema.ts change:  bun run --filter @maestro/runtime db:generate

import { defineConfig } from "drizzle-kit";

export default defineConfig({
  dialect: "sqlite",
  schema: "./src/db/schema.ts",
  out: "./src/db/migrations",
});
