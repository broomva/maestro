/// <reference types="bun" />
// verifier-judge — Stage 2 LLM judge (VERIFIER §2 Stage 2, §3) for BRO-1786
// (`bun test apps/runtime -t verifier-judge`).
//
// Deterministic by construction: the model call is an injected {@link JudgeCaller} (a scripted mock),
// so a judge reply produces a fixed weighted score with ZERO tokens and NO API key. The final block is
// the WIRE path — a real proxy + the mock upstream + a minted VERIFIER bearer — proving the judge dials
// the proxy, the proxy resolves the pinned judge_model from the role, and the judge never holds the key.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { type IndexHandle, indexUrl, openIndex } from "../db/client";
import { BudgetGuard } from "../proxy/budget";
import { MemoryEventSink } from "../proxy/events";
import { createMockModel } from "../proxy/mock-model";
import { DEFAULT_MODEL_PINS, resolvePinnedModel } from "../proxy/models";
import { createModelProxy, serveProxy } from "../proxy/proxy";
import { type SessionContext, SessionTokenRegistry } from "../proxy/tokens";
import {
  assembleJudgeRequest,
  attachJudge,
  type JudgeCaller,
  type JudgeCheckSummary,
  type JudgeRequestPayload,
  parseJudgeReport,
  parseRubric,
  proxyJudgeCaller,
  renderJudgeJson,
  runJudge,
  weightedScore,
} from "./judge";
import type { VerifierResult } from "./verifier";

// A well-formed rubric: two criteria, scale 0-2, threshold 0.8.
const RUBRIC = `---
threshold: 0.8
scale: [0, 1, 2]
criteria:
  - id: coverage
    weight: 2
    ask: "Every meta tag listed in the brief is present and populated."
  - id: no-regressions
    weight: 1
    ask: "No unrelated files changed; diff is scoped to the brief."
---
Judge the diff against the brief. Score each criterion on the scale.`;

/** An Anthropic-shaped assistant reply whose text is the judge's JSON. */
function reply(text: string): { status: number; body: unknown } {
  return {
    status: 200,
    body: {
      id: "msg",
      type: "message",
      role: "assistant",
      content: [{ type: "text", text }],
      stop_reason: "end_turn",
      usage: { input_tokens: 10, output_tokens: 20 },
    },
  };
}

/** A caller that returns a fixed response and records every payload it was handed. */
function recordingCaller(res: { status: number; body: unknown }): {
  call: JudgeCaller;
  payloads: JudgeRequestPayload[];
} {
  const payloads: JudgeRequestPayload[] = [];
  const call: JudgeCaller = async (payload) => {
    payloads.push(payload);
    return res;
  };
  return { call, payloads };
}

function only<T>(arr: readonly T[]): T {
  const v = arr[0];
  if (v === undefined) throw new Error("expected at least one element");
  return v;
}

const CHECKS: JudgeCheckSummary[] = [{ name: "tests", ok: true }];

// ── parseRubric ────────────────────────────────────────────────────────────────────────────────────

describe("verifier-judge — parseRubric", () => {
  test("parses a well-formed rubric (threshold, scale, weighted criteria, body instructions)", () => {
    const r = parseRubric(RUBRIC);
    expect(r.threshold).toBe(0.8);
    expect(r.scale).toEqual([0, 1, 2]);
    expect(r.criteria).toEqual([
      {
        id: "coverage",
        weight: 2,
        ask: "Every meta tag listed in the brief is present and populated.",
      },
      {
        id: "no-regressions",
        weight: 1,
        ask: "No unrelated files changed; diff is scoped to the brief.",
      },
    ]);
    expect(r.instructions).toContain("Judge the diff against the brief");
  });

  test("accepts the minimal valid scale [0, 1] (the length≥2 floor is a boundary, not over-strict)", () => {
    const r = parseRubric(
      "---\nthreshold: 0.5\nscale: [0, 1]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    );
    expect(r.scale).toEqual([0, 1]);
    // and a below-max score is reachable → fail is possible (the point of the ≥2 floor)
    expect(weightedScore({ criteria: [{ id: "a", score: 0, note: "n" }] }, r)).toBe(0);
  });

  test.each([
    ["no frontmatter fence", "threshold: 0.8\nscale: [0,1]\n"],
    [
      "threshold above 1",
      "---\nthreshold: 1.5\nscale: [0,1]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    ],
    [
      "threshold not a number",
      "---\nthreshold: high\nscale: [0,1]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    ],
    [
      "empty scale",
      "---\nthreshold: 0.5\nscale: []\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    ],
    [
      "non-ascending scale",
      "---\nthreshold: 0.5\nscale: [2, 1, 0]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    ],
    [
      "single-element scale (fail-open: only reply is all-max → always pass)",
      "---\nthreshold: 0.5\nscale: [1]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    ],
    [
      "negative scale value (weightedScore would escape [0,1])",
      "---\nthreshold: 0.5\nscale: [-1, 0, 1]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    ],
    ["no criteria", "---\nthreshold: 0.5\nscale: [0,1]\ncriteria: []\n---\n"],
    [
      "criterion missing ask",
      "---\nthreshold: 0.5\nscale: [0,1]\ncriteria:\n  - {id: a, weight: 1}\n---\n",
    ],
    [
      "criterion weight <= 0",
      "---\nthreshold: 0.5\nscale: [0,1]\ncriteria:\n  - {id: a, weight: 0, ask: x}\n---\n",
    ],
    [
      "duplicate criterion id",
      "---\nthreshold: 0.5\nscale: [0,1]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n  - {id: a, weight: 1, ask: y}\n---\n",
    ],
  ])("rejects a malformed rubric: %s", (_label, text) => {
    expect(() => parseRubric(text)).toThrow();
  });
});

// ── assembleJudgeRequest — the isolation boundary ────────────────────────────────────────────────────

describe("verifier-judge — assembleJudgeRequest (transcript isolation)", () => {
  test("carries rubric + brief + diff + checks, temperature 0, and NO writer transcript", () => {
    // The writer's chain-of-thought exists in the run, but the judge input type cannot carry it.
    const TRANSCRIPT = "WRITER_PRIVATE_CHAIN_OF_THOUGHT_9f3";
    const rubric = parseRubric(RUBRIC);
    const payload = assembleJudgeRequest({
      rubric,
      diff: "DIFF_SENTINEL_a1\n+ <meta name=twitter:card>",
      brief: "BRIEF_SENTINEL_b2 add all meta tags",
      checks: [
        { name: "tests", ok: true },
        { name: "lint", ok: false, reason: "fail" },
      ],
    });
    expect(payload.temperature).toBe(0);
    const wire = JSON.stringify(payload);
    expect(wire).toContain("DIFF_SENTINEL_a1");
    expect(wire).toContain("BRIEF_SENTINEL_b2");
    expect(wire).toContain("coverage"); // criterion id
    expect(wire).toContain("tests"); // a check output
    expect(wire).toContain("lint: fail");
    // the crux: the transcript is nowhere in the assembled request
    expect(wire).not.toContain(TRANSCRIPT);
    // and no model id — the proxy resolves it from the role
    expect((payload as unknown as { model?: unknown }).model).toBeUndefined();
  });
});

// ── parseJudgeReport ───────────────────────────────────────────────────────────────────────────────

describe("verifier-judge — parseJudgeReport", () => {
  const rubric = parseRubric(RUBRIC);

  test("parses a clean JSON reply and orders criteria by rubric", () => {
    const raw = '{"criteria":[{"id":"no-regressions","score":2},{"id":"coverage","score":2}]}';
    const r = parseJudgeReport(raw, rubric);
    expect(r.criteria.map((c) => c.id)).toEqual(["coverage", "no-regressions"]); // rubric order
  });

  test("tolerates ```json fences and surrounding prose", () => {
    const raw =
      'Here is my assessment:\n```json\n{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}\n```\nDone.';
    const r = parseJudgeReport(raw, rubric);
    expect(r.criteria).toHaveLength(2);
  });

  test("a stray brace in a preamble does NOT shadow the real report that follows it", () => {
    // extractJsonObjects must skip `{og:image}` (not valid JSON) and pick the real report after it —
    // otherwise a legitimate assessment is spuriously rejected as malformed (round-2 minor).
    const raw =
      'Looking at the {og:image} handling and the if (x) { branch, my scores: {"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}';
    const r = parseJudgeReport(raw, rubric);
    expect(r.criteria).toHaveLength(2);
    expect(r.criteria.every((c) => c.score === 2)).toBe(true);
  });

  test("an invalid criteria-shaped preamble object (e.g. an empty example) does NOT shadow the real report", () => {
    // `{"criteria":[]}` is criteria-SHAPED but not a valid report (missing every rubric criterion); the
    // real report follows. Requiring the ONE fully-valid report picks the real one, not the empty draft.
    const raw =
      'Example format: {"criteria":[]}\nActual: {"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}';
    const r = parseJudgeReport(raw, rubric);
    expect(r.criteria.map((c) => c.score)).toEqual([2, 2]);
  });

  test("TWO distinct valid reports → ambiguous → throws (never silently pick one verdict)", () => {
    // A fail-open guard: a valid all-max draft BEFORE the real below-threshold report must NOT be scored
    // as the verdict. Ambiguity parks blocked rather than guessing.
    const raw =
      '{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}\n{"criteria":[{"id":"coverage","score":0,"note":"nothing"},{"id":"no-regressions","score":2}]}';
    expect(() => parseJudgeReport(raw, rubric)).toThrow(/ambiguous/);
  });

  test("a real report NESTED inside an outer prose-brace span is still surfaced → ambiguous, not silent-pass (P20 R4)", () => {
    // Fail-open the round-4 exactly-one-valid guard assumed away: a standalone valid all-max report,
    // then the REAL below-threshold report nested inside a larger balanced-but-unparseable span
    // (`{reasoning {…}}`). The old extractor jumped past the whole outer span (i = end + 1), dropping the
    // nested real report so ONLY the all-max survived → valid.length === 1 → scored as a silent PASS of
    // failing work. Superset extraction (advance i++ into nested spans) surfaces BOTH reports, so the
    // conflict is caught as ambiguous → verdict error (park blocked), never a guessed pass.
    // Mutation proof: revert `extractJsonObjects` to `i = end + 1` and this test goes RED (no throw).
    const raw =
      'Summary: {"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}. Detail: {reasoning {"criteria":[{"id":"coverage","score":0,"note":"none"},{"id":"no-regressions","score":0,"note":"none"}]}}';
    expect(() => parseJudgeReport(raw, rubric)).toThrow(/ambiguous/);
  });

  test("a single legitimate report with nested criterion sub-objects is NOT spuriously ambiguous", () => {
    // Guards the superset extractor against over-rejection: the criterion sub-objects ({id,score,note})
    // extracted from inside the real report are not report-shaped (no `criteria` array) so they are
    // dropped, leaving exactly one valid report. One valid report in → one report out.
    const raw =
      '{"criteria":[{"id":"coverage","score":1,"note":"missing og:image"},{"id":"no-regressions","score":2}]}';
    const r = parseJudgeReport(raw, rubric);
    expect(r.criteria.map((c) => c.id)).toEqual(["coverage", "no-regressions"]);
  });

  test("does not close the object early on a `}` inside a note string", () => {
    const raw =
      '{"criteria":[{"id":"coverage","score":1,"note":"missing the {og:image} tag}"},{"id":"no-regressions","score":2}]}';
    const r = parseJudgeReport(raw, rubric);
    expect(only(r.criteria).note).toContain("og:image");
  });

  test.each([
    ["no JSON at all", "the diff looks fine to me"],
    ["missing a criterion", '{"criteria":[{"id":"coverage","score":2}]}'],
    [
      "unknown criterion",
      '{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2},{"id":"made-up","score":2}]}',
    ],
    [
      "duplicate criterion",
      '{"criteria":[{"id":"coverage","score":2},{"id":"coverage","score":1,"note":"x"},{"id":"no-regressions","score":2}]}',
    ],
    [
      "score off the scale",
      '{"criteria":[{"id":"coverage","score":3},{"id":"no-regressions","score":2}]}',
    ],
    [
      "below-max score without a note",
      '{"criteria":[{"id":"coverage","score":1},{"id":"no-regressions","score":2}]}',
    ],
  ])("rejects a malformed report: %s", (_label, raw) => {
    expect(() => parseJudgeReport(raw, rubric)).toThrow();
  });
});

// ── weightedScore ─────────────────────────────────────────────────────────────────────────────────

describe("verifier-judge — weightedScore", () => {
  const rubric = parseRubric(RUBRIC); // coverage w2, no-regressions w1, max 2

  test("all-max scores → 1.0", () => {
    const s = weightedScore(
      {
        criteria: [
          { id: "coverage", score: 2 },
          { id: "no-regressions", score: 2 },
        ],
      },
      rubric,
    );
    expect(s).toBeCloseTo(1, 6);
  });

  test("weighted: coverage=2 (w2), no-regressions=0 (w1) → 4/6", () => {
    const s = weightedScore(
      {
        criteria: [
          { id: "coverage", score: 2 },
          { id: "no-regressions", score: 0, note: "x" },
        ],
      },
      rubric,
    );
    expect(s).toBeCloseTo(4 / 6, 6);
  });

  test("MIN-ANCHORED: a 1-based scale [1,2] floors all-min at 0 (not minScale/maxScale) so fail is reachable", () => {
    // Without min-anchoring, scale [1,2] would floor every reply at 1/2 = 0.5, so a threshold-0.5 gate
    // could NEVER fail (the round-2 MAJOR). Min-anchored: all-min → 0, all-max → 1.
    const r = parseRubric(
      "---\nthreshold: 0.5\nscale: [1, 2]\ncriteria:\n  - {id: a, weight: 1, ask: x}\n---\n",
    );
    expect(weightedScore({ criteria: [{ id: "a", score: 1, note: "worst" }] }, r)).toBe(0);
    expect(weightedScore({ criteria: [{ id: "a", score: 2 }] }, r)).toBe(1);
  });

  test("0-based scale is unchanged by min-anchoring (backward compatible)", () => {
    const s = weightedScore(
      {
        criteria: [
          { id: "coverage", score: 1, note: "x" },
          { id: "no-regressions", score: 2 },
        ],
      },
      rubric,
    );
    expect(s).toBeCloseTo((1 * 2 + 2 * 1) / (2 * 3), 6); // 4/6 — identical to the pre-fix formula
  });
});

// ── runJudge (scored via a mock caller) ──────────────────────────────────────────────────────────────

describe("verifier-judge — runJudge", () => {
  test("weighted score ≥ threshold → verdict pass; resolves the pinned verifier model + temp 0", async () => {
    const { call, payloads } = recordingCaller(
      reply('{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}'),
    );
    const r = await runJudge({ rubricText: RUBRIC, diff: "d", brief: "b", checks: CHECKS, call });
    expect(r.verdict).toBe("pass");
    expect(r.score).toBeCloseTo(1, 6);
    expect(r.model).toBe(DEFAULT_MODEL_PINS.verifier);
    expect(only(payloads).temperature).toBe(0);
  });

  test("weighted score below threshold → verdict fail (not error), report retained", async () => {
    // coverage=1 (w2), no-regressions=2 (w1) → 4/6 ≈ 0.667 < 0.8
    const { call } = recordingCaller(
      reply(
        '{"criteria":[{"id":"coverage","score":1,"note":"twitter:card missing"},{"id":"no-regressions","score":2}]}',
      ),
    );
    const r = await runJudge({ rubricText: RUBRIC, diff: "d", brief: "b", checks: CHECKS, call });
    expect(r.verdict).toBe("fail");
    expect(r.score).toBeCloseTo(4 / 6, 6);
    expect(r.report?.criteria.find((c) => c.id === "coverage")?.note).toContain("twitter:card");
  });

  test("a 1-based scale [1,2] can still return FAIL (min-anchoring closes the general fail-open)", async () => {
    // Before min-anchoring, scale [1,2] + threshold 0.5 made fail unreachable (all-min = 0.5 ≥ 0.5).
    const rubric1 =
      "---\nthreshold: 0.5\nscale: [1, 2]\ncriteria:\n  - {id: a, weight: 1, ask: did the work}\n---\n";
    const { call } = recordingCaller(
      reply('{"criteria":[{"id":"a","score":1,"note":"nothing was done"}]}'),
    );
    const r = await runJudge({ rubricText: rubric1, diff: "d", brief: "b", checks: CHECKS, call });
    expect(r.verdict).toBe("fail");
    expect(r.score).toBe(0);
  });

  test("honors the MAESTRO_MODEL_VERIFIER pin override", async () => {
    const { call } = recordingCaller(
      reply('{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}'),
    );
    const r = await runJudge({
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call,
      env: { MAESTRO_MODEL_VERIFIER: "claude-sonnet-5" },
    });
    expect(r.model).toBe("claude-sonnet-5");
  });

  test("a malformed rubric → verdict error (park blocked, never a work fail)", async () => {
    const { call } = recordingCaller(reply("{}"));
    const r = await runJudge({
      rubricText: "not a rubric",
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call,
    });
    expect(r.verdict).toBe("error");
    expect(r.score).toBeNull();
    expect(r.message).toContain("rubric");
  });

  test("a non-2xx proxy response (402 budget) → verdict error, message carried (never burns an attempt)", async () => {
    const call: JudgeCaller = async () => ({
      status: 402,
      body: { error: { code: "budget_exhausted", message: "budget exhausted; run parks blocked" } },
    });
    const r = await runJudge({ rubricText: RUBRIC, diff: "d", brief: "b", checks: CHECKS, call });
    expect(r.verdict).toBe("error");
    expect(r.message).toContain("402");
    expect(r.message).toContain("budget exhausted");
  });

  test("a caller throw → verdict error (never rejects)", async () => {
    const call: JudgeCaller = async () => {
      throw new Error("ECONNREFUSED");
    };
    const r = await runJudge({ rubricText: RUBRIC, diff: "d", brief: "b", checks: CHECKS, call });
    expect(r.verdict).toBe("error");
    expect(r.message).toContain("ECONNREFUSED");
  });

  test("an empty / non-JSON judge reply → verdict error", async () => {
    const { call } = recordingCaller(reply("I think it's fine, no notes."));
    const r = await runJudge({ rubricText: RUBRIC, diff: "d", brief: "b", checks: CHECKS, call });
    expect(r.verdict).toBe("error");
    expect(r.message).toContain("JSON");
  });

  test("a judge.json write failure → verdict error (infra, matches Stage-1 evidence-write contract)", async () => {
    const { call } = recordingCaller(
      reply('{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}'),
    );
    const r = await runJudge({
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call,
      writeJudge: async () => {
        throw new Error("ENOSPC");
      },
    });
    expect(r.verdict).toBe("error");
    expect(r.message).toContain("judge.json write failed");
    expect(r.message).toContain("ENOSPC");
  });

  test("persists judge.json when a writer is provided", async () => {
    const { call } = recordingCaller(
      reply(
        '{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":1,"note":"one tag missing"}]}',
      ),
    );
    const written: Record<string, string> = {};
    const r = await runJudge({
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call,
      writeJudge: async (rel, content) => {
        written[rel] = content;
      },
    });
    expect(r.verdict).toBe("pass"); // 2*2 + 1*1 = 5 / 6 ≈ 0.833 ≥ 0.8
    const doc = JSON.parse(only(Object.values(written)));
    expect(doc.verdict).toBe("pass");
    expect(doc.threshold).toBe(0.8);
    expect(doc.criteria).toHaveLength(2);
    expect(doc.criteria.find((c: { id: string }) => c.id === "coverage").weight).toBe(2);
  });

  test("the judge is handed ONLY rubric/diff/brief/checks — no transcript reaches the recorded payload", async () => {
    const TRANSCRIPT = "SECRET_WRITER_NARRATIVE_zz9";
    const { call, payloads } = recordingCaller(
      reply('{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}'),
    );
    await runJudge({
      rubricText: RUBRIC,
      diff: "DIFFONLY",
      brief: "BRIEFONLY",
      checks: CHECKS,
      call,
    });
    const wire = JSON.stringify(only(payloads));
    expect(wire).toContain("DIFFONLY");
    expect(wire).not.toContain(TRANSCRIPT);
  });
});

// ── renderJudgeJson ────────────────────────────────────────────────────────────────────────────────

describe("verifier-judge — renderJudgeJson", () => {
  test("serializes verdict, score, threshold, model, and weighted criteria", () => {
    const rubric = parseRubric(RUBRIC);
    const json = renderJudgeJson(
      {
        verdict: "fail",
        score: 0.5,
        model: "claude-opus-4-8",
        report: {
          criteria: [
            { id: "coverage", score: 1, note: "missing tag" },
            { id: "no-regressions", score: 2 },
          ],
        },
      },
      rubric,
    );
    const doc = JSON.parse(json);
    expect(doc.verdict).toBe("fail");
    expect(doc.score).toBe(0.5);
    expect(doc.threshold).toBe(0.8);
    expect(doc.criteria).toEqual([
      { id: "coverage", weight: 2, score: 1, note: "missing tag" },
      { id: "no-regressions", weight: 1, score: 2 },
    ]);
  });
});

// ── attachJudge — Stage 2 after Stage 1 ──────────────────────────────────────────────────────────────

describe("verifier-judge — attachJudge (composition)", () => {
  const passStage1: VerifierResult = {
    verdict: "pass",
    tampering: [],
    diffstat: { files: 1, plus: 10, minus: 0 },
    base: "abc123",
    checks: [
      { name: "tests", ok: true, exit: 0, duration_s: 3, log: "checks/tests.log", required: true },
    ],
  };
  const passReply = reply(
    '{"criteria":[{"id":"coverage","score":2},{"id":"no-regressions","score":2}]}',
  );
  const okCall: JudgeCaller = async () => passReply;

  test("a non-pass Stage 1 short-circuits — the judge NEVER runs", async () => {
    let called = false;
    const call: JudgeCaller = async () => {
      called = true;
      return passReply;
    };
    const failStage1: VerifierResult = { ...passStage1, verdict: "fail" };
    const r = await attachJudge(failStage1, {
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call,
    });
    expect(r).toEqual(failStage1);
    expect(called).toBe(false);
  });

  test("no rubric (`judge:` absent) → pass through with judge:{score:null}", async () => {
    const r = await attachJudge(passStage1, {
      rubricText: null,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call: okCall,
    });
    expect(r.verdict).toBe("pass");
    expect(r.judge).toEqual({ score: null });
  });

  test("Stage 1 pass + judge pass → verdict pass, judge receipt attached", async () => {
    const r = await attachJudge(passStage1, {
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call: okCall,
    });
    expect(r.verdict).toBe("pass");
    expect(r.judge?.score).toBeCloseTo(1, 6);
    expect(r.judge?.model).toBe(DEFAULT_MODEL_PINS.verifier);
    expect(r.checks).toBe(passStage1.checks); // Stage 1 result carried through
  });

  test("Stage 1 pass + judge fail → verdict fail (feedback + respawn)", async () => {
    const failCall: JudgeCaller = async () =>
      reply(
        '{"criteria":[{"id":"coverage","score":0,"note":"no meta tags at all"},{"id":"no-regressions","score":2}]}',
      );
    const r = await attachJudge(passStage1, {
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call: failCall,
    });
    expect(r.verdict).toBe("fail");
    expect(r.judge?.score).toBeCloseTo(2 / 6, 6);
  });

  test("a judge error propagates as verdict error (park blocked)", async () => {
    const errCall: JudgeCaller = async () => ({
      status: 502,
      body: { error: { message: "upstream down" } },
    });
    const r = await attachJudge(passStage1, {
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call: errCall,
    });
    expect(r.verdict).toBe("error");
    expect(r.message).toContain("502");
    expect(r.judge?.score).toBeNull();
  });

  test("detail path is set only when judge.json is persisted", async () => {
    const written: Record<string, string> = {};
    const r = await attachJudge(passStage1, {
      rubricText: RUBRIC,
      diff: "d",
      brief: "b",
      checks: CHECKS,
      call: okCall,
      writeJudge: async (rel, content) => {
        written[rel] = content;
      },
    });
    expect(r.judge?.detail).toBe("judge.json");
    expect(written["judge.json"]).toBeDefined();
  });
});

// ── wire path: real proxy + mock upstream + minted VERIFIER bearer ───────────────────────────────────

describe("verifier-judge — through the real model proxy (mock upstream)", () => {
  let handle: IndexHandle;
  beforeEach(async () => {
    handle = await openIndex(indexUrl(":memory:"));
  });
  afterEach(() => {
    handle.client.close();
  });

  test("judge dials the proxy with a verifier bearer; proxy resolves judge_model + meters; score is deterministic", async () => {
    const sink = new MemoryEventSink();
    const guard = new BudgetGuard(handle.db, sink, {
      now: () => 1_700_000_000_000,
      reserveUsd: 0.5,
    });
    await guard.open("judge-run");

    const tokens = new SessionTokenRegistry();
    const ctx: SessionContext = {
      session: "judge-run",
      runDir: "/runs/judge-run",
      role: "verifier",
      budget: { per_run_usd: 100, max_iterations: 1 },
    };
    const bearer = tokens.mint(ctx);

    const judgeReply = JSON.stringify({
      criteria: [
        { id: "coverage", score: 2 },
        { id: "no-regressions", score: 2 },
      ],
    });
    const mock = createMockModel({
      script: [
        {
          body: {
            id: "m",
            type: "message",
            role: "assistant",
            content: [{ type: "text", text: judgeReply }],
            stop_reason: "end_turn",
          },
          usage: { usd: 0.2 },
        },
      ],
    });

    const app = createModelProxy({
      guard,
      tokens,
      upstream: mock,
      apiKey: () => "sk-runtime-secret-never-leaves",
    });
    const server = serveProxy(app, { port: 0 });
    try {
      const r = await runJudge({
        rubricText: RUBRIC,
        diff: "+ <meta name=twitter:card content=summary>",
        brief: "add all meta tags",
        checks: CHECKS,
        call: proxyJudgeCaller({ proxyUrl: server.url, bearer }),
      });
      expect(r.verdict).toBe("pass");
      expect(r.score).toBeCloseTo(1, 6);
      // the proxy resolved the pinned judge model from the VERIFIER role — the judge never named it
      expect(only(mock.calls).model).toBe(resolvePinnedModel("verifier"));
      expect(only(mock.calls).role).toBe("verifier");
      // the request the child sent carried temp 0 and no model id
      const sent = only(mock.calls).payload as { temperature?: number; model?: unknown };
      expect(sent.temperature).toBe(0);
      expect(sent.model).toBeUndefined();
      // budget was metered (the judge call cost was booked)
      expect(guard.dayTotalUsd).toBeCloseTo(0.2, 6);
    } finally {
      server.stop();
    }
  });

  test("a revoked bearer → proxy 401 → verdict error (never a work fail)", async () => {
    const guard = new BudgetGuard(handle.db, new MemoryEventSink(), {
      now: () => 1_700_000_000_000,
    });
    await guard.open("judge-run");
    const tokens = new SessionTokenRegistry();
    const mock = createMockModel();
    const app = createModelProxy({ guard, tokens, upstream: mock, apiKey: () => "sk" });
    const server = serveProxy(app, { port: 0 });
    try {
      const r = await runJudge({
        rubricText: RUBRIC,
        diff: "d",
        brief: "b",
        checks: CHECKS,
        call: proxyJudgeCaller({ proxyUrl: server.url, bearer: "never-minted" }),
      });
      expect(r.verdict).toBe("error");
      expect(r.message).toContain("401");
      expect(mock.calls).toHaveLength(0); // never forwarded
    } finally {
      server.stop();
    }
  });
});
