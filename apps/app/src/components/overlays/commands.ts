// The command registry behind the ⌘K palette (FID-7 · BRO-1894). Pure data + pure helpers, so the
// filter / grouping / match-highlight logic is unit-testable without a DOM.
//
// HONEST DATA (the load-bearing rule the fidelity arc keeps relearning): every entry here does
// something the app can actually do RIGHT NOW — navigate to a real route, toggle the real theme, or
// open the real feedback drawer. The prototype's palette also lists "start a session" / "wake maestro"
// / "approve at the gate" + fabricated recent-artifact/search rows; those need a runtime work-dispatch
// API + read paths the client store does not have yet, so a command that would no-op is OMITTED rather
// than shown as if it works. They land when the dispatch seam is wired (P3 chain).

import {
  Boxes,
  History,
  type LucideIcon,
  MessageSquare,
  Settings,
  Share2,
  SunMoon,
  User,
} from "lucide-react";

/** The routes the palette can jump to — a subset of the registered router paths (all param-free). */
export type NavTo = "/" | "/history" | "/knowledge" | "/settings" | "/account";
/** Non-navigation commands, each backed by a real app capability. */
export type CommandAction = "toggle-theme" | "open-feedback";
export type CommandGroup = "Jump to" | "Commands";

export interface Command {
  id: string;
  /** Verb-led / destination name, sentence case (§Voice). */
  title: string;
  /** One-line subtitle. Never a progress %. */
  meta: string;
  icon: LucideIcon;
  group: CommandGroup;
  /** Extra search terms folded into the match (never rendered). */
  keywords?: string;
  /** A single-key hint shown on the right, when the command has one. */
  kbd?: string;
  /** The one accent command (frosted-blue icon chip) — the primary action. */
  accent?: boolean;
  /** Navigation command: the route to go to. */
  to?: NavTo;
  /** Action command: the capability to run. */
  action?: CommandAction;
}

/** The canonical order groups render in — navigation first (the dominant use), then actions. */
export const GROUP_ORDER: CommandGroup[] = ["Jump to", "Commands"];

export const COMMANDS: Command[] = [
  {
    id: "nav-board",
    title: "Maestro",
    meta: "your work at the gate",
    icon: Boxes,
    group: "Jump to",
    to: "/",
    keywords: "board home work gate needs you",
  },
  {
    id: "nav-history",
    title: "History",
    meta: "your runs and the loop's",
    icon: History,
    group: "Jump to",
    to: "/history",
    keywords: "sessions runs past",
  },
  {
    id: "nav-knowledge",
    title: "Knowledge",
    meta: "what the loop remembers",
    icon: Share2,
    group: "Jump to",
    to: "/knowledge",
    keywords: "graph memory notes",
  },
  {
    id: "nav-settings",
    title: "Settings",
    meta: "configure Maestro",
    icon: Settings,
    group: "Jump to",
    to: "/settings",
    keywords: "config preferences appearance theme",
  },
  {
    id: "nav-account",
    title: "Account",
    meta: "you and your autonomy score",
    icon: User,
    group: "Jump to",
    to: "/account",
    keywords: "user profile you",
  },
  {
    id: "act-theme",
    title: "Toggle theme",
    meta: "switch between light and dark",
    icon: SunMoon,
    group: "Commands",
    action: "toggle-theme",
    keywords: "dark light appearance mode",
    accent: true,
  },
  {
    id: "act-feedback",
    title: "Send feedback",
    meta: "hand it to the loop",
    icon: MessageSquare,
    group: "Commands",
    action: "open-feedback",
    keywords: "feedback idea issue bug praise",
  },
];

/** Case-insensitive substring match on title + meta + keywords. An empty query matches everything. */
export function commandMatches(cmd: Command, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return `${cmd.title} ${cmd.meta} ${cmd.keywords ?? ""}`.toLowerCase().includes(q);
}

export function filterCommands(query: string, commands: Command[] = COMMANDS): Command[] {
  return commands.filter((c) => commandMatches(c, query));
}

export interface CommandGroupView {
  label: CommandGroup;
  items: Command[];
}

/** Group the (already filtered) commands in canonical order; empty groups are dropped. */
export function groupCommands(commands: Command[]): CommandGroupView[] {
  return GROUP_ORDER.map((label) => ({
    label,
    items: commands.filter((c) => c.group === label),
  })).filter((g) => g.items.length > 0);
}

export interface MarkSegment {
  text: string;
  hit: boolean;
}

/**
 * Split `text` into segments, flagging the FIRST case-insensitive occurrence of `query` so the view
 * can wrap it in `<em>` (the matched-substring highlight). Pure, so the highlight is unit-testable.
 */
export function markMatch(text: string, query: string): MarkSegment[] {
  const q = query.trim();
  if (!q) return [{ text, hit: false }];
  const i = text.toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return [{ text, hit: false }];
  const segs: MarkSegment[] = [];
  if (i > 0) segs.push({ text: text.slice(0, i), hit: false });
  segs.push({ text: text.slice(i, i + q.length), hit: true });
  if (i + q.length < text.length) segs.push({ text: text.slice(i + q.length), hit: false });
  return segs;
}
