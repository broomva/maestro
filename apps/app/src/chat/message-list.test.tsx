/// <reference types="bun" />
// message-list.test.tsx (BRO-1826 M4, slice B) — the feed renderers, via renderToStaticMarkup (the repo
// convention: no DOM in the bun runner, so React is proven structurally + interaction is dogfooded in
// Playwright). Anti-vacuity [[self-hosting-vacuous-pass]]: each test folds a REAL part shape and asserts
// the exact DOM signal that part must carry — the streaming caret class, the typing dots, the tool state,
// the link pill, and (closing the P20 coverage gap) the error-render path end to end.

import { describe, expect, test } from "bun:test";
import type { ChatMessage } from "@maestro/protocol";
import { renderToStaticMarkup } from "react-dom/server";
import { finalizeStreamingParts } from "./chat-turn";
import { MessageList, MessageRow } from "./message-list";
import { ChatTransportError } from "./transport";
import { appendErrorMessage } from "./use-bv-chat";

const html = (node: React.ReactNode) => renderToStaticMarkup(node as never);

describe("MessageRow — per-part rendering", () => {
  test("a user turn renders right-aligned matte text (no glass)", () => {
    const msg: ChatMessage = { id: "u1", role: "user", parts: [{ type: "text", text: "hello" }] };
    const out = html(<MessageRow msg={msg} />);
    expect(out).toContain("bv-msg--user");
    expect(out).toContain("hello");
    expect(out).not.toContain("bv-glass"); // the feed is matte — glass only on the composer
  });

  test("a STREAMING assistant text part carries the caret class; a done one does not", () => {
    const streaming: ChatMessage = {
      id: "r1",
      role: "assistant",
      parts: [{ type: "text", text: "typing", state: "streaming" }],
    };
    const done: ChatMessage = {
      id: "r2",
      role: "assistant",
      parts: [{ type: "text", text: "typed", state: "done" }],
    };
    expect(html(<MessageRow msg={streaming} />)).toContain("bv-msg--streaming");
    expect(html(<MessageRow msg={done} />)).not.toContain("bv-msg--streaming");
  });

  test("a reasoning part renders a Lucide icon (not a raw glyph) + the text", () => {
    const msg: ChatMessage = {
      id: "r1",
      role: "assistant",
      parts: [{ type: "reasoning", text: "thinking it through", state: "done" }],
    };
    const out = html(<MessageRow msg={msg} />);
    expect(out).toContain("bv-reasoning");
    expect(out).toContain("lucide-sparkles"); // Lucide, per §Icons — NOT the old ✦ glyph
    expect(out).not.toContain("✦");
    expect(out).toContain("thinking it through");
  });

  test("a tool part renders a matte card with the tool name + a human state label + io lines", () => {
    const msg: ChatMessage = {
      id: "r1",
      role: "assistant",
      parts: [
        {
          type: "tool-shell",
          toolCallId: "c1",
          state: "output-available",
          input: { command: "ls" },
          output: "a.txt",
        },
      ],
    };
    const out = html(<MessageRow msg={msg} />);
    expect(out).toContain("bv-toolpart");
    expect(out).toContain("shell");
    expect(out).toContain('data-done="true"'); // output-available → done
    // Assert the VISIBLE label text, not just the substring "done" (which also lives in the `data-done`
    // attribute name — a tautology): the state span's element text must read "done".
    expect(out).toMatch(/data-done="true">done</);
    // JSON.stringify(input) is rendered as text, so `"` is HTML-escaped to `&quot;` in the markup.
    expect(out).toContain("command");
    expect(out).toContain("ls");
    expect(out).toContain("a.txt");
  });

  test("a running tool shows the running label and is NOT marked done", () => {
    const msg: ChatMessage = {
      id: "r1",
      role: "assistant",
      parts: [{ type: "tool-shell", toolCallId: "c1", state: "input-available", input: {} }],
    };
    const out = html(<MessageRow msg={msg} />);
    expect(out).toMatch(/data-done="false">running</); // the visible label, not a substring coincidence
    expect(out).toContain('data-done="false"');
  });

  test("an inline link pill wraps a backtick span in the assistant text", () => {
    const msg: ChatMessage = {
      id: "r1",
      role: "assistant",
      parts: [{ type: "text", text: "judged `run/7c2f1a` clean", state: "done" }],
    };
    const out = html(<MessageRow msg={msg} />);
    expect(out).toContain("bv-link-pill");
    expect(out).toContain("run/7c2f1a");
    expect(out).not.toContain("`"); // the delimiters are consumed, not rendered
  });

  test("an ERROR part renders the muted error row with a Lucide icon + the message (never red glyph)", () => {
    const msg: ChatMessage = {
      id: "e1",
      role: "assistant",
      parts: [{ type: "error", errorText: "chat is unavailable" }],
    };
    const out = html(<MessageRow msg={msg} />);
    expect(out).toContain("bv-msg--error");
    expect(out).toContain("lucide-circle-alert");
    expect(out).toContain("chat is unavailable");
    expect(out).not.toContain(">!<"); // the old raw "!" glyph is gone
  });
});

describe("MessageList — turn-level signals", () => {
  test("empty transcript → the plain-voice greeting (no Welcome, no emoji)", () => {
    const out = html(<MessageList messages={[]} status="ready" />);
    expect(out).toContain("chat-empty");
    expect(out).toContain("A fresh session");
    expect(out).not.toContain("Welcome");
  });

  test("the greeting names the layer when given", () => {
    const out = html(<MessageList messages={[]} status="ready" layer="inbox" />);
    expect(out).toContain("A fresh session on inbox");
  });

  test("typing dots show while submitted with only the user turn (before the first assistant token)", () => {
    const msgs: ChatMessage[] = [{ id: "u1", role: "user", parts: [{ type: "text", text: "go" }] }];
    expect(html(<MessageList messages={msgs} status="submitted" />)).toContain("chat-typing");
    // Once an assistant message has opened, the caret — not the dots — is the live signal.
    const withAssistant: ChatMessage[] = [
      ...msgs,
      { id: "r1", role: "assistant", parts: [{ type: "text", text: "", state: "streaming" }] },
    ];
    expect(html(<MessageList messages={withAssistant} status="streaming" />)).not.toContain(
      "chat-typing",
    );
  });

  test("no typing dots when ready (idle)", () => {
    const msgs: ChatMessage[] = [{ id: "u1", role: "user", parts: [{ type: "text", text: "go" }] }];
    expect(html(<MessageList messages={msgs} status="ready" />)).not.toContain("chat-typing");
  });
});

describe("appendErrorMessage — the transport-failure render path", () => {
  test("a ChatTransportError becomes an assistant error part carrying its message", () => {
    const out = appendErrorMessage(
      [],
      new ChatTransportError("no model loop", 501, "unsupported_intent"),
    );
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ role: "assistant" });
    expect(out[0]?.parts[0]).toMatchObject({ type: "error", errorText: "no model loop" });
  });

  test("a generic Error uses its message; a non-error uses the fallback; the prior transcript is kept", () => {
    const prior: ChatMessage = { id: "u1", role: "user", parts: [{ type: "text", text: "hi" }] };
    const generic = appendErrorMessage([prior], new Error("network down"));
    expect(generic).toHaveLength(2);
    expect(generic[0]).toBe(prior); // prior turn preserved
    expect(generic[1]?.parts[0]).toMatchObject({ type: "error", errorText: "network down" });

    const unknown = appendErrorMessage([], "weird");
    expect(unknown[0]?.parts[0]).toMatchObject({
      type: "error",
      errorText: "The chat stream failed.",
    });
  });

  test("the appended error part RENDERS through MessageRow (the path is reachable end to end)", () => {
    const [errMsg] = appendErrorMessage([], new ChatTransportError("boom", 500));
    const out = html(<MessageRow msg={errMsg as ChatMessage} />);
    expect(out).toContain("bv-msg--error");
    expect(out).toContain("boom");
  });

  test("the hook's catch transform SETTLES the partial streaming part before appending the error (P20 round-3 MAJOR)", () => {
    // This is EXACTLY what useBvChat's catch runs: appendErrorMessage(finalizeStreamingParts(m), err).
    // A mid-stream transport failure must not leave the truncated assistant text blinking its caret.
    const partial: ChatMessage[] = [
      { id: "u1", role: "user", parts: [{ type: "text", text: "list files" }] },
      {
        id: "r1",
        role: "assistant",
        parts: [{ type: "text", text: "Here are ", state: "streaming" }],
      },
    ];
    const out = appendErrorMessage(
      finalizeStreamingParts(partial),
      new Error("connection dropped"),
    );
    // The truncated text part is settled (no caret) — NOT still "streaming".
    expect(out[1]?.parts[0]).toMatchObject({ text: "Here are ", state: "done" });
    expect(out.some((m) => m.parts.some((p) => p.state === "streaming"))).toBe(false);
    // And the error row is appended after it.
    expect(out.at(-1)?.parts[0]).toMatchObject({ type: "error", errorText: "connection dropped" });
    // Rendering the settled partial message shows NO streaming caret class.
    expect(html(<MessageRow msg={out[1] as ChatMessage} />)).not.toContain("bv-msg--streaming");
  });
});
