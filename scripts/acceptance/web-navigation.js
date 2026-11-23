// Run as an expression in the browser on a running Maestro web page. No owner verb is called.
// Exercises the served script and real rendered responses, rather than a mock router.
(async () => {
  const results = [];
  const token = {};
  window.maestroNavigationCheck = token;
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  if (document.querySelector("[data-answer-bar], [data-enroll]")) {
    assert(!document.querySelector(".mv-navigation-status"), "the tab router must not attach to owner forms");
    return [{ check: "owner forms retain native browser navigation", pass: true }];
  }
  const click = (path) => {
    const link = document.querySelector(`.mv-web-nav a[href='${path}']`);
    assert(link, `missing ${path} link`);
    link.click();
  };
  const arrived = (path, action) => new Promise((resolve, reject) => {
    const started = performance.now();
    const timeout = setTimeout(() => { document.removeEventListener("maestro:page", done); reject(new Error(`timed out on ${path}`)); }, 15000);
    const done = () => {
      if (location.pathname !== path) return;
      clearTimeout(timeout);
      document.removeEventListener("maestro:page", done);
      resolve(performance.now() - started);
    };
    document.addEventListener("maestro:page", done);
    action();
  });
  for (const path of ["/today", "/health", "/decisions"]) {
    const ms = await arrived(path, () => click(path));
    assert(window.maestroNavigationCheck === token, "the document reloaded");
    assert(document.querySelector(".mv-web-nav [aria-current='page']")?.getAttribute("href") === path, "wrong selected destination");
    assert(document.activeElement === document.querySelector("main"), "new content did not receive focus");
    assert(!document.querySelector("main[aria-busy]"), "loading state did not clear");
    assert(document.documentElement.scrollWidth <= innerWidth, "page overflows horizontally");
    results.push({ check: `same document, focus, selection, overflow: ${path}`, ms: Math.round(ms), pass: true });
  }
  await arrived("/health", () => click("/health"));
  await arrived("/decisions", () => history.back());
  results.push({ check: "browser Back returns to Decisions", pass: true });
  const originalFetch = window.fetch;
  try {
    // The older Today response arrives after Health; it must never replace Health.
    window.fetch = async (...args) => {
      const response = await originalFetch(...args);
      if (new URL(args[0], location.href).pathname === "/today") await new Promise(resolve => setTimeout(resolve, 300));
      return response;
    };
    await arrived("/health", () => { click("/today"); click("/health"); });
    await new Promise(resolve => setTimeout(resolve, 600));
    assert(location.pathname === "/health", "an older response replaced the last click");
    results.push({ check: "rapid navigation keeps the last destination", pass: true });
  } finally { window.fetch = originalFetch; }
  return results;
})()
