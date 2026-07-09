/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import { plainVoiceForNode } from "./plain-voice";
import {
  PLANE_VIEWS,
  STORE_SLICES,
  UI_PREF_KEYS,
  WORK_ITEM_EXCLUDED_FIELDS,
  WORKER_LOCATIONS,
  type WorkItem,
} from "./work-item";

// done.check for seam-work-item-store (BRO-1764): `bun test packages/protocol
// --filter work-item`. `--filter` is a no-op in bun test (only `-t` filters by
// name); every describe carries "work-item" so `-t work-item` isolates the suite.

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
      ["budget", "chat", "done", "events", "trigger"].sort(),
    );
    for (const f of WORK_ITEM_EXCLUDED_FIELDS) {
      expect(f in item).toBe(false);
    }
  });

  test("the run branch matches the run/ receipt template", () => {
    expect(item.run?.startsWith("run/")).toBe(true);
  });

  test("look.ran is a receipt string, never a percentage", () => {
    expect(item.look?.ran).not.toMatch(/%/);
  });

  test("the join key to the activity timeline is sessionId, not an embedded array", () => {
    expect(item.sessionId).toBe("7f3a");
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
