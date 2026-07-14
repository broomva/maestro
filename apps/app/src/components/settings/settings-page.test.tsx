/// <reference types="bun" />

// Settings page (BRO-1893 FID-6 slice 3, `MccSettings`) — renderToStaticMarkup (no DOM; the layout
// toggle, section switching, and the live theme write-through are knowledge/settings.pw.ts's concern).
// The page is store-free (local useState + the theme module, guarded for no-document), so it renders
// directly. Asserts: the page + top bar + the honest "preview" affordance render, the two-pane section
// nav lists all sections with the credential badge, the default (runners) section renders, and the copy
// holds §Voice (no em dash, no build-phase jargon) + §Work-states (no progress %).

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CREDS, MEMBERS, SET_SECTIONS } from "./settings-data";
import { SettingsPage } from "./settings-page";

describe("SettingsPage — two-pane (default)", () => {
  const html = renderToStaticMarkup(<SettingsPage />);

  test("renders the page + local top bar", () => {
    expect(html).toContain('data-testid="view-settings"');
    expect(html).toContain("Settings");
    expect(html).toContain("preferences and access"); // honest sub, not "the engine room"
  });

  test("the honest 'preview' affordance (Appearance is live, the rest are not wired)", () => {
    expect(html).toContain("preview");
    expect(html).toContain("preview controls not yet wired to the runtime");
  });

  test("the section nav lists every section, with the credentials badge", () => {
    for (const s of SET_SECTIONS) expect(html).toContain(s.label);
    expect(html).toContain("set-secnav-badge"); // creds carries badge: 1
  });

  test("the default section (runners) renders its fields", () => {
    expect(html).toContain("Runners and worktrees");
    expect(html).toContain("Default runner");
    expect(html).toContain("Worktrees per runner");
    expect(html).toContain("Concurrency cap");
    // the switch is role=switch (accessible), not a checkbox
    expect(html).toContain('role="switch"');
  });

  test("§Work-states — no progress percentage anywhere on the page", () => {
    expect(html).not.toContain("%");
  });

  test("§Voice — no em dash, no build-phase jargon in user-facing copy", () => {
    expect(html).not.toContain("—"); // em dash (U+2014)
    expect(html).not.toContain("P1");
    expect(html).not.toContain("primitive");
    expect(html).not.toContain("engine room"); // disclosure ladder — do not name the substrate
  });
});

describe("settings sample data — shape", () => {
  test("seven sections; only credentials badged", () => {
    expect(SET_SECTIONS).toHaveLength(7);
    expect(SET_SECTIONS.filter((s) => s.badge != null).map((s) => s.id)).toEqual(["creds"]);
  });

  test("credential statuses are the three known states; one warns (a missing scope)", () => {
    for (const c of CREDS) expect(["ok", "warn", "off"]).toContain(c.status);
    const warn = CREDS.filter((c) => c.status === "warn");
    expect(warn).toHaveLength(1);
    expect(warn[0]?.scopes.some((s) => s.endsWith("?"))).toBe(true); // the missing scope is marked
  });

  test("member colours are cool-axis tokens (no warm/hex literals)", () => {
    for (const m of MEMBERS) {
      expect(m.color.startsWith("var(--")).toBe(true);
      expect(m.color).not.toContain("#");
    }
  });
});
