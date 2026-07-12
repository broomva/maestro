// useBvChat (BRO-1826 M4, slice B) — the React binding, canon `AiProtocol.jsx:517-547`. Holds the
// transcript in FEATURE-LOCAL state (NOT the global work store) — this IS "chat is a projection, not a
// store" (the M4 verify criterion, CLAUDE.md §What Maestro is). All fold + status logic is the pure
// `runChatTurn`; this wrapper only owns React state, the abort controller (the stop verb), and the
// error-render policy.
//
// Refs mirror the latest transcript/transport/onData so `sendMessage` stays a STABLE callback (empty
// deps) — the composer's handler identity never changes, and a mid-stream re-render can't stale the
// history the next send builds on.

import type { ChatMessage } from "@maestro/protocol";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  type ChatStatus,
  finalizeStreamingParts,
  type RunChatTurnOptions,
  runChatTurn,
  type TransientDataChunk,
} from "./chat-turn";
import { type ChatTransport, ChatTransportError } from "./transport";

/** A stable client id for a locally-composed user message. `crypto.randomUUID` is a client-only
 *  impurity (the no-ambient-clock discipline is a RUNTIME rule; the browser may mint ids). */
function newUserId(): string {
  return `u-${crypto.randomUUID()}`;
}

export interface UseBvChatOptions {
  /** The transport the turn streams over — the real `RuntimeChatTransport`, or a fixture double. */
  transport: ChatTransport;
  /** Seed transcript (e.g. a resumed session). Defaults to empty. */
  initialMessages?: readonly ChatMessage[];
  /** Transient `data-*` chunks (live ticks) — surfaced here, never persisted into the transcript. */
  onData?: RunChatTurnOptions["onData"];
}

export interface UseBvChatResult {
  messages: readonly ChatMessage[];
  status: ChatStatus | "streaming";
  /** True while a turn is in flight (submitted or streaming) — drives the composer's stop verb. */
  busy: boolean;
  /** Send a user turn: appends it, streams the reply, folds each chunk. Empty/whitespace is a no-op. */
  sendMessage: (input: { text: string }) => void;
  /** Abort the in-flight turn (the stop verb). Safe to call when idle. */
  stop: () => void;
}

/** Render a transport failure as an assistant error part appended to the transcript, so the feed shows
 *  an honest, specific message (slice A's `ChatTransportError` carries the endpoint's code/message)
 *  rather than a silent stall. Uses the reducer's `error` part shape. Exported for unit testing (the
 *  error-render path is otherwise only reachable through a live failing transport). */
export function appendErrorMessage(
  messages: readonly ChatMessage[],
  err: unknown,
): readonly ChatMessage[] {
  const errorText =
    err instanceof ChatTransportError
      ? err.message
      : err instanceof Error
        ? err.message
        : "The chat stream failed.";
  return [
    ...messages,
    { id: `err-${messages.length}`, role: "assistant", parts: [{ type: "error", errorText }] },
  ];
}

export function useBvChat({
  transport,
  initialMessages,
  onData,
}: UseBvChatOptions): UseBvChatResult {
  const [messages, setMessages] = useState<readonly ChatMessage[]>(initialMessages ?? []);
  const [status, setStatus] = useState<ChatStatus>("ready");

  // Latest-value refs so sendMessage can stay a stable identity (empty-dep useCallback).
  const messagesRef = useRef(messages);
  messagesRef.current = messages;
  const transportRef = useRef(transport);
  transportRef.current = transport;
  const onDataRef = useRef(onData);
  onDataRef.current = onData;
  // The controller for the in-flight turn — replaced per send, aborted by `stop`.
  const abortRef = useRef<AbortController | null>(null);

  const sendMessage = useCallback(({ text }: { text: string }) => {
    const trimmed = text.trim();
    if (trimmed.length === 0) return;
    const user: ChatMessage = {
      id: newUserId(),
      role: "user",
      parts: [{ type: "text", text: trimmed }],
    };

    // A fresh controller per turn; abort any prior in-flight turn first (a second send supersedes).
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;

    void (async () => {
      try {
        for await (const step of runChatTurn(transportRef.current, messagesRef.current, user, {
          signal: controller.signal,
          onData: (c: TransientDataChunk) => onDataRef.current?.(c),
        })) {
          setMessages(step.messages);
          setStatus(step.status);
        }
      } catch (err) {
        // A genuine transport failure (not an abort — slice A returns cleanly on abort). Keep the user
        // turn (runChatTurn yielded `submitted` first), SETTLE the partial assistant message's trailing
        // streaming part (else its caret blinks forever on a mid-stream failure — runChatTurn's terminal
        // ready-yield never ran because the throw preceded it), THEN append the error part, settle to
        // ready. This is the THIRD and last terminal exit path for a turn; all three settle the caret:
        //   clean finish → runChatTurn's ready yield · explicit stop → stop() · transport error → here.
        setMessages((m) => appendErrorMessage(finalizeStreamingParts(m), err));
        setStatus("ready");
      } finally {
        // Only clear the controller if it is still the current one (a superseding send owns its own).
        if (abortRef.current === controller) abortRef.current = null;
      }
    })();
  }, []);

  const stop = useCallback(() => {
    abortRef.current?.abort();
    abortRef.current = null;
    // Settle the UI immediately (don't wait for the async loop to unwind): status ready AND settle the
    // trailing streaming part so its caret stops NOW (P20 round-2 MAJOR — else the stopped turn keeps a
    // blinking caret until the loop's own final step lands a tick later, or forever if it never does).
    setMessages((m) => finalizeStreamingParts(m));
    setStatus("ready");
  }, []);

  // Abort the in-flight turn on unmount (P20 slice-B MAJOR). Without this, navigating away mid-stream
  // (e.g. clicking a sidebar Link) leaves the fetch open and the runtime's F10 agent turn generating
  // unread — a leaked, un-observable, metered child run that cuts against the "gate is the human's"
  // thesis. The abort closes the connection, so the runtime sees the client disconnect and stops. A
  // per-session remount (SessionView is keyed by sessionId) makes this cleanup also fire on a session
  // switch, so turn A never bleeds into session B's transcript.
  useEffect(
    () => () => {
      abortRef.current?.abort();
      abortRef.current = null;
    },
    [],
  );

  return {
    messages,
    status,
    busy: status === "submitted" || status === "streaming",
    sendMessage,
    stop,
  };
}
