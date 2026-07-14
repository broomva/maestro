/// <reference types="bun" />

// FilePane (BRO-1890 FID-4) — anti-vacuity [[self-hosting-vacuous-pass]]: every case asserts a concrete
// DOM fact (a folder is inert, a file is openable, the open row is active, a running row is live) or a
// canon rule (no progress percentage — receipts only). renderToStaticMarkup, no DOM lifecycle needed.

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { FileEntry } from "@/store";
import { FilePane } from "./file-pane";

const ENTRIES: FileEntry[] = [
  { path: "hawthorne", name: "hawthorne", depth: 0, kind: "folder", live: false },
  { path: "hawthorne/spec", name: "spec", depth: 1, kind: "file", live: false },
  { path: "hawthorne/run", name: "run", depth: 1, kind: "file", live: true },
];

describe("FilePane", () => {
  test("folders are inert (is-folder + disabled); files are openable buttons", () => {
    const html = renderToStaticMarkup(
      <FilePane entries={ENTRIES} openPath={null} onOpen={() => {}} label="Workspace" />,
    );
    // The label renders.
    expect(html).toContain("Workspace");
    // The folder row is marked is-folder and disabled (not openable).
    expect(html).toMatch(/mcc-ftree-row[^"]*is-folder[^"]*"[^>]*disabled/);
    // Three rows, one per entry.
    expect(html.match(/mcc-ftree-row/g)?.length).toBe(3);
    // Never a progress percentage (receipts only — CLAUDE.md §Work states).
    expect(html).not.toContain("%");
  });

  test("the open file's row is active; a running file shows the live dot", () => {
    const html = renderToStaticMarkup(
      <FilePane entries={ENTRIES} openPath="hawthorne/spec" onOpen={() => {}} />,
    );
    // The open path's row carries is-active + aria-current.
    expect(html).toMatch(/mcc-ftree-row[^"]*is-active/);
    expect(html).toContain('aria-current="page"');
    // The running file (hawthorne/run) renders a live dot (the app's DotComet → bv-dot-live).
    expect(html).toContain("bv-dot-live");
  });

  test("an empty tree renders just the pane (no rows, no crash)", () => {
    const html = renderToStaticMarkup(<FilePane entries={[]} openPath={null} onOpen={() => {}} />);
    expect(html).toContain("mcc-ftree");
    expect(html).not.toContain("mcc-ftree-row");
  });
});
