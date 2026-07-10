/// <reference types="bun" />
// Adversarial fixtures for the check:icons audit (BRO-1797). Every case here would have passed the
// original fail-open gate that P20 flagged — the point of the test is that they now FAIL.

import { describe, expect, test } from "bun:test";
import {
  glyphViolations,
  importSpecifiers,
  isForbiddenIconImport,
  packageOf,
} from "./check-icons.ts";

describe("packageOf", () => {
  test("resolves scoped and bare package names, ignores subpaths", () => {
    expect(packageOf("lucide-react")).toBe("lucide-react");
    expect(packageOf("lucide-react/dynamic")).toBe("lucide-react");
    expect(packageOf("@tabler/icons-react/dist/x")).toBe("@tabler/icons-react");
  });
  test("returns null for local (relative/absolute) imports", () => {
    expect(packageOf("./glyphs/blackhole")).toBeNull();
    expect(packageOf("../../packages/ui/src/icons")).toBeNull();
    expect(packageOf("/abs/path")).toBeNull();
    expect(packageOf("")).toBeNull();
  });
});

describe("isForbiddenIconImport — mixed-library guard (whitelist-of-one)", () => {
  test("allows lucide-react and its subpaths", () => {
    expect(isForbiddenIconImport("lucide-react")).toBe(false);
    expect(isForbiddenIconImport("lucide-react/dynamic")).toBe(false);
  });
  test("allows local glyph imports and ordinary non-icon packages", () => {
    expect(isForbiddenIconImport("./blackhole")).toBe(false);
    expect(isForbiddenIconImport("@maestro/ui")).toBe(false);
    expect(isForbiddenIconImport("react")).toBe(false);
    expect(isForbiddenIconImport("class-variance-authority")).toBe(false);
    expect(isForbiddenIconImport("@tanstack/react-router")).toBe(false);
  });
  test("catches every icon library whose name contains 'icon(s)' — the ones the old denylist listed", () => {
    for (const lib of [
      "@heroicons/react",
      "react-icons",
      "@radix-ui/react-icons",
      "@tabler/icons-react",
      "@phosphor-icons/react",
      "@ant-design/icons",
      "react-bootstrap-icons",
      "@mui/icons-material",
      "boxicons",
      "@iconify/react",
    ]) {
      expect(isForbiddenIconImport(lib)).toBe(true);
    }
  });
  test("catches icon libraries the OLD 12-entry denylist missed (the P20 major)", () => {
    for (const lib of [
      "phosphor-react", //         old name of @phosphor-icons/react — no "icon" in the name
      "react-feather", //          no "icon" in the name
      "lucide", //                 the non-react core, not our React binding
      "@primer/octicons-react", // "octicons" contains "icons"
      "@carbon/icons-react",
      "@fluentui/react-icons",
      "iconoir-react",
      "@remixicon/react",
      "@fortawesome/fontawesome-svg-core", // family prefix, name has no "icon"
    ]) {
      expect(isForbiddenIconImport(lib)).toBe(true);
    }
  });
});

describe("importSpecifiers — covers every import form", () => {
  test("static, side-effect, dynamic, and require specifiers are all collected", () => {
    const src = [
      'import { Home } from "lucide-react";',
      'import "boxicons/css/boxicons.min.css";', // side-effect — the documented boxicons usage
      'const M = await import("@carbon/icons-react");',
      'const F = require("react-feather");',
    ].join("\n");
    const specs = importSpecifiers(src);
    expect(specs).toContain("lucide-react");
    expect(specs).toContain("boxicons/css/boxicons.min.css");
    expect(specs).toContain("@carbon/icons-react");
    expect(specs).toContain("react-feather");
    // and the guard flags the three forbidden ones
    expect(specs.filter(isForbiddenIconImport).sort()).toEqual(
      ["@carbon/icons-react", "boxicons/css/boxicons.min.css", "react-feather"].sort(),
    );
  });
});

describe("glyphViolations — custom-glyph conventions", () => {
  const OK =
    '<svg stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="none"><path d="M0 0"/></svg>';
  test("a canon-conforming glyph has no violations", () => {
    expect(glyphViolations(OK)).toEqual([]);
  });
  test("non-svg source is ignored (barrel/index)", () => {
    expect(glyphViolations('export { Blackhole } from "./blackhole";')).toEqual([]);
  });

  // The stroke-width fail-open major: "24"/"20"/"2.5"/{24}/{2.5} used to pass.
  test.each([
    ['stroke-width="24"', "24"],
    ['stroke-width="20"', "20"],
    ['stroke-width="2.5"', "2.5"],
    ["strokeWidth={24}", "{24}"],
    ["strokeWidth={2.5}", "{2.5}"],
  ])("rejects stroke-width %s (leading-2 fail-open)", (attr) => {
    const src = `<svg stroke="currentColor" ${attr} stroke-linecap="round" fill="none"><path/></svg>`;
    expect(glyphViolations(src).some((v) => v.includes("stroke-width"))).toBe(true);
  });
  test("accepts exact stroke-width in quote and both brace forms", () => {
    for (const attr of ['stroke-width="2"', "strokeWidth={2}", 'strokeWidth={"2"}']) {
      const src = `<svg stroke="currentColor" ${attr} stroke-linecap="round" fill="none"><path/></svg>`;
      expect(glyphViolations(src).some((v) => v.includes("stroke-width"))).toBe(false);
    }
  });
  test("catches a multi-path glyph where one path breaks canon (not just 'a 2 appears')", () => {
    const src = `<svg stroke="currentColor" stroke-linecap="round" fill="none"><path stroke-width="2"/><path stroke-width="4"/></svg>`;
    expect(glyphViolations(src).some((v) => v.includes("stroke-width"))).toBe(true);
  });

  test("requires currentColor", () => {
    const src =
      '<svg stroke="#111827" stroke-width="2" stroke-linecap="round" fill="none"><path/></svg>';
    expect(glyphViolations(src).some((v) => v.includes("currentColor"))).toBe(true);
  });
  test("rejects a hard-coded fill in quote and brace forms, allows none/currentColor", () => {
    const hex = `<svg stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="#ff0000"><path/></svg>`;
    const brace = `<svg stroke="currentColor" stroke-width="2" stroke-linecap="round" fill={"#ff0000"}><path/></svg>`;
    expect(glyphViolations(hex).some((v) => v.includes("fill"))).toBe(true);
    expect(glyphViolations(brace).some((v) => v.includes("fill"))).toBe(true);
    // theme-safe fills are allowed
    const cc = `<svg stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="currentColor"><path/></svg>`;
    expect(glyphViolations(cc).some((v) => v.includes("fill"))).toBe(false);
  });
  test("does not mistake data-fill / fill-rule for a fill color (false-positive fix)", () => {
    const src = `<svg stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="none" data-fill="x" fill-rule="evenodd"><path/></svg>`;
    expect(glyphViolations(src).some((v) => v.includes("fill"))).toBe(false);
  });
  test("accepts round caps in brace form", () => {
    const src = `<svg stroke="currentColor" stroke-width="2" strokeLinecap={"round"} fill="none"><path/></svg>`;
    expect(glyphViolations(src).some((v) => v.includes("line caps"))).toBe(false);
  });
});
