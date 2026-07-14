// Stub views (BRO-1824) — the product routes beyond the board (production-notes §1: the prototype's
// `view` state maps 1:1 to /, /knowledge, /history, /settings, /account). Calm plain-voice placeholders
// until their milestones land: Knowledge + History = BRO-1815 (M4), Settings + Account = BRO-1810 (M4).
// Each renders inside the shell layout's main; the shell chrome (sidebar, header) is the layout route.

// Each stub owns its own scroll + padding: the shell frame is now overflow-hidden (BRO-1886), so a
// simple view wraps itself in a padded scroll container (the mission plane does this via .mcc-plane).
function StubView({ title, note, testid }: { title: string; note: string; testid: string }) {
  return (
    <div className="h-full overflow-y-auto p-6">
      <div data-testid={testid} className="flex flex-col gap-3">
        <h1 className="text-foreground text-h1">{title}</h1>
        <p className="max-w-[520px] text-muted-foreground text-sm">{note}</p>
      </div>
    </div>
  );
}

// KnowledgeView superseded by the real components/knowledge/knowledge-page.tsx (BRO-1893 FID-6 slice 2).
// HistoryView superseded by the real components/history/history-page.tsx (BRO-1893 FID-6 slice 1).

export function SettingsView() {
  return (
    <StubView
      title="Settings"
      testid="view-settings"
      note="Appearance and defaults live here. The orchestrator has no settings page; it is an agent, and its knobs are frontmatter."
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
