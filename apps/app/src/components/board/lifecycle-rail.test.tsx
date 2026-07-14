/// <reference types="bun" />

// LifecycleRail (BRO-1891 FID-5) — anti-vacuity [[self-hosting-vacuous-pass]]: each case asserts the
// CONCRETE phase a given state resolves to (passed vs current vs warn vs upcoming), so a broken index
// map fails the test — not a tautology. renderToStaticMarkup, no DOM lifecycle. The rail is derived
// purely from state; it must never carry a progress percentage (CLAUDE.md §Work states: receipts).

import { describe, expect, test } from "bun:test";
import type { OrchState } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { LifecycleRail } from "./lifecycle-rail";

const html = (state: OrchState) => renderToStaticMarkup(<LifecycleRail state={state} />);

describe("LifecycleRail", () => {
  test("four plain-voice stages, in lifecycle order, with the canon note", () => {
    const h = html("running");
    for (const label of ["Queued", "Running", "Needs you", "Done"]) expect(h).toContain(label);
    expect(h).toContain("Done is earned");
    expect(h).not.toContain("%"); // receipts, never a percentage
  });

  test("running → Running is current, Queued is passed, Needs you/Done upcoming", () => {
    const h = html("running");
    expect(h).toContain(
      'mc-rail-stage is-passed"><span class="mc-rail-dot"></span><span class="mc-rail-name">Queued',
    );
    expect(h).toContain(
      'mc-rail-stage is-current" aria-current="step"><span class="mc-rail-dot"></span><span class="mc-rail-name">Running',
    );
    // upcoming stages carry no phase modifier class + no aria-current.
    expect(h).toContain(
      'mc-rail-stage"><span class="mc-rail-dot"></span><span class="mc-rail-name">Needs you',
    );
  });

  test("blocked → warn (Stuck) at the Running stage, not a stage of its own", () => {
    const h = html("blocked");
    // Running becomes the warn stage (still the current step for AT) and reads '· stuck'; Queued passed.
    expect(h).toContain(
      'mc-rail-stage is-warn" aria-current="step"><span class="mc-rail-dot"></span><span class="mc-rail-name">Running · stuck',
    );
    expect(h).toContain(
      'mc-rail-stage is-passed"><span class="mc-rail-dot"></span><span class="mc-rail-name">Queued',
    );
    expect(h).not.toContain("is-current"); // blocked lights warn, not current
  });

  test("review → Needs you is current, and Running/Queued are behind it", () => {
    const h = html("review");
    expect(h).toContain(
      'mc-rail-stage is-current" aria-current="step"><span class="mc-rail-dot"></span><span class="mc-rail-name">Needs you',
    );
    expect((h.match(/is-passed/g) ?? []).length).toBe(2); // Queued + Running passed
  });

  test("done → the last stage is current, all others passed", () => {
    const h = html("done");
    expect(h).toContain(
      'mc-rail-stage is-current" aria-current="step"><span class="mc-rail-dot"></span><span class="mc-rail-name">Done',
    );
    expect((h.match(/is-passed/g) ?? []).length).toBe(3); // Queued + Running + Needs you passed
  });

  test("canceled → terminal-neutral: NO stage passed or current (never fabricates a completed run)", () => {
    // A canceled item claims no progression — every stage stays inert/upcoming (matches the prototype's
    // absent-entry behavior). It must NOT render blue-passed stages + a lit Done, which would flatly
    // contradict the inspector's own "No run yet" stub and canceled's muted/gray canon tone.
    const h = html("canceled");
    expect(h).not.toContain("is-passed"); // nothing behind — no fabricated progression
    expect(h).not.toContain("is-current"); // Done is not lit — a canceled item did not complete
    expect(h).not.toContain("is-warn");
    expect(h).not.toContain('aria-current="step"'); // no active step for a terminal-neutral item
    // The four stages still render (inert), so the rail reads as a calm, un-traversed lifecycle.
    for (const label of ["Queued", "Running", "Needs you", "Done"]) expect(h).toContain(label);
  });

  test("proposed/reviewing/triggered all collapse to Queued being current (upcoming rest)", () => {
    for (const s of ["proposed", "reviewing", "triggered"] as const) {
      const h = html(s);
      expect(h).toContain(
        'mc-rail-stage is-current" aria-current="step"><span class="mc-rail-dot"></span><span class="mc-rail-name">Queued',
      );
      expect(h).not.toContain("is-passed"); // nothing before Queued
    }
  });
});
