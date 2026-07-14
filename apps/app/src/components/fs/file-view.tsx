// The file view (BRO-1890 FID-4) — a workspace file rendered as a document (`MccFsDoc`). "Every file is
// a contract or a receipt": the node's frontmatter (kind / owner / gate / run) becomes the receipt
// chips, its gate look / block reason the body. REAL node data, not seed — the projector's WorkItem.
// The file's full text lands with the workspace read path (P1); until then this shows the contract +
// latest receipts, surfaced honestly (never a faked body). Matte; the document owns its own scroll.

import type { WorkItem } from "@maestro/protocol";

/** The "~ / seg / seg" breadcrumb from a workspace-relative path (the prototype's `.mcc-doc-crumb`). */
function crumbOf(path: string): string {
  return ["~", ...path.split("/").filter(Boolean)].join(" / ");
}

export function FileView({ node }: { node: WorkItem | undefined }) {
  if (!node) {
    return (
      <div className="mcc-doc" data-testid="file-view">
        <div className="mcc-doc-inner">
          <span className="mcc-doc-crumb">not found</span>
          <h1 className="mcc-doc-title">No such file</h1>
          <p className="mcc-doc-p">This path names no work in the current tree.</p>
        </div>
      </div>
    );
  }

  // The frontmatter contract as receipt chips — real fields only (budget is engine-room, excluded from
  // the WorkItem by design). `run` is the branch receipt when the node has dispatched.
  const chips = [
    `kind: ${node.kind}`,
    node.owner ? `owner: ${node.owner}` : null,
    `gate: ${node.gate}`,
    node.run ?? null,
  ].filter((c): c is string => Boolean(c));

  // A real body line from the node's available metadata (the gate ask, else the block reason).
  const lead = node.look?.ask ?? node.reason;
  const decided = node.look?.decided ?? [];

  return (
    <div className="mcc-doc" data-testid="file-view">
      <div className="mcc-doc-inner">
        <span className="mcc-doc-crumb">{crumbOf(node.path)}</span>
        <h1 className="mcc-doc-title">{node.title}</h1>
        <div className="mcc-fm-chips">
          {chips.map((c) => (
            <span key={c} className="mc-receipt">
              {c}
            </span>
          ))}
        </div>
        {lead ? <p className="mcc-doc-p">{lead}</p> : null}
        {decided.length > 0 ? (
          <ul className="mcc-doc-list">
            {decided.map((d) => (
              <li key={d}>{d}</li>
            ))}
          </ul>
        ) : null}
        <p className="mcc-doc-p text-muted-foreground">
          The file's full contents open with the workspace read path (lands in P1). This view shows
          the node's frontmatter contract and its latest receipts.
        </p>
      </div>
    </div>
  );
}
