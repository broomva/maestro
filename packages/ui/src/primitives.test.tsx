/// <reference types="bun" />

// M1 primitives suite (BRO-1752). The repo runner is bun:test (m0.test.ts records
// why: vitest would fragment the runner and skip the existing gate). These render to
// static markup via react-dom/server — no DOM harness, so they run under CI's plain
// monorepo-wide `bun test` with no preload. Structural + a11y + prop-forwarding are
// covered here; hover/focus are CSS pseudo-classes (visual gate: the /kitchen-sink
// Playwright dogfood), and prop-name parity with the .d.ts contracts is compile-checked
// by the type-level assertions below (tsc --noEmit in the ui typecheck).

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import {
  Avatar,
  type AvatarProps,
  Button,
  type ButtonProps,
  IconButton,
  type IconButtonProps,
  Input,
} from "./index";

// ── Type-level parity with design-system/components/core/*.d.ts ──────────────────
type Expect<T extends true> = T;
type Equal<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type _ButtonVariant = Expect<
  Equal<NonNullable<ButtonProps["variant"]>, "primary" | "secondary" | "soft" | "ghost">
>;
type _ButtonSize = Expect<Equal<NonNullable<ButtonProps["size"]>, "sm" | "md" | "lg">>;
// label is required (no `?`); the icon arrives as children.
type _IconButtonLabel = Expect<Equal<IconButtonProps["label"], string>>;
type _AvatarName = Expect<Equal<NonNullable<AvatarProps["name"]>, string>>;
type _AvatarColor = Expect<Equal<NonNullable<AvatarProps["color"]>, string>>;
type _AvatarSize = Expect<Equal<NonNullable<AvatarProps["size"]>, number>>;

describe("Button", () => {
  test("defaults to a primary md pill with an ink fill", () => {
    const html = renderToStaticMarkup(<Button>New mission</Button>);
    expect(html).toContain("rounded-full");
    expect(html).toContain("bg-primary");
    expect(html).toContain("h-9");
    expect(html).toContain('type="button"');
    expect(html).toContain("New mission");
  });

  test("primary lightens on hover; it does not frost or scale", () => {
    const html = renderToStaticMarkup(<Button variant="primary">x</Button>);
    expect(html).toContain("hover:bg-[var(--bv-ink-hover)]");
    expect(html).not.toContain("scale");
  });

  test("secondary / soft / ghost frost blue on hover, never scale", () => {
    for (const variant of ["secondary", "soft", "ghost"] as const) {
      const html = renderToStaticMarkup(<Button variant={variant}>x</Button>);
      expect(html).toContain("hover:bg-[var(--bv-frost");
      expect(html).not.toContain("scale");
    }
  });

  test("size maps to the 28 / 36 / 44px height ladder", () => {
    expect(renderToStaticMarkup(<Button size="sm">x</Button>)).toContain("h-7");
    expect(renderToStaticMarkup(<Button size="md">x</Button>)).toContain("h-9");
    expect(renderToStaticMarkup(<Button size="lg">x</Button>)).toContain("h-11");
  });

  test("disabled is reflected on the element", () => {
    expect(renderToStaticMarkup(<Button disabled>x</Button>)).toContain("disabled");
  });

  test("forwards a type override and merges an extra className", () => {
    const html = renderToStaticMarkup(
      <Button type="submit" className="w-full">
        x
      </Button>,
    );
    expect(html).toContain('type="submit"');
    expect(html).toContain("w-full");
  });
});

describe("IconButton", () => {
  test("is a 36px square button whose label becomes aria-label + title", () => {
    const html = renderToStaticMarkup(
      <IconButton label="Settings">
        <span>i</span>
      </IconButton>,
    );
    expect(html).toContain('aria-label="Settings"');
    expect(html).toContain('title="Settings"');
    expect(html).toContain("h-9");
    expect(html).toContain("w-9");
    expect(html).toContain("rounded-row");
    expect(html).toContain('type="button"');
  });
});

describe("Input", () => {
  test("is a rounded-input text field and adds no focus ring of its own", () => {
    const html = renderToStaticMarkup(<Input placeholder="Prompt" />);
    expect(html).toContain("<input");
    expect(html).toContain("rounded-input");
    expect(html).toContain('placeholder="Prompt"');
    // The ai-blue ring is global :focus-visible — the component must not add one.
    expect(html).not.toContain("ring-");
  });
});

describe("Avatar", () => {
  test("derives up to two uppercased initials", () => {
    const html = renderToStaticMarkup(<Avatar name="Ana Diaz" />);
    expect(html).toContain("AD");
    expect(html).toContain("rounded-full");
  });

  test("a single word yields one initial", () => {
    expect(renderToStaticMarkup(<Avatar name="Broomva" />)).toContain(">B<");
  });

  test("renders an image with alt text when src is given, dropping initials", () => {
    const html = renderToStaticMarkup(<Avatar name="Ana" src="/a.png" />);
    expect(html).toContain('alt="Ana"');
    expect(html).toContain("/a.png");
    expect(html).not.toContain(">A<");
  });
});
