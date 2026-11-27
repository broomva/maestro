// Fast SPA navigation across Maestro's web pages (/decisions, /decisions/<l>/<id>, /today, /health).
// Keeps the persistent toast stack alive across swaps, prefetches hovered/next decisions so
// moving between asks is instant, and falls back to normal browser navigation on any error.
(() => {
  "use strict";
  const rootEl = document.querySelector("#maestro-root[data-web]");
  if (!rootEl || document.querySelector("[data-enroll]")) return;

  const isAppPath = (p) => p === "/decisions" || p.startsWith("/decisions/") || p === "/today" || p === "/health";
  const CACHE_TTL_MS = 4000;
  const cache = new Map(); // pathname -> { html, at }
  const inflight = new Map(); // pathname -> Promise<string>
  let pending = null;

  const invalidate = () => {
    cache.clear();
    inflight.clear();
  };

  const fetchHtml = (url) => {
    const key = url.pathname;
    const hit = cache.get(key);
    if (hit && Date.now() - hit.at < CACHE_TTL_MS) {
      return Promise.resolve(hit.html);
    }
    const existing = inflight.get(key);
    if (existing) return existing;
    const p = fetch(url.href, { credentials: "same-origin", headers: { "x-maestro-nav": "1" } })
      .then((res) => {
        if (!res.ok || !res.headers.get("content-type")?.includes("text/html")) {
          throw new Error(`HTTP ${res.status}`);
        }
        return res.text();
      })
      .then((html) => {
        cache.set(key, { html, at: Date.now() });
        inflight.delete(key);
        return html;
      })
      .catch((err) => {
        inflight.delete(key);
        throw err;
      });
    inflight.set(key, p);
    return p;
  };

  const prefetch = (href) => {
    if (!href) return;
    let url;
    try {
      url = new URL(href, location.href);
    } catch {
      return;
    }
    if (url.origin !== location.origin || !isAppPath(url.pathname) || url.pathname === location.pathname) return;
    fetchHtml(url).catch(() => {});
  };

  const prefetchNext = () => {
    const nextLink = document.querySelector(".mv-pager a[rel=next]");
    if (nextLink) {
      prefetch(nextLink.getAttribute("href"));
      return;
    }
    const current = document.querySelector(".mv-queue .mv-qrow a[aria-current=true]");
    const nextRow = current?.closest(".mv-qrow")?.nextElementSibling?.querySelector("a[href]");
    if (nextRow) prefetch(nextRow.getAttribute("href"));
  };

  const clearLoading = () => {
    document.querySelectorAll(".mv-web-nav .mv-tab[data-loading]").forEach((t) => delete t.dataset.loading);
    const root = document.getElementById("maestro-root");
    if (root) {
      delete root.dataset.loading;
      root.removeAttribute("aria-busy");
    }
  };

  const tabForPath = (pathname) => (pathname.startsWith("/decisions") ? "/decisions" : pathname);

  const setLoading = (pathname) => {
    clearLoading();
    const tabPath = tabForPath(pathname);
    const tab = document.querySelector(`.mv-web-nav .mv-tab[href="${tabPath}"]`);
    if (tab && tabPath !== tabForPath(location.pathname)) tab.dataset.loading = "true";
    const root = document.getElementById("maestro-root");
    if (root) {
      root.dataset.loading = "true";
      root.setAttribute("aria-busy", "true");
    }
  };

  const swap = (doc, url, replace) => {
    const nextRoot = doc.getElementById("maestro-root");
    const root = document.getElementById("maestro-root");
    if (!nextRoot || !root) return false;
    root.replaceWith(document.adoptNode(nextRoot));
    for (const sel of [".mv-brand-sub", ".mv-nav-count", ".mv-web-tree"]) {
      const cur = document.querySelector(sel);
      const next = doc.querySelector(sel);
      if (cur && next) cur.replaceWith(document.adoptNode(next));
    }
    const activeTab = tabForPath(url.pathname);
    document.querySelectorAll(".mv-web-nav .mv-tab").forEach((t) => {
      if (t.getAttribute("href") === activeTab) t.setAttribute("aria-current", "page");
      else t.removeAttribute("aria-current");
    });
    document.title = doc.title;
    if (replace) history.replaceState({ maestro: true }, "", url.href);
    else if (url.href !== location.href) history.pushState({ maestro: true }, "", url.href);
    window.scrollTo(0, 0);
    document.dispatchEvent(new Event("maestro:page"));
    setTimeout(prefetchNext, 120);
    return true;
  };

  const navigate = async (url, replace = false) => {
    const req = {};
    pending = req;
    const hit = cache.get(url.pathname);
    const isFresh = hit && Date.now() - hit.at < CACHE_TTL_MS;
    if (!isFresh) setLoading(url.pathname);
    try {
      const html = await fetchHtml(url);
      if (pending !== req) return;
      const doc = new DOMParser().parseFromString(html, "text/html");
      clearLoading();
      if (!swap(doc, url, replace)) location.assign(url.href);
    } catch {
      if (pending !== req) return;
      clearLoading();
      location.assign(url.href);
    }
  };

  const targetUrl = (event) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return null;
    const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (!link || link.target || link.hasAttribute("download")) return null;
    let url;
    try {
      url = new URL(link.href, location.href);
    } catch {
      return null;
    }
    if (url.origin !== location.origin || !isAppPath(url.pathname)) return null;
    if (url.pathname === location.pathname && url.search === location.search && url.hash === location.hash) return null;
    return url;
  };

  history.replaceState({ maestro: true }, "", location.href);

  // Instant hover / focus / touchstart prefetch for zero-wait clicks.
  const onHint = (event) => {
    const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (link) prefetch(link.getAttribute("href"));
  };
  document.addEventListener("pointerenter", onHint, true);
  document.addEventListener("focusin", onHint, true);
  document.addEventListener("touchstart", onHint, { capture: true, passive: true });

  document.addEventListener("click", (event) => {
    const url = targetUrl(event);
    if (!url) return;
    // Optimistically move queue selection highlight on click before swap finishes.
    const clickedLink = event.target instanceof Element ? event.target.closest(".mv-queue .mv-qrow a[href]") : null;
    if (clickedLink) {
      document.querySelectorAll(".mv-queue .mv-qrow a[aria-current=true]").forEach((a) => a.removeAttribute("aria-current"));
      clickedLink.setAttribute("aria-current", "true");
    }
    event.preventDefault();
    navigate(url);
  });

  window.addEventListener("popstate", (event) => {
    if (!event.state?.maestro || !isAppPath(location.pathname)) {
      location.reload();
      return;
    }
    navigate(new URL(location.href), true);
  });

  window.addEventListener("pageshow", () => {
    pending = null;
    clearLoading();
  });

  window.__maestroNavigate = (href) => {
    try {
      const url = new URL(href, location.href);
      if (url.origin === location.origin && isAppPath(url.pathname)) {
        navigate(url);
      } else {
        location.assign(url.href);
      }
    } catch {
      location.assign(href);
    }
  };
  window.__maestroInvalidate = invalidate;
  setTimeout(prefetchNext, 120);
})();
