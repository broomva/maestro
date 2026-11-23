// Keep the shell in place while fresh server-rendered pages load. Links still work without JS.
(() => {
  "use strict";
  if (!document.querySelector("#maestro-root[data-web]") || document.querySelector("[data-answer-bar], [data-enroll]")) return;
  const routes = new Set(["/decisions", "/today", "/health"]);
  const warm = new Map();
  const scrolls = new Map();
  let current = location.pathname;
  let pending;
  const status = document.createElement("p");
  status.className = "mv-navigation-status";
  status.setAttribute("role", "status");
  document.body.append(status);
  const clearLoading = () => {
    const main = document.querySelector("#maestro-root");
    main?.removeAttribute("aria-busy");
    if (main) main.inert = false;
    document.querySelectorAll(".mv-web-nav [data-pending]").forEach(a => a.removeAttribute("data-pending"));
    status.textContent = "";
  };
  history.replaceState({ ...history.state, maestro: true }, "");
  const destination = (event) => {
    const link = event.target.closest?.("a[href]");
    if (!link || link.target || link.hasAttribute("download")) return;
    const url = new URL(link.href, location.href);
    if (url.origin !== location.origin || !routes.has(url.pathname) || url.search || url.hash) return;
    return url;
  };
  const read = async (url) => {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 10000);
    try {
      const response = await fetch(url, { credentials: "same-origin", cache: "no-store", redirect: "error", signal: controller.signal });
      if (!response.ok || !response.headers.get("content-type")?.startsWith("text/html")) throw new Error("page unavailable");
      const doc = new DOMParser().parseFromString(await response.text(), "text/html");
      if (!doc.querySelector("#maestro-root[data-web]") || !doc.querySelector(".mv-web-head")) throw new Error("incomplete page");
      // Owner forms keep native navigation and browser form/history restoration throughout.
      if (doc.querySelector("[data-answer-bar], [data-enroll]")) throw new Error("owner form requires native navigation");
      // A deployment may change styles or scripts between clicks. Load that document normally.
      if (doc.querySelector("script")?.textContent !== document.querySelector("script")?.textContent ||
          [...doc.querySelectorAll("style")].map(e => e.textContent).join("") !== [...document.querySelectorAll("style")].map(e => e.textContent).join("")) throw new Error("new application version");
      return doc;
    } finally { clearTimeout(timeout); }
  };
  const prefetch = (event) => {
    const url = destination(event);
    // Decision forms are always fetched at the click, never reused from a warm read.
    if (!url || url.pathname === "/decisions" || url.pathname === current || navigator.connection?.saveData) return;
    const previous = warm.get(url.pathname);
    if (previous && performance.now() - previous.at < 2000) return;
    const entry = { at: performance.now(), promise: null };
    entry.promise = read(url.href).then(doc => { entry.at = performance.now(); return doc; }).catch(() => null);
    warm.set(url.pathname, entry);
  };
  document.addEventListener("pointerover", prefetch);
  document.addEventListener("focusin", prefetch);
  document.addEventListener("pointerdown", prefetch);
  // Warm the two read-only destinations once while the first page is idle. The map holds
  // at most two documents; a warm read is consumed once and expires after two seconds.
  const warmTabs = () => document.querySelectorAll(".mv-web-nav a").forEach(target => prefetch({ target }));
  if ("requestIdleCallback" in window) requestIdleCallback(warmTabs, { timeout: 1000 });
  const navigate = async (url, back = false) => {
    const request = {};
    pending = request;
    scrolls.set(current, [scrollX, scrollY]);
    const main = document.querySelector("#maestro-root");
    main.setAttribute("aria-busy", "true");
    main.inert = true;
    status.textContent = `Loading ${url.pathname.slice(1)}…`;
    document.querySelectorAll(".mv-web-nav a").forEach(a => a.toggleAttribute("data-pending", new URL(a.href).pathname === url.pathname));
    try {
      const entry = warm.get(url.pathname);
      warm.delete(url.pathname);
      const doc = entry && performance.now() - entry.at < 2000 ? await entry.promise : await read(url.href);
      if (!doc) throw new Error("warm read failed");
      if (pending !== request) return;
      if (!back) history.pushState({ maestro: true }, "", url.href);
      document.querySelector(".mv-web-head").replaceWith(doc.querySelector(".mv-web-head"));
      main.replaceWith(doc.querySelector("#maestro-root"));
      document.title = doc.title;
      current = url.pathname;
      const next = document.querySelector("#maestro-root");
      next.tabIndex = -1;
      next.focus({ preventScroll: true });
      window.scrollTo(...(back ? scrolls.get(current) || [0, 0] : [0, 0]));
      status.textContent = "";
      document.dispatchEvent(new Event("maestro:page"));
    } catch {
      if (pending === request) {
        // A native fallback can put this document in the browser's back/forward cache.
        // Never leave the restored document inert or showing a loading destination.
        clearLoading();
        location.assign(url.href);
      }
    }
  };
  document.addEventListener("click", (event) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const url = destination(event);
    if (!url) return;
    event.preventDefault();
    navigate(url);
  });
  window.addEventListener("popstate", (event) => {
    if (!event.state?.maestro || !routes.has(location.pathname)) { location.reload(); return; }
    navigate(new URL(location.href), true);
  });
  window.addEventListener("pageshow", () => { pending = null; clearLoading(); });
})();
