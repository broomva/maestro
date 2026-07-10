/// <reference types="bun" />
// dump-index (BRO-1823) — print an index db's canonical node dump (every node column except the
// volatile `updatedAt`, id-sorted) as JSON to stdout. Used by the p1-exit E2E to read `--rebuild`'s
// OUTPUT directly: Playwright's test loader can't transpile the runtime's bun-only `.sql` text
// imports that `openIndex` transitively pulls in, so the dump has to run in a bun subprocess (the
// same reason the E2E spawns the runtime as a subprocess rather than importing it).
//
//   bun run apps/runtime/scripts/dump-index.ts <indexPath>   →  JSON CanonicalNode[] on stdout

import { indexUrl, openIndex } from "../src/db/client";
import { dumpIndex } from "../src/db/rebuild";

const indexPath = Bun.argv[2];
if (!indexPath) {
  console.error("usage: bun run dump-index.ts <indexPath>");
  process.exit(2);
}

const handle = await openIndex(indexUrl(indexPath));
try {
  process.stdout.write(JSON.stringify(await dumpIndex(handle.db)));
} finally {
  handle.client.close();
}
