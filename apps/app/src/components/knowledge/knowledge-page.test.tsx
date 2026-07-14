/// <reference types="bun" />

// Knowledge page + sub-components (BRO-1893 FID-6 slice 2a) — renderToStaticMarkup (no DOM; the
// interaction is knowledge.pw.ts's concern). The page holds only ephemeral useState (no store read), so
// it renders directly. Asserts: the page renders the graph by default with real sample entities, the
// breadcrumb + scope-kind + honest "sample" badge, the rail, and — the §Work-states invariant — NO
// progress percentage. Plus the pure inspector/list render from props.

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { kgCategory } from "@/lib/kg";
import { KG_SCOPES, type KgScope } from "@/lib/kg-data";
import { KgGraph } from "./kg-graph";
import { KgInspector } from "./kg-inspector";
import { KgListView } from "./kg-list";
import { KnowledgePage } from "./knowledge-page";

const core = KG_SCOPES["hawthorne-core"] as KgScope;

describe("KnowledgePage — the scope graph (default view)", () => {
  const html = renderToStaticMarkup(<KnowledgePage />);

  test("renders the page + the graph canvas by default", () => {
    expect(html).toContain('data-testid="view-knowledge"');
    expect(html).toContain('data-testid="kg-graph"');
  });

  test("the breadcrumb, scope kind/count, and the honest 'sample' badge", () => {
    expect(html).toContain("Broomva"); // root crumb
    expect(html).toContain("vault · 11"); // broomva scope kind · node count
    expect(html).toContain("sample"); // never presents fixture data as live
  });

  test("real sample entities render as graph nodes", () => {
    expect(html).toContain("hawthorne");
    expect(html).toContain("genesis");
    expect(html).toContain("Bookkeeping");
  });

  test("the right rail renders (pinned + what's new)", () => {
    expect(html).toContain('data-testid="kg-rail"');
    expect(html).toContain("What&#x27;s new"); // apostrophe escaped by the SSR renderer
    expect(html).toContain("Pinned"); // the initial pin (broomva/p6)
  });

  test("a legend + type-filter chips render", () => {
    expect(html).toContain("kg-legend");
    expect(html).toContain("kg-chip");
  });

  test("the §Work-states invariant — receipts, never a progress percentage", () => {
    expect(html).not.toContain("%");
  });
});

describe("KgInspector — the entity page (pure, by props)", () => {
  test("empty state describes the scope, no entity manufactured", () => {
    const html = renderToStaticMarkup(<KgInspector node={null} scope={core} />);
    expect(html).toContain('data-testid="kg-inspect-empty"');
    expect(html).toContain("11 entities");
    expect(html).toContain("hawthorne-core/");
  });

  test("a scored decision renders its kind, claim, Nous bars, sources, and backlinks", () => {
    const drun = core.nodes.find((n) => n.id === "drun") ?? null;
    const html = renderToStaticMarkup(<KgInspector node={drun} scope={core} big />);
    expect(html).toContain('data-testid="kg-inspect"');
    expect(html).toContain("decision"); // kind badge
    expect(html).toContain("persist transcript on Run"); // title
    expect(html).toContain("9"); // Nous total (3+3+3)
    expect(html).toContain("fast-path promote"); // verdict for >= 7
    expect(html).toContain("PR #214"); // a source receipt
    expect(html).toContain("spec.md"); // a backlink label
    // The score renders as a receipt (the "9/9" fraction + a verdict), NOT a work-progress percentage.
    // The Nous BAR uses a `width:N%` fill (novelty 3/3 -> full), so a blanket not.toContain("%") does not
    // apply here; the §Work-states "no progress %" invariant is enforced on the whole page + list surfaces
    // (which carry no % at all — asserted above and in the KgListView test).
    expect(html).toContain("/9"); // the score is a bounded receipt fraction, not an unbounded progress %
  });

  test("an unscored node renders no Nous block", () => {
    const relay = core.nodes.find((n) => n.id === "relay") ?? null; // no score
    const html = renderToStaticMarkup(<KgInspector node={relay} scope={core} />);
    expect(html).not.toContain("Nous score");
  });
});

describe("KgListView — the a11y-first table (pure, by props)", () => {
  test("a row per entity with kind, Nous pip, link count; folders read 'folder ›'", () => {
    const html = renderToStaticMarkup(
      <KgListView
        scope={core}
        selectedId={null}
        onSelect={() => {}}
        onNavigate={() => {}}
        typeFilter={new Set()}
      />,
    );
    expect(html).toContain('data-testid="kg-list"');
    expect(html).toContain("persist transcript on Run");
    expect(html).toContain("kg-list-pip"); // Nous pip
    expect(html).not.toContain("%");
  });

  test("a type filter narrows the rows to that category", () => {
    const onlyDecisions = renderToStaticMarkup(
      <KgListView
        scope={core}
        selectedId={null}
        onSelect={() => {}}
        onNavigate={() => {}}
        typeFilter={new Set(["decision"])}
      />,
    );
    expect(onlyDecisions).toContain("persist transcript on Run"); // a decision
    expect(onlyDecisions).not.toContain("multi-turn"); // a concept — filtered out
  });
});

describe("KgGraph — the type filter applies to the a11y tree, not just visually", () => {
  // Only graph nodes carry tabindex="0" in KgGraph, so counting them = the number of keyboard-reachable
  // nodes. The filter must remove off-type nodes from BOTH the tab order and the SR tree (parity with the
  // List view, which drops filtered rows) — not merely dim them.
  const stops = (html: string) => (html.match(/tabindex="0"/g) ?? []).length;
  const graph = (typeFilter: ReadonlySet<string>) =>
    renderToStaticMarkup(
      <KgGraph
        scope={core}
        selectedId={null}
        onSelectNode={() => {}}
        onNavigate={() => {}}
        typeFilter={typeFilter}
      />,
    );

  test("with no filter every node is a tab stop and none is hidden from SR", () => {
    const html = graph(new Set());
    expect(stops(html)).toBe(core.nodes.length);
    expect(html).not.toContain('aria-hidden="true"');
  });

  test("a type filter drops off-type nodes from the tab order AND hides them from SR", () => {
    const decisions = core.nodes.filter((n) => kgCategory(n) === "decision").length;
    expect(decisions).toBeGreaterThan(0);
    expect(decisions).toBeLessThan(core.nodes.length); // some nodes ARE off-type
    const html = graph(new Set(["decision"]));
    expect(stops(html)).toBe(decisions); // only decision nodes stay keyboard-reachable
    expect(html).toContain('aria-hidden="true"'); // the rest leave the a11y tree
  });
});
