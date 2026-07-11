/// <reference types="bun" />
// rotation.test.ts — BRO-1811 done.check `bun test apps/runtime --filter rotation`. session.jsonl
// rotation (DECISIONS §D3): bound each segment at maxBytes/maxLines → session.jsonl.<n> + summary.md;
// the concatenation of segments in order reproduces the append stream GAPLESSLY (the F9-replay
// invariant). Anti-vacuity [[self-hosting-vacuous-pass]]: assert the exact reconstructed lines + the
// exact segment boundaries, not just "a file exists".

import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, readdir, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { and, eq } from "drizzle-orm";
import { type IndexHandle, openIndex } from "../db/client";
import { event } from "../db/schema";
import { bindIndexWriter, fsRotatingJournal, SessionTee } from "./stdio";

const tmps: string[] = [];
const handles: IndexHandle[] = [];
afterEach(async () => {
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});
async function makeDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "maestro-rotation-"));
  tmps.push(dir);
  return dir;
}

/** An event line the child would emit (JSON, one per append). */
const ev = (i: number): string =>
  JSON.stringify({ actor: "agent", type: "run.beat", payload: { i } });

/** Read every segment in F9-replay order — `.1 → .2 → … → session.jsonl` — and return the concatenated
 *  lines (the exact stream a rebuild-from-FS reconstructs). */
async function replay(dir: string): Promise<string[]> {
  const names = await readdir(dir);
  const numbered = names
    .map((n) => /^session\.jsonl\.(\d+)$/.exec(n))
    .filter((m): m is RegExpExecArray => m !== null)
    .sort((a, b) => Number(a[1]) - Number(b[1]))
    .map((m) => m[0]);
  const order = [...numbered, ...(names.includes("session.jsonl") ? ["session.jsonl"] : [])];
  const out: string[] = [];
  for (const name of order) {
    const buf = await readFile(join(dir, name), "utf8");
    for (const line of buf.split("\n")) if (line !== "") out.push(line);
  }
  return out;
}

describe("session.jsonl rotation (D3) — line threshold", () => {
  test("rotates at maxLines; segments concatenate GAPLESSLY in replay order", async () => {
    const dir = await makeDir();
    const j = fsRotatingJournal(dir, { maxLines: 3 });
    const lines = Array.from({ length: 7 }, (_, i) => ev(i));
    for (const l of lines) await j.append(l);

    // 7 lines, cap 3 → segments of 3, 3, 1: session.jsonl.1, .2, and a live session.jsonl.
    const names = (await readdir(dir)).filter((n) => n.startsWith("session.jsonl")).sort();
    expect(names).toEqual(["session.jsonl", "session.jsonl.1", "session.jsonl.2"]);
    // Each rotated segment is bounded at the cap.
    expect(
      (await readFile(join(dir, "session.jsonl.1"), "utf8")).trimEnd().split("\n"),
    ).toHaveLength(3);
    expect(
      (await readFile(join(dir, "session.jsonl.2"), "utf8")).trimEnd().split("\n"),
    ).toHaveLength(3);
    // The replay reproduces the EXACT append stream — no gaps, no dupes, in order.
    expect(await replay(dir)).toEqual(lines);
  });
});

describe("session.jsonl rotation (D3) — byte threshold", () => {
  test("rotates when the next line would exceed maxBytes", async () => {
    const dir = await makeDir();
    const one = ev(0); // ~55 bytes + newline
    const lineBytes = Buffer.byteLength(`${one}\n`, "utf8");
    // Cap just above one line → the 2nd line rotates (1 line per segment).
    const j = fsRotatingJournal(dir, { maxBytes: lineBytes + 1, maxLines: 10_000 });
    const lines = Array.from({ length: 4 }, (_, i) => ev(i));
    for (const l of lines) await j.append(l);
    const segs = (await readdir(dir)).filter((n) => /^session\.jsonl(\.\d+)?$/.test(n));
    expect(segs.length).toBe(4); // .1 .2 .3 + live
    expect(await replay(dir)).toEqual(lines);
  });

  test("a lone line larger than maxBytes lands in its own segment (never infinite-rotates on empty)", async () => {
    const dir = await makeDir();
    const j = fsRotatingJournal(dir, { maxBytes: 10, maxLines: 10_000 });
    const big = ev(0); // > 10 bytes
    await j.append(big); // empty segment → written whole, NO rotation
    expect((await readdir(dir)).filter((n) => /^session\.jsonl\.\d+$/.test(n))).toEqual([]);
    await j.append(ev(1)); // now the non-empty segment is over → rotate first
    expect(await readFile(join(dir, "session.jsonl.1"), "utf8")).toBe(`${big}\n`);
    expect(await replay(dir)).toEqual([big, ev(1)]);
  });
});

describe("session.jsonl rotation (D3) — summary.md digest", () => {
  test("each rotation appends a mechanical event-type histogram to summary.md (no model call)", async () => {
    const dir = await makeDir();
    const j = fsRotatingJournal(dir, { maxLines: 2 });
    // 3 run.beat + 1 tool.call across the first two segments (2 lines each) → 2 rotations.
    await j.append(JSON.stringify({ type: "run.beat" }));
    await j.append(JSON.stringify({ type: "run.beat" }));
    await j.append(JSON.stringify({ type: "tool.call" }));
    await j.append(JSON.stringify({ type: "run.beat" }));
    await j.append(JSON.stringify({ type: "agent.said" })); // triggers the 2nd rotation (segment 2 full)
    const summary = await readFile(join(dir, "summary.md"), "utf8");
    expect(summary).toContain("## session.jsonl.1 — 2 lines");
    expect(summary).toContain("- run.beat: 2");
    expect(summary).toContain("## session.jsonl.2 — 2 lines");
    expect(summary).toContain("- tool.call: 1");
    expect(summary).toContain("- run.beat: 1");
  });
});

describe("session.jsonl rotation (D3) — advisory digest never breaks the load-bearing path", () => {
  test("a failing summary.md write does NOT reject rotate/append, clobber a segment, or lose a line", async () => {
    const dir = await makeDir();
    // Force every summary.md write to fail: pre-create summary.md as a DIRECTORY (appendFile → EISDIR).
    await mkdir(join(dir, "summary.md"));
    const j = fsRotatingJournal(dir, { maxLines: 1 });

    // Each append past the first triggers a rotation whose digest write fails — must NOT throw.
    await j.append(ev(0));
    await j.append(ev(1)); // rotates .1 (summary write throws internally, swallowed)
    await j.append(ev(2)); // rotates .2 — proves `rotations` advanced despite the digest failure (no .1 clobber)

    const numbered = (await readdir(dir)).filter((n) => /^session\.jsonl\.\d+$/.test(n)).sort();
    expect(numbered).toEqual(["session.jsonl.1", "session.jsonl.2"]); // suffix advanced; .1 not overwritten
    // No line lost: the full stream replays gaplessly even though every digest write failed.
    expect(await replay(dir)).toEqual([ev(0), ev(1), ev(2)]);
  });
});

describe("session.jsonl rotation (D3) — respawn continuity", () => {
  test("a fresh journal over the same run dir continues the suffix sequence + seeds counters from disk", async () => {
    const dir = await makeDir();
    // Attempt 1: fill past one rotation (cap 2, 3 lines → .1 + a live segment with 1 line).
    const j1 = fsRotatingJournal(dir, { maxLines: 2 });
    for (let i = 0; i < 3; i++) await j1.append(ev(i));
    expect((await readdir(dir)).filter((n) => /^session\.jsonl\.\d+$/.test(n))).toEqual([
      "session.jsonl.1",
    ]);
    // Attempt 2 (a fresh-context respawn): a NEW journal instance over the SAME dir. It must seed from
    // the live segment (1 line) and continue suffixes at .2 — never clobber .1.
    const j2 = fsRotatingJournal(dir, { maxLines: 2 });
    await j2.append(ev(3)); // live segment now has 2 lines (seeded 1 + this)
    await j2.append(ev(4)); // would exceed → rotates to .2 (NOT .1), then writes
    const numbered = (await readdir(dir)).filter((n) => /^session\.jsonl\.\d+$/.test(n)).sort();
    expect(numbered).toEqual(["session.jsonl.1", "session.jsonl.2"]);
    // The full stream across BOTH journal instances replays gaplessly, in order.
    expect(await replay(dir)).toEqual([ev(0), ev(1), ev(2), ev(3), ev(4)]);
  });
});

describe("session.jsonl rotation (D3) — SessionTee integration (dogfood)", () => {
  test("the tee's rotating journal bounds session.jsonl while the index keeps the full archive", async () => {
    const dir = await makeDir();
    const h = await openIndex(":memory:");
    handles.push(h);
    const tee = new SessionTee({
      writer: bindIndexWriter(h.db),
      journal: fsRotatingJournal(dir, { maxLines: 2 }),
      sessionId: "r1",
      now: () => 1_700_000_000_000,
    });
    // 5 events through the tee (FS-first, then index) → session.jsonl rotates to .1, .2 + a live tail.
    for (let i = 0; i < 5; i++) {
      await tee.append({ actor: "agent", type: "run.beat", payload: { i } });
    }
    // FS: bounded segments, gapless replay of all 5.
    expect((await replay(dir)).length).toBe(5);
    expect((await stat(join(dir, "session.jsonl.1"))).isFile()).toBe(true);
    // Index: the full archive — all 5 events for the session, none lost across rotation.
    const rows = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, "run.beat")));
    expect(rows).toHaveLength(5);
  });
});
