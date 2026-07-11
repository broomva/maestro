/// <reference types="bun" />

// Composer suite (BRO-1762). Structure + the pure send-guard via renderToStaticMarkup /
// direct call — the runner has no DOM, so the Enter/click send is dogfooded in Playwright
// (composer.pw.ts). The invariant the type + structure pin: the Composer is THE one glass
// surface, its default placeholder is "Message Maestro" (D-NAME), and empty never sends.

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Composer, type ComposerProps, composerSendText } from "./index";

type Expect<T extends true> = T;
type Equal<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type _Placeholder = Expect<Equal<NonNullable<ComposerProps["placeholder"]>, string>>;
type _Value = Expect<Equal<NonNullable<ComposerProps["value"]>, string>>;
type _OnSend = Expect<Equal<NonNullable<ComposerProps["onSend"]>, (text: string) => void>>;
// onChange is remapped to (value: string) => void — if it were still the DOM
// ChangeEventHandler this Equal check would fail to compile.
type _OnChange = Expect<Equal<NonNullable<ComposerProps["onChange"]>, (value: string) => void>>;

describe("composerSendText · trim-and-guard (empty never sends)", () => {
  test("trims surrounding whitespace", () => {
    expect(composerSendText("  hello  ")).toBe("hello");
  });
  test("returns null for empty or whitespace-only", () => {
    expect(composerSendText("")).toBeNull();
    expect(composerSendText("   \n\t ")).toBeNull();
  });
  test("keeps interior whitespace", () => {
    expect(composerSendText("  new mission now ")).toBe("new mission now");
  });
});

describe("Composer", () => {
  test("is the one glass surface, with a bare input and a primary send button", () => {
    const html = renderToStaticMarkup(<Composer />);
    expect(html).toContain("bv-glass-composer");
    expect(html).toContain('aria-label="Send"');
    expect(html).toContain('type="button"');
    expect(html).toContain("bg-primary"); // the send button is a primary fill, not ghost
    expect(html).toContain("bg-transparent"); // the input is bare inside the capsule
  });

  test('the default placeholder is "Message Maestro" (D-NAME)', () => {
    expect(renderToStaticMarkup(<Composer />)).toContain('placeholder="Message Maestro"');
  });

  test("a custom placeholder overrides the default", () => {
    expect(renderToStaticMarkup(<Composer placeholder="Ask anything" />)).toContain(
      'placeholder="Ask anything"',
    );
  });

  test("a leading slot switches the grid to three columns", () => {
    const withLeading = renderToStaticMarkup(<Composer leading={<span>clip</span>} />);
    expect(withLeading).toContain("grid-cols-[auto_1fr_auto]");
    expect(withLeading).toContain("clip");
    expect(renderToStaticMarkup(<Composer />)).toContain("grid-cols-[1fr_auto]");
  });

  test("renders a controlled value", () => {
    expect(renderToStaticMarkup(<Composer value="drafting" onChange={() => {}} />)).toContain(
      'value="drafting"',
    );
  });

  test("the focus ring rides the capsule; the input suppresses its own (unlayered-beating inline)", () => {
    const html = renderToStaticMarkup(<Composer />);
    expect(html).toContain("focus-within:[outline:2px_solid_var(--ring)]");
    // Inline `outline:none` on the input — a layered utility can't beat the unlayered global
    // :focus-visible ring, so this must be inline to avoid a double ring (BRO-1762 P20).
    expect(html).toContain("outline:none");
  });

  // ── send/stop verb (BRO-1826 M4) ──
  test("at rest the action button is Send (arrow up)", () => {
    const html = renderToStaticMarkup(<Composer />);
    expect(html).toContain('aria-label="Send"');
    expect(html).not.toContain('aria-label="Stop"');
    // lucide tags each glyph with a class — the send button carries the arrow-up, not the stop square.
    expect(html).toContain("lucide-arrow-up");
    expect(html).not.toContain("lucide-square");
  });

  test("while busy the action button flips to Stop (a square), and the send arrow is gone", () => {
    const html = renderToStaticMarkup(<Composer busy onStop={() => {}} />);
    expect(html).toContain('aria-label="Stop"');
    expect(html).not.toContain('aria-label="Send"');
    expect(html).toContain("lucide-square");
    expect(html).not.toContain("lucide-arrow-up");
  });

  test("busy is false by default — a plain Composer is never in the stop state", () => {
    expect(renderToStaticMarkup(<Composer />)).not.toContain('aria-label="Stop"');
  });
});
