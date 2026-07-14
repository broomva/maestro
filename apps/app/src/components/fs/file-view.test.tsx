/// <reference types="bun" />

// FileView (BRO-1890 FID-4) — anti-vacuity: each case asserts a concrete document fact (the crumb from
// the path, the title, the real frontmatter chips, the honest read-path stub) or a canon rule (receipts
// never a progress percentage). A WorkItem in, a `.mcc-doc` out; the not-found path never crashes.

import { describe, expect, test } from "bun:test";
import type { WorkItem } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { FileView } from "./file-view";

const NODE: WorkItem = {
  id: "x",
  state: "review",
  kind: "project",
  title: "Persist run transcripts",
  owner: "agent:maestro",
  gate: "human",
  path: "hawthorne/hawthorne-core/spec",
  updatedAt: "2026-07-14T00:00:00.000Z",
  run: "run/7c2f1a",
  look: { ran: "run/7c2f1a", decided: ["persist on the Run record"], ask: "merge the branch" },
};

describe("FileView", () => {
  test("renders the crumb from the path, the title, and the real frontmatter chips", () => {
    const html = renderToStaticMarkup(<FileView node={NODE} />);
    expect(html).toContain("~ / hawthorne / hawthorne-core / spec"); // crumb from path
    expect(html).toContain("Persist run transcripts"); // title
    expect(html).toContain("kind: project"); // real frontmatter
    expect(html).toContain("owner: agent:maestro");
    expect(html).toContain("gate: human");
    expect(html).toContain("run/7c2f1a"); // the branch receipt chip
    expect(html).toContain("merge the branch"); // the look.ask lead line
    expect(html).toContain("persist on the Run record"); // a decided item
  });

  test("carries the honest read-path stub, and never a progress percentage", () => {
    const html = renderToStaticMarkup(<FileView node={NODE} />);
    expect(html).toContain("workspace read path"); // the honest "content lands in P1" line
    expect(html).not.toContain("%"); // receipts, never a percentage
  });

  test("a missing node renders a calm not-found, never a crash", () => {
    const html = renderToStaticMarkup(<FileView node={undefined} />);
    expect(html).toContain("No such file");
    expect(html).not.toContain("%");
  });
});
