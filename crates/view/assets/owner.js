// Maestro's owner script (BRO-4042): the one script the web pages run, pinned by its hash.
// It pre-fetches passkey challenges for single-tap Touch ID / Face ID, turns a chosen option
// into a signed answer, moves immediately to the next open decision while a persistent toast
// tracks the undo window and background write, and enrolls the owner's passkey on /owner.
// It reads nothing but the page and talks only to its own origin.
(() => {
  "use strict";
  const b64u = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const unb64u = (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  const post = async (path, body, signal) => {
    const res = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      credentials: "same-origin",
      signal,
    });
    const data = await res.json().catch(() => ({ error: `the server answered ${res.status}` }));
    if (!res.ok) throw new Error(data.error || `the server answered ${res.status}`);
    return data;
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const askKey = (ledger, handle) => `${ledger}\u0000${handle}`;
  const decisionUrl = (ledger, handle) => `/decisions/${encodeURIComponent(ledger)}/${encodeURIComponent(handle)}`;

  // ---- Persistent Toast Stack ----
  const STORE_KEY = "maestro:toasts:v1";
  const toastHost = document.querySelector("[data-toasts]");
  const activeToasts = new Map(); // id -> { el, meta, settled, dismissed }
  const inFlightKeys = new Set(); // askKey -> currently signed/applying
  const writtenKeys = new Set(); // askKey -> written in this session (avoid badge bounce before next fetch)

  let toastAnnouncer = null;
  if (toastHost) {
    toastAnnouncer = document.createElement("p");
    toastAnnouncer.className = "mv-sr";
    toastAnnouncer.setAttribute("role", "status");
    toastAnnouncer.setAttribute("aria-live", "polite");
    toastAnnouncer.setAttribute("aria-atomic", "true");
    toastHost.append(toastAnnouncer);
  }
  const announceToast = (msg) => {
    if (!toastAnnouncer || !msg) return;
    toastAnnouncer.textContent = "";
    setTimeout(() => { toastAnnouncer.textContent = msg; }, 0);
  };

  const renderFailureToast = (titleText, url, message) => {
    if (!toastHost) return;
    const el = document.createElement("div");
    el.className = "mv-toast";
    el.dataset.tone = "fail";
    const head = document.createElement("div");
    head.className = "mv-toast-head";
    const copy = document.createElement("div");
    copy.className = "mv-toast-copy";
    const title = document.createElement("p");
    title.className = "mv-toast-title";
    title.textContent = `Could not submit · ${titleText}`;
    const sub = document.createElement("p");
    sub.className = "mv-toast-sub";
    sub.textContent = message || "Answer submission failed.";
    copy.append(title, sub);
    const close = document.createElement("button");
    close.type = "button";
    close.className = "mv-toast-close";
    close.setAttribute("aria-label", "Dismiss notification");
    close.textContent = "×";
    close.addEventListener("click", () => el.remove());
    head.append(copy, close);
    const actions = document.createElement("div");
    actions.className = "mv-toast-actions";
    if (url) {
      const a = document.createElement("a");
      a.className = "mv-toast-btn";
      a.href = url;
      a.textContent = "Review decision";
      actions.append(a);
    }
    el.append(head, actions);
    toastHost.append(el);
    announceToast(`Could not submit answer: ${message || "Answer submission failed."}`);
  };

  const loadStored = () => {
    try {
      const raw = sessionStorage.getItem(STORE_KEY);
      const parsed = raw ? JSON.parse(raw) : [];
      return Array.isArray(parsed) ? parsed : [];
    } catch {
      return [];
    }
  };
  const saveStored = () => {
    try {
      const list = [];
      for (const t of activeToasts.values()) {
        if (!t.settled) list.push(t.meta);
      }
      if (list.length) sessionStorage.setItem(STORE_KEY, JSON.stringify(list));
      else sessionStorage.removeItem(STORE_KEY);
    } catch {
      // sessionStorage can be unavailable in strict private contexts.
    }
  };

  const hrefToAskKey = (href) => {
    const m = href.match(/^\/decisions\/([^/?#]+)\/([^/?#]+)/);
    if (!m) return null;
    try {
      return askKey(decodeURIComponent(m[1]), decodeURIComponent(m[2]));
    } catch {
      return null;
    }
  };

  const syncPendingUi = () => {
    const rows = document.querySelectorAll(".mv-queue .mv-qrow");
    for (const row of rows) {
      const link = row.querySelector("a[href]");
      if (!link) continue;
      const key = hrefToAskKey(link.getAttribute("href") || "");
      if (key && inFlightKeys.has(key)) {
        row.dataset.pending = "true";
        row.dataset.clientPending = "true";
      } else if (row.dataset.clientPending === "true") {
        delete row.dataset.pending;
        delete row.dataset.clientPending;
      }
    }
    const navCount = document.querySelector(".mv-web-nav .mv-nav-count");
    if (navCount) {
      const base = navCount.dataset.serverCount ?? navCount.textContent.trim();
      navCount.dataset.serverCount = base;
      if (/^\d+$/.test(base)) {
        let localAnswered = 0;
        const rowsInDom = document.querySelectorAll(".mv-queue .mv-qrow a[href]");
        if (rowsInDom.length) {
          for (const a of rowsInDom) {
            const k = hrefToAskKey(a.getAttribute("href") || "");
            if (k && (inFlightKeys.has(k) || writtenKeys.has(k))) localAnswered++;
          }
        } else {
          localAnswered = inFlightKeys.size;
        }
        const n = Math.max(0, Number(base) - localAnswered);
        navCount.textContent = String(n);
      }
    }
  };

  const summarizeOutcome = (receipt) => {
    const parts = [];
    let warn = false;
    if (receipt.woke) {
      const w = receipt.woke;
      if (w.startsWith("told T3 thread")) parts.push("Agent woken");
      else if (w.includes("routed to fallback T3 thread")) parts.push("Routed to orchestrator");
      else if (w === "no session is named on the ask") {
        // Omit noise when no session was attached.
      } else {
        warn = true;
        parts.push(w);
      }
    }
    if (receipt.landing) {
      const l = receipt.landing;
      const pr = l.match(/landing PR #(\d+)/);
      if (l === "local only (not pushed)") parts.push("Saved locally");
      else if (pr && l.includes("set to merge")) parts.push(`PR #${pr[1]} queued`);
      else if (pr && l.includes("auto-merge is disabled")) parts.push(`PR #${pr[1]} open`);
      else if (pr) {
        warn = true;
        parts.push(l);
      } else {
        warn = true;
        parts.push(l);
      }
    }
    return { text: parts.join(" · "), warn };
  };

  const renderToast = (meta, initialReceipt) => {
    if (!toastHost) return null;
    const el = document.createElement("div");
    el.className = "mv-toast";
    el.dataset.tone = "pending";

    const head = document.createElement("div");
    head.className = "mv-toast-head";
    const copy = document.createElement("div");
    copy.className = "mv-toast-copy";
    const title = document.createElement("p");
    title.className = "mv-toast-title";
    title.textContent = `Answered · ${meta.optionLabel || meta.option || "saved"}`;
    const sub = document.createElement("p");
    sub.className = "mv-toast-sub";
    sub.setAttribute("aria-live", "off");
    sub.textContent = "Saving in 8s...";
    copy.append(title, sub);

    const close = document.createElement("button");
    close.type = "button";
    close.className = "mv-toast-close";
    close.setAttribute("aria-label", "Dismiss notification");
    close.textContent = "×";

    head.append(copy, close);

    const actions = document.createElement("div");
    actions.className = "mv-toast-actions";

    const undo = document.createElement("button");
    undo.type = "button";
    undo.className = "mv-toast-btn";
    undo.textContent = "Undo";
    actions.append(undo);

    const bar = document.createElement("div");
    bar.className = "mv-toast-bar";

    el.append(head, actions, bar);
    toastHost.append(el);
    announceToast(`Answered ${meta.optionLabel || meta.option || ""}. Saving in 8 seconds. Undo is available.`);

    const state = { el, meta, settled: false, dismissed: false };
    activeToasts.set(meta.id, state);
    if (meta.ledger && meta.handle) inFlightKeys.add(askKey(meta.ledger, meta.handle));
    saveStored();
    syncPendingUi();

    const dismiss = () => {
      state.dismissed = true;
      state.settled = true;
      if (meta.ledger && meta.handle) inFlightKeys.delete(askKey(meta.ledger, meta.handle));
      activeToasts.delete(meta.id);
      saveStored();
      el.remove();
      syncPendingUi();
    };
    close.addEventListener("click", dismiss);

    const addOpenLink = (label) => {
      if (!meta.url) return;
      actions.replaceChildren();
      const a = document.createElement("a");
      a.className = "mv-toast-btn";
      a.href = meta.url;
      a.textContent = label;
      actions.append(a);
    };

    const windowMs = Math.min(60000, Math.max(1000, (meta.applies_at_ms - ((meta.signed_local_ms ?? Date.now()) + (meta.offset || 0))) || 8000));

    undo.addEventListener("click", async () => {
      undo.disabled = true;
      sub.textContent = "Undoing...";
      try {
        await post("/owner/answer/undo", { id: meta.id });
        state.settled = true;
        if (meta.ledger && meta.handle) inFlightKeys.delete(askKey(meta.ledger, meta.handle));
        saveStored();
        el.dataset.tone = "warn";
        title.textContent = "Answer undone";
        sub.textContent = "Nothing was written to the ledger.";
        announceToast("Answer undone. Nothing was written to the ledger.");
        addOpenLink("Return to decision");
        window.__maestroInvalidate?.();
        syncPendingUi();
      } catch (e) {
        undo.disabled = false;
        sub.textContent = e.message;
      }
    });

    const poll = async () => {
      let receipt = initialReceipt || { id: meta.id, stage: "signed", applies_at_ms: meta.applies_at_ms };
      const started = Date.now();
      for (let i = 0; i < 240 && !state.dismissed; i++) {
        if (receipt.stage === "signed") {
          const leftMs = Math.max(0, receipt.applies_at_ms - (Date.now() + (meta.offset || 0)));
          const leftSec = Math.ceil(leftMs / 1000);
          sub.textContent = leftSec > 0 ? `Saving in ${leftSec}s · ${meta.title || meta.handle}` : "Writing to ledger...";
          bar.style.transform = `scaleX(${Math.max(0, Math.min(1, leftMs / windowMs))})`;
          if (leftSec === 0) {
            undo.remove();
          }
        } else if (receipt.stage === "applying") {
          el.dataset.tone = "pending";
          actions.replaceChildren();
          sub.textContent = `Writing to ledger... ${meta.title || ""}`.trim();
          bar.style.transform = "scaleX(0.12)";
        } else if (receipt.stage === "written") {
          const done = Boolean(receipt.woke && receipt.landing);
          const { text, warn } = summarizeOutcome(receipt);
          el.dataset.tone = warn ? "warn" : "ok";
          title.textContent = `Saved · ${meta.optionLabel || receipt.label || "answered"}`;
          sub.textContent = done
            ? (text || "Written to maestro/ledger.")
            : (text ? `${text} · finishing...` : "Written to ledger · waking agent...");
          actions.replaceChildren();
          if (done) {
            state.settled = true;
            if (meta.ledger && meta.handle) {
              const k = askKey(meta.ledger, meta.handle);
              inFlightKeys.delete(k);
              writtenKeys.add(k);
            }
            saveStored();
            window.__maestroInvalidate?.();
            syncPendingUi();
            announceToast(`Saved ${meta.optionLabel || receipt.label || "answer"}. ${text || ""}`.trim());
            if (!warn) setTimeout(() => { if (!state.dismissed) dismiss(); }, 6000);
            return;
          }
        } else if (receipt.stage === "undone") {
          state.settled = true;
          if (meta.ledger && meta.handle) inFlightKeys.delete(askKey(meta.ledger, meta.handle));
          saveStored();
          el.dataset.tone = "warn";
          title.textContent = "Answer undone";
          sub.textContent = "Nothing was written to the ledger.";
          announceToast("Answer undone. Nothing was written to the ledger.");
          addOpenLink("Return to decision");
          window.__maestroInvalidate?.();
          syncPendingUi();
          return;
        } else if (receipt.stage === "failed") {
          state.settled = true;
          if (meta.ledger && meta.handle) inFlightKeys.delete(askKey(meta.ledger, meta.handle));
          saveStored();
          el.dataset.tone = "fail";
          title.textContent = "Could not write answer";
          sub.textContent = receipt.error || "Write failed.";
          announceToast(`Could not write answer: ${receipt.error || "Write failed."}`);
          addOpenLink("Review decision");
          window.__maestroInvalidate?.();
          syncPendingUi();
          return;
        }

        await sleep(receipt.stage === "signed" ? 350 : 500);
        if (state.dismissed) return;
        try {
          const res = await fetch(`/owner/answer/${encodeURIComponent(meta.id)}`, { credentials: "same-origin" });
          if (res.ok) {
            receipt = await res.json();
          } else if (res.status === 404 && Date.now() - started > 3000) {
            dismiss();
            return;
          }
        } catch {
          // Keep polling through brief reconnects.
        }
      }
    };

    poll();
    return state;
  };

  for (const saved of loadStored()) {
    if (saved && saved.id && !activeToasts.has(saved.id)) {
      renderToast(saved, null);
    }
  }

  // ---- Answering a decision (re-mounted on every SPA swap) ----
  let decisionAbort = null;

  const findNextDecisionUrl = (currentLedger, currentHandle) => {
    const currentKey = askKey(currentLedger, currentHandle);
    const links = [...document.querySelectorAll(".mv-queue .mv-qrow a[href]")];
    const currentIndex = links.findIndex((a) => hrefToAskKey(a.getAttribute("href") || "") === currentKey);
    const ordered = currentIndex >= 0
      ? [...links.slice(currentIndex + 1), ...links.slice(0, currentIndex)]
      : links;
    for (const a of ordered) {
      const href = a.getAttribute("href") || "";
      const key = hrefToAskKey(href);
      if (key && key !== currentKey && !inFlightKeys.has(key) && a.closest(".mv-qrow")?.dataset.pending !== "true") {
        return href;
      }
    }
    const pagerNext = document.querySelector(".mv-pager a[rel=next]") || document.querySelector(".mv-pager a[rel=prev]");
    if (pagerNext) {
      const href = pagerNext.getAttribute("href") || "";
      const key = hrefToAskKey(href);
      if (!key || (key !== currentKey && !inFlightKeys.has(key))) return href;
    }
    return null;
  };

  const buildPublicKeyOptions = (begin) => {
    const descriptors = Array.isArray(begin.credential_descriptors) && begin.credential_descriptors.length
      ? begin.credential_descriptors
      : (begin.credentials || []).map((id) => ({ id, transports: ["internal"] }));
    return {
      challenge: unb64u(begin.challenge),
      rpId: begin.rp_id,
      allowCredentials: descriptors.map((d) => ({
        type: "public-key",
        id: unb64u(d.id),
        transports: Array.isArray(d.transports) && d.transports.length ? d.transports : ["internal"],
      })),
      hints: ["client-device"],
      userVerification: "required",
      timeout: 120000,
    };
  };

  const mountDecision = () => {
    if (decisionAbort) {
      decisionAbort.abort();
      decisionAbort = null;
    }
    syncPendingUi();

    const article = document.querySelector("article.mv-decision[data-ledger]");
    const bar = document.querySelector("[data-answer-bar]");
    if (!article || !bar) return;

    const ac = new AbortController();
    decisionAbort = ac;
    const { signal } = ac;

    const ledger = article.dataset.ledger;
    const handle = article.dataset.handle;
    const hash = article.dataset.hash;
    const askUrl = decisionUrl(ledger, handle);
    const askTitle = article.querySelector(".mv-dtitle")?.textContent?.trim() || handle;

    const go = bar.querySelector("[data-answer=go]");
    const goText = bar.querySelector(".mv-go-text");
    const note = bar.querySelector("textarea[name=note]");
    const status = bar.querySelector(".mv-answer-status");
    const radios = [...article.querySelectorAll("input.mv-opt-input")];
    const chosen = () => radios.find((r) => r.checked);

    const say = (text, tone) => {
      if (!status) return;
      status.textContent = text;
      if (tone) status.dataset.tone = tone;
      else delete status.dataset.tone;
    };

    // Zero-RTT passkey challenge pre-fetch so clicking Answer calls credentials.get() synchronously
    // inside the user gesture. Capped at 60s reuse so 60s + 120s timeout stays inside server 180s TTL.
    let prepared = null;
    let prepareTimer = null;
    let prepareSeq = 0;

    const intentKey = () => {
      const c = chosen();
      return c ? JSON.stringify([ledger, handle, hash, c.value, note.value.trim()]) : null;
    };

    const prefetchChallenge = () => {
      const c = chosen();
      if (!c || !window.PublicKeyCredential) return;
      const key = intentKey();
      if (prepared && prepared.key === key && Date.now() - prepared.at < 50000) return;
      const seq = ++prepareSeq;
      post("/owner/answer/begin", { ledger, handle, hash, option: c.value, note: note.value }, signal)
        .then((begin) => {
          if (signal.aborted || seq !== prepareSeq) return;
          if (intentKey() === key) prepared = { key, begin, at: Date.now() };
        })
        .catch(() => {
          // Silent background pre-fetch; click handler will fetch directly and report any real error.
        });
    };

    const schedulePrefetch = (delayMs) => {
      if (prepareTimer) clearTimeout(prepareTimer);
      if (delayMs === 0) prefetchChallenge();
      else prepareTimer = setTimeout(prefetchChallenge, delayMs);
    };

    signal.addEventListener("abort", () => {
      if (prepareTimer) clearTimeout(prepareTimer);
    });

    const refresh = () => {
      const c = chosen();
      go.disabled = !c;
      bar.dataset.hasChoice = c ? "true" : "false";
      goText.textContent = c ? `Answer: ${c.dataset.label}` : "Choose an option";
      const sign = document.querySelector("[data-sign=option]");
      if (sign) sign.textContent = c ? c.value : "the option you choose";
      if (c) schedulePrefetch(0);
    };

    radios.forEach((r) => r.addEventListener("change", refresh, { signal }));
    note.addEventListener("input", () => {
      prepared = null;
      if (chosen()) schedulePrefetch(220);
    }, { signal });

    // Number keys pick an option; Cmd or Ctrl with Enter answers.
    document.addEventListener("keydown", (e) => {
      if (e.target instanceof HTMLTextAreaElement || (e.target instanceof HTMLInputElement && e.target.type === "text")) {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !go.disabled) go.click();
        return;
      }
      const n = Number(e.key);
      if (n >= 1 && n <= radios.length && !e.metaKey && !e.ctrlKey && !e.altKey) {
        radios[n - 1].checked = true;
        radios[n - 1].focus();
        refresh();
      } else if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !go.disabled) {
        go.click();
      }
    }, { signal });

    refresh();

    const lock = (on) => {
      radios.forEach((r) => (r.disabled = on));
      note.disabled = on;
      go.disabled = on || !chosen();
    };

    const completeWithAssertion = async (begin, cred, c) => {
      const r = cred.response;
      let done;
      try {
        done = await post("/owner/answer/finish", {
          nonce: begin.nonce,
          assertion: {
            credential: b64u(cred.rawId),
            client_data: b64u(r.clientDataJSON),
            authenticator_data: b64u(r.authenticatorData),
            signature: b64u(r.signature),
          },
        });
      } catch (e) {
        if (signal.aborted) {
          renderFailureToast(askTitle, askUrl, e?.message);
          return;
        }
        throw e;
      }
      if (!done || !done.id) throw new Error("server returned an incomplete answer receipt");
      const offset = (typeof done.now_ms === "number" ? done.now_ms : Date.now()) - Date.now();
      renderToast({
        id: done.id,
        ledger,
        handle,
        title: askTitle,
        option: c.value,
        optionLabel: c.dataset.label,
        url: askUrl,
        applies_at_ms: done.applies_at_ms,
        signed_local_ms: Date.now(),
        offset,
      }, done);

      window.__maestroInvalidate?.();

      // If the owner already navigated away mid-ceremony, leave them on their new page.
      if (signal.aborted) return;

      const nextUrl = findNextDecisionUrl(ledger, handle);
      if (nextUrl && typeof window.__maestroNavigate === "function") {
        window.__maestroNavigate(nextUrl);
      } else if (nextUrl) {
        location.assign(nextUrl);
      } else {
        say(`Answered: ${c.dataset.label}. Saving in background. You can undo from the notification.`, "success");
      }
    };

    go.addEventListener("click", () => {
      const c = chosen();
      if (!c) return;
      if (!window.PublicKeyCredential || !navigator.credentials?.get) {
        say("This browser cannot sign with a passkey.", "danger");
        return;
      }
      const key = intentKey();
      const ready = prepared && prepared.key === key && Date.now() - prepared.at < 60000 ? prepared.begin : null;
      prepared = null;

      lock(true);
      say("Confirm with Touch ID or Face ID.", "info");

      const handleErr = (e) => {
        if (signal.aborted || e?.name === "AbortError") return;
        lock(false);
        say(e?.name === "NotAllowedError" ? "Cancelled. Nothing was written." : (e?.message || "Could not sign."), "danger");
        schedulePrefetch(0);
      };

      if (ready) {
        // Synchronous call inside the click handler keeps WebKit transient user activation intact.
        navigator.credentials.get({ publicKey: buildPublicKeyOptions(ready), signal })
          .then((cred) => {
            if (!cred) throw new Error("passkey returned no assertion");
            return completeWithAssertion(ready, cred, c);
          })
          .catch(handleErr);
      } else {
        post("/owner/answer/begin", { ledger, handle, hash, option: c.value, note: note.value }, signal)
          .then((begin) => navigator.credentials.get({ publicKey: buildPublicKeyOptions(begin), signal })
            .then((cred) => {
              if (!cred) throw new Error("passkey returned no assertion");
              return completeWithAssertion(begin, cred, c);
            }))
          .catch(handleErr);
      }
    }, { signal });
  };

  mountDecision();
  document.addEventListener("maestro:page", mountDecision);

  // ---- Enrolling the owner's passkey on /owner ----
  const enroll = document.querySelector("[data-enroll]");
  if (enroll) {
    const code = document.querySelector("input[name=code]");
    const status = document.querySelector("[data-enroll-status]");
    const say = (text, tone) => {
      status.textContent = text;
      if (tone) status.dataset.tone = tone;
      else delete status.dataset.tone;
    };
    enroll.addEventListener("click", async () => {
      if (!code.value.trim()) {
        say("Enter the setup code Maestro printed when it started.", "danger");
        code.focus();
        return;
      }
      enroll.disabled = true;
      say("Confirm with Touch ID or Face ID.", "info");
      try {
        const begin = await post("/owner/enroll/begin", { code: code.value });
        const cred = await navigator.credentials.create({
          publicKey: {
            challenge: unb64u(begin.challenge),
            rp: { id: begin.rp_id, name: "Maestro" },
            user: { id: unb64u(begin.user_id), name: "owner", displayName: "Maestro owner" },
            pubKeyCredParams: [{ type: "public-key", alg: -7 }],
            authenticatorSelection: {
              userVerification: "required",
              residentKey: "preferred",
            },
            hints: ["client-device"],
            timeout: 120000,
          },
        });
        const r = cred.response;
        const pk = r.getPublicKey?.();
        if (!pk) throw new Error("this browser did not return the passkey's public key");
        const transports = typeof r.getTransports === "function" ? r.getTransports() : ["internal"];
        await post("/owner/enroll/finish", {
          nonce: begin.nonce,
          registration: {
            credential: b64u(cred.rawId),
            public_key: b64u(pk),
            algorithm: r.getPublicKeyAlgorithm?.() ?? -7,
            client_data: b64u(r.clientDataJSON),
            authenticator_data: b64u(r.getAuthenticatorData()),
            transports,
          },
        });
        location.reload();
      } catch (e) {
        enroll.disabled = false;
        say(e.name === "NotAllowedError" ? "Cancelled. Nothing was saved." : e.message, "danger");
      }
    });
  }
})();
