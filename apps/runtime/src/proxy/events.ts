// Budget event journaling (HARNESS §3, D-DURABILITY). The proxy emits `budget.refused` and
// `budget.metered`; these are DURABLE — the canonical write is the FS session journal (the `event`
// table is a pure projection of it, D-DURABILITY). The sink is an interface so the guard is
// oblivious to whether it is journaling to disk or being taped in a test.
//
// On-disk shape is the flattened `session.jsonl` line (DATA-MODEL §A.3): `ts`/`actor`/`type` plus the
// payload fields at top level — NOT a `{payload:{…}}` wrapper. The index projection (the
// session.jsonl→event parser, BRO-1767) re-nests them at the wire boundary.

import { appendFile, mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { Actor, EventType } from "@maestro/protocol";

/** A budget event to journal — the semantic content; the sink stamps the on-disk representation. */
export interface EmittedEvent {
  /** ISO-8601 — the runtime supplies the clock (this module has no ambient time). */
  ts: string;
  actor: Actor;
  type: EventType;
  payload: Record<string, unknown>;
}

/** Where budget events go. Injected into the guard so journaling is swappable + testable. */
export interface BudgetEventSink {
  emit(runDir: string, ev: EmittedEvent): Promise<void>;
}

/** The run's durable event journal path. */
export function sessionJournalPath(runDir: string): string {
  return join(runDir, "session.jsonl");
}

/**
 * The durable sink: append a flattened `session.jsonl` line to the run dir. The envelope fields win
 * over payload keys (spread last) so a stray payload `type` can never shadow the real event type.
 */
export function fsJournalSink(): BudgetEventSink {
  return {
    async emit(runDir: string, ev: EmittedEvent): Promise<void> {
      const path = sessionJournalPath(runDir);
      await mkdir(dirname(path), { recursive: true });
      const line = JSON.stringify({ ...ev.payload, ts: ev.ts, actor: ev.actor, type: ev.type });
      await appendFile(path, `${line}\n`, "utf8");
    },
  };
}

/** An in-memory sink — tests, and a fast observability tap over the live budget stream. */
export class MemoryEventSink implements BudgetEventSink {
  readonly events: Array<EmittedEvent & { runDir: string }> = [];
  async emit(runDir: string, ev: EmittedEvent): Promise<void> {
    this.events.push({ runDir, ...ev });
  }
  /** Every emitted event of a given type (test convenience). */
  ofType(type: EventType): Array<EmittedEvent & { runDir: string }> {
    return this.events.filter((e) => e.type === type);
  }
}
