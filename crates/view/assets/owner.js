// Maestro's owner script (BRO-4042): the one script the web pages run, pinned by its hash.
// It turns a chosen option into a passkey-signed answer, shows the undo window and the
// receipt, and enrolls the owner's passkey on /owner. It reads nothing but the page and talks
// only to its own origin.
(() => {
  "use strict";
  const b64u = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const unb64u = (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  const post = async (path, body) => {
    const res = await fetch(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), credentials: "same-origin" });
    const data = await res.json().catch(() => ({ error: `the server answered ${res.status}` }));
    if (!res.ok) throw new Error(data.error || `the server answered ${res.status}`);
    return data;
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // ---- Answering a decision ----
  const article = document.querySelector("article.mv-decision[data-ledger]");
  const bar = document.querySelector("[data-answer-bar]");
  if (article && bar) {
    const go = bar.querySelector("[data-answer=go]");
    const goText = bar.querySelector(".mv-go-text");
    const note = bar.querySelector("textarea[name=note]");
    const status = bar.querySelector(".mv-answer-status");
    const how = bar.querySelector(".mv-answer-how");
    const radios = [...article.querySelectorAll("input.mv-opt-input")];
    const chosen = () => radios.find((r) => r.checked);
    const say = (text, tone) => {
      status.textContent = text;
      if (tone) status.dataset.tone = tone; else delete status.dataset.tone;
    };
    const refresh = () => {
      const c = chosen();
      go.disabled = !c;
      goText.textContent = c ? `Answer: ${c.dataset.label}` : "Choose an option";
      const sign = document.querySelector("[data-sign=option]");
      if (sign) sign.textContent = c ? c.value : "the option you choose";
    };
    radios.forEach((r) => r.addEventListener("change", refresh));
    // Number keys pick an option; Cmd or Ctrl with Enter answers.
    document.addEventListener("keydown", (e) => {
      if (e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLInputElement && e.target.type === "text") {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !go.disabled) go.click();
        return;
      }
      const n = Number(e.key);
      if (n >= 1 && n <= radios.length && !e.metaKey && !e.ctrlKey && !e.altKey) {
        radios[n - 1].checked = true;
        radios[n - 1].focus();
        refresh();
      } else if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !go.disabled) go.click();
    });
    refresh();

    const lock = (on) => {
      radios.forEach((r) => (r.disabled = on));
      note.disabled = on;
      go.disabled = on || !chosen();
    };

    const showReceipt = async (receipt, offset) => {
      const undo = document.createElement("button");
      undo.type = "button";
      undo.className = "mv-undo";
      undo.textContent = "Undo";
      const ring = document.createElement("span");
      ring.className = "mv-undo-count";
      how.replaceChildren(ring, " ", document.createTextNode(`${receipt.label}: signed. Writing in `), undo);
      let undone = false;
      undo.addEventListener("click", async () => {
        undo.disabled = true;
        try {
          await post("/owner/answer/undo", { id: receipt.id });
          undone = true;
          how.textContent = "Undone. Nothing was written. Choose again whenever you are ready.";
          say("", null);
          lock(false);
        } catch (err) {
          say(`Could not undo: ${err.message}`, "danger");
        }
      });
      // The window is the server's: count down to its time, not a timer of our own.
      while (!undone) {
        const left = Math.ceil((receipt.applies_at_ms - (Date.now() + offset)) / 1000);
        if (left <= 0) break;
        ring.textContent = String(left);
        ring.dataset.left = String(left);
        await sleep(250);
      }
      if (undone) return;
      undo.remove();
      ring.remove();
      how.textContent = "Writing your answer.";
      for (let i = 0; i < 240; i++) {
        const r = await fetch(`/owner/answer/${encodeURIComponent(receipt.id)}`, { credentials: "same-origin" }).then((x) => x.json()).catch(() => null);
        if (r && r.stage === "written") {
          bar.dataset.stage = "written";
          how.textContent = "";
          const lines = [
            `Answered: ${r.label}. Written by Maestro, signed by you.`,
            r.woke ? `The agent: ${r.woke}.` : "",
            r.landing ? `Main: ${r.landing}.` : "Main: the landing PR is being opened.",
          ].filter(Boolean);
          say(lines.join(" "), "success");
          const next = document.querySelector("a.mv-pager-link[rel=next], .mv-qrow a:not([aria-current])");
          if (next) {
            const a = document.createElement("a");
            a.href = next.getAttribute("href");
            a.className = "mv-next";
            a.textContent = "Next decision";
            how.append(a);
          }
          if (r.landing) return;
        } else if (r && r.stage === "failed") {
          bar.dataset.stage = "failed";
          how.textContent = "";
          say(`Not written: ${r.error}. Nothing changed; reload to see the ask as it is now.`, "danger");
          lock(false);
          return;
        }
        await sleep(r && r.stage === "written" ? 3000 : 1000);
      }
    };

    go.addEventListener("click", async () => {
      const c = chosen();
      if (!c) return;
      lock(true);
      say("Confirm with Touch ID or Face ID.", "info");
      try {
        const begin = await post("/owner/answer/begin", {
          ledger: article.dataset.ledger,
          handle: article.dataset.handle,
          hash: article.dataset.hash,
          option: c.value,
          note: note.value,
        });
        const cred = await navigator.credentials.get({
          publicKey: {
            challenge: unb64u(begin.challenge),
            rpId: begin.rp_id,
            allowCredentials: begin.credentials.map((id) => ({ type: "public-key", id: unb64u(id) })),
            userVerification: "required",
            timeout: 120000,
          },
        });
        if (!cred) throw new Error("no passkey was used");
        say("Signed. Sending.", "info");
        const receipt = await post("/owner/answer/finish", {
          nonce: begin.nonce,
          assertion: {
            credential: b64u(cred.rawId),
            client_data: b64u(cred.response.clientDataJSON),
            authenticator_data: b64u(cred.response.authenticatorData),
            signature: b64u(cred.response.signature),
          },
        });
        say("", null);
        await showReceipt(receipt, receipt.now_ms - Date.now());
      } catch (err) {
        const cancelled = err && (err.name === "NotAllowedError" || err.name === "AbortError");
        say(cancelled ? "Cancelled. Nothing was signed or written." : `Not answered: ${err.message}`, cancelled ? null : "danger");
        lock(false);
      }
    });
  }

  // ---- Abandoning a flow ----
  const abandon = document.querySelector("[data-abandon-flow]");
  if (abandon) {
    const go = abandon.querySelector("[data-abandon-go]");
    const status = abandon.querySelector("[data-abandon-status]");
    const receiptBox = abandon.querySelector("[data-abandon-receipt]");
    go.addEventListener("click", async () => {
      go.disabled = true;
      receiptBox.replaceChildren();
      try {
        const begin = await post("/owner/flow/abandon/begin", {
          instance: abandon.querySelector("[name=instance]").value.trim(),
          reason: abandon.querySelector("[name=reason]").value.trim(),
        });
        status.textContent = "Confirm with your passkey.";
        const cred = await navigator.credentials.get({ publicKey: {
          challenge: unb64u(begin.challenge), rpId: begin.rp_id,
          allowCredentials: begin.credentials.map((id) => ({ type: "public-key", id: unb64u(id) })),
          userVerification: "required", timeout: 120000,
        }});
        if (!cred) throw new Error("no passkey was used");
        const receipt = await post("/owner/flow/abandon/finish", { nonce: begin.nonce, assertion: {
          credential: b64u(cred.rawId), client_data: b64u(cred.response.clientDataJSON),
          authenticator_data: b64u(cred.response.authenticatorData), signature: b64u(cred.response.signature),
        }});
        const offset = receipt.now_ms - Date.now();
        const undo = document.createElement("button");
        undo.type = "button"; undo.textContent = "Undo";
        receiptBox.append(undo);
        let undone = false;
        undo.addEventListener("click", async () => {
          undo.disabled = true;
          try { await post("/owner/answer/undo", { id: receipt.id }); undone = true; status.textContent = "Undone. Nothing was written."; }
          catch (err) { status.textContent = `Could not undo: ${err.message}`; }
        });
        while (!undone && Date.now() + offset < receipt.applies_at_ms) {
          status.textContent = `Signed. Abandoning in ${Math.ceil((receipt.applies_at_ms - Date.now() - offset) / 1000)} seconds.`;
          await sleep(250);
        }
        undo.remove();
        if (!undone) {
          status.textContent = "Applying your action.";
          for (let i = 0; i < 60; i++) {
            const r = await fetch(`/owner/answer/${encodeURIComponent(receipt.id)}`, { credentials: "same-origin" }).then((x) => x.json());
            if (r.stage === "written") { status.textContent = "Flow instance abandoned."; break; }
            if (r.stage === "failed") { status.textContent = `Not abandoned: ${r.error}`; break; }
            await sleep(1000);
          }
        }
      } catch (err) { status.textContent = `Not abandoned: ${err.message}`; }
      finally { go.disabled = false; }
    });
  }

  // ---- Enrolling the owner's passkey ----
  const enroll = document.querySelector("[data-enroll]");
  if (enroll) {
    const out = document.querySelector("[data-enroll-status]");
    enroll.addEventListener("click", async () => {
      enroll.disabled = true;
      out.textContent = "Confirm with Touch ID or Face ID.";
      try {
        const begin = await post("/owner/enroll/begin", { code: (document.querySelector("[name=code]") || {}).value || "" });
        const cred = await navigator.credentials.create({
          publicKey: {
            challenge: unb64u(begin.challenge),
            rp: { id: begin.rp_id, name: "Maestro" },
            user: { id: unb64u(begin.user_id), name: "owner", displayName: "Maestro owner" },
            pubKeyCredParams: [{ type: "public-key", alg: -7 }],
            authenticatorSelection: { userVerification: "required", residentKey: "preferred" },
            attestation: "none",
            timeout: 120000,
          },
        });
        const r = cred.response;
        await post("/owner/enroll/finish", {
          nonce: begin.nonce,
          registration: {
            credential: b64u(cred.rawId),
            public_key: b64u(r.getPublicKey()),
            algorithm: r.getPublicKeyAlgorithm(),
            client_data: b64u(r.clientDataJSON),
            authenticator_data: b64u(r.getAuthenticatorData()),
          },
        });
        out.textContent = "Your passkey is set up. Decisions now take your answer in place.";
        out.dataset.tone = "success";
      } catch (err) {
        out.textContent = `Not set up: ${err.message}`;
        out.dataset.tone = "danger";
        enroll.disabled = false;
      }
    });
  }
})();
