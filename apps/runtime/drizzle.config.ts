// drizzle-kit config — generates the control-plane index migrations (BRO-1796).
//
// `generate` runs offline (no DB connection): it diffs src/db/schema.ts against
// the last snapshot and emits SQLite DDL into src/db/migrations/. The runtime
// applies that folder with drizzle-orm/libsql's migrator (src/db/client.ts).
//
// dialect: "sqlite" — libSQL is a SQLite fork, so the emitted DDL runs verbatim
// on the embedded libSQL file. The migration journal format is dialect-agnostic,
// so the libsql migrator consumes a sqlite-generated folder unchanged (fs-index
// §7: "types are the contract, the ORM binding is an implementation detail").
//
// Regenerate after any schema.ts change:  bun run --filter @maestro/runtime db:generate

import { defineConfig } from "drizzle-kit";

export default defineConfig({
  dialect: "sqlite",
  schema: "./src/db/schema.ts",
  out: "./src/db/migrations",
});
