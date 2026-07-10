/// <reference types="bun" />
// check:icons (BRO-1797) — the icon-strategy audit.
// Run it with `bun run --filter @maestro/app check:icons` (bun's --filter matches the package
// NAME `@maestro/app`, not the dir — a bare `--filter app` errors "No packages matched" and
// exits 1; the BRO-1782 gotcha), or from apps/app/ as `bun run check:icons`.
//
// Enforces the pinned icon strategy (CLAUDE.md §Icons): exactly ONE icon library — lucide-react —
// plus hand-drawn glyphs kept as local SVG components in `packages/ui/src/icons/`. Two hard checks:
//   1. NO MIXED LIBRARIES — no import from any icon package other than lucide-react, anywhere in
//      the app or ui src. Enforced as a whitelist-of-one (lucide-react is the only allowed icon
//      package), not a hand-maintained denylist — a denylist fails open on the next new icon set.
//   2. CUSTOM-GLYPH CONVENTIONS — every glyph under `packages/ui/src/icons/` draws with
//      `currentColor` + `stroke-width` exactly 2 + round caps, and never hard-codes a fill color.
//      Dormant until BRO-1766 populates that dir, then it holds every glyph to canon.
//
// The size ladder (20 / 16 / 24) is a per-usage prop convention Lucide already defaults sanely and
// is a design-review concern, not statically enforced here. This script owns the machine-checkable
// invariants (single library + conforming custom glyphs). Import scanning is regex-based, not a
// full parser — it covers static / side-effect / dynamic / require forms, which is every form an
// icon library realistically arrives through, but is not an AST and does not claim to be.

import { existsSync } from "node:fs";
import { Glob } from "bun";

// cwd is apps/app. Audit the app AND the shared component library.
const ROOTS = ["src", "../../packages/ui/src"];
const ICONS_DIR = "../../packages/ui/src/icons";

// The ONE allowed icon library (canon: CLAUDE.md §Icons). lucide-react and its subpaths only.
const ALLOWED_ICON_LIB = "lucide-react";

// Icon libraries whose PACKAGE NAME does not contain "icon"/"icons", so the /icons?/i heuristic
// below can't see them — enumerated explicitly. (Anything whose name DOES contain "icon" — the vast
// majority: @heroicons/react, react-icons, @tabler/icons-react, @phosphor-icons/react, boxicons,
// @primer/octicons-react, @carbon/icons-react, @fluentui/react-icons, iconoir-react, … — is caught
// generically and never needs listing here.)
const FORBIDDEN_STRAGGLERS = new Set([
  "phosphor-react", // old package name of @phosphor-icons/react
  "react-feather",
  "lucide", // the framework-agnostic core — not the React binding we standardize on
]);

interface Violation {
  file: string;
  detail: string;
}

/** Resolve an import specifier to its package name, or null for a local (relative/absolute) import. */
export function packageOf(spec: string): string | null {
  if (spec === "" || spec.startsWith(".") || spec.startsWith("/")) return null;
  const parts = spec.split("/");
  return spec.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
}

/** True if an import specifier pulls from a forbidden icon library (anything but lucide-react). */
export function isForbiddenIconImport(spec: string): boolean {
  const pkg = packageOf(spec);
  if (pkg === null) return false; // local glyph import — allowed
  if (pkg === ALLOWED_ICON_LIB) return false; // the one allowed library (incl. subpaths)
  if (FORBIDDEN_STRAGGLERS.has(pkg)) return true;
  if (pkg.startsWith("@fortawesome/")) return true; // a family whose names don't all contain "icon"
  return /icons?/i.test(pkg); // whitelist-of-one: any other icon-named package is forbidden
}

// Every module-specifier form a package can arrive through. We only need the SET of specifiers, so
// mis-attributing which import a specifier belongs to is harmless — each is tested independently.
const SPECIFIER_RES = [
  /\b(?:import|export)\b[^;]*?\bfrom\s*["']([^"']+)["']/g, // static import/export ... from "x"
  /(?:^|[\s;])import\s*["']([^"']+)["']/gm, //                 side-effect import "x"
  /\bimport\s*\(\s*["']([^"']+)["']\s*\)/g, //                 dynamic import("x")
  /\brequire\s*\(\s*["']([^"']+)["']\s*\)/g, //                cjs require("x")
];

/** Collect every import/require specifier in a source file (deduped). */
export function importSpecifiers(src: string): string[] {
  const out = new Set<string>();
  for (const re of SPECIFIER_RES) {
    re.lastIndex = 0;
    let m = re.exec(src);
    while (m !== null) {
      out.add(m[1]);
      m = re.exec(src);
    }
  }
  return [...out];
}

/** Convention violations for a single custom-glyph source (SVG/TSX). Pure — used by the test. */
export function glyphViolations(src: string): string[] {
  const out: string[] = [];
  if (!/<svg[\s>]/i.test(src)) return out; // a barrel/index without inline svg is fine

  // currentColor must appear (theme-safe stroke/fill). Accept either case of the CSS keyword.
  if (!/currentColor/i.test(src)) {
    out.push('glyph must draw with "currentColor" (no hard-coded color)');
  }

  // No hard-coded fill color. `fill="none"` and `fill="currentColor"` are theme-safe and allowed; a
  // hex / rgb / named color is not. Handle both "quote" and {brace} JSX forms; the (?<![\w-]) left
  // boundary keeps `data-fill=` / `fill-rule=` from being read as `fill=`.
  for (const m of src.matchAll(/(?<![\w-])fill\s*=\s*(\{[^}]*\}|["'][^"']*["'])/gi)) {
    const val = m[1].replace(/[{}"'\s]/g, "").toLowerCase();
    if (val !== "none" && val !== "currentcolor") {
      out.push(
        `glyph must not hard-code a fill color (found fill=${m[1]}; use "none" or "currentColor")`,
      );
      break;
    }
  }

  // stroke-width must be EXACTLY 2 on every occurrence — anchored so "24", "20", "2.5", {24} do not
  // pass, and every path is checked (not just "a 2 appears somewhere").
  const strokes = [
    ...src.matchAll(/(?:stroke-width|strokeWidth)\s*=\s*(\{[^}]*\}|["'][^"']*["'])/g),
  ];
  if (strokes.length === 0) {
    out.push('glyph must set stroke-width="2"');
  } else {
    for (const m of strokes) {
      const val = m[1].replace(/[{}"'\s]/g, "");
      if (val !== "2") {
        out.push(`glyph stroke-width must be exactly 2 (found ${m[1]})`);
        break;
      }
    }
  }

  // Round line caps — presence test, both "quote" and {brace} forms.
  if (
    !/(?:stroke-linecap|strokeLinecap)\s*=\s*(?:["']round["']|\{\s*["']round["']\s*\})/.test(src)
  ) {
    out.push('glyph must use round line caps (stroke-linecap="round")');
  }
  return out;
}

async function tsFiles(root: string): Promise<string[]> {
  const out: string[] = [];
  const glob = new Glob("**/*.{ts,tsx}");
  for await (const rel of glob.scan({ cwd: root, onlyFiles: true })) {
    if (rel.includes("node_modules")) continue;
    out.push(`${root}/${rel}`);
  }
  return out;
}

async function checkNoMixedLibraries(): Promise<Violation[]> {
  const violations: Violation[] = [];
  for (const root of ROOTS) {
    for (const file of await tsFiles(root)) {
      const src = await Bun.file(file).text();
      for (const spec of importSpecifiers(src)) {
        if (isForbiddenIconImport(spec)) {
          violations.push({
            file,
            detail: `imports from a forbidden icon library "${spec}" (lucide-react is the only allowed icon package; hand-drawn glyphs go in packages/ui/src/icons)`,
          });
        }
      }
    }
  }
  return violations;
}

async function checkCustomGlyphConventions(): Promise<Violation[]> {
  const violations: Violation[] = [];
  // Dormant until BRO-1766 creates the local-glyph dir; then it holds every glyph to canon.
  if (!existsSync(ICONS_DIR)) return violations;
  const glob = new Glob("**/*.{tsx,svg}");
  for await (const rel of glob.scan({ cwd: ICONS_DIR, onlyFiles: true })) {
    const file = `${ICONS_DIR}/${rel}`;
    const src = await Bun.file(file).text();
    for (const detail of glyphViolations(src)) violations.push({ file, detail });
  }
  return violations;
}

// Only scan when run directly — importing this module (e.g. from the test) must NOT trigger the
// filesystem scan or process.exit.
if (import.meta.main) {
  const [mixed, glyphs] = await Promise.all([
    checkNoMixedLibraries(),
    checkCustomGlyphConventions(),
  ]);
  const all = [...mixed, ...glyphs];
  if (all.length > 0) {
    console.error(`check:icons — ${all.length} violation(s):`);
    for (const v of all) console.error(`  ✗ ${v.file}: ${v.detail}`);
    process.exit(1);
  }
  console.log("check:icons — ok (single icon library: lucide-react; custom glyphs conform)");
}
