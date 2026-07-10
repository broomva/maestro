/// <reference types="bun" />

// Card suite (BRO-1762). renderToStaticMarkup — the same no-DOM-harness discipline as the
// other primitive suites. The load-bearing invariant: Card is **matte always** (never glass,
// never backdrop-filter), and `running` wraps it in the Undertow without touching the card.

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Card, type CardProps } from "./index";

type Expect<T extends true> = T;
type Equal<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type _Interactive = Expect<Equal<NonNullable<CardProps["interactive"]>, boolean>>;
type _Running = Expect<Equal<NonNullable<CardProps["running"]>, boolean>>;

describe("Card", () => {
  test("is a matte rounded-card with a whisper border and edge shadow at rest", () => {
    const html = renderToStaticMarkup(<Card>work</Card>);
    expect(html).toContain("bg-card");
    expect(html).toContain("rounded-card");
    expect(html).toContain("border-border");
    expect(html).toContain("shadow-[var(--bv-shadow-edge)]");
    expect(html).toContain("work");
  });

  test("is never glass — no backdrop-filter, no pill radius, no glass class", () => {
    const html = renderToStaticMarkup(
      <Card interactive running>
        x
      </Card>,
    );
    expect(html).not.toContain("bv-glass");
    expect(html).not.toContain("backdrop");
    expect(html).not.toContain("rounded-full");
  });

  test("interactive lifts the shadow on hover and shows a pointer — it never scales", () => {
    const html = renderToStaticMarkup(<Card interactive>x</Card>);
    expect(html).toContain("hover:shadow-[var(--bv-shadow-card-hover)]");
    expect(html).toContain("cursor-pointer");
    expect(html).not.toContain("scale");
  });

  test("a non-interactive card has no hover lift and no pointer", () => {
    const html = renderToStaticMarkup(<Card>x</Card>);
    expect(html).not.toContain("hover:shadow");
    expect(html).not.toContain("cursor-pointer");
  });

  test("running wraps the card in the Undertow (orbit + halo); the card stays matte", () => {
    const html = renderToStaticMarkup(<Card running>x</Card>);
    // class-boundary match: `bv-undertow` alone is substring-satisfied by `bv-undertow-orbit`,
    // so assert the wrapper actually carries the halo class (BRO-1762 P20 nit).
    expect(html).toContain('class="bv-undertow"');
    expect(html).toContain("bv-undertow-orbit");
    // the wrapped card is still the matte surface
    expect(html).toContain("bg-card");
  });

  test("a card at rest is not wrapped in the Undertow", () => {
    expect(renderToStaticMarkup(<Card>x</Card>)).not.toContain("bv-undertow");
  });

  test("forwards className and arbitrary div attributes", () => {
    const html = renderToStaticMarkup(
      <Card className="w-64" data-kind="mission">
        x
      </Card>,
    );
    expect(html).toContain("w-64");
    expect(html).toContain('data-kind="mission"');
  });
});
