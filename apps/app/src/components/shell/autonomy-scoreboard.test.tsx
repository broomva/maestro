/// <reference types="bun" />
// autonomy-scoreboard.test.tsx (BRO-1818) — the ledger → chrome mapping + render. The AutonomyScoreboard
// is pure divs (no router), so renderToStaticMarkup runs it under plain bun test; the live fetch hook
// (use-ledger) is a P11 concern. The load-bearing canon assertions: receipts not percentages (no `%`), a
// calm empty state (no em-dash placeholder), and the derived geometry reaching the bar.

import { describe, expect, test } from "bun:test";
import type { LedgerResponse } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { AutonomyScoreboard, ledgerToScoreboardProps } from "./autonomy-scoreboard";

const base = (over: Partial<LedgerResponse> = {}): LedgerResponse => ({
  since: 0,
  until: 7_200_000,
  unsupervisedMs: 0,
  humanLooks: 0,
  activeRuns: 0,
  segments: [],
  notches: [],
  label: "0m unsupervised · 0 looks",
  ...over,
});

describe("ledgerToScoreboardProps — the wire → chrome mapping", () => {
  test("null ledger → the calm empty state (no hours, no sub)", () => {
    expect(ledgerToScoreboardProps(null)).toEqual({ segments: [], notches: [] });
  });

  test("a genuinely empty day (0 hours, 0 looks, 0 active) → the empty state", () => {
    expect(ledgerToScoreboardProps(base())).toEqual({ segments: [], notches: [] });
  });

  test("a populated ledger → duration headline + looks footnote + the derived geometry", () => {
    const props = ledgerToScoreboardProps(
      base({
        unsupervisedMs: 3_600_000,
        humanLooks: 3,
        segments: [{ start: 0, width: 50 }],
        notches: [25],
      }),
    );
    expect(props.hours).toBe("1h 0m");
    expect(props.sub).toBe("3 looks today");
    expect(props.segments).toEqual([{ start: 0, width: 50 }]);
    expect(props.notches).toEqual([25]);
  });

  test("a single look reads singular", () => {
    expect(ledgerToScoreboardProps(base({ unsupervisedMs: 60_000, humanLooks: 1 })).sub).toBe(
      "1 look today",
    );
  });

  test("active runs add a running receipt to the footnote", () => {
    expect(
      ledgerToScoreboardProps(base({ unsupervisedMs: 60_000, humanLooks: 2, activeRuns: 1 })).sub,
    ).toBe("2 looks today · 1 running");
  });

  test("a just-started run (sub-minute, still active) is not the empty state", () => {
    // unsupervisedMs 0 but a live run → show "0m" + the live segment, never the empty state.
    const props = ledgerToScoreboardProps(
      base({
        unsupervisedMs: 30_000,
        activeRuns: 1,
        segments: [{ start: 90, width: 10, live: true }],
      }),
    );
    expect(props.hours).toBe("0m");
    expect(props.segments).toEqual([{ start: 90, width: 10, live: true }]);
  });

  test("no mapped field ever carries a percent sign (receipts, not percentages)", () => {
    const props = ledgerToScoreboardProps(
      base({ unsupervisedMs: 8_040_000, humanLooks: 5, activeRuns: 2 }),
    );
    expect(`${props.hours} ${props.sub}`).not.toContain("%");
  });
});

/** Visible text only — strip tags (and thus the `style="…"` attrs, where the bar's positional `%` layout
 *  lives). The canon is "no percentage in what the user READS", not "no `%` in CSS units". */
const visibleText = (html: string): string => html.replace(/<[^>]*>/g, "");

describe("AutonomyScoreboard render (SSR markup)", () => {
  test("a populated ledger renders the duration + looks, no percentage in the copy", () => {
    const html = renderToStaticMarkup(
      <AutonomyScoreboard
        {...ledgerToScoreboardProps(
          base({
            unsupervisedMs: 3_600_000,
            humanLooks: 3,
            segments: [{ start: 0, width: 50 }],
            notches: [25],
          }),
        )}
      />,
    );
    expect(html).toContain("1h 0m");
    expect(html).toContain("3 looks today");
    // The VISIBLE copy carries no percentage (the `%` in the raw HTML is the bar's CSS layout units).
    expect(visibleText(html)).not.toContain("%");
    // The empty-state copy must NOT appear when there is data.
    expect(html).not.toContain("no unsupervised runs yet");
  });

  test("the empty ledger renders the calm empty-state line, no em-dash placeholder", () => {
    const html = renderToStaticMarkup(<AutonomyScoreboard {...ledgerToScoreboardProps(null)} />);
    expect(html).toContain("no unsupervised runs yet");
    const text = visibleText(html);
    expect(text).not.toContain("—"); // §Voice: no em-dash placeholders in chrome
    expect(text).not.toContain("%");
  });
});
