// Session route (BRO-1826 M4, slice B) — the chat surface. A session renders work; it never owns it
// (CLAUDE.md §What Maestro is: "Chat is a projection"). This route drives ONE `useBvChat` and paints it
// into TWO surfaces — the thread (the 768px conversation column with the composer) and a side panel (a
// read-only mirror of the same transcript). Same session, same reducer, two projections: the M4 verify.
//
// The transport is the real `RuntimeChatTransport` bound to the path session id, UNLESS `?fixture=1`
// selects the recorded `FixtureChatTransport` (the demo/test seam, inert in production — mirrors the
// router's `?crash` probe). The shell layout owns the SSE stream + chrome; this is just its child view.

import { Composer } from "@maestro/ui";
import { useParams } from "@tanstack/react-router";
import { useMemo } from "react";
import { ChatFeed } from "@/chat/chat-feed";
import { FixtureChatTransport, fixtureRequested } from "@/chat/fixture-transport";
import type { ChatTransport } from "@/chat/transport";
import { RuntimeChatTransport } from "@/chat/transport";
import { useBvChat } from "@/chat/use-bv-chat";

export function SessionView() {
  // Loose param read (the route is registered in router.tsx; `strict:false` avoids a config import cycle).
  const params = useParams({ strict: false }) as { sessionId?: string };
  const sessionId = params.sessionId ?? "orchestrator";

  // Bind the transport once per session id. Fixture mode is a demo/test seam (query-triggered), inert
  // otherwise — production streams over the real F10 endpoint.
  const transport = useMemo<ChatTransport>(
    () =>
      fixtureRequested() ? new FixtureChatTransport() : new RuntimeChatTransport({ sessionId }),
    [sessionId],
  );

  const { messages, status, busy, sendMessage, stop } = useBvChat({ transport });

  return (
    <div className="flex h-full min-h-0" data-testid="session-view">
      {/* The thread — the conversation column. The feed grows and scrolls; the composer sits in a
          bottom footer, both centered to the canon 768px measure (the DS class centers the feed; the
          footer matches). This is the primary surface. */}
      <section className="flex min-w-0 flex-1 flex-col" aria-label="Thread">
        <ChatFeed messages={messages} status={status} label="Conversation" />
        <div className="mx-auto w-full max-w-[768px] shrink-0 px-4 pb-4 pt-2">
          <Composer
            placeholder="Message Maestro"
            busy={busy}
            onSend={(text) => sendMessage({ text })}
            onStop={stop}
          />
        </div>
      </section>

      {/* The side panel — the same session as a read-only mirror (rung 3, for verifying not operating).
          Hidden on narrow viewports; the layout rule reserves the right panel for wide screens. This is
          the "same run in a side panel" the M4 verify names — one hook, a second projection, no composer. */}
      <aside
        className="hidden w-[380px] shrink-0 flex-col border-border border-l xl:flex"
        aria-label="Session panel"
        data-testid="session-panel"
      >
        <header className="flex h-[44px] shrink-0 items-center border-border border-b px-4">
          <span className="font-medium text-sm">This session</span>
        </header>
        <ChatFeed messages={messages} status={status} label="This session" />
      </aside>
    </div>
  );
}
