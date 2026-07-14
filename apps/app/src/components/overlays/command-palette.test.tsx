/// <reference types="bun" />

// The ⌘K palette's pure view (BRO-1894 FID-7). renderToStaticMarkup — no DOM harness (the open/keyboard/
// focus behaviour is overlays.pw.ts's browser concern). The container (CommandPalette) portals to <body>,
// so it is NOT server-rendered; this covers the markup + §Voice on the presentational half.

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CommandPaletteView } from "./command-palette";
import { filterCommands, groupCommands } from "./commands";

const noop = () => {};

/** Render the view for a given query (default: all commands, first item active). */
function renderView(query = "", activeId: string | null = "nav-board"): string {
  const groups = groupCommands(filterCommands(query));
  return renderToStaticMarkup(
    <CommandPaletteView
      query={query}
      onQueryChange={noop}
      groups={groups}
      activeId={activeId}
      onActivate={noop}
      onRun={noop}
      onKeyDown={noop}
      onScrimDown={noop}
    />,
  );
}

/** §Voice guards on rendered markup — the machine-checkable half of the plain-language rule. */
function assertVoice(html: string) {
  expect(html).not.toContain("—"); // no em dashes in user-facing copy
  expect(html).not.toContain("%"); // no progress percentages
  expect(html).not.toContain("P1"); // no primitive jargon
  expect(html).not.toContain("primitive");
  expect(html).not.toContain("engine room");
}

describe("CommandPaletteView — the glass combo (BRO-1894)", () => {
  test("is a dialog with the earned-glass combo + the scrim", () => {
    const html = renderView();
    expect(html).toContain('class="cmdk-combo"');
    expect(html).toContain('class="cmdk-scrim"');
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('aria-label="Command palette"');
  });

  test("the input is a combobox tied to the listbox + active option (WAI-ARIA)", () => {
    const html = renderView();
    expect(html).toContain('role="combobox"');
    expect(html).toContain('aria-controls="cmdk-listbox"');
    // First item active → aria-activedescendant points at it.
    expect(html).toContain('aria-activedescendant="nav-board"');
    expect(html).toContain('id="cmdk-listbox"');
    expect(html).toContain('role="listbox"');
    expect(html).toContain("Find a page or run a command");
  });

  test("renders both groups with the real commands", () => {
    const html = renderView();
    expect(html).toContain("Jump to");
    expect(html).toContain("Commands");
    for (const label of ["Maestro", "History", "Knowledge", "Settings", "Account"]) {
      expect(html).toContain(label);
    }
    expect(html).toContain("Toggle theme");
    expect(html).toContain("Send feedback");
    // Options carry the option role + ids (keyboard-selectable).
    expect(html).toContain('role="option"');
    expect(html).toContain('id="nav-history"');
  });

  test("the active command is marked for AT + styling", () => {
    const html = renderView("", "nav-settings");
    // The Settings option is the selected one.
    expect(html).toContain('id="nav-settings" type="button" role="option" aria-selected="true"');
    // Exactly one option is selected.
    expect(html.match(/aria-selected="true"/g)?.length).toBe(1);
  });

  test("filtering narrows the visible commands", () => {
    const html = renderView("history");
    expect(html).toContain("History");
    expect(html).not.toContain("Toggle theme");
    expect(html).not.toContain("Send feedback");
  });

  test("the matched substring is wrapped for the highlight", () => {
    const html = renderView("ist"); // History → H<em>ist</em>ory
    expect(html).toContain("<em>ist</em>");
  });

  test("an empty result set shows the plain-voice empty state", () => {
    const html = renderView("zzzznope", null);
    expect(html).toContain('class="cmdk-empty"');
    expect(html).toContain("No matches");
  });

  test("the footer carries the keyboard hints + the brand presence", () => {
    const html = renderView();
    expect(html).toContain("navigate");
    expect(html).toContain("open");
    expect(html).toContain("close");
    expect(html).toContain('class="cmdk-foot-brand"');
    expect(html).toContain("bv-dot-live"); // the live presence mark, not a static dot
  });

  test("§Voice — the palette copy is plain-language (no em dash / % / jargon)", () => {
    // Guard both the default render and a filtered render (different copy paths).
    assertVoice(renderView());
    assertVoice(renderView("set"));
    // Settings must NOT inherit the prototype's "the engine room" meta.
    expect(renderView("settings")).toContain("configure Maestro");
  });
});
