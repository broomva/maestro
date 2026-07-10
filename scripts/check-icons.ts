/// <reference types="bun" />
// check:icons — the monorepo icon-strategy audit (BRO-1797 · BRO-1766).
//
// ONE shared audit for the whole monorepo. Thin per-package entry points delegate here so the
// logic lives in a single place (no duplication, no drift):
//   root         `bun run check:icons`                        → --scope all  (both packages)
//   apps/app     `bun run --filter @maestro/app check:icons`  → --scope app
//   packages/ui  `bun run --filter @maestro/ui  check:icons`  → --scope ui
// (bun's --filter matches the package NAME — `@maestro/app` / `@maestro/ui`, not the dir — so a
//  bare `--filter app` / `--filter ui` errors "No packages matched" and exits 1; the BRO-1782
//  gotcha. The done.check writes the bare form; use the qualified names above.)
//
// Two hard checks (design canon: CLAUDE.md §Icons + handoff porting-notes translation-table row 3):
//   1. NO MIXED LIBRARIES — lucide-react is the only allowed icon package, anywhere in app/ui src.
//      Enforced as a whitelist-of-one (not a hand-maintained denylist — a denylist fails open on
//      the next new icon set).
//   2. CUSTOM-GLYPH CONVENTIONS — every glyph under `packages/ui/src/icons/` draws with
//      `currentColor` + `stroke-width` exactly 2 + round caps, and never hard-codes a fill or
//      stroke color. The blackhole BRAND mark is NOT a UI icon (it rides a fixed dark chip in a
//      fixed brand color with a filled singularity), so it lives in `packages/ui/src/brand.tsx`,
//      deliberately outside this audited dir. The check is dormant until a real custom glyph lands.
//
// Import scanning is regex-based (static / side-effect / dynamic / require forms) — every form an
// icon library realistically arrives through — not a full AST, and does not claim to be.

import { existsSync } from "node:fs";
import { join, relative } from "node:path";
import { Glob } from "bun";

const REPO_ROOT = join(import.meta.dir, "..");
const APP_SRC = join(REPO_ROOT, "apps/app/src");
const UI_SRC = join(REPO_ROOT, "packages/ui/src");
const ICONS_DIR = join(UI_SRC, "icons"); // custom UI glyphs live here (canon)

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
  "@mdi/react", // Material Design Icons — no "icon" in the name
  "@mdi/js",
  "css.gg",
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

  // No hard-coded stroke color either — same rule as fill, checked per-occurrence (not a file-wide
  // "currentColor appears somewhere" test, which would fail open on a multi-element glyph). The
  // (?<![\w-]) boundary + bare `stroke=` isolates the stroke color attr from stroke-width /
  // stroke-linecap / strokeWidth / data-stroke, none of which are `stroke` immediately before `=`.
  for (const m of src.matchAll(/(?<![\w-])stroke\s*=\s*(\{[^}]*\}|["'][^"']*["'])/gi)) {
    const val = m[1].replace(/[{}"'\s]/g, "").toLowerCase();
    if (val !== "none" && val !== "currentcolor") {
      out.push(
        `glyph must not hard-code a stroke color (found stroke=${m[1]}; use "currentColor")`,
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
  if (!existsSync(root)) return out;
  const glob = new Glob("**/*.{ts,tsx}");
  for await (const rel of glob.scan({ cwd: root, onlyFiles: true })) {
    if (rel.includes("node_modules")) continue;
    out.push(join(root, rel));
  }
  return out;
}

async function checkNoMixedLibraries(roots: string[]): Promise<Violation[]> {
  const violations: Violation[] = [];
  for (const root of roots) {
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
  // Dormant until a custom glyph lands in packages/ui/src/icons; then it holds every glyph to canon.
  if (!existsSync(ICONS_DIR)) return violations;
  const glob = new Glob("**/*.{tsx,svg}");
  for await (const rel of glob.scan({ cwd: ICONS_DIR, onlyFiles: true })) {
    const file = join(ICONS_DIR, rel);
    const src = await Bun.file(file).text();
    for (const detail of glyphViolations(src)) violations.push({ file, detail });
  }
  return violations;
}

export type Scope = "app" | "ui" | "all";

/** Run the audit for a scope. Custom-glyph conventions apply only when ui is in scope (glyphs live
 *  in packages/ui/src/icons). Returns the full violation list. */
export async function runAudit(scope: Scope): Promise<Violation[]> {
  const roots: string[] = [];
  if (scope === "app" || scope === "all") roots.push(APP_SRC);
  if (scope === "ui" || scope === "all") roots.push(UI_SRC);
  const mixed = await checkNoMixedLibraries(roots);
  const glyphs = scope === "app" ? [] : await checkCustomGlyphConventions();
  return [...mixed, ...glyphs];
}

if (import.meta.main) {
  const i = Bun.argv.indexOf("--scope");
  const arg = i >= 0 ? Bun.argv[i + 1] : "all";
  const scope: Scope = arg === "app" || arg === "ui" ? arg : "all";
  const violations = await runAudit(scope);
  if (violations.length > 0) {
    console.error(`check:icons (${scope}) — ${violations.length} violation(s):`);
    for (const v of violations) console.error(`  ✗ ${relative(REPO_ROOT, v.file)}: ${v.detail}`);
    process.exit(1);
  }
  console.log(
    `check:icons (${scope}) — ok (single icon library: lucide-react; custom glyphs conform)`,
  );
}
