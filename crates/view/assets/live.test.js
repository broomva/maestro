// The live script (crates/view/assets/live.js): focus across a swap (between `focus:begin` and
// `focus:end`), then the refresh itself (between `body:begin` and `body:end`). Focus across a swap:
// the focused control is found again by what it is, never by where it sits. Runs the script's own
// code, cut from the file, against small stand-ins for the page. `bun crates/view/assets/live.test.js`
// (scripts/ci.sh); an optional path tests another copy of the script, such as an earlier one.
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";

const path = process.argv[2] ?? new URL("./live.js", import.meta.url).pathname;
const source = readFileSync(path, "utf8");
const from = source.indexOf("  // focus:begin");
const to = source.indexOf("  // focus:end");
if (from < 0 || to < 0) throw new Error(`${path}: no focus:begin and focus:end markers`);

let failed = 0;
const check = (name, ok) => {
  console.log(`${ok ? "ok  " : "FAIL"}  ${name}`);
  if (!ok) failed++;
};

// A control: a tag, an href, and the disclosure it sits in.
const control = (name, tagName, { href = null, details = null } = {}) => ({
  name,
  tagName,
  getAttribute: (a) => (a === "href" ? href : null),
  closest: (q) => (q === "details" && details !== null ? { id: details } : null),
  focus() { focused = this; },
});
let focused = null;

// One page region holding `controls`, and the code under test bound to it.
const page = (controls) => {
  const region = { contains: (e) => controls.includes(e), querySelectorAll: () => controls };
  const document = { body: {}, activeElement: null };
  const code = runInNewContext(
    `${source.slice(from, to)}\n({ focusKey, refocus })`,
    { document, live: () => region, head: () => null },
  );
  return { document, ...code, set: (c) => controls.splice(0, controls.length, ...c) };
};

{
  // The run before the focused summary leaves the list: the focus stays on the same summary.
  const run = control("a run", "A", { href: "/flows/run/fi_1" });
  const idle = control("idle summary", "SUMMARY", { details: "mf-idle" });
  const source = control("source summary", "SUMMARY", { details: "mf-source" });
  const p = page([run, idle, source]);
  p.document.activeElement = source;
  const key = p.focusKey();
  const fresh = [control("idle summary", "SUMMARY", { details: "mf-idle" }), control("source summary", "SUMMARY", { details: "mf-source" })];
  p.set(fresh);
  focused = null;
  p.refocus(key);
  check("a summary keeps its focus when a control before it goes", focused === fresh[1]);
}

{
  // A link is found again by its href wherever it moved.
  const a = control("kernel a", "A", { href: "/flows/kernel/a" });
  const b = control("kernel b", "A", { href: "/flows/kernel/b" });
  const p = page([a, b]);
  p.document.activeElement = b;
  const key = p.focusKey();
  const fresh = [control("kernel b", "A", { href: "/flows/kernel/b" }), control("new", "A", { href: "/flows/kernel/c" })];
  p.set(fresh);
  focused = null;
  p.refocus(key);
  check("a link keeps its focus by its href", focused === fresh[0]);
}

{
  // Two links to one place (a run page's breadcrumb and its lineage both link the kernel): the
  // second keeps its focus, on an unchanged page and after a control before both goes.
  const twice = () => [control("breadcrumb", "A", { href: "/flows/kernel/ship" }), control("lineage", "A", { href: "/flows/kernel/ship" })];
  const before = [control("a run", "A", { href: "/flows/run/x" }), ...twice()];
  const p = page(before);
  p.document.activeElement = before[2];
  const key = p.focusKey();
  let fresh = [control("a run", "A", { href: "/flows/run/x" }), ...twice()];
  p.set(fresh);
  focused = null;
  p.refocus(key);
  check("the second of two links to one place keeps its focus", focused === fresh[2]);
  fresh = twice();
  p.set(fresh);
  focused = null;
  p.refocus(key);
  check("and keeps it when a control before both goes", focused === fresh[1]);
}

{
  // A control with no identity is not guessed at: nothing is focused rather than a neighbour.
  const loose = control("summary without an id", "SUMMARY", { details: "" });
  const other = control("another summary", "SUMMARY", { details: "" });
  const p = page([loose, other]);
  p.document.activeElement = other;
  const key = p.focusKey();
  p.set([control("summary without an id", "SUMMARY", { details: "" })]);
  focused = null;
  p.refocus(key);
  check("a control with no identity is left unfocused", focused === null);
}

// The refresh: the script's whole body (between `body:begin` and `body:end`) against stand-ins for
// the page, the network and the parser. Each case runs one `refresh()` and checks what it left.
const body = (() => {
  const a = source.indexOf("  // body:begin");
  const b = source.indexOf("  // body:end");
  if (a < 0 || b < 0) throw new Error(`${path}: no body:begin and body:end markers`);
  return source.slice(a, b);
})();

// A page: a live block (with its status line) and a header, which a swap replaces.
const block = (name) => {
  const note = { textContent: "" };
  const status = { dataset: {}, querySelector: (q) => (q === "[data-live-note]" ? note : null) };
  return { name, dataset: { live: "2000" }, status, note, querySelectorAll: () => [], contains: () => false };
};
const run = async ({ respond, script = "s", afterFetch = () => {}, twice = false }) => {
  const shown = { live: block("old"), head: { name: "old head" } };
  shown.live.replaceWith = (x) => { shown.live = x; };
  shown.head.replaceWith = (x) => { shown.head = x; };
  const location = { href: "http://maestro/flows/run/fi_1" };
  let fetched = 0;
  const document = {
    body: {},
    activeElement: null,
    addEventListener: () => {},
    visibilityState: "visible",
    adoptNode: (x) => x,
    querySelector: (q) =>
      q === "#maestro-root [data-live]" ? shown.live
        : q === "#maestro-root [data-live-status]" ? shown.live.status
        : q === ".mv-web-head" ? shown.head
        : q === "#maestro-root" ? { hasAttribute: () => false }
        : q === "script" ? { textContent: "s" }
        : null,
    querySelectorAll: () => [],
  };
  const fresh = block("new");
  const parsed = {
    querySelector: (q) =>
      q === "#maestro-root [data-live]" ? fresh : q === ".mv-web-head" ? { name: "new head" } : q === "script" ? { textContent: script } : null,
    querySelectorAll: () => [],
  };
  const fetch = async () => {
    fetched++;
    afterFetch(location);
    return respond();
  };
  // Timers held, not run: the loop's ticks are run by hand; the read's abort timer never fires.
  const timers = new Map();
  let next = 1;
  const setTimeout = (fn, ms) => { timers.set(next, { fn, ms }); return next++; };
  const clearTimeout = (id) => timers.delete(id);
  const sandbox = {
    document, location, fetch, AbortController, setTimeout, clearTimeout,
    getSelection: () => ({ isCollapsed: true }),
    DOMParser: class { parseFromString() { return parsed; } },
  };
  const before = shown.live;
  // Evaluating the body starts the script as the page does: it marks the page live and schedules
  // the first tick.
  const api = runInNewContext(`${body}\n({ refresh, loop })`, sandbox);
  const started = { state: before.status.dataset.liveState, ticks: [...timers.values()].filter((t) => t.ms === 2000).length };
  await api.refresh();
  // A second refresh, for the cases that must have stopped: it fetches nothing more.
  if (twice) await api.refresh();
  // Run the loop's next tick: the one timer at the page's interval.
  const tick = async () => {
    const [id, t] = [...timers].find(([, t]) => t.ms === 2000) ?? [];
    if (!t) return false;
    timers.delete(id);
    await t.fn();
    await new Promise((r) => globalThis.setTimeout(r, 0));
    return true;
  };
  return {
    shown, before, fresh, api, tick, started,
    get fetched() { return fetched; },
    ticks: () => [...timers.values()].filter((t) => t.ms === 2000).length,
  };
};
const ok = () => ({ status: 200, ok: true, headers: { get: () => "text/html; charset=utf-8" }, text: async () => "<html>" });

{
  const r = await run({ respond: ok });
  check("a read swaps the page and its header in place", r.shown.live === r.fresh && r.shown.head.name === "new head");
  check("and marks the new page live", r.fresh.status.dataset.liveState === "live" && r.fresh.note.textContent.includes("live"));
}
{
  const r = await run({ respond: () => { throw new Error("offline"); } });
  check("a failed read keeps the page", r.shown.live === r.before);
  check("and says it is not updating, beside its read time", r.before.status.dataset.liveState === "stale" && r.before.note.textContent.includes("not updating"));
}
{
  const r = await run({ respond: ok, afterFetch: (l) => { l.href = "http://maestro/flows"; } });
  check("a read the reader navigated away from swaps nothing", r.shown.live === r.before);
}
{
  const r = await run({ respond: () => { throw new Error("late"); }, afterFetch: (l) => { l.href = "http://maestro/flows"; } });
  check("nor marks a page it was not for as stale", r.before.status.dataset.liveState === "live");
}
{
  const r = await run({ respond: ok, script: "a new version", twice: true });
  check("a new version stops the refresh and swaps nothing", r.shown.live === r.before && r.before.status.dataset.liveState === "stale" && r.fetched === 1);
}
{
  const r = await run({ respond: () => ({ status: 404, ok: false, headers: { get: () => "text/plain" }, text: async () => "" }), twice: true });
  check("a page the ledger no longer holds stops and says so", r.before.status.dataset.liveState === "gone" && r.fetched === 1 && r.shown.live === r.before);
}

{
  // Starting: the script marks the page live and schedules one tick; each tick refreshes and
  // schedules the next.
  const r = await run({ respond: ok });
  check("starting marks the page live and schedules one tick", r.started.state === "live" && r.started.ticks === 1);
  check("the loop keeps one tick at the page's interval", r.ticks() === 1);
  const before = r.fetched;
  await r.tick();
  check("a tick reads the page and schedules the next", r.fetched === before + 1 && r.ticks() === 1);
}
{
  // A page the ledger no longer holds stops only itself: another page refreshes again.
  let status = 404;
  const r = await run({ respond: () => ({ status, ok: status === 200, headers: { get: () => "text/html" }, text: async () => "" }) });
  const after404 = r.fetched;
  await r.tick();
  check("a gone page is not read again", r.fetched === after404 && r.ticks() === 1);
  r.shown.live = block("another page");
  r.shown.live.replaceWith = () => {};
  status = 200;
  await r.tick();
  check("and moving to another page reads again", r.fetched === after404 + 1);
}
{
  // A new version of Maestro stops the loop itself.
  const r = await run({ respond: ok, script: "a new version" });
  await r.tick();
  check("a new version leaves no tick scheduled", r.ticks() === 0);
}

if (failed) {
  console.log(`${failed} failed`);
  process.exit(1);
}
console.log("focus and refresh: all checks passed");
