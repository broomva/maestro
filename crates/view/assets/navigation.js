// Fast SPA navigation across Maestro's web pages (/decisions, /decisions/<l>/<id>, /today, /health).
// Keeps the persistent toast stack alive across swaps, prefetches hovered/next decisions with
// generation-guarded invalidation, verifies inline script/style identity across server upgrades,
// preserves focus and screen-reader announcements, and falls back to full navigation on any error.
(() => {
  "use strict";
  const rootEl = document.querySelector("#maestro-root[data-web]");
  if (!rootEl || document.querySelector("[data-enroll]")) return;

  const isAppPath = (p) => p === "/decisions" || p.startsWith("/decisions/") || p === "/today" || p === "/health";
  const CACHE_TTL_MS = 4000;
  const FETCH_TIMEOUT_MS = 10000;
  const cache = new Map(); // pathname -> { html, at }
  const inflight = new Map(); // pathname -> Promise<string>
  let generation = 0;
  let pending = null;

  const statusNote = document.createElement("p");
  statusNote.className = "mv-sr";
  statusNote.setAttribute("role", "status");
  statusNote.setAttribute("aria-live", "polite");
  statusNote.setAttribute("aria-atomic", "true");
  document.body.append(statusNote);

  const announce = (text) => {
    statusNote.textContent = "";
    setTimeout(() => { statusNote.textContent = text; }, 0);
  };

  const assetSignature = (doc) => {
    const scripts = [...doc.querySelectorAll("script")].map((s) => s.textContent || "").join("\u0000");
    const styles = [...doc.querySelectorAll("style")].map((s) => s.textContent || "").join("\u0000");
    return `${scripts}\u0001${styles}`;
  };
  const currentAssets = assetSignature(document);

  const invalidate = () => {
    generation++;
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
    const gen = generation;
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), FETCH_TIMEOUT_MS);
    const p = fetch(url.href, {
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      headers: { "x-maestro-nav": "1" },
      signal: ac.signal,
    })
      .then((res) => {
        if (!res.ok || !res.headers.get("content-type")?.includes("text/html")) {
          throw new Error(`HTTP ${res.status}`);
        }
        return res.text();
      })
      .then((html) => {
        if (gen === generation) cache.set(key, { html, at: Date.now() });
        if (inflight.get(key) === p) inflight.delete(key);
        clearTimeout(timer);
        return html;
      })
      .catch((err) => {
        if (inflight.get(key) === p) inflight.delete(key);
        clearTimeout(timer);
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
    document.querySelectorAll(".mv-web-nav .mv-tab[data-pending]").forEach((t) => delete t.dataset.pending);
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
    if (tab && tabPath !== tabForPath(location.pathname)) tab.dataset.pending = "true";
    const root = document.getElementById("maestro-root");
    if (root) {
      root.dataset.loading = "true";
      root.setAttribute("aria-busy", "true");
    }
  };

  const swap = (doc, url, replace) => {
    if (assetSignature(doc) !== currentAssets) return false;
    const nextRoot = doc.getElementById("maestro-root");
    const root = document.getElementById("maestro-root");
    if (!nextRoot || !root) return false;
    const adopted = document.adoptNode(nextRoot);
    root.replaceWith(adopted);
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
    adopted.setAttribute("tabindex", "-1");
    adopted.focus({ preventScroll: true });
    announce(doc.title || "Page updated");
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
    // Let native browser hash jumps handle same-page fragment links.
    if (url.pathname === location.pathname && url.search === location.search) return null;
    return url;
  };

  history.replaceState({ maestro: true }, "", location.href);

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
