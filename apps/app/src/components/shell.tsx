import { Avatar, BlackholeMark, cn, DotComet } from "@maestro/ui";
import { Boxes, FileText, History, type LucideIcon, Settings } from "lucide-react";
import type { ReactNode } from "react";
import { ThemeToggle } from "./theme-toggle";

/**
 * Shell — the chrome the whole product lives in (BRO-1771, BUILD-PLAN §M2, CLAUDE.md §Layout).
 * 200px fixed matte sidebar + 52px top bar + flex main. **The shell never scrolls — only the
 * inner panels do** (the sidebar and the main both own their overflow; the shell is h-dvh +
 * overflow-hidden). Real routing lands the router into `children` in BRO-1824; until then the
 * `/app` route renders the placeholder below.
 */

const NAV: { icon: LucideIcon; label: string; active?: boolean }[] = [
  { icon: Boxes, label: "Board", active: true },
  { icon: FileText, label: "Knowledge" },
  { icon: History, label: "History" },
  { icon: Settings, label: "Settings" },
];

function NavItem({ icon: Icon, label, active }: (typeof NAV)[number]) {
  return (
    <button
      type="button"
      aria-current={active ? "page" : undefined}
      className={cn(
        "flex h-9 items-center gap-2 rounded-row px-2.5 text-left text-sm transition-colors",
        active
          ? "bg-[var(--bv-frost-8)] font-medium text-foreground"
          : "text-muted-foreground hover:bg-[var(--bv-frost-8)] hover:text-foreground",
      )}
    >
      <Icon size={16} strokeWidth={2} className="shrink-0" />
      <span className="flex-1 truncate">{label}</span>
    </button>
  );
}

/**
 * The blackhole brand mark on its own dark cool-axis chip (BUILD-PLAN §M2: the mark rides a
 * dark/frosted surface, never a bare tile). The glyph is the shared `BlackholeMark` from
 * `@maestro/ui` (BRO-1766 extracted it out of this inline SVG) — not the raster asset, which was
 * an opaque baseline JPEG that painted a pure-#000 square on the light sidebar (BRO-1771 P20).
 * `--bv-ink` is the barely-blue near-black (stable across themes, cool-axis, never #000 per
 * CLAUDE.md §Color), so the chip stays a consistent dark brand tile in light and dark; the chip's
 * `text-[var(--bv-white)]` sets the mark's `currentColor` to brand-white.
 */
function BrandMark() {
  return (
    <span
      data-testid="brand-mark"
      className="grid size-6 shrink-0 place-items-center rounded-lg bg-[var(--bv-ink)] text-[var(--bv-white)] ring-1 ring-[var(--bv-border-15)] ring-inset"
    >
      <BlackholeMark size={16} />
    </span>
  );
}

/**
 * The orchestrator's presence — an agent, not a settings button. The tidepool DotComet is the
 * presence signal; the chip opens the orchestrator's session (wired in a later milestone).
 */
function PresenceChip() {
  return (
    <button
      type="button"
      title="Open the orchestrator"
      className="inline-flex items-center gap-2 rounded-row px-2 py-1 text-left transition-colors hover:bg-[var(--bv-frost-8)]"
    >
      <DotComet size={13} />
      <span className="font-medium text-sm">maestro</span>
      <span className="text-muted-foreground text-xs">standing</span>
    </button>
  );
}

const PLACEHOLDER_ROWS = Array.from({ length: 40 }, (_, i) => `Panel row ${i + 1}`);

function ShellPlaceholder() {
  return (
    <div className="flex flex-col gap-3">
      <h1 className="text-h2">The shell</h1>
      <p className="max-w-[520px] text-muted-foreground text-sm">
        200px sidebar, 52px top bar, flex main. The shell never scrolls; this panel does. The board,
        knowledge, history, and settings surfaces mount here as routing lands.
      </p>
      {/* Tall filler so the main panel scrolls while the chrome holds (M2 verify). */}
      <div className="flex flex-col gap-2">
        {PLACEHOLDER_ROWS.map((label) => (
          <div
            key={label}
            className="rounded-card border border-border bg-card px-3.5 py-3 text-sm"
          >
            {label}
          </div>
        ))}
      </div>
    </div>
  );
}

export function Shell({ children }: { children?: ReactNode }) {
  return (
    <div className="grid h-dvh grid-cols-[200px_1fr] overflow-hidden bg-background text-foreground">
      <aside className="flex flex-col gap-1 overflow-y-auto border-border border-r bg-sidebar px-2 py-3">
        <button
          type="button"
          title="Switch workspace"
          className="flex items-center gap-2 rounded-row px-1.5 py-1 text-left transition-colors hover:bg-[var(--bv-frost-8)]"
        >
          <BrandMark />
          <span className="flex-1 truncate font-medium text-sm">Broomva</span>
        </button>

        <nav className="mt-1 flex flex-col gap-0.5">
          {NAV.map((item) => (
            <NavItem key={item.label} {...item} />
          ))}
        </nav>

        <div className="flex-1" />

        <button
          type="button"
          className="flex h-9 items-center gap-2 rounded-row px-1.5 text-left text-sm transition-colors hover:bg-[var(--bv-frost-8)]"
        >
          <Avatar name="Ana Diaz" size={22} color="var(--bv-gray-600)" />
          <span className="flex-1 truncate">Ana Diaz</span>
        </button>
      </aside>

      <div className="grid grid-rows-[52px_1fr] overflow-hidden">
        <header className="flex h-[52px] shrink-0 items-center justify-between border-border border-b px-4">
          <PresenceChip />
          <ThemeToggle />
        </header>
        <main className="overflow-y-auto p-6" data-testid="shell-main">
          {children ?? <ShellPlaceholder />}
        </main>
      </div>
    </div>
  );
}
