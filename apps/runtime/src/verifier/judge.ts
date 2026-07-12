// Verifier Stage 2 — the LLM judge (VERIFIER §2 Stage 2, §3). Runs ONLY after a Stage 0 pass AND a
// green Stage 1, and ONLY when the contract carries a `judge:` rubric. The judge grades the WORK, not
// the narrative: its input is the rubric + the diff + the brief (the `_work.md` body) + the
// deterministic check outputs — NEVER the writer's chat transcript (`session.jsonl`). That isolation
// is the whole point (writer≠judge): a judge that could read the writer's reasoning would grade the
// story it was told, not the diff on disk. The isolation is enforced BY TYPE here — {@link JudgeInput}
// has no transcript field, so the assembled prompt structurally cannot carry one.
//
// The model call goes through the same metered model proxy every child uses (HARNESS §3): the judge
// authenticates with a per-session VERIFIER bearer, the proxy resolves the pinned `judge_model` from
// the role (the judge never names a model id) and attaches the runtime key at forward time — so the
// judge never holds the Anthropic key. Temperature 0 for a deterministic score.
//
// Pass = weighted score ≥ the rubric threshold. A malformed rubric, a non-2xx proxy response (e.g. a
// 402 budget refusal), an empty/unparseable judge reply, or an evidence-write failure is an INFRA
// `error` — the run parks `blocked`, an attempt is NEVER burned on a broken harness (VERIFIER §2). The
// judge never turns a real work `fail` into a silent pass, and never fails the work for its own
// misbehaviour.

import { parse as parseYaml } from "yaml";
import type { ChildRole } from "../harness/spawn-contract";
import { resolvePinnedModel } from "../proxy/models";
import type { VerifierResult } from "./verifier";

const VERIFIER_ROLE: ChildRole = "verifier";

/** Default output ceiling for a judge reply — a per-criterion score + one sentence each is small. */
export const DEFAULT_JUDGE_MAX_TOKENS = 2048;

/** One rubric criterion (VERIFIER §3). `weight` scales its contribution; `ask` is the yes/no-ish test. */
export interface RubricCriterion {
  /** Stable id — Loop 4 tracks these over time. */
  id: string;
  /** Relative weight in the final score (> 0). */
  weight: number;
  /** The plain-language criterion the judge scores. */
  ask: string;
}

/** A parsed `rubric.md` (VERIFIER §3): frontmatter (threshold/scale/criteria) + the body instructions. */
export interface Rubric {
  /** Weighted pass line, 0–1. */
  threshold: number;
  /** Per-criterion scoring scale, ascending; the last element is the max. */
  scale: number[];
  criteria: RubricCriterion[];
  /** The rubric body — the judge's plain-language instructions (may be empty). */
  instructions: string;
}

/** A judge/contract error that maps to verdict `error` (park blocked) — never a work `fail`. */
export type JudgeErrorCode = "malformed_rubric" | "malformed_report" | "model_error";
export class JudgeError extends Error {
  code: JudgeErrorCode;
  constructor(code: JudgeErrorCode, message: string) {
    super(message);
    this.name = "JudgeError";
    this.code = code;
  }
}

/** A summary of one Stage-1 check the judge is told about (the check OUTPUTS, never the transcript). */
export interface JudgeCheckSummary {
  name: string;
  ok: boolean;
  /** Why the check is not ok, when applicable. */
  reason?: "fail" | "timeout";
}

/** Everything the judge is allowed to see. NOTE: there is deliberately NO transcript field — the
 *  writer's chat is not an input (VERIFIER §2 Stage 2). The isolation invariant is enforced by type. */
export interface JudgeInput {
  rubric: Rubric;
  /** The unified diff of the run (`git diff <base>..run/<id>`). */
  diff: string;
  /** The brief — the `_work.md` body the work was scoped from. */
  brief: string;
  /** The deterministic Stage-1 check outcomes. */
  checks: JudgeCheckSummary[];
}

/** The Anthropic Messages request the judge forwards. No `model` — the proxy resolves it from the
 *  VERIFIER role (the judge never names a model). `temperature: 0` for a deterministic score. */
export interface JudgeRequestPayload {
  max_tokens: number;
  temperature: 0;
  system: string;
  messages: { role: "user"; content: string }[];
}

/** One criterion's judged score (the shape the judge is prompted to emit, per criterion). */
export interface JudgeCriterionScore {
  id: string;
  score: number;
  /** A specific, actionable sentence — required when the score is below the scale max. */
  note?: string;
}

/** The parsed judge reply — one score per rubric criterion. */
export interface JudgeReport {
  criteria: JudgeCriterionScore[];
}

/** The judge stage outcome. `error` parks blocked; `pass`/`fail` are the weighted-score verdict. */
export interface JudgeResult {
  verdict: "pass" | "fail" | "error";
  /** The weighted score 0–1, or null on `error`. */
  score: number | null;
  /** The pinned judge model that scored (or would have scored) the work. */
  model: string;
  /** The per-criterion report, present on `pass`/`fail`. */
  report?: JudgeReport;
  /** Set on `error` — the infra failure that stopped the judge. */
  message?: string;
}

/** How the judge calls the model. The default posts to the proxy; injected as a mock in tests. Returns
 *  the proxy's `{status, body}` verbatim — a non-2xx is the judge's `model_error` signal. */
export type JudgeCaller = (
  payload: JudgeRequestPayload,
) => Promise<{ status: number; body: unknown }>;

// ── rubric parsing ────────────────────────────────────────────────────────────────────────────────

/** Split a `---`-fenced frontmatter document into its YAML head and body. Throws `malformed_rubric` if
 *  the file does not open with a frontmatter fence. */
function splitFrontmatter(text: string): { data: string; body: string } {
  const m = /^---\r?\n([\s\S]*?)\r?\n---[ \t]*\r?\n?([\s\S]*)$/.exec(text);
  if (m === null) {
    throw new JudgeError("malformed_rubric", "rubric.md must open with a --- frontmatter fence");
  }
  return { data: m[1] ?? "", body: m[2] ?? "" };
}

function isFiniteNumber(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

/** Parse + validate a `rubric.md` (VERIFIER §3). Every violation is a `malformed_rubric` JudgeError —
 *  a broken rubric is the contract author's bug, so the run parks blocked rather than failing the work. */
export function parseRubric(text: string): Rubric {
  const { data, body } = splitFrontmatter(text);
  let doc: unknown;
  try {
    doc = parseYaml(data);
  } catch (e) {
    throw new JudgeError("malformed_rubric", `rubric frontmatter is not valid YAML: ${msg(e)}`);
  }
  if (doc === null || typeof doc !== "object" || Array.isArray(doc)) {
    throw new JudgeError("malformed_rubric", "rubric frontmatter must be a mapping");
  }
  const obj = doc as Record<string, unknown>;

  if (!isFiniteNumber(obj.threshold) || obj.threshold < 0 || obj.threshold > 1) {
    throw new JudgeError("malformed_rubric", "threshold must be a number in [0, 1]");
  }
  const threshold = obj.threshold;

  // A scale needs ≥2 non-negative, strictly-ascending points. Length ≥ 2 keeps a below-max score
  // REACHABLE, so the judge can actually return `fail` — a single-element scale (e.g. `[1]`, a
  // fat-finger for "0-1") would pin every well-formed reply at the max, forcing weightedScore to a
  // constant 1.0 = an always-pass FAIL-OPEN. Non-negative + ascending keeps weightedScore in [0, 1]
  // (each score ∈ [0, maxScale], so achieved ∈ [0, possible]). A malformed scale parks blocked, never
  // becomes a silent auto-pass (VERIFIER §2).
  if (
    !Array.isArray(obj.scale) ||
    obj.scale.length < 2 ||
    !obj.scale.every((n) => isFiniteNumber(n) && n >= 0) ||
    !isStrictlyAscending(obj.scale)
  ) {
    throw new JudgeError(
      "malformed_rubric",
      "scale must be a strictly ascending array of at least 2 non-negative numbers",
    );
  }
  const scale = obj.scale as number[];

  if (!Array.isArray(obj.criteria) || obj.criteria.length === 0) {
    throw new JudgeError("malformed_rubric", "criteria must be a non-empty array");
  }
  const seen = new Set<string>();
  const criteria: RubricCriterion[] = obj.criteria.map((raw, i) => {
    if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
      throw new JudgeError("malformed_rubric", `criteria[${i}] must be a mapping`);
    }
    const c = raw as Record<string, unknown>;
    const id = typeof c.id === "string" ? c.id.trim() : "";
    if (id === "") throw new JudgeError("malformed_rubric", `criteria[${i}].id is required`);
    if (seen.has(id)) throw new JudgeError("malformed_rubric", `duplicate criterion id "${id}"`);
    seen.add(id);
    if (!isFiniteNumber(c.weight) || c.weight <= 0) {
      throw new JudgeError("malformed_rubric", `criteria[${i}].weight must be a number > 0`);
    }
    const ask = typeof c.ask === "string" ? c.ask.trim() : "";
    if (ask === "") throw new JudgeError("malformed_rubric", `criteria[${i}].ask is required`);
    return { id, weight: c.weight, ask };
  });

  return { threshold, scale, criteria, instructions: body.trim() };
}

function isStrictlyAscending(xs: number[]): boolean {
  for (let i = 1; i < xs.length; i++) {
    const prev = xs[i - 1];
    const cur = xs[i];
    if (prev === undefined || cur === undefined || cur <= prev) return false;
  }
  return true;
}

// ── prompt assembly (the isolation boundary) ────────────────────────────────────────────────────────

/** The instruction the judge is held to — score every criterion on the scale, emit STRICT JSON only. */
function systemPrompt(rubric: Rubric): string {
  const max = maxScale(rubric);
  return [
    "You are a code-change verifier. Grade the WORK (the diff), never a narrative.",
    "You are given a rubric, the brief the work was scoped from, the unified diff, and the",
    "deterministic check outputs. You are NOT given the author's chat — do not ask for it.",
    "",
    `Score each criterion on the integer scale [${rubric.scale.join(", ")}] (max ${max}).`,
    "Return ONLY a JSON object, no prose, no markdown fences, shaped exactly:",
    '{"criteria":[{"id":"<criterion id>","score":<integer from the scale>,"note":"<one specific,',
    'actionable sentence; REQUIRED whenever score is below the max>"}]}',
    "Include every criterion exactly once and no others.",
  ].join("\n");
}

/** Render the check outcomes the judge sees — names + pass/fail only (the OUTPUTS, not the transcript). */
function renderChecks(checks: JudgeCheckSummary[]): string {
  if (checks.length === 0) return "(no deterministic checks)";
  return checks
    .map((c) => `- ${c.name}: ${c.ok ? "pass" : `fail (${c.reason ?? "fail"})`}`)
    .join("\n");
}

/**
 * Assemble the judge's model request from ONLY the isolated inputs (VERIFIER §2 Stage 2). There is no
 * transcript parameter — the writer's chat can never reach the judge through this function. Temperature
 * is pinned to 0 for a deterministic score.
 */
export function assembleJudgeRequest(
  input: JudgeInput,
  maxTokens: number = DEFAULT_JUDGE_MAX_TOKENS,
): JudgeRequestPayload {
  const { rubric, diff, brief, checks } = input;
  const criteriaBlock = rubric.criteria
    .map((c) => `- ${c.id} (weight ${c.weight}): ${c.ask}`)
    .join("\n");
  const content = [
    "# Rubric",
    rubric.instructions || "(no extra instructions)",
    "",
    "## Criteria",
    criteriaBlock,
    "",
    "# Brief",
    brief.trim() || "(no brief provided)",
    "",
    "# Check outputs",
    renderChecks(checks),
    "",
    "# Diff",
    "```diff",
    diff,
    "```",
  ].join("\n");
  return {
    max_tokens: maxTokens,
    temperature: 0,
    system: systemPrompt(rubric),
    messages: [{ role: "user", content }],
  };
}

// ── reply parsing + scoring ──────────────────────────────────────────────────────────────────────────

/** The scale max — the score that earns full credit for a criterion. */
function maxScale(rubric: Rubric): number {
  return rubric.scale[rubric.scale.length - 1] as number; // scale is validated non-empty
}

/** Pull the assistant text out of an Anthropic Messages response body (`content: [{type:"text",…}]`). */
export function extractText(body: unknown): string {
  if (body === null || typeof body !== "object") return "";
  const content = (body as { content?: unknown }).content;
  if (!Array.isArray(content)) return "";
  return content
    .filter(
      (b): b is { type: string; text: string } =>
        b !== null &&
        typeof b === "object" &&
        (b as { type?: unknown }).type === "text" &&
        typeof (b as { text?: unknown }).text === "string",
    )
    .map((b) => b.text)
    .join("");
}

/** Extract EVERY balanced JSON object from model text at EVERY depth — the real object may be wrapped in
 *  ``` fences or prose (before AND after) and may be NESTED inside a larger balanced-but-unparseable span
 *  (e.g. `Detail: {reasoning {"criteria":[…]}}`). Bracket-depth scan (string/escape-aware) so a `}` inside
 *  a string does not close an object early. After recording a balanced span we advance by a single char
 *  (`i++`, not past its end) so any object nested inside it is ALSO surfaced as its own candidate. That
 *  keeps this strictly a *superset* extractor: junk candidates (brace-in-string, non-JSON prose spans,
 *  criterion sub-objects) fail JSON.parse / the report-shape check / rubric validation downstream and are
 *  dropped, while the caller's exactly-one-valid guard sees every real report — so a report hidden inside
 *  an outer prose-brace span can never be silently dropped to let a preceding candidate shadow it
 *  (fail-open → the reply resolves to an ambiguous-report `error`, i.e. park blocked, per VERIFIER §2).
 *  Replies are small, so the O(n²) worst case from re-scanning nested spans is bounded. */
function extractJsonObjects(text: string): string[] {
  const objs: string[] = [];
  let i = 0;
  while (i < text.length) {
    if (text[i] !== "{") {
      i++;
      continue;
    }
    let depth = 0;
    let inString = false;
    let escaped = false;
    let end = -1;
    for (let j = i; j < text.length; j++) {
      const ch = text[j];
      if (inString) {
        if (escaped) escaped = false;
        else if (ch === "\\") escaped = true;
        else if (ch === '"') inString = false;
        continue;
      }
      if (ch === '"') inString = true;
      else if (ch === "{") depth++;
      else if (ch === "}") {
        depth--;
        if (depth === 0) {
          end = j;
          break;
        }
      }
    }
    if (end === -1) {
      // An unbalanced open — a false start (e.g. a bare `if (x) {` in prose). Skip just this `{` and
      // keep looking: a real balanced object can still follow it. (Bounded work — replies are small.)
      i++;
      continue;
    }
    objs.push(text.slice(i, end + 1));
    // Advance by one char (NOT past `end`) so an object nested inside this balanced span — a real report
    // wrapped in an outer prose-brace span like `{reasoning {…}}` — is still surfaced as its own
    // candidate. Superset extraction; downstream parse/shape/rubric validation drops the junk.
    i++;
  }
  return objs;
}

/**
 * Parse + validate the judge reply against the rubric. Every criterion must appear exactly once with a
 * score drawn from the scale; a below-max score must carry a non-empty note; no unknown criteria are
 * allowed (a hallucinated id is a `malformed_report`). Any violation throws — the judge does not get to
 * fail the work with a malformed verdict.
 */
/** Validate one raw `criteria` array against the rubric → a rubric-ordered {@link JudgeReport}. Throws a
 *  `malformed_report` JudgeError on any violation (a non-object criterion, an unknown/duplicate/missing
 *  criterion, a score off the scale, or a below-max score with no note). */
function buildReport(rawCriteria: unknown[], rubric: Rubric): JudgeReport {
  const allowed = new Set(rubric.criteria.map((c) => c.id));
  const max = maxScale(rubric);
  const byId = new Map<string, JudgeCriterionScore>();
  for (const raw of rawCriteria) {
    if (raw === null || typeof raw !== "object") {
      throw new JudgeError("malformed_report", "each criterion must be an object");
    }
    const c = raw as Record<string, unknown>;
    const id = typeof c.id === "string" ? c.id : "";
    if (!allowed.has(id)) {
      throw new JudgeError("malformed_report", `judge scored unknown criterion "${id}"`);
    }
    if (byId.has(id)) {
      throw new JudgeError("malformed_report", `judge scored "${id}" more than once`);
    }
    if (!isFiniteNumber(c.score) || !rubric.scale.includes(c.score)) {
      throw new JudgeError(
        "malformed_report",
        `criterion "${id}" score must be one of the scale [${rubric.scale.join(", ")}]`,
      );
    }
    const note = typeof c.note === "string" ? c.note.trim() : "";
    if (c.score < max && note === "") {
      throw new JudgeError("malformed_report", `criterion "${id}" scored below max needs a note`);
    }
    byId.set(id, note === "" ? { id, score: c.score } : { id, score: c.score, note });
  }

  const missing = rubric.criteria.filter((c) => !byId.has(c.id)).map((c) => c.id);
  if (missing.length > 0) {
    throw new JudgeError("malformed_report", `judge omitted criteria: ${missing.join(", ")}`);
  }
  // Emit in rubric order (stable receipts), not reply order.
  return { criteria: rubric.criteria.map((c) => byId.get(c.id) as JudgeCriterionScore) };
}

export function parseJudgeReport(text: string, rubric: Rubric): JudgeReport {
  // Collect every balanced object that PARSES and FULLY VALIDATES against the rubric, then require
  // EXACTLY ONE. A judge is prompted to return exactly one JSON object; requiring one *valid* report
  // (not merely the first criteria-SHAPED object) means a stray criteria-shaped object in a preamble
  // (an example, an empty draft) cannot shadow the real report, AND two conflicting valid reports can
  // never silently resolve to one — an ambiguous reply parks blocked (VERIFIER §2: never guess the
  // verdict). 0 valid → the specific validation reason when a candidate was criteria-shaped but wrong,
  // else "no report"; ≥2 valid → ambiguous.
  const valid: JudgeReport[] = [];
  let lastError: JudgeError | null = null;
  for (const candidate of extractJsonObjects(text)) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(candidate);
    } catch {
      continue; // a false-start brace in prose (e.g. `if (x) {`) — try the next balanced candidate
    }
    if (
      parsed === null ||
      typeof parsed !== "object" ||
      !Array.isArray((parsed as { criteria?: unknown }).criteria)
    ) {
      continue; // not a report shape
    }
    try {
      valid.push(buildReport((parsed as { criteria: unknown[] }).criteria, rubric));
    } catch (e) {
      if (e instanceof JudgeError)
        lastError = e; // criteria-shaped but invalid — remember why
      else throw e;
    }
  }

  if (valid.length === 1) return valid[0] as JudgeReport;
  if (valid.length > 1) {
    throw new JudgeError(
      "malformed_report",
      `judge reply carried ${valid.length} distinct valid reports — ambiguous, cannot pick a verdict`,
    );
  }
  if (lastError !== null) throw lastError;
  throw new JudgeError(
    "malformed_report",
    "no JSON object with a `criteria` array in the judge reply",
  );
}

/**
 * Weighted score 0–1, MIN-ANCHORED: Σ((score−minScale)·weight) / ((maxScale−minScale)·Σweight).
 * Anchoring to the scale MINIMUM (not just dividing by the max) is what makes `fail` reachable for a
 * non-zero-based scale: a Likert `scale: [1, 2]` would otherwise floor every reply at minScale/maxScale
 * (= 0.5 here), so a below-threshold verdict could be unreachable — a silent fail-open. All-min scores
 * → 0, all-max → 1, for ANY strictly-ascending scale. Backward-compatible with a 0-based scale
 * ([0,1,2] → minScale 0 → the plain achieved/possible). Pass = score ≥ threshold. Assumes `report` was
 * validated against `rubric` (every criterion present, score in scale). The span (maxScale−minScale) is
 * > 0 because `parseRubric` requires a strictly-ascending scale of length ≥ 2.
 */
export function weightedScore(report: JudgeReport, rubric: Rubric): number {
  const max = maxScale(rubric);
  const min = rubric.scale[0] as number; // validated non-empty + ascending → element 0 is the minimum
  const span = max - min;
  const weightOf = new Map(rubric.criteria.map((c) => [c.id, c.weight]));
  let achieved = 0;
  let possible = 0;
  for (const c of report.criteria) {
    const w = weightOf.get(c.id) ?? 0;
    achieved += (c.score - min) * w;
    possible += span * w;
  }
  return possible === 0 ? 0 : achieved / possible;
}

// ── judge.json receipt ────────────────────────────────────────────────────────────────────────────────

/** The `judge.json` receipt (VERIFIER §2, referenced by verdict.md §4). Serialized deterministically. */
export function renderJudgeJson(result: JudgeResult, rubric: Rubric): string {
  const weightOf = new Map(rubric.criteria.map((c) => [c.id, c.weight]));
  return `${JSON.stringify(
    {
      verdict: result.verdict,
      score: result.score,
      threshold: rubric.threshold,
      model: result.model,
      criteria: (result.report?.criteria ?? []).map((c) => ({
        id: c.id,
        weight: weightOf.get(c.id) ?? 0,
        score: c.score,
        ...(c.note !== undefined ? { note: c.note } : {}),
      })),
    },
    null,
    2,
  )}\n`;
}

// ── the stage ────────────────────────────────────────────────────────────────────────────────────────

export interface RunJudgeDeps {
  /** The raw `rubric.md` contents (read from the worktree by the caller). */
  rubricText: string;
  /** The unified diff of the run. */
  diff: string;
  /** The brief (`_work.md` body). */
  brief: string;
  /** The Stage-1 check outcomes. */
  checks: JudgeCheckSummary[];
  /** How to call the model (default posts to the proxy; a mock in tests). */
  call: JudgeCaller;
  /** Env for the model pin (`MAESTRO_MODEL_VERIFIER` override); defaults to `process.env`. */
  env?: Record<string, string | undefined>;
  /** Output ceiling for the judge reply (default {@link DEFAULT_JUDGE_MAX_TOKENS}). */
  maxTokens?: number;
  /** Persist the `judge.json` receipt (default: none — receipt assembly is the caller's, BRO-1794). A
   *  write failure maps to verdict `error` (park blocked), matching the Stage-1 evidence-write contract. */
  writeJudge?: (relPath: string, content: string) => Promise<void>;
}

/**
 * Run the LLM judge (Stage 2). Resolves the pinned VERIFIER model, assembles the isolated request
 * (rubric + diff + brief + checks, NO transcript), calls the model through the proxy at temperature 0,
 * parses + weights the reply. Never throws: a malformed rubric, a non-2xx proxy response, an empty or
 * unparseable reply, or an evidence-write failure all resolve to verdict `error` (park blocked, never
 * burn an attempt). `pass` iff the weighted score ≥ the rubric threshold.
 */
export async function runJudge(deps: RunJudgeDeps): Promise<JudgeResult> {
  const model = resolvePinnedModel(VERIFIER_ROLE, deps.env);

  let rubric: Rubric;
  try {
    rubric = parseRubric(deps.rubricText);
  } catch (e) {
    return { verdict: "error", score: null, model, message: msg(e) };
  }

  const payload = assembleJudgeRequest(
    { rubric, diff: deps.diff, brief: deps.brief, checks: deps.checks },
    deps.maxTokens,
  );

  let res: { status: number; body: unknown };
  try {
    res = await deps.call(payload);
  } catch (e) {
    return { verdict: "error", score: null, model, message: `judge model call failed: ${msg(e)}` };
  }
  if (res.status < 200 || res.status >= 300) {
    return {
      verdict: "error",
      score: null,
      model,
      message: `judge model returned ${res.status}: ${errorText(res.body)}`,
    };
  }

  const text = extractText(res.body);
  if (text.trim() === "") {
    return { verdict: "error", score: null, model, message: "judge reply had no text content" };
  }

  let report: JudgeReport;
  try {
    report = parseJudgeReport(text, rubric);
  } catch (e) {
    return { verdict: "error", score: null, model, message: msg(e) };
  }

  const score = weightedScore(report, rubric);
  const verdict: JudgeResult["verdict"] = score >= rubric.threshold ? "pass" : "fail";
  const result: JudgeResult = { verdict, score, model, report };

  if (deps.writeJudge !== undefined) {
    try {
      await deps.writeJudge("judge.json", renderJudgeJson(result, rubric));
    } catch (e) {
      return {
        verdict: "error",
        score: null,
        model,
        message: `judge.json write failed: ${msg(e)}`,
      };
    }
  }
  return result;
}

// ── composition: Stage 2 after Stage 1 ────────────────────────────────────────────────────────────────

export interface AttachJudgeDeps extends Omit<RunJudgeDeps, "rubricText"> {
  /** The `rubric.md` contents, or null when the contract has no `judge:` (Stage 2 is skipped). */
  rubricText: string | null;
}

/**
 * Compose Stage 2 onto a Stage-1 {@link VerifierResult} (VERIFIER §2 Stage 3 hand-off is BRO-1794; this
 * is the pure composition it will use). Short-circuits exactly like the pipeline:
 *   - Stage 1 did not pass (fail/error) → return it verbatim, the judge NEVER runs.
 *   - No rubric (`judge:` absent) → pass through with `judge: { score: null }` (VERIFIER §4).
 *   - Otherwise run the judge; a judge `error` propagates as verdict `error`, a judge `fail` fails the
 *     verification (feedback + respawn), a judge `pass` leaves Stage 1's pass intact. The `judge`
 *     receipt (score/model/detail) is attached in every non-short-circuit case.
 */
export async function attachJudge(
  stage1: VerifierResult,
  deps: AttachJudgeDeps,
): Promise<VerifierResult> {
  if (stage1.verdict !== "pass") return stage1;
  if (deps.rubricText === null) {
    return { ...stage1, judge: { score: null } };
  }
  const { rubricText, ...rest } = deps;
  const jr = await runJudge({ ...rest, rubricText });
  if (jr.verdict === "error") {
    return {
      ...stage1,
      verdict: "error",
      message: jr.message,
      judge: { score: null, model: jr.model },
    };
  }
  return {
    ...stage1,
    verdict: jr.verdict, // "pass" stays pass; "fail" fails the verification
    judge: {
      score: jr.score,
      model: jr.model,
      ...(deps.writeJudge !== undefined ? { detail: "judge.json" } : {}),
    },
  };
}

// ── the default proxy caller ──────────────────────────────────────────────────────────────────────────

export interface ProxyCallerConfig {
  /** `BROOMVA_MODEL_PROXY` — the supervisor-owned metered proxy base URL. */
  proxyUrl: string;
  /** The per-session VERIFIER bearer minted for this judge run. */
  bearer: string;
  /** Injected fetch (tests); defaults to the global, bound so a browser/global `this` check can't throw. */
  fetchImpl?: typeof fetch;
}

/**
 * The production {@link JudgeCaller}: POST the request to the proxy's `/v1/messages` with the VERIFIER
 * bearer. The proxy resolves the pinned `judge_model` from the role, reserves + meters budget, and
 * attaches the runtime key — the judge never holds it. A network throw surfaces as `runJudge`'s
 * `model_error`; a non-2xx body is passed back for the verdict-`error` mapping.
 */
export function proxyJudgeCaller(cfg: ProxyCallerConfig): JudgeCaller {
  const doFetch = cfg.fetchImpl ?? globalThis.fetch.bind(globalThis);
  return async (payload) => {
    const res = await doFetch(`${cfg.proxyUrl}/v1/messages`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${cfg.bearer}` },
      body: JSON.stringify(payload),
    });
    const body = await res.json().catch(() => ({}));
    return { status: res.status, body };
  };
}

// ── helpers ────────────────────────────────────────────────────────────────────────────────────────────

function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
}

/** A short human string from a proxy error body (`{error:{message|code|type}}`), for the verdict message. */
function errorText(body: unknown): string {
  const e = (body as { error?: unknown } | null)?.error;
  if (e !== null && typeof e === "object") {
    const o = e as { message?: unknown; code?: unknown; type?: unknown };
    const m = o.message ?? o.code ?? o.type;
    if (typeof m === "string") return m;
  }
  return "upstream error";
}
