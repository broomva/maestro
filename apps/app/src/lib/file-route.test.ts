/// <reference types="bun" />

// activeFilePath (BRO-1890 FID-4) — the "/file/$path" route decoder that both the shell layout and the
// tab strip read on EVERY render. The load-bearing guarantee: it NEVER throws. An unguarded
// decodeURIComponent on a malformed percent-escape would take down the whole shell (the shell layout
// has no error boundary of its own — a render throw blanks the chrome). Anti-vacuity: the malformed
// cases would each throw a URIError under a naive `decodeURIComponent(slice)`, so a passing test here
// is a real proof the guard is present, not a tautology.

import { describe, expect, test } from "bun:test";
import { activeFilePath } from "./file-route";

describe("activeFilePath", () => {
  test("returns null off the /file/ route", () => {
    expect(activeFilePath("/")).toBeNull();
    expect(activeFilePath("/session/abc")).toBeNull();
    expect(activeFilePath("/filery")).toBeNull(); // prefix must be exactly "/file/"
  });

  test("decodes a well-formed path", () => {
    expect(activeFilePath("/file/hawthorne/spec")).toBe("hawthorne/spec");
    expect(activeFilePath("/file/a%20b/c")).toBe("a b/c"); // percent-decoded
    expect(activeFilePath("/file/")).toBe(""); // empty splat, not a crash
  });

  test("never throws on a malformed percent-escape — falls back to the raw slice", () => {
    // Each of these makes a naive decodeURIComponent throw a URIError; the guard must swallow it.
    expect(() => activeFilePath("/file/foo%")).not.toThrow();
    expect(activeFilePath("/file/foo%")).toBe("foo%");
    expect(activeFilePath("/file/%zz")).toBe("%zz");
    expect(activeFilePath("/file/bad%E0%A4")).toBe("bad%E0%A4");
  });
});
