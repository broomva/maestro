// chat-feed (BRO-1826 M4, slice B) — the scrollable feed surface. A thin shell around `MessageList`
// that owns the inner scroll: `.bv-chat-feed` scrolls, the app shell never does (CLAUDE.md §Layout).
// Auto-sticks to the bottom as messages/status change, matching the prototype's feed effect. Rendered
// by BOTH the thread and the side panel from ONE `useBvChat` result — the same session, two projections
// (the M4 "chat is a projection" verify).

import type { ChatMessage } from "@maestro/protocol";
import { useEffect, useRef } from "react";
import type { ChatStatus } from "./chat-turn";
import { MessageList } from "./message-list";

export function ChatFeed({
  messages,
  status,
  layer,
  label,
}: {
  messages: readonly ChatMessage[];
  status: ChatStatus;
  layer?: string;
  /** aria-label for the scroll region (e.g. "Conversation" for the thread, "This session" for the panel). */
  label: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  // Stick to the bottom on any transcript/status change. `messages`/`status` are TRIGGER deps — the
  // effect body reads neither (only the ref), so biome's exhaustive-deps flags them, but removing them
  // would run the scroll once and never again (the bug). They belong here.
  // biome-ignore lint/correctness/useExhaustiveDependencies: re-run to bottom-anchor on every change
  useEffect(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, status]);

  return (
    // A live conversation log — role="log" is the labelled scroll region a chat feed is (and it supports
    // aria-label, which a bare div does not).
    <div ref={ref} role="log" className="bv-chat-feed" aria-label={label} data-testid="chat-feed">
      <MessageList messages={messages} status={status} layer={layer} />
    </div>
  );
}
