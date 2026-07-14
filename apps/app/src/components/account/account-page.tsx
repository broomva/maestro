// The account / user page (BRO-1893 FID-6 slice 4, `MccUser`) — a full-page frame inside the shell
// main, like the Knowledge / History / Settings pages. Ported from the prototype's ConceptUser.jsx:
// the center of gravity is who you are and your AUTONOMY SCORE — how long work ran without you (the
// number this product is really about). Two views toggled in the local top bar: an Overview dashboard
// and an editable Account page (identity · preferences · security).
//
// Honesty (the Knowledge/Settings "sample" pattern, and canon §the-branch-is-the-receipt):
//  - The "Your sessions" card is REAL — `selectHistory(server)` sliced to the most-recent runs, each a
//    projection OF work with real receipts (state dot, agent, folder). No session read path is needed:
//    History derives runs from work STATE (project.ts §selectHistory). Empty is honest (empty state).
//  - Theme (Account › Personal preferences) is REAL — it writes through the theme module to
//    <html data-theme> live, stays in sync with the top-bar toggle (shared `useThemeState`).
//  - Everything else has no user/account read-write path yet, so it is SAMPLE, labelled honestly: the
//    autonomy-score hero + weekly bars carry a "sample" affordance; the identity form does NOT claim it
//    syncs ("not saved" receipt, not the prototype's "syncs to your profile"); the score shows
//    receipts (hours / counts / durations), never a fabricated progress percentage (§Work states).
//  - `--bv-danger` red is used ONLY for genuine destructive actions (revoke a session, sign out) — a
//    needs-you / halt state is never red (§Color); those are accent-blue elsewhere.
//
// Store-read → SPLIT (the History/Inspector lesson): zustand's `useStore` reads the PINNED initial
// snapshot under renderToStaticMarkup, so the store read lives in the `AccountPage` container and the
// pure `AccountView` is prop-driven + unit-testable by seeding `sessions`. The overview/account view
// toggle + form state are ephemeral, so they live in the pure view.

import { Avatar, Button, DotComet, STATUS_DOT_VAR, workStatusView } from "@maestro/ui";
import { KeyRound, Laptop, LogOut, Pencil, Smartphone } from "lucide-react";
import { type ReactNode, useState } from "react";
import { useStore } from "zustand";
import { type HistorySession, maestroStore, selectHistory } from "@/store";
import { useThemeState } from "@/theme";
import { SetSegmented, SetSwitch } from "../settings/controls";

/** The sample weekly-autonomy viz (no autonomy-ledger read path yet — labelled sample on the hero). */
const USR_WEEK: { d: string; h: number; peak?: boolean }[] = [
  { d: "Mon", h: 4.1 },
  { d: "Tue", h: 6.4 },
  { d: "Wed", h: 2.0 },
  { d: "Thu", h: 7.6, peak: true },
  { d: "Fri", h: 5.2 },
  { d: "Sat", h: 1.1 },
  { d: "Sun", h: 4.8 },
];

const SAMPLE = {
  name: "Ana Diaz",
  handle: "Ana",
  email: "ana@broomva.ai",
  role: "Operator",
  joined: "joined Mar 2025",
  tz: "America/Mexico_City (GMT-6)",
} as const;

/** The store-reading container — reads the real run history, then renders the pure view. */
export function AccountPage() {
  const server = useStore(maestroStore, (s) => s.server);
  const sessions = selectHistory(server).slice(0, 6);
  return <AccountView sessions={sessions} />;
}

/**
 * The pure presentational account page — prop-driven over the already-derived `sessions` (like the
 * Inspector takes `item` and History takes `sessions`), so the render is unit-testable by seeding a
 * fixture. The overview/account toggle + the live theme control are ephemeral view state.
 */
export function AccountView({ sessions }: { sessions: HistorySession[] }) {
  const [view, setView] = useState<"overview" | "account">("overview");
  // Local, non-persisted preference toggles (no user-prefs read-write path yet — honest ephemeral UI).
  const [startView, setStartView] = useState("needs");
  const [runner, setRunner] = useState("claude");
  const [digest, setDigest] = useState(true);
  const [showClock, setShowClock] = useState(true);
  const [shortcuts, setShortcuts] = useState(true);
  const [reducedMotion, setReducedMotion] = useState(false);
  // Theme IS real — shared reactive state, writes through live and mirrors the top-bar toggle.
  const [theme, setTheme] = useThemeState();

  const maxH = Math.max(...USR_WEEK.map((d) => d.h));

  const identityHeader = (big: boolean): ReactNode => (
    <div className="usr-id">
      <div className="usr-id-avatar">
        <Avatar name={SAMPLE.name} color="var(--bv-gray-600)" size={big ? 76 : 64} />
      </div>
      <div className="usr-id-meta">
        <div className="usr-id-name">{SAMPLE.name}</div>
        <div className="usr-id-line">
          <span className="usr-role">Operator · Owner</span>
          <span className="usr-id-sep" />
          <span>{SAMPLE.email}</span>
          <span className="usr-id-sep" />
          <span>{SAMPLE.joined}</span>
        </div>
      </div>
      <Button size="sm" variant="secondary" onClick={() => setView("account")}>
        <Pencil size={16} strokeWidth={2} />
        Edit profile
      </Button>
    </div>
  );

  return (
    <div className="usr-page" data-testid="view-account" data-screen-label="Account page">
      <header className="set-topbar">
        <div className="set-topleft">
          <Avatar name={SAMPLE.name} color="var(--bv-gray-600)" size={20} />
          <span className="set-topbar-title">{SAMPLE.name}</span>
        </div>
        <div className="set-topright">
          <span className="usr-clock">
            <span className="usr-clock-dot" />
            31h 12m unsupervised this week
          </span>
          <span
            className="set-preview"
            title="Your autonomy score is sample data until the autonomy ledger is wired to this view. Your sessions and theme are real."
          >
            sample
          </span>
          <SetSegmented
            label="Account view"
            value={view}
            set={(v) => setView(v as "overview" | "account")}
            options={[
              ["overview", "Overview"],
              ["account", "Account"],
            ]}
          />
        </div>
      </header>

      {view === "overview" ? (
        <div className="usr-wrap" data-screen-label="Account · overview">
          <div className="usr-inner">
            {identityHeader(true)}

            {/* Autonomy score hero — receipts (hours / counts / durations), never a progress %. Sample. */}
            <div className="usr-score">
              <div className="usr-score-head">
                <span className="usr-score-title">Your autonomy score</span>
                <span className="usr-score-sub">
                  the number this product is really about · how long work ran without you
                </span>
              </div>
              <div className="usr-score-stats">
                <div className="usr-stat">
                  <div className="usr-stat-val">
                    31<small>h</small> 12<small>m</small>
                  </div>
                  <div className="usr-stat-label">Unsupervised this week</div>
                  <div className="usr-stat-foot">up 4h 40m on last week</div>
                </div>
                <div className="usr-stat">
                  <div className="usr-stat-val">9</div>
                  <div className="usr-stat-label">Times you had to look</div>
                  <div className="usr-stat-foot">2 today · mostly scope grants</div>
                </div>
                <div className="usr-stat">
                  <div className="usr-stat-val">
                    3<small>h</small> 50<small>m</small>
                  </div>
                  <div className="usr-stat-label">Longest single run</div>
                  <div className="usr-stat-foot">execution loop · Tue</div>
                </div>
              </div>
              <div className="usr-week">
                <div className="usr-week-bars">
                  {USR_WEEK.map((d) => (
                    <div className="usr-week-day" key={d.d}>
                      <div
                        className={`usr-week-bar${d.peak ? " is-peak" : ""}`}
                        style={{ height: `${Math.round((d.h / maxH) * 56)}px` }}
                        title={`${d.h}h`}
                      />
                      <span className="usr-week-lab">{d.d}</span>
                    </div>
                  ))}
                </div>
              </div>
            </div>

            {/* Two columns: your sessions (real) + preferences (local) */}
            <div className="usr-cols">
              <div className="usr-card">
                <div className="usr-card-head">
                  <span className="usr-card-head-title">Your sessions</span>
                  <a className="usr-card-link" href="/history">
                    Open History
                  </a>
                </div>
                {sessions.length === 0 ? (
                  <div className="usr-sess-empty">
                    No runs yet. When you start a mission or the loop dispatches work, your sessions
                    appear here.
                  </div>
                ) : (
                  sessions.map((s) => {
                    const v = workStatusView(s.state, "task");
                    return (
                      <a className="usr-sess" href="/history" key={s.id}>
                        {v.running ? (
                          <DotComet size={12} />
                        ) : (
                          <span
                            className="mc-chip-dot"
                            style={{ background: STATUS_DOT_VAR[v.tone] }}
                          />
                        )}
                        <span className="usr-sess-body">
                          <span className="usr-sess-title">{s.title}</span>
                          <span className="usr-sess-meta">{s.folder}</span>
                        </span>
                        <span className={`usr-sess-kind usr-sess-kind--${s.kind}`}>{s.kind}</span>
                      </a>
                    );
                  })
                )}
              </div>

              <div className="usr-card">
                <div className="usr-card-head">
                  <span className="usr-card-head-title">Preferences</span>
                  <span className="mc-receipt">this device</span>
                </div>
                <div className="usr-prow">
                  <div className="usr-prow-main">
                    <span className="usr-prow-label">Start view</span>
                    <span className="usr-prow-desc">Where the app opens</span>
                  </div>
                  <div className="usr-prow-control">
                    <SetSegmented
                      label="Start view"
                      value={startView}
                      set={setStartView}
                      options={[
                        ["needs", "Needs you"],
                        ["mc", "Mission"],
                      ]}
                    />
                  </div>
                </div>
                <div className="usr-prow">
                  <div className="usr-prow-main">
                    <span className="usr-prow-label">Default runner</span>
                    <span className="usr-prow-desc">For sessions you start</span>
                  </div>
                  <div className="usr-prow-control">
                    <SetSegmented
                      label="Default runner"
                      value={runner}
                      set={setRunner}
                      options={[
                        ["claude", "claude"],
                        ["codex", "codex"],
                      ]}
                    />
                  </div>
                </div>
                <div className="usr-prow">
                  <div className="usr-prow-main">
                    <span className="usr-prow-label">Digest email</span>
                    <span className="usr-prow-desc">A morning summary of overnight work</span>
                  </div>
                  <div className="usr-prow-control">
                    <SetSwitch
                      label="Digest email"
                      on={digest}
                      onToggle={() => setDigest(!digest)}
                    />
                  </div>
                </div>
                <div className="usr-prow">
                  <div className="usr-prow-main">
                    <span className="usr-prow-label">Show autonomy clock</span>
                    <span className="usr-prow-desc">In the sidebar footer</span>
                  </div>
                  <div className="usr-prow-control">
                    <SetSwitch
                      label="Show autonomy clock"
                      on={showClock}
                      onToggle={() => setShowClock(!showClock)}
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      ) : (
        <div className="usr-wrap" data-screen-label="Account · account">
          <div className="usr-inner">
            {identityHeader(false)}

            {/* Editable identity — local only, honestly not persisted (no profile sync path yet). */}
            <div className="usr-card">
              <div className="usr-card-head">
                <span className="usr-card-head-title">Identity</span>
                <span className="mc-receipt">not saved yet</span>
              </div>
              <div className="usr-form">
                <div className="usr-form-field">
                  <label className="usr-form-label" htmlFor="usr-fullname">
                    Full name
                  </label>
                  <input id="usr-fullname" className="set-input" defaultValue={SAMPLE.name} />
                </div>
                <div className="usr-form-field">
                  <label className="usr-form-label" htmlFor="usr-display">
                    Display name
                  </label>
                  <input id="usr-display" className="set-input" defaultValue={SAMPLE.handle} />
                </div>
                <div className="usr-form-field">
                  <label className="usr-form-label" htmlFor="usr-email">
                    Email
                  </label>
                  <input id="usr-email" className="set-input" defaultValue={SAMPLE.email} />
                </div>
                <div className="usr-form-field">
                  <label className="usr-form-label" htmlFor="usr-role">
                    Role
                  </label>
                  <input
                    id="usr-role"
                    className="set-input"
                    defaultValue={SAMPLE.role}
                    disabled
                    style={{ opacity: 0.6 }}
                  />
                </div>
                <div className="usr-form-field usr-form-field--full">
                  <label className="usr-form-label" htmlFor="usr-tz">
                    Timezone
                  </label>
                  <input
                    id="usr-tz"
                    className="set-input set-input--full"
                    defaultValue={SAMPLE.tz}
                  />
                </div>
              </div>
            </div>

            {/* Personal preferences — Theme is REAL (writes live); the rest are local. */}
            <div className="usr-card">
              <div className="usr-card-head">
                <span className="usr-card-head-title">Personal preferences</span>
              </div>
              <div className="usr-prow">
                <div className="usr-prow-main">
                  <span className="usr-prow-label">Theme</span>
                  <span className="usr-prow-desc">
                    Calm monochrome, light or dark. Writes live.
                  </span>
                </div>
                <div className="usr-prow-control">
                  <SetSegmented
                    label="Theme"
                    value={theme}
                    set={(v) => setTheme(v === "dark" ? "dark" : "light")}
                    options={[
                      ["light", "Light"],
                      ["dark", "Dark"],
                    ]}
                  />
                </div>
              </div>
              <div className="usr-prow">
                <div className="usr-prow-main">
                  <span className="usr-prow-label">Keyboard shortcuts</span>
                  <span className="usr-prow-desc">
                    <code>gh</code> History · <code>gk</code> Knowledge
                  </span>
                </div>
                <div className="usr-prow-control">
                  <SetSwitch
                    label="Keyboard shortcuts"
                    on={shortcuts}
                    onToggle={() => setShortcuts(!shortcuts)}
                  />
                </div>
              </div>
              <div className="usr-prow">
                <div className="usr-prow-main">
                  <span className="usr-prow-label">Reduced motion</span>
                  <span className="usr-prow-desc">Calm the Undertow and live signals</span>
                </div>
                <div className="usr-prow-control">
                  <SetSwitch
                    label="Reduced motion"
                    on={reducedMotion}
                    onToggle={() => setReducedMotion(!reducedMotion)}
                  />
                </div>
              </div>
            </div>

            {/* Security — sample rows (no auth read-write path yet). */}
            <div className="usr-card">
              <div className="usr-card-head">
                <span className="usr-card-head-title">Security</span>
                <span className="mc-receipt">sample</span>
              </div>
              <div className="usr-prow">
                <div className="usr-prow-main">
                  <span className="usr-prow-label">Sign-in method</span>
                  <span className="usr-prow-desc">Google · ana@broomva.ai · passkey enabled</span>
                </div>
                <div className="usr-prow-control">
                  <Button size="sm" variant="secondary">
                    Manage
                  </Button>
                </div>
              </div>
              <div className="usr-prow">
                <div className="usr-prow-main">
                  <span className="usr-prow-label">API keys</span>
                  <span className="usr-prow-desc">
                    <code>brm_live_••••4f2a</code> · 2 active
                  </span>
                </div>
                <div className="usr-prow-control">
                  <Button size="sm" variant="secondary">
                    <KeyRound size={16} strokeWidth={2} />
                    Keys
                  </Button>
                </div>
              </div>
            </div>

            {/* Where you're signed in — sample device rows; revoke/sign out are the sanctioned red. */}
            <div className="usr-card">
              <div className="usr-card-head">
                <span className="usr-card-head-title">Where you're signed in</span>
                <span className="mc-receipt">sample</span>
              </div>
              <div className="usr-dev">
                <span className="set-rowglyph">
                  <Laptop size={16} strokeWidth={2} />
                </span>
                <div className="usr-dev-body">
                  <span className="usr-dev-name">
                    MacBook Pro · Chrome <span className="usr-here">this device</span>
                  </span>
                  <span className="usr-dev-meta">Mexico City · active now</span>
                </div>
              </div>
              <div className="usr-dev">
                <span className="set-rowglyph">
                  <Smartphone size={16} strokeWidth={2} />
                </span>
                <div className="usr-dev-body">
                  <span className="usr-dev-name">iPhone 15 · Broomva PWA</span>
                  <span className="usr-dev-meta">Mexico City · 2h ago</span>
                </div>
                <button type="button" className="usr-danger">
                  Revoke
                </button>
              </div>
              <div className="usr-dev">
                <span className="set-rowglyph">
                  <Laptop size={16} strokeWidth={2} />
                </span>
                <div className="usr-dev-body">
                  <span className="usr-dev-name">Linux · cloud sandbox runner</span>
                  <span className="usr-dev-meta">us-east · 6h ago</span>
                </div>
                <button type="button" className="usr-danger">
                  Revoke
                </button>
              </div>
            </div>

            <Button
              size="sm"
              variant="secondary"
              style={{ alignSelf: "flex-start", color: "var(--bv-danger)" }}
            >
              <LogOut size={16} strokeWidth={2} />
              Sign out
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
