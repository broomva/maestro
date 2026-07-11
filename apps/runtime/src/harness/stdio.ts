/// <reference types="bun" />
// The supervisor's stdio plumbing (HARNESS §2 / FLOWS §F2 step 6) — the one seam where a child's
// NDJSON stdout becomes durable session state, and the supervisor's control lines reach the child's
// stdin. The shape of the deal (HARNESS §2):
//
//   * stdout = events. Each line is a `session.jsonl` event (DATA-MODEL §A.3). The supervisor TEES
//     it: (1) append to `session.jsonl` FIRST — the filesystem is truth (D-DURABILITY) — then
//     (2) project it into the index `event` table. The child NEVER writes `session.jsonl`; one
//     writer, no interleaving.
//   * stdin = control (NDJSON): `chat` routes a user message into the live loop, `stop` asks for a
//     graceful finish, `ping` probes liveness.
//   * stderr = raw crash forensics, appended to `child.stderr.log`, never parsed.
//   * liveness: a child silent past `hungMs` is SIGTERM'd, then SIGKILL'd after `graceMs`, and its
//     session parks `blocked` with a `run.hung` event.
//
// The SSE fan-out (HARNESS §2 step 3) is NOT a second push path here: `api/stream.ts` tails the
// `event` table by `seq > cursor`, so inserting the row IS the fan-out. That tailer is gapless only
// because commit order == `seq` order == the order lines arrived — which is exactly what the tee's
// single-writer serialization guarantees (see `SessionTee`).
//
// On-disk shape pin (DATA-MODEL §A.3): a `session.jsonl` line is FLATTENED — `{...payload, ts, actor,
// type}` at top level, ISO-8601 `ts`, NO `seq`/`sessionId`/`payload:{}` wrapper. This MUST stay
// byte-compatible with the co-writer on the same file: `fsJournalSink` (proxy/events.ts) writes
// budget events to this same `session.jsonl` with exactly `JSON.stringify({...payload, ts, actor,
// type})`. The index projection re-nests payload at the wire boundary (event-projection.ts
// `toEnvelope`); the on-disk line stays flat.

import { appendFile, mkdir, readdir, readFile, rename } from "node:fs/promises";
import { dirname, join } from "node:path";
import {
  ACTORS,
  type Actor,
  EVENT_TYPES,
  type EventType,
  isWireEventType,
  type SessionStatus,
} from "@maestro/protocol";
import { eq } from "drizzle-orm";
import {
  DEFAULT_CHILD_GRACE_MS,
  DEFAULT_CHILD_HEARTBEAT_MS,
  DEFAULT_CHILD_HUNG_MS,
  DEFAULT_ROTATE_MAX_BYTES,
  DEFAULT_ROTATE_MAX_LINES,
  type RuntimeConfig,
} from "../config";
import type { IndexDb } from "../db/client";
import { event, session } from "../db/schema";
import type { ChildEmittedEvent } from "./runner";

/** An over-liveness run parks `blocked` (F3.1) — the one place that mapping lives for HARNESS §2. */
const PARK_STATE = "blocked" satisfies SessionStatus;

// ── 1. Line splitter ─────────────────────────────────────────────────────────
// Bun's `proc.stdout` is a `ReadableStream<Uint8Array>` that hands us arbitrary byte chunks — a
// single read may carry half a line, or several lines plus a partial. This buffers across chunks and
// yields only COMPLETE lines, retaining the trailing partial for the next push. A pathological child
// that streams megabytes with no newline must not grow the buffer without bound — past `maxLineBytes`
// the buffer is dropped and counted (an `overflow`), so the pump can never OOM the supervisor.

/** Guard against an unbounded partial line (a child streaming bytes with no `\n`). 16 MiB is far
 *  above any real event — a coalesced `agent.said` turn is kilobytes — so a trip means abuse/bug. */
const DEFAULT_MAX_LINE_BYTES = 16 * 1024 * 1024;

export interface NdjsonSplitter {
  /** Feed a decoded chunk; return every complete line it completes (newline-delimited, `\n` stripped). */
  push(chunk: string): string[];
  /** Any trailing partial with no final newline (a child that exits mid-line), or null. */
  flush(): string | null;
  /** How many times the buffer overflowed `maxLineBytes` and was dropped. */
  overflows(): number;
}

export function createNdjsonSplitter(maxLineBytes = DEFAULT_MAX_LINE_BYTES): NdjsonSplitter {
  let buf = "";
  let overflowCount = 0;
  return {
    push(chunk: string): string[] {
      buf += chunk;
      const lines: string[] = [];
      let idx = buf.indexOf("\n");
      while (idx !== -1) {
        // strip an optional trailing `\r` so a CRLF child never yields a corrupt fragment
        const line = buf.slice(0, idx);
        lines.push(line.endsWith("\r") ? line.slice(0, -1) : line);
        buf = buf.slice(idx + 1);
        idx = buf.indexOf("\n");
      }
      // No newline in a buffer this large means an abusive/broken line — drop it, keep the pump alive.
      if (buf.length > maxLineBytes) {
        buf = "";
        overflowCount++;
      }
      return lines;
    },
    flush(): string | null {
      const rest = buf;
      buf = "";
      return rest.length > 0 ? rest : null;
    },
    overflows: () => overflowCount,
  };
}

// ── 2. Line classification ─────────────────────────────────────────────────────
// Three kinds of stdout line: a SESSION EVENT (`{actor, type, payload?}`, a wire type) to persist; a
// LIVENESS signal (`pong`/`heartbeat`) that resets liveness but is NOT persisted; and everything else
// (malformed JSON, a bogus/unknown type) which is dropped and counted. A non-wire `type` is a drop,
// never a persist — the `event.type` column is `$type<EventType>()`, so a child cannot poison it.

export type ClassifiedLine =
  | { kind: "event"; event: ChildEmittedEvent }
  | { kind: "liveness"; signal: "pong" | "heartbeat" }
  | { kind: "drop"; reason: string };

const LIVENESS_TYPES = new Set(["pong", "heartbeat"]);
const ACTOR_SET = new Set<string>(ACTORS);

export function classifyLine(line: string): ClassifiedLine {
  const trimmed = line.trim();
  if (trimmed.length === 0) return { kind: "drop", reason: "empty" };
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return { kind: "drop", reason: "malformed json" };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { kind: "drop", reason: "not an object" };
  }
  const obj = parsed as Record<string, unknown>;
  const type = obj.type;
  if (typeof type !== "string") return { kind: "drop", reason: "missing type" };
  // Liveness signals are matched by their reserved type FIRST — they carry no actor and are never
  // wire event types, so they can never be confused with a session event.
  if (LIVENESS_TYPES.has(type)) return { kind: "liveness", signal: type as "pong" | "heartbeat" };
  const actor = obj.actor;
  if (typeof actor !== "string" || !ACTOR_SET.has(actor)) {
    return { kind: "drop", reason: "missing/invalid actor" };
  }
  if (!isWireEventType(type)) return { kind: "drop", reason: `non-wire type ${type}` };
  const payload = obj.payload;
  const ev: ChildEmittedEvent = { actor: actor as Actor, type: type as EventType };
  // payload is optional; only carry it when the child sent an object (never a scalar/array).
  if (typeof payload === "object" && payload !== null && !Array.isArray(payload)) {
    ev.payload = payload as Record<string, unknown>;
  }
  return { kind: "event", event: ev };
}

// ── 3. Index writer + journal ports ────────────────────────────────────────────
// Narrow ports so the tee/escalation depend on a tiny surface (testable with a fake), and the ONE
// place that touches drizzle + the schema tables is the `bindIndexWriter` adapter.

/** A row for the append-only `event` table (index-schema shape; `seq` is the table's autoincrement). */
export interface EventInsertRow {
  sessionId: string | null;
  ts: number;
  actor: Actor;
  type: EventType;
  /** JSON-encoded payload, or null (synthetics/empty), matching `event.payload_json`. */
  payload: string | null;
}

/** The two index mutations HARNESS §2 needs: append an event row, and park a session `blocked`. */
export interface IndexWriter {
  insertEvent(row: EventInsertRow): Promise<void>;
  markSessionBlocked(sessionId: string, updatedAt: number): Promise<void>;
}

/** Bind the narrow writer to the real drizzle handle — the only drizzle/schema touch in this module. */
export function bindIndexWriter(db: IndexDb): IndexWriter {
  return {
    async insertEvent(row: EventInsertRow): Promise<void> {
      await db.insert(event).values(row);
    },
    async markSessionBlocked(sessionId: string, updatedAt: number): Promise<void> {
      await db
        .update(session)
        .set({ status: PARK_STATE, updatedAt })
        .where(eq(session.id, sessionId));
    },
  };
}

/** Where flattened `session.jsonl` lines land. Injected so the tee is testable without a filesystem. */
export interface Journal {
  append(line: string): Promise<void>;
}

/** The FS journal: append `line + "\n"` to `runs/run-<id>/session.jsonl`, mkdir'ing the run dir once. */
export function fsJournal(runDir: string): Journal {
  const path = join(runDir, "session.jsonl");
  let ensured: Promise<unknown> | null = null;
  return {
    async append(line: string): Promise<void> {
      if (ensured === null) ensured = mkdir(dirname(path), { recursive: true });
      await ensured;
      await appendFile(path, `${line}\n`, "utf8");
    },
  };
}

/** Rotation thresholds — a segment is bounded at whichever it reaches first (DECISIONS §D3). */
export interface RotateOptions {
  /** Bytes ceiling for a session.jsonl segment (default DEFAULT_ROTATE_MAX_BYTES = 5 MB). */
  maxBytes?: number;
  /** Line ceiling for a segment (default DEFAULT_ROTATE_MAX_LINES = 5,000). */
  maxLines?: number;
}

/** Count the newlines in a buffer — the segment's committed line count (each append writes one `\n`). */
function countLines(buf: string): number {
  let n = 0;
  for (let i = 0; i < buf.length; i++) if (buf.charCodeAt(i) === 10) n++;
  return n;
}

/** The highest existing `session.jsonl.<n>` suffix in `runDir` (0 if none) — so a respawn (which reuses
 *  the run dir, HARNESS §5) continues the suffix sequence rather than clobbering an earlier segment. */
async function highestRotationSuffix(runDir: string): Promise<number> {
  let max = 0;
  try {
    for (const name of await readdir(runDir)) {
      const m = /^session\.jsonl\.(\d+)$/.exec(name);
      if (m) max = Math.max(max, Number(m[1]));
    }
  } catch {
    // run dir not created yet — no prior segments
  }
  return max;
}

/**
 * Digest a just-rotated segment into `summary.md` — the "summarize every 10–20 steps" rule applied at
 * the file layer (DECISIONS §D3). v1 is a MECHANICAL digest (no model call, pinned): the segment's line/
 * byte totals + an event-type histogram parsed from its lines. Appended (never rewritten), so the digest
 * accretes across rotations — the FS keeps the tail + summaries while the index keeps the full archive.
 */
async function appendRotationSummary(
  runDir: string,
  n: number,
  rotatedPath: string,
  lineCount: number,
  byteCount: number,
): Promise<void> {
  const histogram = new Map<string, number>();
  try {
    const buf = await readFile(rotatedPath, "utf8");
    for (const line of buf.split("\n")) {
      if (line === "") continue;
      let type = "(unparseable)";
      try {
        const t = (JSON.parse(line) as { type?: unknown }).type;
        if (typeof t === "string") type = t;
      } catch {
        // a non-JSON line (should not occur on our own append path) still counts, bucketed
      }
      histogram.set(type, (histogram.get(type) ?? 0) + 1);
    }
  } catch {
    // the rotated file vanished (a concurrent teardown) — record the totals without a histogram
  }
  const rows = [...histogram.entries()]
    .sort((a, b) => b[1] - a[1])
    .map(([type, count]) => `- ${type}: ${count}`)
    .join("\n");
  const section = `## session.jsonl.${n} — ${lineCount} lines, ${byteCount} bytes\n${rows || "- (empty)"}\n\n`;
  try {
    await appendFile(join(runDir, "summary.md"), section, "utf8");
  } catch {
    // The digest is ADVISORY + best-effort (D3: the FS keeps the tail + summaries, the index keeps the
    // full archive; summary.md is never replayed). A failed summary write must NEVER reject the
    // load-bearing rotate/append path — else a digest-only failure (e.g. summary.md unwritable) would
    // reap a healthy child. Same guard-every-best-effort-write discipline as the readFile above.
  }
}

/**
 * A ROTATING FS journal (DECISIONS §D3, BRO-1811). Appends to `session.jsonl` like `fsJournal`, but
 * bounds each segment: when the next line would push the CURRENT segment over `maxBytes` OR `maxLines`,
 * it rotates `session.jsonl` → `session.jsonl.<n>` (n increments — segments are never re-renamed, so
 * reading `.1 → .2 → … → session.jsonl` reproduces the append order gaplessly for F9 replay), digests the
 * rotated segment into `summary.md`, and resumes on a fresh `session.jsonl`. The index `event` table
 * keeps the full archive (seq is the archive); this only bounds the on-disk tail.
 *
 * Counters seed LAZILY from the existing file on first append (a fresh-context respawn reuses the run
 * dir, so a new journal instance must continue the running segment's size — not restart at zero). A
 * single line larger than `maxBytes` never triggers a rotate on an EMPTY segment (it would loop
 * rotating forever) — it is written whole, and the NEXT line rotates.
 */
export function fsRotatingJournal(runDir: string, opts: RotateOptions = {}): Journal {
  const maxBytes = opts.maxBytes ?? DEFAULT_ROTATE_MAX_BYTES;
  const maxLines = opts.maxLines ?? DEFAULT_ROTATE_MAX_LINES;
  const path = join(runDir, "session.jsonl");
  let init: Promise<void> | null = null;
  let bytes = 0;
  let lines = 0;
  let rotations = 0;

  async function ensureInit(): Promise<void> {
    await mkdir(dirname(path), { recursive: true });
    rotations = await highestRotationSuffix(runDir);
    try {
      const buf = await readFile(path, "utf8");
      bytes = Buffer.byteLength(buf, "utf8");
      lines = countLines(buf);
    } catch {
      bytes = 0;
      lines = 0; // no current segment yet
    }
  }

  async function rotate(): Promise<void> {
    const n = rotations + 1;
    const rotatedPath = `${path}.${n}`;
    const segLines = lines;
    const segBytes = bytes;
    await rename(path, rotatedPath);
    // Advance state on the LOAD-BEARING op (the rename succeeded — the segment is durable in `.n`).
    // This MUST happen before the advisory digest: if it lagged, a summary-write failure would leave
    // `rotations` un-advanced, and a retry would recompute the same `n` and rename-CLOBBER the durable
    // `.n` segment — a gap in the F9 replay stream (P20 BRO-1811). `appendRotationSummary` is internally
    // guarded (never throws), so the digest is purely additive after state is already consistent.
    rotations = n;
    bytes = 0;
    lines = 0;
    await appendRotationSummary(runDir, n, rotatedPath, segLines, segBytes);
  }

  return {
    async append(line: string): Promise<void> {
      if (init === null) init = ensureInit();
      await init;
      const chunk = `${line}\n`;
      const chunkBytes = Buffer.byteLength(chunk, "utf8");
      // Rotate BEFORE appending if this line would push a NON-EMPTY segment over either threshold. The
      // non-empty guard means a lone oversized line lands in its own segment rather than rotating forever.
      if (lines > 0 && (bytes + chunkBytes > maxBytes || lines + 1 > maxLines)) {
        await rotate();
      }
      await appendFile(path, chunk, "utf8");
      bytes += chunkBytes;
      lines += 1;
    },
  };
}

/** Where raw child stderr lands. Injected for the same testability reason as `Journal`. */
export interface StderrSink {
  write(bytes: Uint8Array): Promise<void>;
}

/** The FS stderr log: append raw bytes to `runs/run-<id>/child.stderr.log`, never parsed. */
export function fsStderrLog(runDir: string): StderrSink {
  const path = join(runDir, "child.stderr.log");
  let ensured: Promise<unknown> | null = null;
  return {
    async write(bytes: Uint8Array): Promise<void> {
      if (ensured === null) ensured = mkdir(dirname(path), { recursive: true });
      await ensured;
      await appendFile(path, bytes);
    },
  };
}

// ── 4. SessionTee — the FS-FIRST, single-writer event sink ─────────────────────
// The load-bearing invariant (HARNESS §2 + api/stream.ts WATERMARK SAFETY): line arrival order ==
// `session.jsonl` line order == `event.seq` order == commit order. Two disciplines enforce it:
//   (a) FS FIRST — the journal append resolves BEFORE the index insert, so the FS is never behind the
//       index (a rebuild replays the journal as truth; the index is a derived projection).
//   (b) SERIALIZED — every `append()` chains on the previous one through `#queue`, so even concurrent
//       callers (the stdout pump AND the hung-escalation's `run.hung`) never interleave writes.

export interface SessionTeeDeps {
  writer: IndexWriter;
  journal: Journal;
  sessionId: string;
  /** Epoch-ms clock (injected — this module holds no ambient time). */
  now: () => number;
}

export class SessionTee {
  #queue: Promise<void> = Promise.resolve();
  constructor(private readonly deps: SessionTeeDeps) {}

  /** Tee one event: append the flattened journal line FIRST, then project the index row. Serialized. */
  append(ev: ChildEmittedEvent): Promise<void> {
    const run = this.#queue.then(() => this.#write(ev));
    // Keep the chain alive even if one write rejects — a single failed write must not wedge the queue
    // for every later event. Callers still see this write's rejection via the returned promise.
    this.#queue = run.catch(() => {});
    return run;
  }

  /** Resolve once every enqueued write has settled (used to drain before teardown). */
  drain(): Promise<void> {
    return this.#queue;
  }

  async #write(ev: ChildEmittedEvent): Promise<void> {
    const ms = this.deps.now();
    const payload = ev.payload;
    // (a) FS FIRST: the flattened A.3 line, byte-identical to fsJournalSink (proxy/events.ts) — spread
    // payload first, envelope fields last so a stray payload `type` can never shadow the real type.
    const line = JSON.stringify({
      ...(payload ?? {}),
      ts: new Date(ms).toISOString(),
      actor: ev.actor,
      type: ev.type,
    });
    await this.deps.journal.append(line);
    // (b) THEN the index row (numeric ts, JSON-string payload). SSE fan-out is automatic off this insert.
    await this.deps.writer.insertEvent({
      sessionId: this.deps.sessionId,
      ts: ms,
      actor: ev.actor,
      type: ev.type,
      payload: payload === undefined ? null : JSON.stringify(payload),
    });
  }
}

// ── 5. ChildControl — the stdin control channel ────────────────────────────────
// Supervisor → child NDJSON. Writing to a DEAD child's stdin must never throw unhandled — control is
// best-effort (the reap path owns the real lifecycle), so a closed pipe is swallowed, not fatal.

/** The narrow stdin surface the control channel needs (a Bun `FileSink`'s `.write` satisfies it). */
export type StdinWriter = (bytes: string) => void | Promise<void>;

export class ChildControl {
  constructor(private readonly write: StdinWriter) {}

  /** F10 — route a user message into the live loop. */
  chat(message: unknown): Promise<void> {
    return this.#send({ type: "chat", message });
  }

  /** Graceful stop — the child finishes the beat, writes memory, exits 10 (HARNESS §2). */
  stop(reason: string): Promise<void> {
    return this.#send({ type: "stop", reason });
  }

  /** Liveness probe — the child echoes `{"type":"pong"}`. */
  ping(): Promise<void> {
    return this.#send({ type: "ping" });
  }

  async #send(obj: unknown): Promise<void> {
    try {
      await this.write(`${JSON.stringify(obj)}\n`);
    } catch {
      // stdin closed (the child already died) — control is best-effort, never an unhandled rejection.
    }
  }
}

// ── 6. LivenessMonitor — tick-driven, injectable clock ─────────────────────────
// No real timers here: `activity()` records the last time the child spoke, and `tick()` (called by a
// caller-owned interval in production, or directly by tests) decides. Idle past `hungMs` → escalate
// (a ONE-WAY latch — a late pong can't un-hang it); else idle past `pingIdleMs` → ping once until the
// next activity resets the gate (so we don't ping every tick).

export interface LivenessMonitorDeps {
  /** Ping when the child has been silent this long (config.childHeartbeatMs). */
  pingIdleMs: number;
  /** Escalate when the child has been silent this long (config.childHungMs). */
  hungMs: number;
  /** Epoch-ms clock (injected). */
  now: () => number;
  /** Send a ping (the caller wires this to ChildControl.ping). */
  onPing: () => void;
  /** Escalate a hung child (the caller wires this to escalateHung). Fired at most once. */
  onHung: () => void;
}

export class LivenessMonitor {
  #lastActivityAt: number;
  #hung = false;
  #pingedSinceActivity = false;
  constructor(private readonly deps: LivenessMonitorDeps) {
    this.#lastActivityAt = deps.now();
  }

  /** Any byte from the child (event or liveness signal) counts as activity. */
  activity(): void {
    this.#lastActivityAt = this.deps.now();
    this.#pingedSinceActivity = false;
  }

  /** One liveness decision. Idempotent once hung (the latch). */
  tick(): void {
    if (this.#hung) return;
    const idle = this.deps.now() - this.#lastActivityAt;
    if (idle > this.deps.hungMs) {
      this.#hung = true; // one-way latch BEFORE onHung so a re-entrant tick can't double-escalate
      this.deps.onHung();
      return;
    }
    if (idle > this.deps.pingIdleMs && !this.#pingedSinceActivity) {
      this.#pingedSinceActivity = true;
      this.deps.onPing();
    }
  }

  get hung(): boolean {
    return this.#hung;
  }
}

// ── 7. Hung escalation — SIGTERM → grace → SIGKILL → park blocked ───────────────
// The kill sequence the monitor's `onHung` triggers. SIGTERM first (graceful, same as `stop`), race
// the child's exit against a `graceMs` window, SIGKILL if it outlives the grace, THEN record the
// escalation: a `run.hung` event through the tee (journal + index) and the session row parked
// `blocked`. `delay` is injectable so tests drive the grace deterministically (no wall-time sleeps).

/** The narrow child surface escalation needs — a subset of the full stdio port. */
export interface KillablePort {
  kill(signal?: number | NodeJS.Signals): void;
  exited: Promise<number>;
}

export interface HungEscalationDeps {
  child: KillablePort;
  tee: SessionTee;
  writer: IndexWriter;
  sessionId: string;
  now: () => number;
  graceMs: number;
  /** Grace timer — injectable; default a real `setTimeout`. Tests pass an immediate/controlled resolve. */
  delay?: (ms: number) => Promise<void>;
}

export async function escalateHung(deps: HungEscalationDeps): Promise<void> {
  const delay = deps.delay ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  // 1. graceful SIGTERM
  deps.child.kill("SIGTERM");
  // 2. race the child's exit against the grace window. A rejected `exited` (spawn error) still counts
  // as "gone" — no point SIGKILLing a process that already failed.
  let exited = false;
  const exitedP = deps.child.exited.then(
    () => {
      exited = true;
    },
    () => {
      exited = true;
    },
  );
  await Promise.race([exitedP, delay(deps.graceMs)]);
  // 3. still alive after grace → the kill switch (F8), no cooperation assumed
  if (!exited) deps.child.kill("SIGKILL");
  // 4. record the escalation. Both writes are best-effort and INDEPENDENTLY guarded: a failed run.hung
  // append (disk full, index closed) must NOT skip the park — parking `blocked` is the load-bearing
  // state change (HARNESS §2), and D5 reconcile re-derives the event log on restart. Nothing throws:
  // escalateHung is fired fire-and-forget from the liveness tick, so an escaped rejection would be
  // unhandled (mirrors the internal swallow that makes onPing / ChildControl.#send safe).
  try {
    await deps.tee.append({
      actor: "system",
      type: EVENT_TYPES.RUN_HUNG,
      payload: { reason: "child silent past hungMs", graceMs: deps.graceMs },
    });
  } catch {
    // journal/index write failed — the park below is what matters; attempt it regardless
  }
  try {
    await deps.writer.markSessionBlocked(deps.sessionId, deps.now());
  } catch {
    // a closed/full index leaves the child killed but the row unparked; D5 re-derives on restart
  }
}

// ── 8. superviseChildStdio — wire it all together ──────────────────────────────
// The one entry point BRO-1779 (dispatch) calls. `child` is a NARROW port (not `Bun.Subprocess`) so
// dispatch and tests both satisfy it — `fromBunSubprocess` adapts the real thing. Returns the control
// channel, the liveness monitor, and a `done` promise that resolves when stdout drains (the child
// closed its stdout), after the tee has flushed. `stop()` clears the tick interval.

/** The narrow child port `superviseChildStdio` drives (a `Bun.Subprocess` adapts to it). */
export interface ChildStdioPort extends KillablePort {
  stdout: ReadableStream<Uint8Array>;
  stderr: ReadableStream<Uint8Array>;
  /** Write control bytes to the child's stdin. */
  writeStdin: StdinWriter;
}

export interface SuperviseDeps {
  db: IndexDb;
  sessionId: string;
  runDir: string;
  now: () => number;
  config?: RuntimeConfig;
  /** Override the tick scheduler (default `setInterval`); tests inject a manual ticker. Returns a canceller. */
  scheduleTick?: (fn: () => void, ms: number) => () => void;
  /** Override the grace timer for escalation (threaded to escalateHung). */
  delay?: (ms: number) => Promise<void>;
  /** Override the index writer (default binds `deps.db`). Tests inject a failing writer. */
  writer?: IndexWriter;
  /** Override the FS journal (default `fsJournal(runDir)`). Tests inject a failing journal. */
  journal?: Journal;
}

export interface SupervisedChild {
  control: ChildControl;
  liveness: LivenessMonitor;
  tee: SessionTee;
  /** Resolves when the child's stdout closes and the tee has drained. */
  done: Promise<void>;
  /** Stop the liveness tick interval (idempotent). Call on reap. */
  stop(): void;
  /** True once a tee-write failure REAPED the child in-band (SIGKILL + run.failed + park blocked). The
   *  supervisor's reap reads this to avoid emitting a DUPLICATE run.failed terminal event (BRO-1779). */
  supervisionFailed(): boolean;
}

export function superviseChildStdio(child: ChildStdioPort, deps: SuperviseDeps): SupervisedChild {
  const writer = deps.writer ?? bindIndexWriter(deps.db);
  // The bulk session.jsonl writer ROTATES at the D3 thresholds (BRO-1811). The supervisor's terminal-
  // event journal (a handful of run.finished/killed/failed lines at reap) stays plain fsJournal — it
  // appends to whatever segment is current, too few lines to warrant its own rotation bookkeeping.
  const journal =
    deps.journal ??
    fsRotatingJournal(deps.runDir, {
      maxBytes: deps.config?.rotateMaxBytes,
      maxLines: deps.config?.rotateMaxLines,
    });
  const stderrSink = fsStderrLog(deps.runDir);
  const tee = new SessionTee({ writer, journal, sessionId: deps.sessionId, now: deps.now });
  const control = new ChildControl(child.writeStdin);

  const pingIdleMs = deps.config?.childHeartbeatMs ?? DEFAULT_CHILD_HEARTBEAT_MS;
  const hungMs = deps.config?.childHungMs ?? DEFAULT_CHILD_HUNG_MS;
  const graceMs = deps.config?.childGraceMs ?? DEFAULT_CHILD_GRACE_MS;

  const liveness = new LivenessMonitor({
    pingIdleMs,
    hungMs,
    now: deps.now,
    onPing: () => {
      void control.ping();
    },
    onHung: () => {
      // Fire-and-forget from the tick — escalateHung swallows its own write failures, but a `.catch`
      // here is the belt to that suspenders so a rejection can never escape unhandled.
      void escalateHung({
        child,
        tee,
        writer,
        sessionId: deps.sessionId,
        now: deps.now,
        graceMs,
        delay: deps.delay,
      }).catch(() => {});
    },
  });

  // Tick at ~1/5 of the tightest window so hang detection overshoots by at most a fraction of it
  // (a 60 s heartbeat → 12 s tick → ≤12 s late on the 5 min hung cutoff), floored at 500 ms so a
  // tiny test cadence never busy-spins. Caller-owned so it can be stopped on reap and swapped in tests.
  const tickMs = Math.max(500, Math.floor(Math.min(pingIdleMs, hungMs) / 5));
  const schedule =
    deps.scheduleTick ??
    ((fn, ms) => {
      const id = setInterval(fn, ms);
      return () => clearInterval(id);
    });
  const cancelTick = schedule(() => liveness.tick(), tickMs);
  let stopped = false;
  const stop = (): void => {
    if (stopped) return;
    stopped = true;
    cancelTick();
  };

  // A tee write failure means durability is lost (disk full, index closed/busy) — we can no longer
  // supervise this child safely. REAP it: SIGKILL + best-effort run.failed + park `blocked`. Without
  // this, a rejected tee.append propagates out of pumpStdout, is swallowed by `.catch(() => {})`, and
  // `done` resolves as a clean finish while the child is STILL ALIVE — then stop() cancels the tick and
  // the child silently loses hang detection forever (the P20-caught CRITICAL). Idempotent; skipped once
  // `stopped` (an intentional teardown, where a late write failing is benign).
  let supervisionFailed = false;
  const failSupervision = async (reason: string): Promise<void> => {
    if (supervisionFailed || stopped) return;
    supervisionFailed = true;
    stop(); // we own the failure now — no more liveness ticks / redundant hung-escalation
    child.kill("SIGKILL"); // cannot durably record its work → do not leave it running unsupervised
    try {
      await tee.append({ actor: "system", type: EVENT_TYPES.RUN_FAILED, payload: { reason } });
    } catch {
      // the write path is the thing failing — best-effort; the park below is the load-bearing change
    }
    try {
      await writer.markSessionBlocked(deps.sessionId, deps.now());
    } catch {
      // closed/full index — the child is killed; D5 reconcile re-derives the row on restart
    }
  };

  const splitter = createNdjsonSplitter();
  const decoder = new TextDecoder();

  const onLine = async (line: string): Promise<void> => {
    liveness.activity(); // any output resets liveness, before we decide what the line is
    const c = classifyLine(line);
    if (c.kind === "event") {
      try {
        await tee.append(c.event);
      } catch (err) {
        // Durability lost mid-supervision — reap the still-live child instead of ending silently.
        await failSupervision(`tee write failed: ${String((err as Error)?.message ?? err)}`);
      }
    }
    // liveness/drop: nothing else to do — activity already recorded; drops are silently discarded
  };

  const pumpStdout = async (): Promise<void> => {
    const reader = child.stdout.getReader();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value === undefined) continue;
        const text = decoder.decode(value, { stream: true });
        if (text.length === 0) continue;
        // Await each line so FS+DB backpressure bounds memory AND enqueue order == line order.
        for (const l of splitter.push(text)) await onLine(l);
      }
      const tail = splitter.flush();
      if (tail !== null) await onLine(tail);
    } finally {
      reader.releaseLock();
    }
  };

  const pumpStderr = async (): Promise<void> => {
    const reader = child.stderr.getReader();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value !== undefined && value.length > 0) await stderrSink.write(value);
      }
    } finally {
      reader.releaseLock();
    }
  };

  // stderr pumps alongside but isn't part of `done` (forensics only, and it may outlive stdout).
  void pumpStderr().catch(() => {});

  const done = pumpStdout()
    .catch(() => {})
    .then(() => tee.drain())
    .then(() => stop());

  return { control, liveness, tee, done, stop, supervisionFailed: () => supervisionFailed };
}

/** Adapt a Bun subprocess (stdout/stderr piped, stdin piped) to the narrow `ChildStdioPort`. */
export function fromBunSubprocess(proc: Bun.Subprocess<"pipe", "pipe", "pipe">): ChildStdioPort {
  return {
    stdout: proc.stdout,
    stderr: proc.stderr,
    exited: proc.exited,
    kill: (signal) => proc.kill(signal),
    writeStdin: (bytes: string) => {
      proc.stdin.write(bytes);
      // flush so a control line reaches the child promptly rather than sitting in the sink buffer
      void proc.stdin.flush();
    },
  };
}
