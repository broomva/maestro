/// <reference types="bun" />
// check:icons (BRO-1797) — the icon-strategy audit, run as `bun run --filter app check:icons`.
//
// Enforces the pinned icon strategy (production-notes §3, CLAUDE.md §Icons): exactly ONE icon
// library — lucide-react — plus hand-drawn glyphs kept as local SVG components in
// `packages/ui/src/icons/`. Two hard checks:
//   1. NO MIXED LIBRARIES — no import from any other icon package anywhere in the app or ui src.
//   2. CUSTOM-GLYPH CONVENTIONS — every glyph under `packages/ui/src/icons/` draws with
//      `currentColor` + `stroke-width="2"` + round caps (never a hard-coded hex, never a fill
//      icon). This gate is dormant until BRO-1766 populates that dir, then holds every glyph to canon.
//
// The Lucide size ladder (20 / 16 / 24) + stroke are per-usage props Lucide already defaults to
// canon (strokeWidth 2); they are a design-review convention, not statically enforced here — this
// script owns the machine-checkable invariants (single library + conforming custom glyphs).

import { existsSync } from "node:fs";
import { Glob } from "bun";

// cwd is apps/app (the `--filter app` script). Audit the app AND the shared component library.
const ROOTS = ["src", "../../packages/ui/src"];
const ICONS_DIR = "../../packages/ui/src/icons";

// Any import from one of these is a mixed-library violation (denylist — reliable, no false
// positives on ordinary non-icon imports the way an allowlist would).
const FORBIDDEN_ICON_LIBS = [
  "@heroicons/react",
  "react-icons",
  "@radix-ui/react-icons",
  "@tabler/icons-react",
  "react-feather",
  "@fortawesome/",
  "@phosphor-icons/react",
  "@ant-design/icons",
  "react-bootstrap-icons",
  "@mui/icons-material",
  "boxicons",
  "@iconify/react",
];

const IMPORT_RE = /(?:import|export)[\s\S]*?from\s*["']([^"']+)["']/g;

interface Violation {
  file: string;
  detail: string;
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
      IMPORT_RE.lastIndex = 0;
      let m: RegExpExecArray | null = IMPORT_RE.exec(src);
      while (m !== null) {
        const source = m[1];
        const bad = FORBIDDEN_ICON_LIBS.find((lib) => source === lib || source.startsWith(lib));
        if (bad) {
          violations.push({
            file,
            detail: `imports from a forbidden icon library "${source}" (use lucide-react or a local glyph in packages/ui/src/icons)`,
          });
        }
        m = IMPORT_RE.exec(src);
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
    if (!/<svg[\s>]/i.test(src)) continue; // a barrel/index without inline svg is fine
    if (!src.includes("currentColor")) {
      violations.push({
        file,
        detail: 'glyph must stroke/fill with "currentColor" (no hard-coded color)',
      });
    }
    if (/fill\s*=\s*["'](?!none)(?!currentColor)[^"']+["']/i.test(src)) {
      violations.push({
        file,
        detail:
          "glyph must not use a hard-coded fill (Broomva icons are stroke-only, currentColor)",
      });
    }
    if (!/stroke-width\s*=\s*["']?2["']?|strokeWidth\s*=\s*[{"']?2/.test(src)) {
      violations.push({ file, detail: 'glyph must use stroke-width="2"' });
    }
    if (!/stroke-linecap\s*=\s*["']round["']|strokeLinecap\s*=\s*["']round["']/.test(src)) {
      violations.push({ file, detail: 'glyph must use round line caps (stroke-linecap="round")' });
    }
  }
  return violations;
}

const mixed = await checkNoMixedLibraries();
const glyphs = await checkCustomGlyphConventions();
const all = [...mixed, ...glyphs];

if (all.length > 0) {
  console.error(`check:icons — ${all.length} violation(s):`);
  for (const v of all) console.error(`  ✗ ${v.file}: ${v.detail}`);
  process.exit(1);
}
console.log("check:icons — ok (single icon library: lucide-react; custom glyphs conform)");
