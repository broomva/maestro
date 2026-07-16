// Work-contract frontmatter parser + serializer (`_work.md`), DATA-MODEL §A.2 +
// FLOWS §F1 step 2.
//
// The `_work.md` frontmatter IS the orchestration contract (WorkContract, work.ts).
// This module is the read/write seam: parse a file to a validated, typed contract
// (typed errors on every malformation), serialize a contract back to canonical
// frontmatter, and resolve a child's defaults from its parent (folder depth is
// meaning — a child inherits owner/gate/budget unless it overrides them).
//
// YAML is parsed via the `yaml` package; its Document API lets `reserializeWorkFile`
// round-trip a source file preserving comments + key order. Runtime-side code
// (the scanner, BRO-1800) reads files through here; the client never parses files
// (it reads WorkItem over the API), so `yaml` tree-shakes out of the client bundle.

import { parseDocument, parse as parseYaml, stringify as stringifyYaml } from "yaml";
import { KINDS, TRIGGER_KINDS, type Trigger } from "./intents";
import { GATE_MODES, type GateMode, ORCH_STATES } from "./state";
import {
  type Budget,
  type Done,
  type DoneCheck,
  hasCheck,
  isValidRubricRef,
  MAX_CHECK_TIMEOUT_S,
  STOP_CONDITIONS,
  type StopCondition,
  type WorkContract,
} from "./work";

// ── Typed errors ─────────────────────────────────────────────────────────────

export type WorkContractErrorCode =
  | "no_frontmatter"
  | "invalid_yaml"
  | "missing_field"
  | "invalid_type"
  | "invalid_enum"
  | "malformed_done"
  | "malformed_rubric"
  | "gate_auto_no_check";

/** Every parse/validation failure is this one class, discriminated by `code`. */
export class WorkContractError extends Error {
  readonly code: WorkContractErrorCode;
  readonly field: string | undefined;
  constructor(code: WorkContractErrorCode, message: string, field?: string) {
    super(message);
    this.name = "WorkContractError";
    this.code = code;
    this.field = field;
  }
}

// ── Shapes ───────────────────────────────────────────────────────────────────

/**
 * The frontmatter as authored: the inheritance-eligible fields (owner/gate/budget)
 * may be absent, to be filled from the parent (F1 step 2) before materializing to a
 * full WorkContract. Distinct from WorkContract, whose `gate` is always resolved.
 */
export interface WorkContractInput {
  id: string;
  kind: WorkContract["kind"];
  state: WorkContract["state"];
  owner?: string;
  gate?: GateMode;
  budget?: Budget;
  done?: Done;
  trigger?: Trigger;
  created: string;
  updated: string;
}

/** A parsed `_work.md`: the materialized contract plus its markdown brief. */
export interface WorkFile {
  contract: WorkContract;
  /** Markdown body after the frontmatter — the brief (the "look" the gate shows). */
  brief: string;
}

/** A parsed `_work.md` kept at the input layer (for parent-defaults resolution). */
export interface ParsedInput {
  input: WorkContractInput;
  brief: string;
}

// ── Small validators (each narrows a type or throws a typed error) ───────────

function isMapping(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function requireString(obj: Record<string, unknown>, key: string): string {
  const v = obj[key];
  if (v === undefined || v === null) {
    throw new WorkContractError("missing_field", `missing required field: ${key}`, key);
  }
  if (typeof v !== "string") {
    throw new WorkContractError("invalid_type", `field ${key} must be a string`, key);
  }
  return v;
}

function optionalString(obj: Record<string, unknown>, key: string): string | undefined {
  const v = obj[key];
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "string") {
    throw new WorkContractError("invalid_type", `field ${key} must be a string`, key);
  }
  return v;
}

function requireEnum<T extends string>(value: unknown, allowed: readonly T[], key: string): T {
  if (value === undefined || value === null) {
    throw new WorkContractError("missing_field", `missing required field: ${key}`, key);
  }
  if (typeof value !== "string" || !(allowed as readonly string[]).includes(value)) {
    throw new WorkContractError(
      "invalid_enum",
      `field ${key} must be one of ${allowed.join(" | ")}, got ${JSON.stringify(value)}`,
      key,
    );
  }
  return value as T;
}

function optionalNumber(
  obj: Record<string, unknown>,
  key: string,
  ctx: string,
): number | undefined {
  const v = obj[key];
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "number" || !Number.isFinite(v)) {
    throw new WorkContractError(
      "invalid_type",
      `${ctx}.${key} must be a finite number`,
      `${ctx}.${key}`,
    );
  }
  return v;
}

function requireDateString(obj: Record<string, unknown>, key: string): string {
  const v = obj[key];
  if (v === undefined || v === null) {
    throw new WorkContractError("missing_field", `missing required field: ${key}`, key);
  }
  // The YAML core schema keeps `2026-06-25` a string; coerce a Date defensively.
  if (v instanceof Date) return v.toISOString().slice(0, 10);
  if (typeof v === "string") return v;
  throw new WorkContractError("invalid_type", `field ${key} must be an ISO date string`, key);
}

function parseStringArray(raw: unknown, ctx: string): string[] {
  if (!Array.isArray(raw)) {
    throw new WorkContractError("invalid_type", `${ctx} must be a list`, ctx);
  }
  return raw.map((v, i) => {
    if (typeof v !== "string") {
      throw new WorkContractError("invalid_type", `${ctx}[${i}] must be a string`, ctx);
    }
    return v;
  });
}

function parseBudget(raw: unknown): Budget | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (!isMapping(raw)) {
    throw new WorkContractError("invalid_type", "budget must be a mapping", "budget");
  }
  const budget: Budget = {};
  const perRun = optionalNumber(raw, "per_run_usd", "budget");
  const perDay = optionalNumber(raw, "per_day_usd", "budget");
  const maxIter = optionalNumber(raw, "max_iterations", "budget");
  if (perRun !== undefined) budget.per_run_usd = perRun;
  if (perDay !== undefined) budget.per_day_usd = perDay;
  if (maxIter !== undefined) budget.max_iterations = maxIter;
  return budget;
}

function parseDoneCheck(raw: unknown): string | DoneCheck[] {
  if (typeof raw === "string") {
    if (raw.trim().length === 0) {
      throw new WorkContractError(
        "malformed_done",
        "done.check string must be non-empty",
        "done.check",
      );
    }
    return raw;
  }
  if (Array.isArray(raw)) {
    if (raw.length === 0) {
      throw new WorkContractError(
        "malformed_done",
        "done.check list must be non-empty (symmetric with the empty-string rule)",
        "done.check",
      );
    }
    return raw.map((item, i): DoneCheck => {
      if (!isMapping(item)) {
        throw new WorkContractError(
          "malformed_done",
          `done.check[${i}] must be a mapping`,
          "done.check",
        );
      }
      if (typeof item.name !== "string" || typeof item.run !== "string") {
        throw new WorkContractError(
          "malformed_done",
          `done.check[${i}] requires string name + run`,
          "done.check",
        );
      }
      const check: DoneCheck = { name: item.name, run: item.run };
      const timeout = optionalNumber(item, "timeout_s", `done.check[${i}]`);
      if (timeout !== undefined) {
        // VERIFIER §1: per-check timeout is positive and never exceeds the hard cap (default 600,
        // cap 1800). An unbounded timeout would let a check hang a run past every guardrail.
        if (timeout <= 0 || timeout > MAX_CHECK_TIMEOUT_S) {
          throw new WorkContractError(
            "malformed_done",
            `done.check[${i}].timeout_s must be in (0, ${MAX_CHECK_TIMEOUT_S}], got ${timeout}`,
            "done.check",
          );
        }
        check.timeout_s = timeout;
      }
      if (item.required !== undefined) {
        if (typeof item.required !== "boolean") {
          throw new WorkContractError(
            "malformed_done",
            `done.check[${i}].required must be a boolean`,
            "done.check",
          );
        }
        check.required = item.required;
      }
      return check;
    });
  }
  throw new WorkContractError(
    "malformed_done",
    "done.check must be a string or a list of named checks",
    "done.check",
  );
}

function parseDone(raw: unknown): Done | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (!isMapping(raw)) {
    throw new WorkContractError("malformed_done", "done must be a mapping", "done");
  }
  if (raw.check === undefined || raw.check === null) {
    throw new WorkContractError("malformed_done", "done.check is required", "done.check");
  }
  const done: Done = { check: parseDoneCheck(raw.check) };
  const judge = optionalString(raw, "judge");
  if (judge !== undefined) {
    // VERIFIER §1/§3: `judge` is a rubric.md reference living inside the work subtree. Reject a
    // malformed ref (absolute, parent-traversal, non-`.md`, empty) — the verifier resolves this path
    // to load the rubric, so an escaping/garbage ref is a tamper vector, not a silent no-op.
    if (!isValidRubricRef(judge)) {
      throw new WorkContractError(
        "malformed_rubric",
        `done.judge must be a relative .md rubric path within the work tree, got ${JSON.stringify(judge)}`,
        "done.judge",
      );
    }
    done.judge = judge;
  }
  if (raw.protect !== undefined) done.protect = parseStringArray(raw.protect, "done.protect");
  if (raw.diff !== undefined) {
    if (!isMapping(raw.diff)) {
      throw new WorkContractError("malformed_done", "done.diff must be a mapping", "done.diff");
    }
    const diff: NonNullable<Done["diff"]> = {};
    const maxFiles = optionalNumber(raw.diff, "max_files", "done.diff");
    const maxLines = optionalNumber(raw.diff, "max_lines", "done.diff");
    if (maxFiles !== undefined) diff.max_files = maxFiles;
    if (maxLines !== undefined) diff.max_lines = maxLines;
    done.diff = diff;
  }
  if (raw.stop_on !== undefined) {
    done.stop_on = parseStringArray(raw.stop_on, "done.stop_on").map(
      (s): StopCondition => requireEnum(s, STOP_CONDITIONS, "done.stop_on"),
    );
  }
  return done;
}

function parseTrigger(raw: unknown): Trigger | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (!isMapping(raw)) {
    throw new WorkContractError("invalid_type", "trigger must be a mapping", "trigger");
  }
  const trigger: Trigger = { on: requireEnum(raw.on, TRIGGER_KINDS, "trigger.on") };
  const at = optionalString(raw, "at");
  if (at !== undefined) trigger.at = at;
  const idempotency = optionalString(raw, "idempotency");
  if (idempotency !== undefined) trigger.idempotency = idempotency;
  return trigger;
}

// ── Frontmatter split ────────────────────────────────────────────────────────

// `---\n <yaml> \n---` then an optional markdown body. Tolerates a BOM, CRLF, and
// trailing spaces on the fence lines. A file that is frontmatter-only (no body) is
// valid; the body group is then empty.
const FRONTMATTER = /^\uFEFF?---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n([\s\S]*))?$/;

function splitFrontmatter(source: string): { yaml: string; brief: string } {
  const m = FRONTMATTER.exec(source);
  if (!m) {
    throw new WorkContractError("no_frontmatter", "no `---` YAML frontmatter block found");
  }
  // Trim both ends: leading/trailing blank lines around the brief are insignificant
  // markdown, and trimming keeps parse → serialize round-trips byte-stable.
  return { yaml: m[1] ?? "", brief: (m[2] ?? "").trim() };
}

function parseYamlBlock(yaml: string): Record<string, unknown> {
  let data: unknown;
  try {
    data = parseYaml(yaml);
  } catch (err) {
    throw new WorkContractError(
      "invalid_yaml",
      `frontmatter is not valid YAML: ${(err as Error).message}`,
    );
  }
  if (data === null || data === undefined) {
    throw new WorkContractError("missing_field", "frontmatter is empty");
  }
  if (!isMapping(data)) {
    throw new WorkContractError("invalid_type", "frontmatter must be a YAML mapping");
  }
  return data;
}

// ── Public API ───────────────────────────────────────────────────────────────

/** Parse `_work.md` to the input layer (owner/gate/budget kept optional for resolution). */
export function parseWorkInput(source: string): ParsedInput {
  const { yaml, brief } = splitFrontmatter(source);
  const data = parseYamlBlock(yaml);

  const input: WorkContractInput = {
    id: requireString(data, "id"),
    kind: requireEnum(data.kind, KINDS, "kind"),
    state: requireEnum(data.state, ORCH_STATES, "state"),
    created: requireDateString(data, "created"),
    updated: requireDateString(data, "updated"),
  };
  const owner = optionalString(data, "owner");
  if (owner !== undefined) input.owner = owner;
  if (data.gate !== undefined && data.gate !== null) {
    input.gate = requireEnum(data.gate, GATE_MODES, "gate");
  }
  const budget = parseBudget(data.budget);
  if (budget !== undefined) input.budget = budget;
  const done = parseDone(data.done);
  if (done !== undefined) input.done = done;
  const trigger = parseTrigger(data.trigger);
  if (trigger !== undefined) input.trigger = trigger;

  return { input, brief };
}

/**
 * Materialize an input to a full WorkContract: `gate` defaults to `human` (the safe
 * default for anything irreversible, DATA-MODEL §A.2), and the gate-pairing rule is
 * enforced — `gate: auto` requires a non-empty `done.check` (VERIFIER §1).
 */
export function materialize(input: WorkContractInput): WorkContract {
  const contract: WorkContract = {
    id: input.id,
    kind: input.kind,
    state: input.state,
    gate: input.gate ?? "human",
    created: input.created,
    updated: input.updated,
  };
  if (input.owner !== undefined) contract.owner = input.owner;
  if (input.budget !== undefined) contract.budget = input.budget;
  if (input.done !== undefined) contract.done = input.done;
  if (input.trigger !== undefined) contract.trigger = input.trigger;

  if (contract.gate === "auto" && !hasCheck(contract.done)) {
    throw new WorkContractError(
      "gate_auto_no_check",
      "gate:auto requires a non-empty done.check; judge-only contracts must use gate:human (VERIFIER §1)",
      "gate",
    );
  }
  return contract;
}

/** Parse `_work.md` to a validated, materialized contract + its brief. */
export function parseWorkFile(source: string): WorkFile {
  const { input, brief } = parseWorkInput(source);
  return { contract: materialize(input), brief };
}

/** Parse `_work.md` to just the validated contract. */
export function parseWorkContract(source: string): WorkContract {
  return parseWorkFile(source).contract;
}

/**
 * Resolve a child's contract against its parent (FLOWS §F1 step 2): the child
 * inherits `owner`, `gate`, and `budget` from the parent unless it sets its own.
 * `done` and `trigger` are never inherited — they are per-unit-of-work.
 */
export function resolveWorkContract(
  child: WorkContractInput,
  parent?: WorkContractInput | WorkContract,
): WorkContract {
  if (!parent) return materialize(child);
  return materialize({
    ...child,
    owner: child.owner ?? parent.owner,
    gate: child.gate ?? parent.gate,
    budget: child.budget ?? parent.budget,
  });
}

function serializeDone(done: Done): Record<string, unknown> {
  const out: Record<string, unknown> = { check: done.check };
  if (done.judge !== undefined) out.judge = done.judge;
  if (done.protect !== undefined) out.protect = done.protect;
  if (done.diff !== undefined) out.diff = done.diff;
  if (done.stop_on !== undefined) out.stop_on = done.stop_on;
  return out;
}

/**
 * Serialize a contract to canonical `_work.md` — fields in DATA-MODEL §A.2 order,
 * optional fields omitted when absent. Canonical (comment-free); use
 * `reserializeWorkFile` to round-trip a source file preserving its comments.
 */
export function serializeWorkContract(contract: WorkContract, brief = ""): string {
  const fm: Record<string, unknown> = {
    id: contract.id,
    kind: contract.kind,
    state: contract.state,
  };
  if (contract.owner !== undefined) fm.owner = contract.owner;
  fm.gate = contract.gate;
  if (contract.budget !== undefined) fm.budget = contract.budget;
  if (contract.done !== undefined) fm.done = serializeDone(contract.done);
  if (contract.trigger !== undefined) fm.trigger = contract.trigger;
  fm.created = contract.created;
  fm.updated = contract.updated;

  const yaml = stringifyYaml(fm, { lineWidth: 0 }).replace(/\n+$/, "");
  const body = brief.trim();
  return body.length > 0 ? `---\n${yaml}\n---\n\n${body}\n` : `---\n${yaml}\n---\n`;
}

/** Serialize a parsed WorkFile (contract + brief) back to canonical `_work.md`. */
export function serializeWorkFile(file: WorkFile): string {
  return serializeWorkContract(file.contract, file.brief);
}

/**
 * Serialize an AUTHORED contract (the input layer) to `_work.md`. Unlike
 * `serializeWorkContract`, the inheritance-eligible fields (`owner`/`gate`/`budget`)
 * are OMITTED when absent: the write path (new_mission, FLOWS §F1) authors only what
 * it sets, and the parent defaults are resolved at scan time (`resolveWorkContract`,
 * BRO-1800). Omitting `owner`/`budget` lets them track the parent.
 *
 * `gate` is the exception the CALLER must weigh, not omit blindly: a checkless contract
 * cannot be `gate: auto` (VERIFIER §1), so new_mission PINS `gate: human` rather than
 * leave it to inherit — an inherited `gate: auto` (from an auto parent) would make the
 * checkless child invalid and get it SILENTLY DROPPED at scan (the BRO-1820 write-loss).
 * Do not "restore" gate to track the parent here. Key order matches `serializeWorkContract`.
 */
export function serializeWorkInput(input: WorkContractInput, brief = ""): string {
  const fm: Record<string, unknown> = {
    id: input.id,
    kind: input.kind,
    state: input.state,
  };
  if (input.owner !== undefined) fm.owner = input.owner;
  if (input.gate !== undefined) fm.gate = input.gate;
  if (input.budget !== undefined) fm.budget = input.budget;
  if (input.done !== undefined) fm.done = serializeDone(input.done);
  if (input.trigger !== undefined) fm.trigger = input.trigger;
  fm.created = input.created;
  fm.updated = input.updated;

  const yaml = stringifyYaml(fm, { lineWidth: 0 }).replace(/\n+$/, "");
  const body = brief.trim();
  return body.length > 0 ? `---\n${yaml}\n---\n\n${body}\n` : `---\n${yaml}\n---\n`;
}

/**
 * Round-trip a source `_work.md` preserving comments + key order (the "where
 * feasible" round-trip). Fully validates the contract first — including the
 * VERIFIER §1 gate:auto⇒done.check rule via `materialize`, so the write path
 * (BRO-1800 scanner) rejects exactly what every read path rejects — then re-emits
 * the ORIGINAL frontmatter via the YAML Document CST so nothing but insignificant
 * whitespace changes.
 */
export function reserializeWorkFile(source: string): string {
  parseWorkFile(source); // full validation (parse + materialize) — throws on any malformation
  const { yaml, brief } = splitFrontmatter(source);
  const doc = parseDocument(yaml);
  const yamlOut = doc.toString({ lineWidth: 0 }).replace(/\n+$/, "");
  return brief.length > 0 ? `---\n${yamlOut}\n---\n\n${brief}\n` : `---\n${yamlOut}\n---\n`;
}

/**
 * Patch one or more scalar frontmatter fields of a `_work.md`, preserving comments and key
 * order (like {@link reserializeWorkFile}), returning the re-emitted source. This is the write
 * side of the durable node-state advance (BRO-1914): a node's runtime `state`/`owner` is
 * AUTHORITATIVE in the FS — the scanner re-derives both on an `--rebuild` rescan (its
 * `CONTENT_KEYS` covers `state` and `owner`), so a DB-only write is LOST on rebuild. Persisting
 * the advance to `_work.md` is what makes it survive a restart.
 *
 * Validates BOTH ends: the source first (parse + materialize — throws on any malformation, so
 * this write path rejects exactly what every read path rejects) AND the patched result (so a
 * caller cannot write a `state`/`owner` that would make the contract invalid). Each patch is set
 * on the YAML Document CST (`doc.set` — an existing key is updated IN PLACE, keeping its line
 * comment; a new key is appended); a `null` value DELETES the key (clearing an inherited
 * `owner`). The yaml lib quotes/escapes scalar values, so a value cannot inject extra YAML.
 */
export function setWorkFileFields(source: string, patches: Record<string, string | null>): string {
  parseWorkFile(source); // full validation of the source — throws on any malformation
  const { yaml, brief } = splitFrontmatter(source);
  const doc = parseDocument(yaml);
  for (const [key, value] of Object.entries(patches)) {
    if (value === null) doc.delete(key);
    else doc.set(key, value);
  }
  const yamlOut = doc.toString({ lineWidth: 0 }).replace(/\n+$/, "");
  const result = brief.length > 0 ? `---\n${yamlOut}\n---\n\n${brief}\n` : `---\n${yamlOut}\n---\n`;
  parseWorkFile(result); // the patched result must STILL be a valid contract (guards a bad state/owner)
  return result;
}
