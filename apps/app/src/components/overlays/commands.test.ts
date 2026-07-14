/// <reference types="bun" />

// The ⌘K command registry (BRO-1894 FID-7) — pure filter / group / match-highlight logic, no DOM.

import { describe, expect, test } from "bun:test";
import { COMMANDS, commandMatches, filterCommands, groupCommands, markMatch } from "./commands";

describe("commands — the registry is honest (only real capabilities)", () => {
  test("every command navigates, toggles the theme, or opens feedback — nothing no-ops", () => {
    // The load-bearing rule: a command that can't do anything is omitted, not shown. So every entry
    // carries either a real route (`to`) or a real action.
    for (const cmd of COMMANDS) {
      const wired = cmd.to != null || cmd.action != null;
      expect(wired).toBe(true);
    }
    // Exactly one nav per real route, plus the two actions.
    expect(COMMANDS.filter((c) => c.to).length).toBe(5);
    expect(COMMANDS.filter((c) => c.action === "toggle-theme").length).toBe(1);
    expect(COMMANDS.filter((c) => c.action === "open-feedback").length).toBe(1);
  });

  test("no fabricated work-dispatch commands (start a session / wake / approve)", () => {
    const titles = COMMANDS.map((c) => c.title.toLowerCase()).join(" | ");
    expect(titles).not.toContain("start a session");
    expect(titles).not.toContain("wake maestro");
    expect(titles).not.toContain("approve at the gate");
  });
});

describe("filterCommands — case-insensitive substring over title + meta + keywords", () => {
  test("empty query returns every command", () => {
    expect(filterCommands("").length).toBe(COMMANDS.length);
    expect(filterCommands("   ").length).toBe(COMMANDS.length);
  });

  test("a title match narrows to that command", () => {
    const hit = filterCommands("knowledge");
    expect(hit.length).toBe(1);
    expect(hit[0]?.id).toBe("nav-knowledge");
  });

  test("matches on keywords, not just the visible title", () => {
    // "profile" is only in Account's keywords, never its title/meta.
    const hit = filterCommands("profile");
    expect(hit.map((c) => c.id)).toEqual(["nav-account"]);
  });

  test("case-insensitive", () => {
    expect(filterCommands("HISTORY").map((c) => c.id)).toEqual(["nav-history"]);
  });

  test("no match returns an empty list", () => {
    expect(filterCommands("zzzznotacommand")).toEqual([]);
  });

  test("commandMatches is the underlying predicate", () => {
    const account = COMMANDS.find((c) => c.id === "nav-account");
    if (!account) throw new Error("nav-account missing");
    expect(commandMatches(account, "you")).toBe(true);
    expect(commandMatches(account, "knowledge")).toBe(false);
  });
});

describe("groupCommands — canonical order, empty groups dropped", () => {
  test("the full set is Jump to (5) then Commands (2)", () => {
    const groups = groupCommands(COMMANDS);
    expect(groups.map((g) => g.label)).toEqual(["Jump to", "Commands"]);
    expect(groups[0]?.items.length).toBe(5);
    expect(groups[1]?.items.length).toBe(2);
  });

  test("a filtered set with only nav yields a single group", () => {
    const groups = groupCommands(filterCommands("history"));
    expect(groups.map((g) => g.label)).toEqual(["Jump to"]);
  });

  test("a filtered set with only an action yields a single Commands group", () => {
    const groups = groupCommands(filterCommands("toggle"));
    expect(groups.map((g) => g.label)).toEqual(["Commands"]);
    expect(groups[0]?.items[0]?.id).toBe("act-theme");
  });
});

describe("markMatch — the matched-substring highlight", () => {
  test("splits around the first case-insensitive occurrence", () => {
    expect(markMatch("History", "ist")).toEqual([
      { text: "H", hit: false },
      { text: "ist", hit: true },
      { text: "ory", hit: false },
    ]);
  });

  test("a leading match has no empty prefix segment", () => {
    expect(markMatch("Knowledge", "know")).toEqual([
      { text: "Know", hit: true },
      { text: "ledge", hit: false },
    ]);
  });

  test("empty query and no-match both return the whole text un-hit", () => {
    expect(markMatch("Settings", "")).toEqual([{ text: "Settings", hit: false }]);
    expect(markMatch("Settings", "zzz")).toEqual([{ text: "Settings", hit: false }]);
  });
});
