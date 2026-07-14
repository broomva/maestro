// The ⌘K command palette (FID-7 · BRO-1894) — LiveCommand.jsx `MccCommandPalette`, ported honest.
// A glass overlay anchored under the top bar's command field: type to filter, arrow keys to move, Enter
// to run, Esc to close (and return focus to whatever was focused when it opened). One of the exactly-
// three glass places (CLAUDE.md §Glass). The scrim + combo carry the inner light line.
//
// SPLIT (the store/router-read → pure-view pattern the arc keeps relearning): `CommandPaletteView` is
// pure markup, unit-tested directly (renderToStaticMarkup) with the §Voice guards; `CommandPalette` is
// the stateful container (query, keyboard, positioning, focus, portal) proven in the browser (pw). The
// container portals to <body>, so it is NEVER server-rendered — it returns null before the portal call
// when closed, which keeps the shell's SSR render (shell.test.tsx) safe.

import { useNavigate } from "@tanstack/react-router";
import { Search } from "lucide-react";
import { Fragment, type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { toggleTheme } from "@/theme";
import {
  type Command,
  type CommandGroupView,
  filterCommands,
  groupCommands,
  markMatch,
} from "./commands";

/** The anchored position of the combo under the command field. */
interface Rect {
  left: number;
  top: number;
  width: number;
}

interface CommandPaletteViewProps {
  query: string;
  onQueryChange: (q: string) => void;
  groups: CommandGroupView[];
  /** id of the virtually-focused (active) command, or null when the list is empty. */
  activeId: string | null;
  onActivate: (id: string) => void;
  onRun: (cmd: Command) => void;
  onKeyDown: (e: React.KeyboardEvent) => void;
  onScrimDown: () => void;
  inputRef?: React.Ref<HTMLInputElement>;
  comboRef?: React.Ref<HTMLDivElement>;
  style?: React.CSSProperties;
}

/** Render one title with its matched substring wrapped in `<em>` (the highlight). */
function TitleWithMatch({ title, query }: { title: string; query: string }): ReactNode {
  return markMatch(title, query).map((seg, i) =>
    seg.hit ? (
      // Segments are positional within a single title render — index is a stable key here.
      // biome-ignore lint/suspicious/noArrayIndexKey: positional text segments, stable per render
      <em key={`h${i}`}>{seg.text}</em>
    ) : (
      // biome-ignore lint/suspicious/noArrayIndexKey: positional text segments, stable per render
      <Fragment key={`t${i}`}>{seg.text}</Fragment>
    ),
  );
}

/** Pure presentational combo — no state, no portal. This is what the unit tests render. */
export function CommandPaletteView({
  query,
  onQueryChange,
  groups,
  activeId,
  onActivate,
  onRun,
  onKeyDown,
  onScrimDown,
  inputRef,
  comboRef,
  style,
}: CommandPaletteViewProps) {
  return (
    <>
      {/* Decorative dismiss backdrop (aria-hidden): keyboard users close with Esc; every command is reachable. */}
      <div className="cmdk-scrim" aria-hidden="true" onMouseDown={onScrimDown} />
      <div
        className="cmdk-combo"
        ref={comboRef}
        style={style}
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
      >
        <div className="cmdk-input-row">
          <Search size={17} />
          <input
            ref={inputRef}
            className="cmdk-input"
            placeholder="Find a page or run a command…"
            value={query}
            onChange={(e) => onQueryChange(e.target.value)}
            onKeyDown={onKeyDown}
            role="combobox"
            aria-expanded="true"
            aria-controls="cmdk-listbox"
            aria-activedescendant={activeId ?? undefined}
            aria-label="Find a page or run a command"
          />
          <span className="cmdk-esc">esc</span>
        </div>

        <div className="cmdk-results" id="cmdk-listbox" role="listbox" aria-label="Commands">
          {groups.length === 0 ? (
            <div className="cmdk-empty">No matches. Try a page name or a command.</div>
          ) : null}
          {groups.map((grp) => (
            <Fragment key={grp.label}>
              <div className="cmdk-group-label">{grp.label}</div>
              {grp.items.map((cmd) => {
                const Icon = cmd.icon;
                const isActive = cmd.id === activeId;
                return (
                  <button
                    key={cmd.id}
                    id={cmd.id}
                    type="button"
                    role="option"
                    aria-selected={isActive}
                    className={`cmdk-item${isActive ? " is-active" : ""}`}
                    onMouseEnter={() => onActivate(cmd.id)}
                    onClick={() => onRun(cmd)}
                  >
                    <span className={`cmdk-ic${cmd.accent ? " cmdk-ic--accent" : ""}`}>
                      <Icon size={15} />
                    </span>
                    <span className="cmdk-item-body">
                      <span className="cmdk-item-title">
                        <TitleWithMatch title={cmd.title} query={query} />
                      </span>
                      <span className="cmdk-item-meta">{cmd.meta}</span>
                    </span>
                    <span className="cmdk-item-right">
                      {cmd.kbd ? <span className="cmdk-kbd">{cmd.kbd}</span> : null}
                      <span className="cmdk-enter">↵</span>
                    </span>
                  </button>
                );
              })}
            </Fragment>
          ))}
        </div>

        <div className="cmdk-foot">
          <span className="cmdk-foot-hint">
            <span className="cmdk-foot-kbd">↑↓</span> navigate
          </span>
          <span className="cmdk-foot-hint">
            <span className="cmdk-foot-kbd">↵</span> open
          </span>
          <span className="cmdk-foot-hint">
            <span className="cmdk-foot-kbd">esc</span> close
          </span>
          <span className="cmdk-foot-spacer" />
          <span className="cmdk-foot-brand">
            <span className="bv-dot-live" style={{ width: 9, height: 9 }} /> maestro
          </span>
        </div>
      </div>
    </>
  );
}

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  /** Run the "Send feedback" command — the host opens the drawer. */
  onOpenFeedback: () => void;
}

/** Stateful container: keyboard + positioning + focus lifecycle, portaled to <body>. */
export function CommandPalette({ open, onClose, onOpenFeedback }: CommandPaletteProps) {
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const [rect, setRect] = useState<Rect | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const comboRef = useRef<HTMLDivElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  const groups = useMemo(() => groupCommands(filterCommands(query)), [query]);
  const flat = useMemo(() => groups.flatMap((g) => g.items), [groups]);

  // Keep the active index inside the (possibly shrunk) filtered list.
  useEffect(() => {
    setActiveIndex((a) => Math.min(a, Math.max(0, flat.length - 1)));
  }, [flat.length]);

  // Position the combo under the top bar's command field; fall back to top-center if no anchor is
  // mounted on the current route (the field is shell chrome, so it is present on every route).
  const place = useCallback(() => {
    const anchor = document.querySelector("[data-cmdk-anchor]");
    if (anchor) {
      const r = anchor.getBoundingClientRect();
      const width = Math.max(r.width, 440);
      let left = r.left + (r.width - width) / 2;
      left = Math.max(10, Math.min(left, window.innerWidth - width - 10));
      setRect({ left, top: r.bottom + 6, width });
    } else {
      const width = 480;
      setRect({ left: (window.innerWidth - width) / 2, top: 66, width });
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    // Capture the trigger so Esc / close can return focus to it (WAI-ARIA dialog).
    restoreFocusRef.current = document.activeElement as HTMLElement | null;
    setQuery("");
    setActiveIndex(0);
    place();
    const t = setTimeout(() => inputRef.current?.focus(), 30);
    const onReflow = () => place();
    window.addEventListener("resize", onReflow, true);
    window.addEventListener("scroll", onReflow, true);
    return () => {
      clearTimeout(t);
      window.removeEventListener("resize", onReflow, true);
      window.removeEventListener("scroll", onReflow, true);
      restoreFocusRef.current?.focus?.();
    };
  }, [open, place]);

  if (!open) return null;

  const run = (cmd: Command) => {
    if (cmd.to) {
      onClose();
      navigate({ to: cmd.to });
      return;
    }
    if (cmd.action === "toggle-theme") {
      toggleTheme();
      onClose();
      return;
    }
    if (cmd.action === "open-feedback") {
      onClose();
      onOpenFeedback();
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((a) => Math.min(a + 1, flat.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((a) => Math.max(a - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const cmd = flat[activeIndex];
      if (cmd) run(cmd);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "Tab") {
      // Trap focus: real focus stays on the input (the active option is virtual, via
      // aria-activedescendant), so Tab must not move focus out to the app behind the scrim.
      e.preventDefault();
    }
  };

  const activeId = flat[activeIndex]?.id ?? null;

  return createPortal(
    <CommandPaletteView
      query={query}
      onQueryChange={(q) => {
        setQuery(q);
        setActiveIndex(0);
      }}
      groups={groups}
      activeId={activeId}
      onActivate={(id) => {
        const i = flat.findIndex((c) => c.id === id);
        if (i >= 0) setActiveIndex(i);
      }}
      onRun={run}
      onKeyDown={onKeyDown}
      onScrimDown={onClose}
      inputRef={inputRef}
      comboRef={comboRef}
      style={rect ? { left: rect.left, top: rect.top, width: rect.width } : { left: -9999, top: 0 }}
    />,
    document.body,
  );
}
