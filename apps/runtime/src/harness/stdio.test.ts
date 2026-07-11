/// <reference types="bun" />
// stdio.test.ts — the BRO-1767 supervisor-tee suite (`bun test apps/runtime --filter stdio`).
//
// Two anchor tests carry the load-bearing invariants (and are written to FAIL if the invariant is
// broken — the anti-vacuity discipline, [[self-hosting-vacuous-pass]]):
//   1. append-first ordering — SessionTee writes session.jsonl BEFORE the index row (swap the order
//      and the ordered-log assertion fails).
//   2. hung escalation — escalateHung sends SIGTERM then SIGKILL, appends run.hung, parks blocked
//      (drop the SIGKILL and the exact-signal-sequence assertion fails).
// Plus the byte-compatibility proof against fsJournalSink (the co-writer on the same file), and unit
// coverage of the splitter, classifier, control channel, and liveness monitor.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { desc, eq } from "drizzle-orm";
import { type IndexHandle, openIndex } from "../db/client";
import { event, session } from "../db/schema";
import { fsJournalSink } from "../proxy/events";
import type { ChildEmittedEvent } from "./runner";
import {
  ChildControl,
  classifyLine,
  createNdjsonSplitter,
  type EventInsertRow,
  escalateHung,
  type IndexWriter,
  type Journal,
  type KillablePort,
  LivenessMonitor,
  SessionTee,
  superviseChildStdio,
} from "./stdio";

// ── Test doubles ───────────────────────────────────────────────────────────────

/** An IndexWriter + Journal that record into ONE ordered log — the append-first discriminator. */
function orderedFakes() {
  const log: string[] = [];
  const rows: EventInsertRow[] = [];
  const blocked: Array<{ sessionId: string; updatedAt: number }> = [];
  const journal: Journal = {
    async append(line: string) {
      log.push(`fs:${line}`);
    },
  };
  const writer: IndexWriter = {
    async insertEvent(row: EventInsertRow) {
      log.push("db:insert");
      rows.push(row);
    },
    async markSessionBlocked(sessionId: string, updatedAt: number) {
      log.push("db:block");
      blocked.push({ sessionId, updatedAt });
    },
  };
  return { log, rows, blocked, journal, writer };
}

/** A fake child that records the kill signals it receives; its `exited` resolves only if told to. */
function fakeChild(opts: { neverExits?: boolean } = {}): KillablePort & { signals: string[] } {
  const signals: string[] = [];
  return {
    signals,
    kill(signal?: number | NodeJS.Signals) {
      signals.push(String(signal));
    },
    // A never-exiting child leaves `exited` pending forever (the hung case).
    exited: opts.neverExits ? new Promise<number>(() => {}) : Promise.resolve(0),
  };
}

/** A ReadableStream over the given chunks (strings encoded UTF-8) — a fake child stdout/stderr. */
function streamOf(chunks: Array<string | Uint8Array>): ReadableStream<Uint8Array> {
  const enc = new TextEncoder();
  const arr = chunks.map((c) => (typeof c === "string" ? enc.encode(c) : c));
  let i = 0;
  return new ReadableStream<Uint8Array>({
    pull(controller) {
      if (i < arr.length) controller.enqueue(arr[i++] as Uint8Array);
      else controller.close();
    },
  });
}

const FIXED_MS = 1_700_000_000_000; // 2023-11-14T22:13:20.000Z
const fixedNow = () => FIXED_MS;

const handles: IndexHandle[] = [];
const tmpDirs: string[] = [];
afterEach(async () => {
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmpDirs.splice(0)) await rm(d, { recursive: true, force: true });
});
async function mkTmp(): Promise<string> {
  const d = await mkdtemp(join(tmpdir(), "maestro-stdio-"));
  tmpDirs.push(d);
  return d;
}

// ── 1. Line splitter ─────────────────────────────────────────────────────────

describe("createNdjsonSplitter", () => {
  test("splits complete lines and retains the trailing partial across chunks", () => {
    const s = createNdjsonSplitter();
    expect(s.push('{"a":1}\n{"b":2}\n{"c"')).toEqual(['{"a":1}', '{"b":2}']);
    expect(s.push(":3}\n")).toEqual(['{"c":3}']);
    expect(s.flush()).toBeNull();
  });

  test("a child that exits mid-line surfaces the partial via flush", () => {
    const s = createNdjsonSplitter();
    expect(s.push('{"a":1}\n{"partial"')).toEqual(['{"a":1}']);
    expect(s.flush()).toBe('{"partial"');
  });

  test("strips a trailing CR so a CRLF child never yields a corrupt fragment", () => {
    const s = createNdjsonSplitter();
    expect(s.push('{"a":1}\r\n')).toEqual(['{"a":1}']);
  });

  test("a buffer past maxLineBytes with no newline is dropped and counted (no OOM)", () => {
    const s = createNdjsonSplitter(8);
    expect(s.push("abcdefghij")).toEqual([]); // 10 bytes, no newline → over the 8-byte cap
    expect(s.overflows()).toBe(1);
    // the pump keeps working after a drop
    expect(s.push('{"a":1}\n')).toEqual(['{"a":1}']);
  });

  test("extracts a COMPLETE line before the overflow drop — a valid line co-resident with an over-cap prefix is not lost", () => {
    // BRO-1862 P20 anti-vacuity backstop: the overflow drop must run AFTER line extraction, so a runaway
    // (over-cap, no-newline) prefix that shares one push with a following complete line does NOT eat that
    // line. This is the invariant the child's stdin control reader relies on (a dropped `stop` would keep a
    // run beating after the human hit stop). Asserted DETERMINISTICALLY here — a single push, no stdin pipe
    // — so a drop-before-extract regression reds regardless of any Bun stdin chunk-size change.
    const s = createNdjsonSplitter(16);
    const junk = "A".repeat(20); // a COMPLETE line longer than the 16-byte cap
    // one push: the over-cap complete line + a valid partial after its newline.
    expect(s.push(`${junk}\n{"ok":1}`)).toEqual([junk]); // the complete line is returned (extract first)
    expect(s.overflows()).toBe(0); // nothing dropped — a drop-before-extract bug would trip overflow here
    expect(s.push("\n")).toEqual(['{"ok":1}']); // the buffered partial SURVIVED and completes next push
  });
});

// ── 2. Classification ──────────────────────────────────────────────────────────

describe("classifyLine", () => {
  test("a well-formed session event classifies as event with its payload", () => {
    const c = classifyLine('{"actor":"agent","type":"tool.call","payload":{"tool":"edit"}}');
    expect(c).toEqual({
      kind: "event",
      event: { actor: "agent", type: "tool.call", payload: { tool: "edit" } },
    });
  });

  test("pong and heartbeat are liveness signals, never persisted", () => {
    expect(classifyLine('{"type":"pong"}')).toEqual({ kind: "liveness", signal: "pong" });
    expect(classifyLine('{"type":"heartbeat"}')).toEqual({ kind: "liveness", signal: "heartbeat" });
  });

  test("malformed json, missing actor, and a non-wire type all drop", () => {
    expect(classifyLine("not json").kind).toBe("drop");
    expect(classifyLine('{"type":"tool.call"}').kind).toBe("drop"); // no actor
    expect(classifyLine('{"actor":"agent","type":"bogus.type"}').kind).toBe("drop"); // non-wire
    expect(classifyLine('{"actor":"martian","type":"tool.call"}').kind).toBe("drop"); // bad actor
    expect(classifyLine("[1,2,3]").kind).toBe("drop"); // array, not an object
  });

  test("an event without payload carries no payload key", () => {
    const c = classifyLine('{"actor":"system","type":"run.started"}');
    expect(c).toEqual({ kind: "event", event: { actor: "system", type: "run.started" } });
    if (c.kind === "event") expect("payload" in c.event).toBe(false);
  });
});

// ── 3. SessionTee — ANCHOR: append-first ordering ──────────────────────────────

describe("SessionTee (append-first, single-writer)", () => {
  test("ANCHOR: session.jsonl append resolves BEFORE the index insert", async () => {
    const { log, journal, writer } = orderedFakes();
    const tee = new SessionTee({ writer, journal, sessionId: "s1", now: fixedNow });
    await tee.append({ actor: "agent", type: "tool.call", payload: { tool: "edit" } });
    // The order is the invariant: FS first, THEN the index. Swap the two awaits in #write and this
    // fails — that is the anti-vacuity guarantee (order asserted, not just "both happened").
    expect(log).toEqual([
      'fs:{"tool":"edit","ts":"2023-11-14T22:13:20.000Z","actor":"agent","type":"tool.call"}',
      "db:insert",
    ]);
  });

  test("a burst of appends preserves line order == insert order (serialized queue)", async () => {
    const { rows, journal, writer } = orderedFakes();
    const tee = new SessionTee({ writer, journal, sessionId: "s1", now: fixedNow });
    // fire without awaiting between calls — the tee's internal queue must still serialize them
    const ps = [0, 1, 2, 3, 4].map((n) =>
      tee.append({ actor: "agent", type: "run.beat", payload: { iteration: n } }),
    );
    await Promise.all(ps);
    expect(rows.map((r) => JSON.parse(r.payload ?? "{}").iteration)).toEqual([0, 1, 2, 3, 4]);
  });

  test("the flattened line is byte-identical to fsJournalSink (the co-writer on session.jsonl)", async () => {
    // Capture the tee's journal line…
    let teeLine = "";
    const journal: Journal = {
      async append(line: string) {
        teeLine = line;
      },
    };
    const writer: IndexWriter = { async insertEvent() {}, async markSessionBlocked() {} };
    const tee = new SessionTee({ writer, journal, sessionId: "s1", now: fixedNow });
    const ev: ChildEmittedEvent = {
      actor: "system",
      type: "budget.metered",
      payload: { usd: 0.02 },
    };
    await tee.append(ev);
    // …and what fsJournalSink actually writes to disk for the same logical event.
    const dir = await mkTmp();
    await fsJournalSink().emit(dir, {
      ts: new Date(FIXED_MS).toISOString(),
      actor: ev.actor,
      type: ev.type,
      payload: ev.payload ?? {},
    });
    const onDisk = (await readFile(join(dir, "session.jsonl"), "utf8")).trimEnd();
    expect(teeLine).toBe(onDisk);
  });
});

// ── 4. ChildControl ────────────────────────────────────────────────────────────

describe("ChildControl", () => {
  test("chat/stop/ping write NDJSON control lines", async () => {
    const lines: string[] = [];
    const control = new ChildControl((b) => {
      lines.push(b);
    });
    await control.chat({ role: "user", parts: [] });
    await control.stop("user_stop");
    await control.ping();
    expect(lines).toEqual([
      '{"type":"chat","message":{"role":"user","parts":[]}}\n',
      '{"type":"stop","reason":"user_stop"}\n',
      '{"type":"ping"}\n',
    ]);
  });

  test("writing to a dead child's stdin never throws unhandled", async () => {
    const control = new ChildControl(() => {
      throw new Error("EPIPE: stdin closed");
    });
    // best-effort: the rejection is swallowed, the caller's await resolves
    await expect(control.ping()).resolves.toBeUndefined();
  });
});

// ── 5. LivenessMonitor ──────────────────────────────────────────────────────────

describe("LivenessMonitor (tick-driven, injectable clock)", () => {
  test("idle past pingIdleMs pings once until the next activity", () => {
    let clock = 0;
    let pings = 0;
    let hangs = 0;
    const m = new LivenessMonitor({
      pingIdleMs: 100,
      hungMs: 1000,
      now: () => clock,
      onPing: () => pings++,
      onHung: () => hangs++,
    });
    clock = 50;
    m.tick();
    expect(pings).toBe(0); // not idle yet
    clock = 150;
    m.tick();
    m.tick(); // still idle — must NOT ping again until activity resets the gate
    expect(pings).toBe(1);
    m.activity(); // child spoke
    clock = 300; // 150ms since activity → idle again
    m.tick();
    expect(pings).toBe(2);
    expect(hangs).toBe(0);
  });

  test("idle past hungMs escalates exactly once (one-way latch)", () => {
    let clock = 0;
    let hangs = 0;
    const m = new LivenessMonitor({
      pingIdleMs: 100,
      hungMs: 1000,
      now: () => clock,
      onPing: () => {},
      onHung: () => hangs++,
    });
    clock = 1500;
    m.tick();
    expect(m.hung).toBe(true);
    expect(hangs).toBe(1);
    clock = 5000;
    m.tick(); // latched — a later tick must not re-escalate
    expect(hangs).toBe(1);
  });
});

// ── 6. escalateHung — ANCHOR: SIGTERM → SIGKILL → run.hung → blocked ────────────

describe("escalateHung", () => {
  test("ANCHOR: a child that never exits gets SIGTERM then SIGKILL, run.hung appended, session blocked", async () => {
    const { log, rows, blocked, journal, writer } = orderedFakes();
    const tee = new SessionTee({ writer, journal, sessionId: "s1", now: fixedNow });
    const child = fakeChild({ neverExits: true });
    await escalateHung({
      child,
      tee,
      writer,
      sessionId: "s1",
      now: fixedNow,
      graceMs: 15_000,
      delay: () => Promise.resolve(), // grace elapses instantly, deterministically
    });
    // The exact signal SEQUENCE is the invariant — drop the SIGKILL and this fails (anti-vacuity).
    expect(child.signals).toEqual(["SIGTERM", "SIGKILL"]);
    // a run.hung event went through the tee (journal + index), THEN the session parked blocked
    const hung = rows.find((r) => r.type === EVENT_TYPES.RUN_HUNG);
    expect(hung).toBeDefined();
    expect(blocked).toEqual([{ sessionId: "s1", updatedAt: FIXED_MS }]);
    // ordering: the run.hung tee (fs then db) precedes the block
    expect(log).toEqual([expect.stringContaining('"type":"run.hung"'), "db:insert", "db:block"]);
  });

  test("a child that exits within grace is NOT SIGKILLed", async () => {
    const { journal, writer } = orderedFakes();
    const tee = new SessionTee({ writer, journal, sessionId: "s1", now: fixedNow });
    const child = fakeChild({ neverExits: false }); // exited resolves immediately
    await escalateHung({
      child,
      tee,
      writer,
      sessionId: "s1",
      now: fixedNow,
      graceMs: 15_000,
      // a grace that never elapses — only the child's own exit can win the race
      delay: () => new Promise<void>(() => {}),
    });
    expect(child.signals).toEqual(["SIGTERM"]); // graceful exit → no kill switch
  });

  test("parks blocked even when the run.hung append fails, and never throws (P20 MAJOR guard)", async () => {
    // The journal (FS-first) rejects, so the run.hung append fails — markSessionBlocked MUST still run,
    // and the whole thing must resolve (it is fired fire-and-forget; an escaped rejection is unhandled).
    const failingJournal: Journal = {
      async append() {
        throw new Error("EIO: journal write failed");
      },
    };
    const blocked: Array<{ sessionId: string; updatedAt: number }> = [];
    const writer: IndexWriter = {
      async insertEvent() {}, // never reached — journal fails first (FS-first)
      async markSessionBlocked(sessionId: string, updatedAt: number) {
        blocked.push({ sessionId, updatedAt });
      },
    };
    const tee = new SessionTee({ writer, journal: failingJournal, sessionId: "s1", now: fixedNow });
    const child = fakeChild({ neverExits: true });
    await expect(
      escalateHung({
        child,
        tee,
        writer,
        sessionId: "s1",
        now: fixedNow,
        graceMs: 15_000,
        delay: () => Promise.resolve(),
      }),
    ).resolves.toBeUndefined();
    // the load-bearing state change happened despite the failed event append
    expect(blocked).toEqual([{ sessionId: "s1", updatedAt: FIXED_MS }]);
    expect(child.signals).toEqual(["SIGTERM", "SIGKILL"]);
  });
});

// ── 7. superviseChildStdio — end-to-end over a real :memory: index ──────────────

describe("superviseChildStdio (integration)", () => {
  test("tees valid events to session.jsonl + the event table, skips liveness + malformed", async () => {
    const handle = await openIndex(":memory:");
    handles.push(handle);
    const runDir = await mkTmp();
    const stdout = streamOf([
      '{"actor":"system","type":"run.started","payload":{"branch":"run/7f3a"}}\n',
      '{"type":"heartbeat"}\n', // liveness — not persisted
      '{"actor":"agent","type":"agent.said","payload":{"text":"working"}}\n',
      "not json at all\n", // malformed — dropped
    ]);
    const port = {
      stdout,
      stderr: streamOf([]),
      writeStdin: () => {},
      kill: () => {},
      exited: Promise.resolve(0),
    };
    const sup = superviseChildStdio(port, {
      db: handle.db,
      sessionId: "s1",
      runDir,
      now: fixedNow,
      // no-op ticker so no real interval fires during the test
      scheduleTick: () => () => {},
    });
    await sup.done;

    // event table: exactly the two real events, in arrival order (seq ascending)
    const rows = await handle.db.select().from(event).orderBy(desc(event.seq));
    expect(rows.map((r) => r.type)).toEqual(["agent.said", "run.started"]); // desc
    expect(rows.every((r) => r.sessionId === "s1")).toBe(true);

    // session.jsonl: two flattened lines, byte-shape as the co-writer would write
    const jsonl = (await readFile(join(runDir, "session.jsonl"), "utf8")).trim().split("\n");
    expect(jsonl).toHaveLength(2);
    expect(JSON.parse(jsonl[0] as string)).toEqual({
      branch: "run/7f3a",
      ts: "2023-11-14T22:13:20.000Z",
      actor: "system",
      type: "run.started",
    });
  });

  test("failure-injection: a rejected tee write REAPS the still-live child and parks it blocked (P20 CRITICAL guard)", async () => {
    // The P20-caught hole: a tee write failure used to let `done` resolve as a clean finish with the
    // child still alive, silently ending supervision. Now it must SIGKILL the child and park blocked.
    const handle = await openIndex(":memory:");
    handles.push(handle);
    const runDir = await mkTmp();
    const sessionId = "s1";
    await handle.db.insert(session).values({
      id: sessionId,
      nodeId: "n1",
      branch: "run/x",
      status: "running",
      startedAt: FIXED_MS,
      updatedAt: FIXED_MS,
    });
    const signals: string[] = [];
    const port = {
      // a valid event the tee will try (and fail) to persist; the stream then closes
      stdout: streamOf(['{"actor":"agent","type":"agent.said","payload":{"text":"hi"}}\n']),
      stderr: streamOf([]),
      writeStdin: () => {},
      kill: (s?: number | NodeJS.Signals) => {
        signals.push(String(s));
      },
      exited: Promise.resolve(0),
    };
    // Only the FS journal fails (ENOSPC); the real db writer stays healthy so the park is observable.
    const failingJournal: Journal = {
      async append() {
        throw new Error("ENOSPC: no space left on device");
      },
    };
    const sup = superviseChildStdio(port, {
      db: handle.db,
      sessionId,
      runDir,
      now: fixedNow,
      scheduleTick: () => () => {},
      journal: failingJournal,
    });
    await sup.done;
    // reaped: SIGKILL sent even though the child never hung — the write failure triggered it
    expect(signals).toContain("SIGKILL");
    // parked: markSessionBlocked ran on the healthy db writer (only the journal failed)
    const rows = await handle.db.select().from(session).where(eq(session.id, sessionId));
    expect(rows[0]?.status).toBe("blocked");
  });
});
