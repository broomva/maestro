// Run this expression twice on a read-only Maestro page. First run arms a transport stall
// and clicks Decisions; after native fallback, rerun to assert the saved pagehide receipt.
// Only synthetic test observations are stored; no ledger, draft or owner action is written.
(() => {
  const key = "maestro-stalled-navigation-check";
  const saved = sessionStorage.getItem(key);
  if (saved) {
    sessionStorage.removeItem(key);
    const receipt = JSON.parse(saved);
    if (receipt.inert || receipt.busy || receipt.status || receipt.elapsedMs < 9000 || receipt.elapsedMs > 14000) throw new Error(`bad timeout recovery: ${saved}`);
    return { check: "stalled fetch times out, clears loading and falls back natively", pass: true, ...receipt };
  }
  if (document.querySelector("[data-answer-bar], [data-enroll]")) throw new Error("start on a read-only page");
  const started = performance.now();
  window.addEventListener("pagehide", () => {
    const main = document.querySelector("main");
    sessionStorage.setItem(key, JSON.stringify({ inert: main.inert, busy: main.hasAttribute("aria-busy"), status: document.querySelector(".mv-navigation-status")?.textContent || "", elapsedMs: Math.round(performance.now() - started) }));
  }, { once: true });
  window.fetch = (_url, options) => new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => reject(new DOMException("test transport stalled", "AbortError")), { once: true });
  });
  document.querySelector(".mv-web-nav a[href='/decisions']").click();
  return { armed: true, rerunAfterNativeNavigation: true };
})()
