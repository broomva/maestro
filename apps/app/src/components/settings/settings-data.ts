// Settings sample data (BRO-1893 FID-6 slice 3, `MccSettings`). The engine-room config surfaces —
// runners, credentials, routines, members — have NO runtime read-write path yet, so these are SAMPLE
// rows, shown under the page's "preview" affordance (honest, never faked as live — the Knowledge
// "sample" pattern). Appearance (theme) is the one live section and does not live here. Ported from
// ConceptSettings.jsx's module constants; colours pinned to the cool axis (no warm --bv-purple).

/** A section in the two-pane nav / one-scroll ToC. Icons are mapped to Lucide in the component. */
export interface SetSection {
  id: string;
  label: string;
  short: string;
  badge?: number;
}

export const SET_SECTIONS: readonly SetSection[] = [
  { id: "runners", label: "Runners and worktrees", short: "Runners" },
  { id: "creds", label: "Credentials and scopes", short: "Credentials", badge: 1 },
  { id: "autonomy", label: "Autonomy defaults", short: "Autonomy" },
  { id: "routines", label: "Routines and wake", short: "Routines" },
  { id: "notify", label: "Notifications", short: "Notifications" },
  { id: "appearance", label: "Appearance", short: "Appearance" },
  { id: "members", label: "Workspace and members", short: "Members" },
];

export type CredStatus = "ok" | "warn" | "off";
export interface Cred {
  name: string;
  glyph: string;
  desc: string;
  /** A trailing "?" marks a missing scope. */
  scopes: readonly string[];
  status: CredStatus;
}

export const CREDS: readonly Cred[] = [
  {
    name: "GitHub",
    glyph: "GH",
    desc: "hawthorne · 4 repos",
    scopes: ["repo", "workflow", "read:org"],
    status: "ok",
  },
  {
    name: "Linear",
    glyph: "LN",
    desc: "import cycles · blocking 1 run",
    scopes: ["read", "write?"],
    status: "warn",
  },
  {
    name: "Anthropic API",
    glyph: "AI",
    desc: "runner claude · sonnet + opus",
    scopes: ["messages", "batches"],
    status: "ok",
  },
  {
    name: "Obsidian vault",
    glyph: "OB",
    desc: "the session log writes here",
    scopes: ["local fs"],
    status: "ok",
  },
  { name: "Slack", glyph: "SL", desc: "not connected", scopes: [], status: "off" },
];

export interface Routine {
  name: string;
  when: string;
  on: boolean;
}
export const ROUTINES: readonly Routine[] = [
  { name: "Nightly digest", when: "daily · 02:00", on: true },
  { name: "Morning briefing", when: "weekdays · 07:30", on: true },
  { name: "Linear import", when: "every 6h", on: false },
];

export interface Wake {
  name: string;
  desc: string;
  on: boolean;
}
export const WAKES: readonly Wake[] = [
  { name: "On push to main", desc: "review and queue follow-up work", on: true },
  { name: "On new issue", desc: "triage into the right folder", on: true },
  { name: "On credential restored", desc: "retry the runs it blocked", on: true },
];

export type MemberRole = "Owner" | "Operator" | "Viewer";
export interface Member {
  name: string;
  role: MemberRole;
  email: string;
  color: string;
}
export const MEMBERS: readonly Member[] = [
  { name: "Ana Diaz", role: "Owner", email: "ana@broomva.ai", color: "var(--bv-gray-600)" },
  { name: "Theo Park", role: "Operator", email: "theo@broomva.ai", color: "var(--bv-blue)" },
  { name: "Maya Lin", role: "Viewer", email: "maya@broomva.ai", color: "var(--bv-glow-indigo)" },
];
