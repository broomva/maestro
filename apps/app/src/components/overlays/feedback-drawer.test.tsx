/// <reference types="bun" />

// The feedback drawer (BRO-1894 FID-7). It renders in place (fixed positioning, no portal), so
// renderToStaticMarkup at open=true covers the markup + §Voice; the open/close/send/focus behaviour is
// overlays.pw.ts's browser concern.

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { FEEDBACK_PREVIEW_NOTE, FeedbackDrawer } from "./feedback-drawer";

const noop = () => {};

function render(open: boolean): string {
  return renderToStaticMarkup(<FeedbackDrawer open={open} onClose={noop} />);
}

function assertVoice(html: string) {
  expect(html).not.toContain("—");
  expect(html).not.toContain("%");
  expect(html).not.toContain("P1");
  expect(html).not.toContain("primitive");
  expect(html).not.toContain("engine room");
}

describe("FeedbackDrawer — the right-docked drawer (BRO-1894)", () => {
  test("renders nothing when closed", () => {
    expect(render(false)).toBe("");
  });

  test("is a matte drawer dialog with the scrim", () => {
    const html = render(true);
    expect(html).toContain('class="fb-drawer"');
    expect(html).toContain('data-testid="feedback-drawer"');
    expect(html).toContain('class="fb-scrim"');
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('aria-label="Feedback"');
  });

  test("the header is honest: a title + a 'preview' chip (delivery is not wired)", () => {
    const html = render(true);
    expect(html).toContain("Feedback");
    expect(html).toContain('class="set-preview"');
    expect(html).toContain(">preview<");
    expect(html).toContain("Tell the loop what to build or fix");
  });

  test("the composer has the three types, an aria-labelled send, and the attach toggle", () => {
    const html = render(true);
    for (const label of ["Idea", "Issue", "Praise"]) {
      expect(html).toContain(label);
    }
    // Idea is the default type → its placeholder shows, and its pill is active/pressed.
    expect(html).toContain("What would make the loop work better for you?");
    expect(html).toContain('aria-pressed="true"');
    expect(html).toContain('aria-label="Send feedback"');
    // The attach control is a real, native checkbox (semantic + keyboard), checked by default.
    expect(html).toContain('type="checkbox"');
    expect(html).toContain("checked");
    expect(html).toContain("Attach this screen and its context");
  });

  test("the thread history is clearly a SAMPLE, not live threads", () => {
    const html = render(true);
    expect(html).toContain("Recent feedback");
    // The section carries a 'sample' chip (honest data — no live feedback read path yet).
    expect(html).toContain(">sample<");
    expect(html).toContain("Let the gate queue group by folder, not just by time");
    // No fabricated live-session claim ("maestro picked this up" / "unsupervised").
    expect(html).not.toContain("maestro picked this up");
    expect(html).not.toContain("unsupervised");
  });

  test("§Voice — the drawer copy + the honest receipt are plain-language", () => {
    assertVoice(render(true));
    // The send receipt is conditionally rendered (after a send), so guard the const directly.
    expect(FEEDBACK_PREVIEW_NOTE).not.toContain("—");
    expect(FEEDBACK_PREVIEW_NOTE).not.toContain("%");
    expect(FEEDBACK_PREVIEW_NOTE.toLowerCase()).toContain("preview");
  });
});
