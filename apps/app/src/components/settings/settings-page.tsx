// The Settings page (BRO-1893 FID-6 slice 3, `MccSettings`) — a full-page frame inside the shell main,
// like the Knowledge + History pages. Ported from ConceptSettings.jsx: seven config sections in two
// layouts (a two-pane section nav, and a one-scroll editorial with a sticky table of contents), toggled
// in the local top bar.
//
// Honesty (the Knowledge "sample" pattern, and canon): Settings is app/account configuration, NOT the
// orchestrator (the orchestrator is an agent with its own presence, never a settings panel). Appearance
// > Theme is the ONE live control — it writes through to the app via the real theme module. The
// engine-room sections (runners, credentials, routines, members) have no runtime read-write path yet, so
// they render SAMPLE data under a "preview" affordance and their controls are local-only — never faked as
// persisted or wired. The page is store-free (local useState + the theme module), so it renders directly
// under renderToStaticMarkup (no store-reading container split needed — the History/Knowledge lesson).

import { Avatar, Button } from "@maestro/ui";
import {
  Bell,
  Clock,
  Cpu,
  KeyRound,
  Palette,
  Plus,
  Settings as SettingsIcon,
  SlidersHorizontal,
  Users,
} from "lucide-react";
import { type ReactNode, useState } from "react";
import { useThemeState } from "@/theme";
import { SetSegmented, SetSlider, SetStepper, SetSwitch } from "./controls";
import { CREDS, MEMBERS, type MemberRole, ROUTINES, SET_SECTIONS, WAKES } from "./settings-data";

type Layout = "twopane" | "scroll";

const SECTION_ICON: Record<string, ReactNode> = {
  runners: <Cpu size={16} strokeWidth={2} />,
  creds: <KeyRound size={16} strokeWidth={2} />,
  autonomy: <SlidersHorizontal size={16} strokeWidth={2} />,
  routines: <Clock size={16} strokeWidth={2} />,
  notify: <Bell size={16} strokeWidth={2} />,
  appearance: <Palette size={16} strokeWidth={2} />,
  members: <Users size={16} strokeWidth={2} />,
};

function credStatusLabel(s: (typeof CREDS)[number]["status"]): string {
  return s === "ok" ? "connected" : s === "warn" ? "needs scope" : "off";
}
function credAction(s: (typeof CREDS)[number]["status"]): string {
  return s === "off" ? "Connect" : s === "warn" ? "Grant write" : "Manage";
}

export function SettingsPage() {
  const [layout, setLayout] = useState<Layout>("twopane");
  const [active, setActive] = useState("runners");

  // Appearance > Theme is REAL — shared reactive theme state (stays in sync with the top-bar toggle, and
  // writes through to <html data-theme> live). SSR/test-safe (no document → stays "light").
  const [theme, setTheme] = useThemeState();
  const applyTheme = (t: string) => setTheme(t === "dark" ? "dark" : "light");
  // Appearance previews (no applied effect yet — honest, not persisted).
  const [density, setDensity] = useState("calm");
  const [blue, setBlue] = useState(1);

  // Engine-room preview state — local only (never persisted / wired to the runtime).
  const [runner, setRunner] = useState("claude");
  const [worktrees, setWorktrees] = useState(2);
  const [sandbox, setSandbox] = useState("both");
  const [concurrency, setConcurrency] = useState(3);
  const [autoClean, setAutoClean] = useState(true);
  const [budget, setBudget] = useState(20);
  const [gate, setGate] = useState("risk");
  const [cascade, setCascade] = useState(2);
  const [spend, setSpend] = useState(40);
  const [routines, setRoutines] = useState(ROUTINES.map((r) => r.on));
  const [wakes, setWakes] = useState(WAKES.map((w) => w.on));
  const [notif, setNotif] = useState({ app: true, email: true, slack: false });
  const [pingWhen, setPingWhen] = useState("blocks");
  const [memberRoles, setMemberRoles] = useState<MemberRole[]>(MEMBERS.map((m) => m.role));

  // A plain render helper, NOT a component: it is CALLED (`{renderSection(id)}`), so its JSX inlines into
  // this render with no reconciliation boundary. Defining it as a component and using `<Section/>` would
  // give it a new function identity every render → React would remount the whole section subtree on every
  // keystroke, dropping segmented keyboard focus, breaking slider drag, and resetting the uncontrolled
  // workspace-name input. (Confirmed MAJOR, P20 slice-3.)
  const renderSection = (id: string): ReactNode => {
    switch (id) {
      case "runners":
        return (
          <div className="set-section" id="set-runners">
            <div className="set-section-head">
              <div className="set-section-title">
                <Cpu size={17} strokeWidth={2} />
                Runners and worktrees
              </div>
              <div className="set-section-sub">
                The runner the loop arms by default, and how many checkouts run at once.
              </div>
            </div>
            <div className="set-panel">
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Default runner</span>
                  <span className="set-field-desc">
                    The model the loop arms by default. Per-folder overrides win.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSegmented
                    label="Default runner"
                    value={runner}
                    set={setRunner}
                    options={[
                      ["claude", "claude"],
                      ["codex", "codex"],
                      ["local", "local"],
                    ]}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Worktrees per runner</span>
                  <span className="set-field-desc">
                    Parallel git checkouts a single runner can hold.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetStepper
                    label="worktrees per runner"
                    value={worktrees}
                    set={setWorktrees}
                    min={1}
                    max={6}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Where work runs</span>
                  <span className="set-field-desc">
                    Local machine, an ephemeral cloud sandbox, or whichever is free.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSegmented
                    label="Where work runs"
                    value={sandbox}
                    set={setSandbox}
                    options={[
                      ["local", "Local"],
                      ["cloud", "Cloud"],
                      ["both", "Both"],
                    ]}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Concurrency cap</span>
                  <span className="set-field-desc">
                    Most sessions live at once across the whole workspace.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetStepper
                    label="concurrency cap"
                    value={concurrency}
                    set={setConcurrency}
                    min={1}
                    max={12}
                    suffix="live"
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Auto-clean merged worktrees</span>
                  <span className="set-field-desc">Drop a checkout once its branch lands.</span>
                </div>
                <div className="set-field-control">
                  <SetSwitch
                    label="Auto-clean merged worktrees"
                    on={autoClean}
                    onToggle={() => setAutoClean(!autoClean)}
                  />
                </div>
              </div>
            </div>
          </div>
        );
      case "creds":
        return (
          <div className="set-section" id="set-creds">
            <div className="set-section-head">
              <div className="set-section-title">
                <KeyRound size={17} strokeWidth={2} />
                Credentials and scopes
              </div>
              <div className="set-section-sub">
                The thing that blocks runs. A missing scope halts the loop until a human grants it.
              </div>
            </div>
            <div className="set-panel">
              {CREDS.map((c) => (
                <div className="set-field" key={c.name}>
                  <span className="set-rowglyph">{c.glyph}</span>
                  <div className="set-field-main">
                    <span className="set-field-label">{c.name}</span>
                    <span className="set-field-desc">{c.desc}</span>
                    {c.scopes.length > 0 ? (
                      <div className="set-scopes" style={{ marginTop: 4 }}>
                        {c.scopes.map((s) => {
                          const miss = s.endsWith("?");
                          return (
                            <span key={s} className={`set-scope${miss ? " set-scope--miss" : ""}`}>
                              {s.replace("?", "")}
                              {miss ? " · missing" : ""}
                            </span>
                          );
                        })}
                      </div>
                    ) : null}
                  </div>
                  <div
                    className="set-field-control"
                    style={{ flexDirection: "column", alignItems: "flex-end", gap: 8 }}
                  >
                    <span className={`set-status set-status--${c.status}`}>
                      <span className="set-status-dot" />
                      {credStatusLabel(c.status)}
                    </span>
                    <Button size="sm" variant="secondary">
                      {credAction(c.status)}
                    </Button>
                  </div>
                </div>
              ))}
            </div>
            <Button size="sm" variant="soft" style={{ alignSelf: "flex-start" }}>
              <Plus size={16} strokeWidth={2} />
              Add credential
            </Button>
          </div>
        );
      case "autonomy":
        return (
          <div className="set-section" id="set-autonomy">
            <div className="set-section-head">
              <div className="set-section-title">
                <SlidersHorizontal size={17} strokeWidth={2} />
                Autonomy defaults
              </div>
              <div className="set-section-sub">
                How far the loop runs before it has to come back to your gate.
              </div>
            </div>
            <div className="set-panel">
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Weekly budget</span>
                  <span className="set-field-desc">
                    Unsupervised hours the loop may spend before pausing for review.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSlider
                    label="Weekly budget"
                    value={budget}
                    set={setBudget}
                    min={0}
                    max={60}
                    fmt={(v) => `${v} h / wk`}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Gate policy</span>
                  <span className="set-field-desc">
                    When a session needs you. <code>Ask on risk</code> stops only at irreversible
                    steps.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSegmented
                    label="Gate policy"
                    value={gate}
                    set={setGate}
                    options={[
                      ["ask", "Ask first"],
                      ["risk", "Ask on risk"],
                      ["free", "Run free"],
                    ]}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Cascade depth</span>
                  <span className="set-field-desc">
                    How many levels deep maestro may spawn sub-agents.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetStepper
                    label="cascade depth"
                    value={cascade}
                    set={setCascade}
                    min={0}
                    max={5}
                    suffix="levels"
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Spend cap</span>
                  <span className="set-field-desc">
                    Hard ceiling on model and sandbox cost per week.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSlider
                    label="Spend cap"
                    value={spend}
                    set={setSpend}
                    min={0}
                    max={200}
                    step={5}
                    fmt={(v) => `$${v} / wk`}
                  />
                </div>
              </div>
            </div>
          </div>
        );
      case "routines":
        return (
          <div className="set-section" id="set-routines">
            <div className="set-section-head">
              <div className="set-section-title">
                <Clock size={17} strokeWidth={2} />
                Routines and wake triggers
              </div>
              <div className="set-section-sub">
                Standing loops on a schedule, and the events that wake the orchestrator.
              </div>
            </div>
            <div className="set-panel">
              {ROUTINES.map((r, i) => (
                <div className="set-field" key={r.name}>
                  <span className="set-rowglyph">
                    <Clock size={16} strokeWidth={2} />
                  </span>
                  <div className="set-field-main">
                    <span className="set-field-label">{r.name}</span>
                    <span className="set-field-desc">{r.when}</span>
                  </div>
                  <div className="set-field-control">
                    <span className="mc-receipt">routine</span>
                    <SetSwitch
                      label={`${r.name} routine`}
                      on={routines[i] ?? false}
                      onToggle={() => setRoutines(routines.map((v, j) => (j === i ? !v : v)))}
                    />
                  </div>
                </div>
              ))}
            </div>
            <div className="set-panel">
              {WAKES.map((w, i) => (
                <div className="set-field" key={w.name}>
                  <div className="set-field-main">
                    <span className="set-field-label">{w.name}</span>
                    <span className="set-field-desc">{w.desc}</span>
                  </div>
                  <div className="set-field-control">
                    <SetSwitch
                      label={w.name}
                      on={wakes[i] ?? false}
                      onToggle={() => setWakes(wakes.map((v, j) => (j === i ? !v : v)))}
                    />
                  </div>
                </div>
              ))}
            </div>
          </div>
        );
      case "notify":
        return (
          <div className="set-section" id="set-notify">
            <div className="set-section-head">
              <div className="set-section-title">
                <Bell size={17} strokeWidth={2} />
                Notifications
              </div>
              <div className="set-section-sub">
                When and where the loop pings you · kept quiet by default.
              </div>
            </div>
            <div className="set-panel">
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">In-app</span>
                  <span className="set-field-desc">
                    The <b>Needs you</b> lens badge.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSwitch
                    label="In-app notifications"
                    on={notif.app}
                    onToggle={() => setNotif({ ...notif, app: !notif.app })}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Email</span>
                  <span className="set-field-desc">ana@broomva.ai</span>
                </div>
                <div className="set-field-control">
                  <SetSwitch
                    label="Email notifications"
                    on={notif.email}
                    onToggle={() => setNotif({ ...notif, email: !notif.email })}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Slack</span>
                  <span className="set-field-desc">Connect Slack in Credentials first.</span>
                </div>
                <div className="set-field-control">
                  <SetSwitch
                    label="Slack notifications"
                    on={notif.slack}
                    onToggle={() => setNotif({ ...notif, slack: !notif.slack })}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Ping me when</span>
                  <span className="set-field-desc">
                    Every halt is chatty; <code>Only blocks</code> waits for a real wall.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSegmented
                    label="Ping me when"
                    value={pingWhen}
                    set={setPingWhen}
                    options={[
                      ["halt", "Every halt"],
                      ["blocks", "Only blocks"],
                      ["digest", "Daily digest"],
                    ]}
                  />
                </div>
              </div>
            </div>
          </div>
        );
      case "appearance":
        return (
          <div className="set-section" id="set-appearance">
            <div className="set-section-head">
              <div className="set-section-title">
                <Palette size={17} strokeWidth={2} />
                Appearance
              </div>
              <div className="set-section-sub">
                Theme writes through to the app live. Density and blue intensity are previews.
              </div>
            </div>
            <div className="set-panel">
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Theme</span>
                  <span className="set-field-desc">Calm monochrome, light or dark.</span>
                </div>
                <div className="set-field-control">
                  <SetSegmented
                    label="Theme"
                    value={theme}
                    set={applyTheme}
                    options={[
                      ["light", "Light"],
                      ["dark", "Dark"],
                    ]}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Density</span>
                  <span className="set-field-desc">
                    Calm gives cards room; dense packs the feed. Preview.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSegmented
                    label="Density"
                    value={density}
                    set={setDensity}
                    options={[
                      ["calm", "Calm"],
                      ["dense", "Dense"],
                    ]}
                  />
                </div>
              </div>
              <div className="set-field">
                <div className="set-field-main">
                  <span className="set-field-label">Blue intensity</span>
                  <span className="set-field-desc">
                    How much the ai-blue glow tints frost, shadow and the Undertow. Preview.
                  </span>
                </div>
                <div className="set-field-control">
                  <SetSlider
                    label="Blue intensity"
                    value={blue}
                    set={setBlue}
                    min={0}
                    max={2}
                    step={0.1}
                    fmt={(v) => `${v.toFixed(1)}×`}
                  />
                </div>
              </div>
            </div>
          </div>
        );
      case "members":
        return (
          <div className="set-section" id="set-members">
            <div className="set-section-head">
              <div className="set-section-title">
                <Users size={17} strokeWidth={2} />
                Workspace and members
              </div>
              <div className="set-section-sub">
                Who shares this orchestration plane, and what they can do.
              </div>
            </div>
            <div className="set-panel">
              <div className="set-field set-field--stacked">
                <span className="set-field-label">Workspace name</span>
                <input
                  className="set-input set-input--full"
                  aria-label="Workspace name"
                  defaultValue="Broomva"
                />
              </div>
            </div>
            <div className="set-panel">
              {MEMBERS.map((m, i) => (
                <div className="set-field" key={m.email}>
                  <Avatar name={m.name} color={m.color} size={32} />
                  <div className="set-field-main">
                    <span className="set-field-label">{m.name}</span>
                    <span className="set-field-desc">{m.email}</span>
                  </div>
                  <div className="set-field-control">
                    <SetSegmented
                      label={`Role for ${m.name}`}
                      value={memberRoles[i] ?? m.role}
                      set={(v) =>
                        setMemberRoles(memberRoles.map((r, j) => (j === i ? (v as MemberRole) : r)))
                      }
                      options={[
                        ["Owner", "Owner"],
                        ["Operator", "Operator"],
                        ["Viewer", "Viewer"],
                      ]}
                    />
                  </div>
                </div>
              ))}
            </div>
            <Button size="sm" variant="soft" style={{ alignSelf: "flex-start" }}>
              <Plus size={16} strokeWidth={2} />
              Invite member
            </Button>
          </div>
        );
      default:
        return null;
    }
  };

  return (
    <div className="set-page" data-testid="view-settings" data-screen-label="Settings page">
      <header className="set-topbar">
        <div className="set-topleft">
          <SettingsIcon size={16} strokeWidth={2} />
          <span className="set-topbar-title">Settings</span>
          <span>· preferences and access</span>
        </div>
        <div className="set-topright">
          <span
            className="set-preview"
            title="Appearance applies live. The other sections preview controls not yet wired to the runtime."
          >
            preview
          </span>
          <SetSegmented
            label="Settings layout"
            value={layout}
            set={(v) => setLayout(v as Layout)}
            options={[
              ["twopane", "Two-pane"],
              ["scroll", "One scroll"],
            ]}
          />
        </div>
      </header>

      {layout === "twopane" ? (
        <div className="set-twopane" data-screen-label="Settings · two-pane">
          <nav className="set-secnav" aria-label="Settings sections">
            <div className="set-secnav-label">Sections</div>
            {SET_SECTIONS.map((s) => (
              <button
                key={s.id}
                type="button"
                className={`set-secnav-btn${active === s.id ? " is-active" : ""}`}
                aria-current={active === s.id ? "true" : undefined}
                onClick={() => setActive(s.id)}
              >
                {SECTION_ICON[s.id]}
                <span>{s.label}</span>
                {s.badge ? <span className="set-secnav-badge">{s.badge}</span> : null}
              </button>
            ))}
          </nav>
          <div className="set-content">
            <div className="set-content-inner">{renderSection(active)}</div>
          </div>
        </div>
      ) : (
        <div className="set-scrollwrap" data-screen-label="Settings · one scroll">
          <div className="set-scrollgrid">
            <div className="set-scrollmain">
              {SET_SECTIONS.map((s, i) => (
                <div key={s.id} className="set-scrollsec">
                  <div className="set-bignum">{String(i + 1).padStart(2, "0")}</div>
                  {renderSection(s.id)}
                </div>
              ))}
            </div>
            <nav className="set-toc" aria-label="On this page">
              <div className="set-toc-label">On this page</div>
              {SET_SECTIONS.map((s, i) => (
                <a
                  key={s.id}
                  href={`#set-${s.id}`}
                  className={`set-toc-btn${i === 0 ? " is-active" : ""}`}
                >
                  <span className="set-toc-num">{String(i + 1).padStart(2, "0")}</span>
                  <span>{s.short}</span>
                </a>
              ))}
            </nav>
          </div>
        </div>
      )}
    </div>
  );
}
