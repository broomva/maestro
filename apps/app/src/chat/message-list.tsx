// message-list (BRO-1826 M4, slice B) — the feed renderers, canon `AiProtocol.jsx` MccMessage/MccToolPart.
// The transcript is folded by the shared `bvApplyChunk` (protocol); THIS is the only place a part shape
// becomes DOM. One renderer per part type; the DS classes (bv-msg / bv-toolpart / bv-reasoning / …) carry
// the matte, plain-voice look — glass never reaches the feed (CLAUDE.md §Glass). data-* parts render
// nothing here: data-gate is the gate queue's (BRO-1789), data-tick is deferred F6.5 gen-UI.

import type { ChatMessage, ChatPart } from "@maestro/protocol";
import { SquareCheck } from "lucide-react";
import type { ReactNode } from "react";
import type { ChatStatus } from "./chat-turn";
import { emptySessionGreeting, tokenizeAssistantText } from "./format";

/** Assistant prose with inline link pills — backtick spans become the one colored inline element
 *  (`.bv-link-pill`, 1 of 5 sanctioned color uses). Plain runs stay text. */
function AssistantText({ text }: { text: string }): ReactNode {
  const tokens = tokenizeAssistantText(text);
  return (
    <>
      {tokens.map((tok, i) =>
        tok.kind === "pill" ? (
          // biome-ignore lint/suspicious/noArrayIndexKey: render-only token stream, no reorder/state
          <span key={i} className="bv-link-pill">
            {tok.value}
          </span>
        ) : (
          // biome-ignore lint/suspicious/noArrayIndexKey: render-only token stream, no reorder/state
          <span key={i}>{tok.value}</span>
        ),
      )}
    </>
  );
}

/** The human-readable state label for a tool part — "streaming input…" → "running" → "done"/"failed". */
function toolStateLabel(state: string | undefined): string {
  switch (state) {
    case "output-available":
      return "done";
    case "output-error":
    case "input-error":
      return "failed";
    case "input-available":
      return "running";
    default:
      return "streaming input…";
  }
}

/** A tool call — a matte card (never glass): the tool name, its live state, and its io lines. */
function ToolPart({ part }: { part: ChatPart }): ReactNode {
  const name = part.type.slice("tool-".length);
  const done = part.state === "output-available";
  const label = toolStateLabel(part.state);
  return (
    <div className="bv-toolpart" data-testid="chat-tool">
      <div className="bv-toolpart-head">
        <SquareCheck size={13} strokeWidth={2} aria-hidden="true" />
        <b>{name}</b>
        <span className="bv-toolpart-state" data-done={done}>
          {label}
        </span>
      </div>
      {part.input !== undefined ? (
        <code className="bv-toolpart-line">input {JSON.stringify(part.input)}</code>
      ) : part.inputText ? (
        <code className="bv-toolpart-line">input {part.inputText}</code>
      ) : null}
      {part.output !== undefined ? (
        <code className="bv-toolpart-line">output {JSON.stringify(part.output)}</code>
      ) : null}
      {part.errorText ? <code className="bv-toolpart-line">error {part.errorText}</code> : null}
    </div>
  );
}

/** One part of an assistant message → its element (or null for parts rendered elsewhere / deferred). */
function AssistantPart({ part }: { part: ChatPart }): ReactNode {
  if (part.type === "text") {
    const streaming = part.state === "streaming";
    return (
      <div
        className={`bv-msg bv-msg--assistant${streaming ? " bv-msg--streaming" : ""}`}
        data-testid="chat-assistant-text"
      >
        <AssistantText text={part.text ?? ""} />
      </div>
    );
  }
  if (part.type === "reasoning") {
    return (
      <div className="bv-reasoning" data-testid="chat-reasoning">
        <span aria-hidden="true">✦</span>
        <span>{part.text}</span>
      </div>
    );
  }
  if (part.type === "error") {
    return (
      <div className="bv-msg--error" data-testid="chat-error">
        <span aria-hidden="true">!</span>
        <span>{part.errorText}</span>
      </div>
    );
  }
  if (part.type.startsWith("tool-")) return <ToolPart part={part} />;
  // data-* (gate queue / deferred tick) render nothing in the feed.
  return null;
}

/** A single message → a row (user) or a sequence of part elements (assistant). */
export function MessageRow({ msg }: { msg: ChatMessage }): ReactNode {
  if (msg.role === "user") {
    const text = msg.parts
      .filter((p) => p.type === "text")
      .map((p) => p.text ?? "")
      .join("");
    return (
      <div className="bv-msg bv-msg--user" data-testid="chat-user">
        {text}
      </div>
    );
  }
  return (
    <>
      {msg.parts.map((part, i) => (
        // Key by the part's own id/toolCallId when present, else its index. A part that renders nothing
        // (data-*) returns null from AssistantPart — a keyed component returning null adds no DOM.
        <AssistantPart key={part.id ?? part.toolCallId ?? `p-${i}`} part={part} />
      ))}
    </>
  );
}

/** Three bouncing dots — the pre-first-token "thinking" signal while a turn is submitted. */
function TypingDots(): ReactNode {
  return (
    <div
      className="bv-typing"
      role="status"
      aria-label="Maestro is thinking"
      data-testid="chat-typing"
    >
      <span />
      <span />
      <span />
    </div>
  );
}

/** The empty state — a calm plain-voice greeting, sentence case, no emoji (CLAUDE.md §Voice). */
function EmptyState({ layer }: { layer?: string }): ReactNode {
  return (
    <div className="bv-msg bv-msg--assistant" data-testid="chat-empty">
      {emptySessionGreeting(layer)}
    </div>
  );
}

/**
 * The whole transcript. Renders each message via the shared renderers; shows the typing dots when a turn
 * is `submitted` and no assistant text has arrived yet (the caret takes over once a text part streams).
 * The bottom-anchor + inner scroll live on `.bv-chat-feed` in the DS layer; the feed element is owned by
 * the caller (ChatThread) so it can hold the scroll ref.
 */
export function MessageList({
  messages,
  status,
  layer,
}: {
  messages: readonly ChatMessage[];
  status: ChatStatus;
  layer?: string;
}): ReactNode {
  if (messages.length === 0) return <EmptyState layer={layer} />;
  // Show the typing dots only before the first assistant token of THIS turn: when submitted and the last
  // message is still the user's (no assistant message opened yet). Once `start` folds an assistant
  // message, the streaming caret is the live signal.
  const last = messages[messages.length - 1];
  const showTyping = status === "submitted" && last?.role === "user";
  return (
    <>
      {messages.map((msg) => (
        <MessageRow key={msg.id} msg={msg} />
      ))}
      {showTyping ? <TypingDots /> : null}
    </>
  );
}
