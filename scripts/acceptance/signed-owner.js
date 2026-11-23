// Fixture-only T3 preview expression. This is an emulated authenticator, not a biometric test.
// 1. Make/start owner-fixture.py, open http://localhost:7190/owner, fill its setup code,
//    then run this expression. It clicks the real enrollment control.
// 2. Navigate to the returned decisionUrl and run the expression again. It chooses an
//    option, types a note, signs through owner.js, clicks Undo, then signs/writes for real.
// All signatures use WebCrypto ES256; the production Rust verifier/undo/writer run unchanged.
(async () => {
  "use strict";
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  assert(location.origin === "http://localhost:7190", "Refusing nonfixture origin; only localhost:7190 is allowed");
  const decisionUrl = "/decisions/owner-dogfood-fixture.yaml/signed-fixture";
  const fixture = await fetch(decisionUrl).then(async response => ({ ok: response.ok, html: await response.text() }));
  const parsed = new DOMParser().parseFromString(fixture.html, "text/html");
  const fixtureAsk = parsed.querySelector("article.mv-decision[data-ledger]");
  assert(fixture.ok && fixtureAsk?.dataset.ledger === "owner-dogfood-fixture.yaml" && fixtureAsk.dataset.handle === "signed-fixture" && fixtureAsk.textContent.includes("Dogfood fixture"), "Refusing a page without the isolated fixture decision");
  assert(crypto?.subtle && navigator.credentials, "Secure context/WebCrypto credentials interface unavailable");
  const storageKey = "maestro.test-only.owner-dogfood-key";
  const id = new TextEncoder().encode("maestro-local-dogfood-authenticator");
  const b64u = bytes => btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const utf8 = value => new TextEncoder().encode(value);
  const concat = (...parts) => { const bytes = new Uint8Array(parts.reduce((sum, p) => sum + p.byteLength, 0)); let at = 0; for (const p of parts) { bytes.set(new Uint8Array(p), at); at += p.byteLength; } return bytes; };
  const wait = async (test, label, timeout = 30000) => {
    const end = performance.now() + timeout;
    while (performance.now() < end) { const value = test(); if (value) return value; await new Promise(resolve => setTimeout(resolve, 100)); }
    throw new Error(`Timed out: ${label}`);
  };
  const authData = async rp => {
    assert(rp === "localhost", "Refusing a nonfixture relying party");
    const auth = new Uint8Array(37);
    auth.set(new Uint8Array(await crypto.subtle.digest("SHA-256", utf8(rp))));
    auth[32] = 0x05; // User-present + user-verified flags from this explicitly emulated fixture.
    return auth;
  };
  const clientData = (kind, challenge) => utf8(JSON.stringify({ type: kind, challenge: b64u(challenge), origin: location.origin, crossOrigin: false }));
  const derSignature = raw => {
    const signature = new Uint8Array(raw);
    assert(signature.length === 64, "Expected WebCrypto P-256 IEEE P1363 signature");
    const integer = part => { let i = 0; while (i < part.length - 1 && part[i] === 0) i++; const body = part.slice(i); return body[0] & 0x80 ? concat(new Uint8Array([0]), body) : body; };
    const r = integer(signature.slice(0, 32)), s = integer(signature.slice(32));
    return concat(new Uint8Array([0x30, 4 + r.length + s.length, 0x02, r.length]), r, new Uint8Array([0x02, s.length]), s);
  };
  const originalCreate = navigator.credentials.create;
  const originalGet = navigator.credentials.get;
  const originalFetch = window.fetch;
  const receipts = [];
  let privateKey;
  try {
    if (location.pathname === "/owner") {
      const button = document.querySelector("[data-enroll]");
      const code = document.querySelector("input[name=code]");
      assert(button && code?.value, "Fill the isolated fixture's printed setup code before running enrollment");
      const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
      sessionStorage.setItem(storageKey, JSON.stringify(await crypto.subtle.exportKey("jwk", pair.privateKey)));
      Object.defineProperty(navigator.credentials, "create", { configurable: true, value: async ({ publicKey: options }) => {
        const data = await authData(options.rp.id);
        const publicKey = await crypto.subtle.exportKey("spki", pair.publicKey);
        return { rawId: id, response: { clientDataJSON: clientData("webauthn.create", options.challenge), getPublicKey: () => publicKey, getPublicKeyAlgorithm: () => -7, getAuthenticatorData: () => data } };
      } });
      button.click();
      const status = await wait(() => document.querySelector("[data-enroll-status][data-tone]"), "enrollment status");
      assert(status.dataset.tone === "success", status.textContent);
      return { pass: true, stage: "enrolled", authenticator: "WebCrypto fixture only; no physical biometrics", decisionUrl: location.origin + decisionUrl };
    }
    assert(location.pathname === decisionUrl, "Open the exact fixture decision route before running signed answers");
    const jwk = JSON.parse(sessionStorage.getItem(storageKey) || "null");
    assert(jwk, "Enroll this fixture in the same preview tab first");
    privateKey = await crypto.subtle.importKey("jwk", jwk, { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
    Object.defineProperty(navigator.credentials, "get", { configurable: true, value: async ({ publicKey: options }) => {
      assert(options.allowCredentials.some(c => b64u(c.id) === b64u(id)), "The emulated fixture credential is not allowed");
      const data = await authData(options.rpId);
      const client = clientData("webauthn.get", options.challenge);
      const signed = concat(data, await crypto.subtle.digest("SHA-256", client));
      const signature = derSignature(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, privateKey, signed));
      return { rawId: id, response: { clientDataJSON: client, authenticatorData: data, signature } };
    } });
    window.fetch = async (...args) => {
      const response = await originalFetch(...args);
      if (new URL(args[0], location.href).pathname === "/owner/answer/finish" && response.ok) receipts.push(await response.clone().json());
      return response;
    };
    const article = document.querySelector("article.mv-decision[data-ledger]");
    assert(article?.dataset.ledger === "owner-dogfood-fixture.yaml" && article.dataset.handle === "signed-fixture", "Visible decision is not the fixture");
    const option = article.querySelector("input.mv-opt-input[value=hold]");
    const bar = document.querySelector("[data-answer-bar]");
    const note = bar?.querySelector("textarea[name=note]");
    const go = bar?.querySelector("[data-answer=go]");
    assert(option && note && go, "Fixture answer controls missing");
    option.checked = true;
    option.dispatchEvent(new Event("change", { bubbles: true }));
    note.value = "Dogfood fixture: signed browser note";
    note.dispatchEvent(new Event("input", { bubbles: true }));
    assert(!go.disabled && go.textContent.includes("Hold fixture"), "Option selection did not enable the correct action");
    go.click();
    const undo = await wait(() => bar.querySelector("button.mv-undo:not(:disabled)"), "signed answer Undo control");
    assert(receipts.length === 1 && receipts[0].stage === "signed", "No signed receipt before Undo");
    undo.click();
    await wait(() => !go.disabled && bar.textContent.includes("Undone. Nothing was written."), "Undo completed and controls unlocked");
    const undone = await originalFetch(`/owner/answer/${encodeURIComponent(receipts[0].id)}`).then(r => r.json());
    assert(undone.stage === "undone" && !undone.commit, "Undo receipt must have no committed write");
    go.click();
    await wait(() => receipts.length === 2, "second signed receipt");
    await wait(() => bar.dataset.stage === "written" || bar.dataset.stage === "failed", "signed answer durable result");
    assert(bar.dataset.stage === "written", bar.textContent);
    const written = await originalFetch(`/owner/answer/${encodeURIComponent(receipts[1].id)}`).then(r => r.json());
    assert(written.stage === "written" && /^[0-9a-f]{40,64}$/.test(written.commit || ""), "No durable answer commit in receipt");
    assert(written.intent.option === "hold" && written.intent.note === note.value, "Receipt did not retain the signed option and note");
    assert(written.woke === "no session is named on the ask", "Fixture unexpectedly named or notified an agent session");
    assert(!written.landing, "Local fixture unexpectedly opened a landing PR");
    assert(bar.textContent.includes("Main: no landing result recorded."), "Local mode made an unsupported landing claim");
    sessionStorage.removeItem(storageKey);
    return { pass: true, checks: ["real owner.js selection/note", "server-verified ES256 assertion", "signed receipt", "Undo with no commit", "second signature", "durable isolated answer commit", "no agent wake or hosted landing"], undone: { id: undone.id, stage: undone.stage, commit: undone.commit }, written: { id: written.id, stage: written.stage, commit: written.commit }, authenticator: "WebCrypto fixture only; physical biometric/PWA behavior not covered" };
  } finally {
    Object.defineProperty(navigator.credentials, "create", { configurable: true, value: originalCreate });
    Object.defineProperty(navigator.credentials, "get", { configurable: true, value: originalGet });
    window.fetch = originalFetch;
    privateKey = null;
  }
})();
