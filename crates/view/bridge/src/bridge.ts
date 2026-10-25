// The one script in Maestro's MCP App. It connects to the host with the official
// MCP Apps client, takes the HTML that maestro_ui_render returned, and keeps it
// current by polling the app-only maestro_ui_poll tool with the revision it holds.
// It renders nothing itself: every value on screen came from Rust.
import { App, applyDocumentTheme, applyHostStyleVariables } from "@modelcontextprotocol/ext-apps/app-with-deps";

type Rendered = { html?: unknown; revision?: unknown; spec?: unknown };

const root = document.getElementById("maestro-root") as HTMLElement;
const pollMs = Math.max(5000, Number(root.dataset.pollMs) || 15000);
let spec: unknown = null;
let revision: string | null = null;
let timer: ReturnType<typeof setTimeout> | undefined;

// What the owner chose on screen, which a swap must keep: the tab checked in each radio group
// and the inbox rows opened, by block and ask. Markup from Rust carries no such state.
function held() {
  const tabs = [...root.querySelectorAll<HTMLInputElement>("input[type=radio]:checked")].map((i) => [i.name, i.value]);
  const open = [...root.querySelectorAll<HTMLElement>(".mv-irow[data-ask]")]
    .filter((li) => li.querySelector(":scope > details")?.hasAttribute("open"))
    .map((li) => `${li.closest("[data-block]")?.getAttribute("data-block")}\u0000${li.dataset.ask}`);
  return { tabs, open: new Set(open) };
}

function restore(state: ReturnType<typeof held>) {
  for (const [name, value] of state.tabs) {
    for (const input of root.querySelectorAll<HTMLInputElement>("input[type=radio]")) {
      if (input.name === name && input.value === value) input.checked = true;
    }
  }
  for (const li of root.querySelectorAll<HTMLElement>(".mv-irow[data-ask]")) {
    const key = `${li.closest("[data-block]")?.getAttribute("data-block")}\u0000${li.dataset.ask}`;
    if (state.open.has(key)) li.querySelector(":scope > details")?.setAttribute("open", "");
  }
}

function swap(html: string) {
  const apply = () => {
    const state = held();
    root.innerHTML = html;
    restore(state);
  };
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  type Transition = { ready: Promise<void>; finished: Promise<void>; updateCallbackDone: Promise<void> };
  const doc = document as Document & { startViewTransition?: (cb: () => void) => Transition };
  if (still || typeof doc.startViewTransition !== "function") return apply();
  // The update runs even when the browser skips the animation (WebKit skips it when the host
  // resizes the frame mid-swap); only the animation's promises reject, so settle them here.
  const t = doc.startViewTransition(apply);
  for (const p of [t.ready, t.finished, t.updateCallbackDone]) p.catch(() => {});
}

// Accept one render or poll result. Unchanged polls carry no html.
function take(result: { structuredContent?: Rendered } | undefined) {
  const s = result?.structuredContent;
  if (!s) return;
  if (s.spec !== undefined) spec = s.spec;
  if (typeof s.revision !== "string") return;
  if (typeof s.html === "string" && s.revision !== revision) swap(s.html);
  revision = s.revision;
  root.dataset.revision = revision;
}

function schedule() {
  clearTimeout(timer);
  timer = setTimeout(poll, pollMs);
}

async function poll() {
  if (spec === null || document.hidden) return schedule();
  try {
    take((await app.callServerTool({ name: "maestro_ui_poll", arguments: { spec, revision } })) as { structuredContent?: Rendered });
    root.dataset.stale = "false";
  } catch {
    // Keep the last render and say so; never blank the screen on a failed poll.
    root.dataset.stale = "true";
  }
  schedule();
}

function theme(ctx: ReturnType<App["getHostContext"]>) {
  if (ctx?.theme) applyDocumentTheme(ctx.theme);
  if (ctx?.styles?.variables) applyHostStyleVariables(ctx.styles.variables);
}

const app = new App({ name: "maestro-view", version: "1" }, {});
app.ontoolresult = (result) => {
  take(result as { structuredContent?: Rendered });
  schedule();
};
app.onhostcontextchanged = theme;
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) poll();
});
await app.connect();
theme(app.getHostContext());
