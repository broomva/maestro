// Keep a live Flows page current: re-read it at the interval its `data-live` names and swap the
// page in place, keeping what the reader opened, scrolled, focused and selected. Without JS the
// page is a snapshot and says only when it was read; this script is what marks it live. A new
// version of Maestro stops the refresh until a reload; a page the ledger no longer holds stops its own.
(() => {
  "use strict";
  if (!document.querySelector("#maestro-root[data-web]")) return;
  // body:begin (crates/view/assets/live.test.js runs everything from here to body:end)
  let busy = false;
  // A new version of Maestro stops every refresh; a page the ledger no longer holds stops only
  // its own, so moving to another page refreshes again.
  let versionChanged = false;
  let goneFor = null;
  // Every box that scrolls on its own; each keeps both its scroll positions across a swap.
  const scrollers = ".mf-graph, .mf-table-wrap, .mf-steps";
  const navigating = () => document.querySelector("#maestro-root")?.hasAttribute("aria-busy");
  const selecting = () => {
    const s = getSelection();
    return !!s && !s.isCollapsed && !!live()?.contains(s.anchorNode);
  };
  const live = () => document.querySelector("#maestro-root [data-live]");
  const head = () => document.querySelector(".mv-web-head");
  // focus:begin. Keyboard focus survives a swap: the focused control is found again in the new
  // page by what it is, never by where it sits among other controls. A summary is its
  // disclosure's id; a link is its tag, href and which of the links to that href it is (a run page
  // links its kernel twice). A control with no such identity is left unfocused, not guessed at.
  const focusable = "a[href], summary, button, [tabindex]";
  const identity = (el) => {
    if (el?.tagName === "SUMMARY") {
      const id = el.closest("details")?.id;
      return id ? `details#${id}` : null;
    }
    const href = el?.getAttribute("href");
    return href ? `${el.tagName}:${href}` : null;
  };
  const focusKey = () => {
    const el = document.activeElement;
    if (!el || el === document.body) return null;
    const region = [live(), head()].find(r => r?.contains(el));
    const id = identity(el);
    if (!region || !id) return null;
    const nth = [...region.querySelectorAll(focusable)].filter(e => identity(e) === id).indexOf(el);
    return { inHead: region === head(), id, nth };
  };
  const refocus = (key) => {
    if (!key) return;
    const all = [...((key.inHead ? head() : live())?.querySelectorAll(focusable) ?? [])];
    all.filter(e => identity(e) === key.id)[key.nth]?.focus({ preventScroll: true });
  };
  // focus:end
  // The status line keeps its read time; a state and a note sit beside it.
  const say = (state, note) => {
    const line = document.querySelector("#maestro-root [data-live-status]");
    if (!line) return;
    line.dataset.liveState = state;
    const slot = line.querySelector("[data-live-note]");
    if (slot) slot.textContent = note;
  };
  const mark = () => {
    const every = Math.round(Math.max(1000, Number(live()?.dataset.live) || 5000) / 1000);
    say("live", ` · live, re-read every ${every} s`);
  };
  const sameVersion = (doc) =>
    doc.querySelector("script")?.textContent === document.querySelector("script")?.textContent &&
    [...doc.querySelectorAll("style")].map(e => e.textContent).join("") ===
      [...document.querySelectorAll("style")].map(e => e.textContent).join("");
  const refresh = async () => {
    const now = live();
    if (!now || busy || versionChanged || now === goneFor || document.visibilityState !== "visible" || navigating()) return;
    if (selecting()) { say("paused", " · paused while text is selected"); return; }
    busy = true;
    const url = location.href;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 8000);
    try {
      const response = await fetch(url, { credentials: "same-origin", cache: "no-store", redirect: "error", signal: controller.signal });
      if (response.status === 404) {
        // The ledger no longer holds what this page shows (a run retired out of it, say).
        if (location.href === url && live() === now) {
          goneFor = now;
          say("gone", " · no longer in the ledger, so it stopped updating; reload");
        }
        return;
      }
      if (!response.ok || !response.headers.get("content-type")?.startsWith("text/html")) throw new Error("page unavailable");
      const doc = new DOMParser().parseFromString(await response.text(), "text/html");
      const fresh = doc.querySelector("#maestro-root [data-live]");
      if (!fresh) throw new Error("page unavailable");
      if (!sameVersion(doc)) {
        versionChanged = true;
        say("stale", " · a new version of Maestro is running; reload to keep this page live");
        return;
      }
      // The reader may have moved on, started navigating or begun selecting while the read ran.
      if (location.href !== url || live() !== now || navigating() || selecting()) return;
      const open = new Map([...now.querySelectorAll("details[id]")].map(d => [d.id, d.open]));
      fresh.querySelectorAll("details[id]").forEach(d => { if (open.has(d.id)) d.open = open.get(d.id); });
      const scrolls = [...now.querySelectorAll(scrollers)].map(e => [e.scrollLeft, e.scrollTop]);
      const focused = focusKey();
      now.replaceWith(document.adoptNode(fresh));
      document.querySelectorAll(`#maestro-root :is(${scrollers})`).forEach((e, i) => {
        const [left, top] = scrolls[i] ?? [0, 0];
        e.scrollLeft = left;
        e.scrollTop = top;
      });
      const freshHead = doc.querySelector(".mv-web-head");
      if (freshHead) head()?.replaceWith(document.adoptNode(freshHead));
      refocus(focused);
      mark();
    } catch {
      // Only the page this read was for is said stale: the reader may have moved on meanwhile.
      if (location.href === url && live() === now) say("stale", " · not updating: the last read failed; retrying");
    } finally {
      clearTimeout(timeout);
      busy = false;
    }
  };
  const loop = () => {
    if (versionChanged) return;
    const every = Math.max(1000, Number(live()?.dataset.live) || 5000);
    setTimeout(() => refresh().finally(loop), every);
  };
  document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") refresh(); });
  mark();
  loop();
  // body:end
})();
