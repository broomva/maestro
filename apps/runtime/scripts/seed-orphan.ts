/// <reference types="bun" />
// seed-orphan (BRO-1827) — write the post-crash index state a runtime leaves behind when it is
// SIGKILLed mid-run: a still-`running` session plus the `agent.said` events it had already emitted
// before dying. Booting a runtime over this index makes crash-recovery's parkOrphans (F9.3,
// `db/recovery.ts`) park the session `blocked` and append `run.orphaned` — the "orphan parked at
// Stuck" the P2 exit asserts. The seeded events are the "nothing lost" artifact: parkOrphans only
// ADDS `run.orphaned`, so they must survive the restart.
//
// A crash cannot be timed against the sub-second CLI-booted mock model (it does one beat and exits 0),
// so the deterministic way to reproduce a still-`running` session is to construct it directly — exactly
// what `db/recovery.test.ts` does as a unit; this is its E2E counterpart, seeding a real index file the
// runtime then boots + recovers over.
//
// Runs as a bun subprocess because Playwright's loader can't transpile the runtime's bun-only `.sql`
// text imports that `openIndex` transitively pulls in (same reason `dump-index.ts` is a subprocess).
//
//   bun run apps/runtime/scripts/seed-orphan.ts <indexPath> <sessionId> <nodeId>
//   → inserts one running session + two agent.said events; prints the sessionId on success.

import { indexUrl, openIndex } from "../src/db/client";
import { event, session } from "../src/db/schema";

const indexPath = Bun.argv[2];
const sessionId = Bun.argv[3];
const nodeId = Bun.argv[4];
if (!indexPath || !sessionId || !nodeId) {
  console.error("usage: bun run seed-orphan.ts <indexPath> <sessionId> <nodeId>");
  process.exit(2);
}

const handle = await openIndex(indexUrl(indexPath));
try {
  // A run that was live when the crash hit: status `running`, no terminal event (row shape mirrors
  // recovery.test.ts's seedSession — the fixture parkOrphans is proven against).
  await handle.db.insert(session).values({
    id: sessionId,
    nodeId,
    branch: `run/${sessionId}`,
    status: "running" as never,
    startedAt: 1,
    updatedAt: 1,
  });
  // The work it had already streamed before the crash. These must still be served after recovery —
  // "nothing lost". Two coalesced assistant turns (agent.said), the wire shape chat.ts projects to text.
  await handle.db.insert(event).values([
    {
      sessionId,
      ts: 1_700_000_000_000,
      actor: "agent" as never,
      type: "agent.said" as never,
      payload: JSON.stringify({ text: "Picking up the mission." }),
    },
    {
      sessionId,
      ts: 1_700_000_000_001,
      actor: "agent" as never,
      type: "agent.said" as never,
      payload: JSON.stringify({ text: "Ran one step, still working." }),
    },
  ]);
  process.stdout.write(sessionId);
} finally {
  handle.client.close();
}
