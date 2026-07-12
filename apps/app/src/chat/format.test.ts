/// <reference types="bun" />
// format.test.ts (BRO-1826 M4, slice B) — the pure text helpers. The tokenizer is the trigger for the
// only colored inline element in the feed (the link pill), so its edge behavior (unpaired/empty
// backticks) is pinned here rather than discovered in the browser.

import { describe, expect, test } from "bun:test";
import { emptySessionGreeting, tokenizeAssistantText } from "./format";

describe("tokenizeAssistantText — plain runs + inline link pills", () => {
  test("no backticks → a single text token (the common case stays a plain string)", () => {
    expect(tokenizeAssistantText("Listed the files.")).toEqual([
      { kind: "text", value: "Listed the files." },
    ]);
  });

  test("a backtick span becomes a pill, flanked by its surrounding text", () => {
    expect(tokenizeAssistantText("Judged `run/7c2f1a` clean.")).toEqual([
      { kind: "text", value: "Judged " },
      { kind: "pill", value: "run/7c2f1a" },
      { kind: "text", value: " clean." },
    ]);
  });

  test("multiple pills in one line", () => {
    expect(tokenizeAssistantText("`a` and `b`")).toEqual([
      { kind: "pill", value: "a" },
      { kind: "text", value: " and " },
      { kind: "pill", value: "b" },
    ]);
  });

  test("an unpaired backtick is literal text, never eating the rest of the message", () => {
    expect(tokenizeAssistantText("a `b c")).toEqual([{ kind: "text", value: "a `b c" }]);
  });

  test("an empty span `` stays literal (nothing silently vanishes)", () => {
    expect(tokenizeAssistantText("x``y")).toEqual([
      { kind: "text", value: "x" },
      { kind: "text", value: "``" },
      { kind: "text", value: "y" },
    ]);
  });

  test("empty input → one empty text token", () => {
    expect(tokenizeAssistantText("")).toEqual([{ kind: "text", value: "" }]);
  });
});

describe("emptySessionGreeting", () => {
  test("names the layer, sentence case, no emoji / no Welcome", () => {
    const g = emptySessionGreeting("inbox");
    expect(g).toBe("A fresh session on inbox");
    expect(g).not.toMatch(/welcome/i);
  });
  test("falls back to a bare greeting when no layer is given", () => {
    expect(emptySessionGreeting()).toBe("A fresh session");
    expect(emptySessionGreeting("   ")).toBe("A fresh session");
  });
});
