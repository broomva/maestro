/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import { plainVoiceForNode } from "./plain-voice";
import {
  PLANE_VIEWS,
  type RunBranch,
  STORE_SLICES,
  UI_PREF_KEYS,
  WORK_ITEM_EXCLUDED_FIELDS,
  WORKER_LOCATIONS,
  type WorkItem,
  type WorkItemExcludedField,
} from "./work-item";

// done.check for seam-work-item-store (BRO-1764): `bun run typecheck && bun test
// packages/protocol -t work-item`. `--filter` is a no-op in bun test (only `-t`
// filters by name); every describe carries "work-item" so `-t work-item` isolates.
// typecheck is REQUIRED in the check: `bun test` STRIPS types, so the compile-time
// witnesses (NoExcludedLeak, RunBranch, proposed-optionality, RequiredHold) only bite
// under `tsc --noEmit` — running `bun test` alone gives a false green on a broken shape.

// A full fixture exercising the derived surface — the shape the board / feed /
// inspector all render (START-HERE §5 seam 2).
const item: WorkItem = {
  id: "7f3a9c",
  state: "review",
  kind: "task",
  title: "Fix meta tags",
  owner: "@alex",
  gate: "human",
  path: "growth/seo-refresh/fix-meta-tags",
  parentId: "growth/seo-refresh",
  updatedAt: "2026-06-25T06:01:10Z",
  created: "2026-06-25",
  sessionId: "7f3a",
  gateId: "g-7f3a",
  initiative: "Growth",
  project: "SEO refresh",
  lastEventAt: "2026-06-25T06:01:10Z",
  worker: { name: "agent:maestro", where: "local worktree" },
  run: "run/7f3a",
  verdict: "Checks passed · 14 tests added",
  look: { ran: "2h 14m unsupervised · 41 events", decided: ["ran the suite"], ask: "Merge?" },
};

describe("work-item shape — the read-side projection (data-contract §work item shape)", () => {
  test("a work-item carries no chat and no events fields", () => {
    // Chat + the activity timeline are SEPARATE projections joined by sessionId,
    // never embedded in the work item.
    expect("chat" in item).toBe(false);
    expect("events" in item).toBe(false);
  });

  test("a work-item omits the engine-room fields (budget / done / trigger)", () => {
    // Disclosure ladder: the read surface shows signals/verbs/receipts, not the
    // orchestration contract internals.
    for (const f of ["budget", "done", "trigger"]) {
      expect(f in item).toBe(false);
    }
  });

  test("WORK_ITEM_EXCLUDED_FIELDS names every excluded surface, and the fixture honors it", () => {
    expect([...(WORK_ITEM_EXCLUDED_FIELDS as readonly string[])].sort()).toEqual(
      ["budget", "chat", "done", "events", "trigger", "percent", "progress"].sort(),
    );
    for (const f of WORK_ITEM_EXCLUDED_FIELDS) {
      expect(f in item).toBe(false);
    }
  });

  test("the run branch matches the run/ receipt template", () => {
    expect(item.run?.startsWith("run/")).toBe(true);
  });

  test("RunBranch enforces the run/ receipt prefix at the type level (negative-compile guard)", () => {
    const ok: RunBranch = "run/7f3a";
    // @ts-expect-error — a branch without the run/ prefix is not a RunBranch. If
    // RunBranch is ever loosened to plain `string`, this directive goes unused and
    // `tsc --noEmit` fails, protecting the "branch is the receipt" prefix invariant.
    const bad: RunBranch = "feature/nope";
    expect(ok.startsWith("run/")).toBe(true);
    expect(bad as string).toBe("feature/nope"); // referenced so it is not an unused binding
  });

  test("a review item carries gateId — the verb-dispatch key, distinct from the node id", () => {
    // The round-5 P20 fix: gate verbs (approve/revise/block/escalate/grant) all take a
    // gateId (intents.ts). A review/blocked item exposes it so rung-2 controls dispatch
    // through it, NOT the node id (the runtime rejects a nodeId as a gateId). `look` is
    // display-only and carries no dispatch key.
    expect(item.gateId).toBe("g-7f3a");
    expect(item.gateId).not.toBe(item.id);
  });

  test("the join key to the activity timeline is sessionId, not an embedded array", () => {
    expect(item.sessionId).toBe("7f3a");
  });

  test("a blocked item carries reason but NO gateId (Stuck is redispatchable, not gated)", () => {
    // reason fixture coverage (referenced by name so a rename fails tsc). AND the round-8
    // P20 fix: a blocked node has no gate row — gates surface as `review`, and
    // `resolveGateVerdict` throws off-review; `blocked` is cleared by a nodeId `dispatch`,
    // not a verdict. gateId is review-only; the earlier fabricated `gateId: "g-b1"` masked
    // the wrong pin (the suite stayed green while asserting a shape the runtime can't make).
    const blocked: WorkItem = {
      id: "b1",
      state: "blocked",
      kind: "task",
      title: "Deploy is failing",
      gate: "human",
      path: "ops/deploy",
      updatedAt: "2026-06-26T00:00:00Z",
      sessionId: "b1s",
      reason: "lease held by another run",
    };
    expect(blocked.reason).toBe("lease held by another run");
    expect("gateId" in blocked).toBe(false);
  });

  test("a completed node keeps its session id + worker so the receipt survives completion", () => {
    // sessionId/worker are current-or-most-recent, NOT live-only — the inspector
    // renders receipts for done/standing items, the ones users inspect most.
    const doneItem: WorkItem = {
      id: "9b1",
      state: "done",
      kind: "task",
      title: "Ship the thing",
      gate: "human",
      path: "growth/ship",
      updatedAt: "2026-06-26T00:00:00Z",
      created: "2026-06-25",
      sessionId: "9b1a", // the ended session — still present
      run: "run/9b1a",
      worker: { name: "agent:maestro", where: "local worktree" },
      verdict: "Checks passed",
    };
    expect(doneItem.sessionId).toBe("9b1a");
    expect(doneItem.worker).toBeDefined();
  });
});

// A COMPILE-TIME WITNESS that the post-dispatch / no-gate fields are genuinely optional:
// a never-dispatched `proposed` node has no session and no open gate, so it MUST
// construct without sessionId / gateId / worker / run / lastEventAt / initiative /
// project. The moment any is tightened to required, THIS declaration fails `tsc --noEmit`
// — the type error a downstream store or "Queued" board column would otherwise hit only
// at runtime (unable to build a real proposed row, or back-filling a bogus session/gate id).
const proposed: WorkItem = {
  id: "p1",
  state: "proposed",
  kind: "task",
  title: "Draft the SEO refresh",
  gate: "human",
  path: "growth/seo-refresh",
  updatedAt: "2026-06-25T00:00:00Z",
  // `created` deliberately OMITTED — it is optional (unconsumed on the read surface),
  // so this construction also witnesses that `created` can be absent (if it were ever
  // made required, THIS declaration fails tsc).
};

// The SYMMETRIC witness (required stays required), mirroring the `proposed` fixture's
// optional-stays-optional guard. `RequiredKeys` keeps only the keys whose type does NOT
// admit `undefined`; asserting the required server-truth fields are all among them fails
// `tsc --noEmit` if ANY is loosened to optional — notably `updatedAt`, whose loosening
// would let the universal-age derivation (`lastEventAt ?? updatedAt`) silently go
// undefined on every card. Deterministic (no @ts-expect-error line-positioning).
type RequiredKeys<T> = { [K in keyof T as undefined extends T[K] ? never : K]: 0 };
type ServerTruthRequired = "id" | "state" | "kind" | "title" | "gate" | "path" | "updatedAt";
type RequiredHold = ServerTruthRequired extends keyof RequiredKeys<WorkItem> ? true : never;
const requiredHold: RequiredHold = true;

describe("work-item optionality — the never-dispatched witness (data-contract §work item shape)", () => {
  test("the required server-truth fields stay required (compile-time witness collapses to never otherwise)", () => {
    // If `updatedAt` (or any required server-truth field) is loosened to optional,
    // `RequiredHold` becomes `never` and `const requiredHold: never = true` fails tsc.
    // The runtime assert only anchors the type check.
    expect(requiredHold).toBe(true);
  });

  test("a proposed node carries none of the post-dispatch derived fields", () => {
    // Runtime assertions anchor the compile-time witness above; the type check is the
    // real enforcement (a proposed node cannot name a session that was never born).
    for (const f of [
      "sessionId",
      "gateId",
      "worker",
      "run",
      "lastEventAt",
      "initiative",
      "project",
    ]) {
      expect(f in proposed).toBe(false);
    }
  });

  test("a queued/proposed card still renders an age — the universal source is updatedAt, not lastEventAt", () => {
    // The blocker the round-4 P20 caught: the demo shows a per-card age on EVERY card,
    // incl. proposed backlog. A downstream that formats age from `lastEventAt` alone
    // renders blank/NaN here (no session -> no lastEventAt). The contract routes the
    // universal age at `updatedAt` (always present), refined by lastEventAt when live.
    expect("lastEventAt" in proposed).toBe(false);
    const age = proposed.lastEventAt ?? proposed.updatedAt; // the universal derivation
    expect(age).toBe(proposed.updatedAt);
    expect(Number.isNaN(Date.parse(age))).toBe(false); // formattable
  });

  test("a fired standing routine idling at triggered keeps its most-recent session (dispatch-history-keyed)", () => {
    // The round-3 P20 fix: sessionId is keyed on dispatch history, NOT current state.
    // `triggered` is never-dispatched backlog OR a fired routine idled back — the
    // latter retains its receipts, so a state-keyed selector would wrongly drop them.
    const firedRoutine: WorkItem = {
      id: "r1",
      state: "triggered",
      kind: "routine",
      title: "Nightly triage",
      gate: "human",
      path: "ops/nightly-triage",
      updatedAt: "2026-06-26T06:00:00Z",
      created: "2026-06-01",
      sessionId: "r1-last", // the last fire's session — retained though idle at triggered
      run: "run/r1-last",
      worker: { name: "agent:maestro", where: "local worktree" },
    };
    expect(plainVoiceForNode(firedRoutine.state, firedRoutine.kind).label).toBe("Standing");
    expect(firedRoutine.sessionId).toBe("r1-last");
  });
});

// A COMPILE-TIME guard, not a fixture tautology: if any excluded field ever becomes
// a key of WorkItem — e.g. an optional `chat?: UIMessage[]` (the exact conflation
// this contract prevents) — `NoExcludedLeak` collapses to `never` and this file
// FAILS `tsc --noEmit`. The runtime assertion only anchors the type check.
type NoExcludedLeak = Extract<keyof WorkItem, WorkItemExcludedField> extends never ? true : never;

describe("work-item shape — excluded fields are forbidden at the type level", () => {
  test("no excluded field can leak into the WorkItem type (compile-time guard)", () => {
    const enforced: NoExcludedLeak = true;
    expect(enforced).toBe(true);
  });
});

describe("work-item worker location (data-contract §work item shape)", () => {
  test("WORKER_LOCATIONS is exactly the two canon locations", () => {
    expect(WORKER_LOCATIONS).toEqual(["local worktree", "cloud sandbox"]);
  });
});

describe("work-item plain voice (references plain-voice.ts, no redefinition)", () => {
  test("a work-item in review reads Needs you", () => {
    expect(plainVoiceForNode(item.state, item.kind).label).toBe("Needs you");
  });

  test("a routine work-item between fires reads Standing, but at the gate reads Needs you", () => {
    // Mirrors the P20 attention-mask guard in plain-voice.test.ts.
    expect(plainVoiceForNode("triggered", "routine").label).toBe("Standing");
    expect(plainVoiceForNode("review", "routine").label).toBe("Needs you");
    expect(plainVoiceForNode("blocked", "routine").label).toBe("Stuck");
  });
});

describe("work-item client store taxonomy (porting-notes §State taxonomy)", () => {
  test("PLANE_VIEWS is feed / board / list (was localStorage mc4-view)", () => {
    expect(PLANE_VIEWS).toEqual(["feed", "board", "list"]);
  });

  test("UI_PREF_KEYS absorbs the three ad-hoc localStorage keys", () => {
    // mc4-view -> view, bv-nav-open -> navOpen, bv-ml-cols -> cols.
    expect([...(UI_PREF_KEYS as readonly string[])].sort()).toEqual(["cols", "navOpen", "view"]);
  });

  test("STORE_SLICES names the three homes every useState lands in", () => {
    expect([...(STORE_SLICES as readonly string[])].sort()).toEqual(
      ["ephemeral", "persisted-ui-prefs", "server-truth"].sort(),
    );
  });
});
