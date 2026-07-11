// Stub views (BRO-1824) — the product routes beyond the board (production-notes §1: the prototype's
// `view` state maps 1:1 to /, /knowledge, /history, /settings, /account). Calm plain-voice placeholders
// until their milestones land: Knowledge + History = BRO-1815 (M4), Settings + Account = BRO-1810 (M4).
// Each renders inside the shell layout's main; the shell chrome (sidebar, header) is the layout route.

function StubView({ title, note, testid }: { title: string; note: string; testid: string }) {
  return (
    <div data-testid={testid} className="flex flex-col gap-3">
      <h1 className="text-foreground text-h1">{title}</h1>
      <p className="max-w-[520px] text-muted-foreground text-sm">{note}</p>
    </div>
  );
}

export function KnowledgeView() {
  return (
    <StubView
      title="Knowledge"
      testid="view-knowledge"
      note="The knowledge graph over your workspace — files as nodes, frontmatter links as edges — lands here."
    />
  );
}

export function HistoryView() {
  return (
    <StubView
      title="History"
      testid="view-history"
      note="Past runs and sessions with their receipts — branch, diffstat, verdict — land here."
    />
  );
}

export function SettingsView() {
  return (
    <StubView
      title="Settings"
      testid="view-settings"
      note="Appearance and defaults live here. The orchestrator has no settings page — it is an agent; its knobs are frontmatter."
    />
  );
}

export function AccountView() {
  return (
    <StubView
      title="Account"
      testid="view-account"
      note="Your profile and workspace access land here."
    />
  );
}
