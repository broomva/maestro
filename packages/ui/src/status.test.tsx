/// <reference types="bun" />

// StatusBadge + DotComet suite (BRO-1757). Same discipline as primitives.test.tsx: render
// to static markup via react-dom/server (no DOM harness — runs under CI's plain bun test),
// with type-level parity against the design-system .d.ts contracts.
//
// The load-bearing test is the OrchState → view mapping table: it pins the plain-voice
// labels AND the dot colors in one place, and it is where the canon color rule lives —
// **review ("Needs you") is accent-blue, never red** (CLAUDE.md §Work states / D-COLOR).

import { describe, expect, test } from "bun:test";
import type { OrchState, PlainVoice } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import {
  DotComet,
  type DotCometProps,
  STATUS_DOT_VAR,
  StatusBadge,
  type StatusBadgeProps,
  type StatusTone,
  workStatusView,
} from "./index";

// ── Type-level parity ────────────────────────────────────────────────────────────
type Expect<T extends true> = T;
type Equal<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
// The .d.ts status enum is success/info/warning/danger/neutral; `accent` (accent-blue
// "Needs you") is the canon-required extension — assert the full tone union.
type _StatusTone = Expect<
  Equal<
    NonNullable<StatusBadgeProps["status"]>,
    "success" | "info" | "warning" | "danger" | "neutral" | "accent"
  >
>;
type _StatusPulse = Expect<Equal<NonNullable<StatusBadgeProps["pulse"]>, boolean>>;
type _DotCometSize = Expect<Equal<NonNullable<DotCometProps["size"]>, number>>;
type _DotCometColor = Expect<Equal<NonNullable<DotCometProps["color"]>, string>>;

// ── The mapping table: OrchState → plain-voice label + dot color (the deliverable) ──
const EXPECTED: Record<
  OrchState,
  { label: PlainVoice; tone: StatusTone; running: boolean; pulse: boolean }
> = {
  proposed: { label: "Queued", tone: "neutral", running: false, pulse: false },
  reviewing: { label: "Queued", tone: "neutral", running: false, pulse: false },
  triggered: { label: "Queued", tone: "neutral", running: false, pulse: false },
  running: { label: "Running", tone: "info", running: true, pulse: false },
  blocked: { label: "Stuck", tone: "warning", running: false, pulse: false },
  review: { label: "Needs you", tone: "accent", running: false, pulse: false },
  done: { label: "Done", tone: "success", running: false, pulse: false },
  canceled: { label: "Done", tone: "neutral", running: false, pulse: false },
};

describe("workStatusView · OrchState → plain-voice label + dot color (D-ENUM / D-COLOR)", () => {
  for (const [state, want] of Object.entries(EXPECTED) as [
    OrchState,
    (typeof EXPECTED)[OrchState],
  ][]) {
    test(`${state} → "${want.label}" (${want.tone} dot)`, () => {
      const view = workStatusView(state);
      expect(view.label).toBe(want.label);
      expect(view.tone).toBe(want.tone);
      expect(view.running).toBe(want.running);
      expect(view.pulse).toBe(want.pulse);
    });
  }

  test('"Needs you" (review) is accent-blue, never red', () => {
    const view = workStatusView("review");
    expect(view.label).toBe("Needs you");
    expect(view.tone).toBe("accent");
    // The done.check: the accent-blue token is asserted for the review state.
    expect(STATUS_DOT_VAR[view.tone]).toBe("var(--bv-blue-accent)");
    expect(STATUS_DOT_VAR[view.tone]).not.toContain("danger");
    expect(STATUS_DOT_VAR[view.tone]).not.toContain("red");
  });

  test("a routine between fires reads Standing (pulse), but a gated routine still needs you", () => {
    const standing = workStatusView("triggered", "routine");
    expect(standing.label).toBe("Standing");
    expect(standing.pulse).toBe(true);
    expect(standing.tone).toBe("neutral");
    // The overlay must NOT mask the human gate (protocol P20 catch).
    const gated = workStatusView("review", "routine");
    expect(gated.label).toBe("Needs you");
    expect(gated.tone).toBe("accent");
  });

  test("isRunning overrides state for the DotComet decision", () => {
    expect(workStatusView("triggered", "task", { isRunning: true }).running).toBe(true);
    expect(workStatusView("running", "task", { isRunning: false }).running).toBe(false);
  });
});

// STATUS_DOT_VAR must reference only @maestro/tokens vars (never a hardcoded hex).
describe("STATUS_DOT_VAR · dot tokens", () => {
  test("every tone maps to a --bv- CSS var", () => {
    for (const value of Object.values(STATUS_DOT_VAR)) {
      expect(value).toMatch(/^var\(--bv-[a-z0-9-]+\)$/);
    }
  });
  test("accent is accent-blue; danger exists but is unused chrome", () => {
    expect(STATUS_DOT_VAR.accent).toBe("var(--bv-blue-accent)");
    expect(STATUS_DOT_VAR.danger).toBe("var(--bv-danger)");
  });
});

describe("StatusBadge", () => {
  test("is a matte gray pill with a sentence-case label and an info dot by default", () => {
    const html = renderToStaticMarkup(<StatusBadge>Running</StatusBadge>);
    expect(html).toContain("rounded-full");
    expect(html).toContain("bg-muted");
    expect(html).toContain("h-[26px]");
    expect(html).toContain("var(--bv-info)");
    expect(html).toContain("Running");
  });

  test("the dot carries the status color; the accent tone is accent-blue", () => {
    const html = renderToStaticMarkup(<StatusBadge status="accent">Needs you</StatusBadge>);
    expect(html).toContain("var(--bv-blue-accent)");
    // The capsule stays matte gray — no accent on the pill itself.
    expect(html).toContain("bg-muted");
  });

  test("pulse uses the canon .bv-dot--pulse breath, not a bespoke keyframe", () => {
    const html = renderToStaticMarkup(
      <StatusBadge status="neutral" pulse>
        Standing
      </StatusBadge>,
    );
    expect(html).toContain("bv-dot--pulse");
    expect(renderToStaticMarkup(<StatusBadge>Queued</StatusBadge>)).not.toContain("bv-dot--pulse");
  });

  test("the dot is aria-hidden (the label is the accessible name)", () => {
    expect(renderToStaticMarkup(<StatusBadge>Done</StatusBadge>)).toContain('aria-hidden="true"');
  });
});

describe("DotComet", () => {
  test("renders the canon .bv-dot-live tidepool, aria-hidden, at the default 15px", () => {
    const html = renderToStaticMarkup(<DotComet />);
    expect(html).toContain("bv-dot-live");
    expect(html).toContain('aria-hidden="true"');
    expect(html).toContain("width:15px");
    expect(html).toContain("height:15px");
  });

  test("size overrides the diameter", () => {
    const html = renderToStaticMarkup(<DotComet size={8} />);
    expect(html).toContain("width:8px");
    expect(html).toContain("height:8px");
  });
});
