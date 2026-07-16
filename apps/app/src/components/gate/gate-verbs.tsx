// gate-verbs.tsx — the shared gate-verb machine (BRO-1809 M5). The human's control at a gate is VERBS,
// never a form (disclosure ladder). Two surfaces offer them: the gate QUEUE (rung 2 — a queue of cards,
// gate-queue.tsx keeps its per-card machine because a card can LEAVE the queue and must DROP its pending,
// not commit it) and the INSPECTOR (rung 3 — one selected item that persists; `GateVerbs` below is its
// single-item machine, commit-on-unmount). Both share this module's PRESENTATION + pure helpers, so there
// is one verb look and one grace rule. gate.ts §PendingVerdict: a chosen verdict is reversible for a beat
// (GATE_GRACE_WINDOW_MS), sent exactly once, never silently dropped — only Undo cancels.

import {
  GATE_GRACE_WINDOW_MS,
  GATE_VERDICT_VERBS,
  type Intent,
  type WorkItem,
} from "@maestro/protocol";
import { Button } from "@maestro/ui";
import { Check, Undo2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

/** The dispatch seam — `postIntent` in production, a double in tests. Resolves on the 202 ack. */
export type IntentDispatch = (
  intent: Intent,
  opts?: { idempotencyKey?: string },
) => Promise<unknown>;

/** A verdict the human chose but that has not yet committed (mirrors gate.ts `GracePhase`). During
 *  `grace` it is undoable and the intent is NOT sent; the timer fires the send exactly once. */
export interface Pending {
  /** the plain-voice label shown once chosen ("Approved" · "Sent back" · "Blocked" · …). */
  label: string;
  intent: Intent;
  phase: "grace" | "sending" | "sent" | "failed";
  /** epoch ms the human clicked — the grace window is [chosenAt, chosenAt + GATE_GRACE_WINDOW_MS). */
  chosenAt: number;
  /** whether this verb is grace-windowed (verdicts) or fires immediately (redispatch). */
  graced: boolean;
  error?: string;
}

export const secondsLeft = (chosenAt: number, now: number): number =>
  Math.max(0, Math.ceil((GATE_GRACE_WINDOW_MS - (now - chosenAt)) / 1000));

export function errText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return typeof e === "string" ? e : "network error";
}

/** The verb row (the rung-2/3 control). Review → Approve (primary) + Send back (secondary); Block +
 *  Escalate are the inspector's secondary controls (D-GATE), rendered ONLY when their handlers are given
 *  (the gate queue passes none, so its row is unchanged). Blocked (Stuck, no gate) → Redispatch. */
export function GateActions({
  item,
  onApprove,
  onSendBack,
  onRedispatch,
  onBlock,
  onEscalate,
}: {
  item: WorkItem;
  onApprove: () => void;
  onSendBack: () => void;
  onRedispatch: () => void;
  onBlock?: () => void;
  onEscalate?: () => void;
}) {
  return (
    <div className="mc-detail-actions">
      {item.state === "review" ? (
        <>
          {/* Labels come from GATE_VERDICT_VERBS (gate.ts) — the single source that keeps the wire verb
              `escalate` surfacing in plain voice as "Point" (CLAUDE.md §Voice: never expose wire verbs). */}
          <Button variant="primary" size="sm" onClick={onApprove}>
            <Check size={13} strokeWidth={2} />
            {GATE_VERDICT_VERBS.approve}
          </Button>
          <Button variant="secondary" size="sm" onClick={onSendBack}>
            {GATE_VERDICT_VERBS.revise}
          </Button>
          {onBlock ? (
            <Button variant="secondary" size="sm" onClick={onBlock}>
              {GATE_VERDICT_VERBS.block}
            </Button>
          ) : null}
          {onEscalate ? (
            <Button variant="secondary" size="sm" onClick={onEscalate}>
              {GATE_VERDICT_VERBS.escalate}
            </Button>
          ) : null}
        </>
      ) : (
        <Button variant="secondary" size="sm" onClick={onRedispatch}>
          Redispatch
        </Button>
      )}
    </div>
  );
}

/** The send-back note — a revise carries feedback (intents.ts `revise{gateId, feedback}`), so the verb
 *  collects it inline. Empty note can't send. */
export function SendBackNote({
  value,
  onChange,
  onCancel,
  onSend,
}: {
  value: string;
  onChange: (v: string) => void;
  onCancel: () => void;
  onSend: () => void;
}) {
  return (
    <div className="mcc-gateq-note">
      <textarea
        className="mcc-gateq-note-input"
        placeholder="What should change? (sent back with notes)"
        aria-label="What should change? Sent back with notes"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={2}
      />
      <div className="mc-detail-actions">
        <Button variant="primary" size="sm" disabled={value.trim().length === 0} onClick={onSend}>
          {GATE_VERDICT_VERBS.revise}
        </Button>
        <Button variant="secondary" size="sm" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** The escalate target — an escalate carries a `to` (intents.ts `escalate{gateId, to}`: point the gate
 *  at an owner, node STAYS at review, re-decidable). Surfaces in plain voice as "Point" (the send button)
 *  though the wire verb is `escalate`. Collected inline like the send-back note; empty target can't send. */
function EscalateNote({
  value,
  onChange,
  onCancel,
  onSend,
}: {
  value: string;
  onChange: (v: string) => void;
  onCancel: () => void;
  onSend: () => void;
}) {
  return (
    <div className="mcc-gateq-note">
      <input
        className="mcc-gateq-note-input"
        placeholder="Point it at whom? (an owner handle)"
        aria-label="Point it at whom? An owner handle"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
      <div className="mc-detail-actions">
        <Button variant="primary" size="sm" disabled={value.trim().length === 0} onClick={onSend}>
          {GATE_VERDICT_VERBS.escalate}
        </Button>
        <Button variant="secondary" size="sm" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** The chosen state — reversible for a beat (grace), then "takes effect on the next tick"; a failed send
 *  re-surfaces with an error chip (never a silent drop, gate.ts §PendingVerdict). */
export function GateDone({
  pending,
  secondsLeft: secs,
  onUndo,
}: {
  pending: Pending;
  secondsLeft: number;
  onUndo: () => void;
}) {
  if (pending.phase === "failed") {
    return (
      <div className="mcc-gateq-done" data-testid="gate-failed">
        <span className="mcc-gateq-error">Could not send · {pending.error ?? "try again"}</span>
        <Button variant="secondary" size="sm" className="mcc-gateq-undo" onClick={onUndo}>
          Dismiss
        </Button>
      </div>
    );
  }
  const graceable = pending.phase === "grace" && pending.graced;
  return (
    <div className="mcc-gateq-done" data-testid="gate-done">
      <Check size={14} strokeWidth={2} />
      {pending.label}
      <span className="mcc-gateq-done-note">
        {pending.phase === "sent"
          ? "takes effect on the next tick"
          : pending.phase === "sending"
            ? "sending…"
            : "reversible for a beat"}
      </span>
      {graceable ? (
        <Button variant="secondary" size="sm" className="mcc-gateq-undo" onClick={onUndo}>
          <Undo2 size={13} strokeWidth={2} />
          Undo · {secs}s
        </Button>
      ) : null}
    </div>
  );
}

/** Which inline input a chosen-but-uncollected verb is gathering (revise → feedback, escalate → owner). */
type NoteMode = "revise" | "escalate" | null;

/**
 * The inspector's single-item verb machine (rung 3). One selected `item`, so the state is a single
 * `pending` (not a per-id map). The grace machine mirrors the gate queue's — choose → grace window →
 * send exactly once; Undo cancels; **unmount COMMITS** a still-in-grace verdict (closing the inspector or
 * switching selection is not Undo, gate.ts §PendingVerdict). Unlike the queue, a selected item does not
 * "leave" mid-grace (an approve moves it review→done in place), so there is no leave-drop path here.
 */
export function GateVerbs({ item, onIntent }: { item: WorkItem; onIntent: IntentDispatch }) {
  const [pending, setPending] = useState<Pending | null>(null);
  const [noteMode, setNoteMode] = useState<NoteMode>(null);
  const [noteText, setNoteText] = useState("");
  const [now, setNow] = useState(() => Date.now());
  // The single in-flight grace timer + its flush (fire-the-send-now) closure, so unmount can COMMIT a
  // chosen-but-not-yet-sent verdict rather than drop it.
  const timer = useRef<{ timer: ReturnType<typeof setTimeout>; flush: () => void } | null>(null);

  const clearTimer = useCallback(() => {
    if (timer.current !== null) {
      clearTimeout(timer.current.timer);
      timer.current = null;
    }
  }, []);

  // Send the chosen intent exactly once. The idempotency key is STABLE per decision, so any retry of THIS
  // POST at the transport layer (or an unmount flush racing the timer) de-dupes server-side.
  const send = useCallback(
    async (label: string, intent: Intent, chosenAt: number) => {
      setPending((cur) => (cur ? { ...cur, phase: "sending" } : cur));
      try {
        await onIntent(intent, { idempotencyKey: `${item.id}:${label}:${chosenAt}` });
        setPending((cur) => (cur ? { ...cur, phase: "sent" } : cur));
      } catch (e: unknown) {
        setPending((cur) => (cur ? { ...cur, phase: "failed", error: errText(e) } : cur));
      }
    },
    [onIntent, item.id],
  );

  const choose = useCallback(
    (label: string, intent: Intent, graced: boolean) => {
      const chosenAt = Date.now();
      setNow(chosenAt); // sync the countdown clock to the click so the first render shows a full window
      setPending({ label, intent, phase: "grace", chosenAt, graced });
      setNoteMode(null);
      if (graced) {
        clearTimer();
        timer.current = {
          timer: setTimeout(() => {
            timer.current = null;
            send(label, intent, chosenAt);
          }, GATE_GRACE_WINDOW_MS),
          flush: () => send(label, intent, chosenAt),
        };
      } else {
        send(label, intent, chosenAt); // redispatch: no grace, fire now
      }
    },
    [send, clearTimer],
  );

  const undo = useCallback(() => {
    clearTimer();
    setPending(null);
  }, [clearTimer]);

  // A grace-countdown clock: ticks at 500ms only while a verdict is inside its window (calm otherwise).
  const graceOpen = pending?.phase === "grace" && pending.graced;
  useEffect(() => {
    if (!graceOpen) return;
    const t = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(t);
  }, [graceOpen]);

  // On unmount, COMMIT (don't drop) a still-in-grace verdict — leaving the surface is not Undo. The stable
  // idempotency key means a flush + an about-to-fire timer can't double-apply.
  useEffect(() => {
    const t = timer;
    return () => {
      if (t.current !== null) {
        clearTimeout(t.current.timer);
        t.current.flush();
      }
    };
  }, []);

  // A verdict/redispatch was chosen — show the reversible-for-a-beat chip.
  if (pending) {
    return (
      <GateDone
        pending={pending}
        secondsLeft={pending.graced ? secondsLeft(pending.chosenAt, now) : 0}
        onUndo={undo}
      />
    );
  }

  // Blocked (Stuck) has no gate row — the only verb is an immediate, non-destructive redispatch (keyed
  // on the node, not a gate verdict).
  if (item.state === "blocked") {
    return (
      <GateActions
        item={item}
        onApprove={() => {}}
        onSendBack={() => {}}
        onRedispatch={() => choose("Redispatching", { type: "dispatch", nodeId: item.id }, false)}
      />
    );
  }
  const gateId = item.gateId;
  // A gate verdict needs a `review` node AND its open gate row. Without both there is nothing to decide
  // (a non-review node, or the degraded review-with-no-gate) — the receipts still show; no verbs.
  if (item.state !== "review" || !gateId) return null;

  if (noteMode === "revise") {
    return (
      <SendBackNote
        value={noteText}
        onChange={setNoteText}
        onCancel={() => setNoteMode(null)}
        onSend={() => {
          const feedback = noteText.trim();
          if (!feedback) return;
          choose("Sent back", { type: "revise", gateId, feedback }, true);
          setNoteText("");
        }}
      />
    );
  }
  if (noteMode === "escalate") {
    return (
      <EscalateNote
        value={noteText}
        onChange={setNoteText}
        onCancel={() => setNoteMode(null)}
        onSend={() => {
          const to = noteText.trim();
          if (!to) return;
          // Plain-voice confirmation of the "Point" verb (the wire verb is `escalate`).
          choose("Pointed", { type: "escalate", gateId, to }, true);
          setNoteText("");
        }}
      />
    );
  }

  return (
    <GateActions
      item={item}
      onApprove={() => choose("Approved", { type: "approve", gateId }, true)}
      onSendBack={() => {
        setNoteMode("revise");
        setNoteText("");
      }}
      onRedispatch={() => {}}
      onBlock={() => choose("Blocked", { type: "block", gateId }, true)}
      onEscalate={() => {
        setNoteMode("escalate");
        setNoteText("");
      }}
    />
  );
}
