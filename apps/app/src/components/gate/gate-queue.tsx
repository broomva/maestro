// The gate queue (BRO-1888 FID-3) — the disclosure-ladder RUNG 2: the human looks, then acts with
// verbs (approve · send back · redispatch), never a form. It is a DERIVED VIEW over the server-truth
// leaves that need a human (`selectGateQueue` → review + blocked), the same SSE-fed store the mission
// plane reads — never a separate store (gate.ts §Membership). "Needs you" is accent-blue, never red.
//
// The one sanctioned timing component is the GRACE WINDOW (gate.ts `GATE_GRACE_WINDOW_MS`): a chosen
// verdict is reversible for a beat before its intent is sent, so a mis-click never fires an approve.
// The verb becomes an `Intent` posted to POST /api/intents (intents in, events out) — the RESULT lands
// on the event stream and re-projects the node (review→done), which is what DEQUEUES the card. So the
// component never mutates the store; it overlays the pending/undo/sent state and lets server truth win.
//
// Verb scope (this slice): review → Approve (grace) + Send back with a note (grace); blocked →
// Redispatch (immediate, non-destructive). Grant / Point need a capability / target and are the
// inspector's rung-3 controls (gate.ts: "Block / Point are secondary in the inspector") — FID-5.

import { GATE_GRACE_WINDOW_MS, type Intent, type WorkItem } from "@maestro/protocol";
import { Button, STATUS_DOT_VAR, workStatusView } from "@maestro/ui";
import { Check, Undo2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { relativeTime } from "../board/board-view";

/** The dispatch seam — `postIntent` in production, a double in tests. Resolves on the 202 ack. */
export type IntentDispatch = (
  intent: Intent,
  opts?: { idempotencyKey?: string },
) => Promise<unknown>;

/** A verdict the human chose but that has not yet committed (mirrors gate.ts `GracePhase`). During
 *  `grace` it is undoable and the intent is NOT sent; the timer fires the send exactly once. */
interface Pending {
  /** the plain-voice label shown once chosen ("Approved" · "Sent back" · "Redispatching"). */
  label: string;
  intent: Intent;
  phase: "grace" | "sending" | "sent" | "failed";
  /** epoch ms the human clicked — the grace window is [chosenAt, chosenAt + GATE_GRACE_WINDOW_MS). */
  chosenAt: number;
  /** whether this verb is grace-windowed (verdicts) or fires immediately (redispatch). */
  graced: boolean;
  error?: string;
}

const secondsLeft = (chosenAt: number, now: number): number =>
  Math.max(0, Math.ceil((GATE_GRACE_WINDOW_MS - (now - chosenAt)) / 1000));

export interface GateQueueProps {
  /** The gate-queue items — server-truth leaves that need a human (`selectGateQueue`). */
  items: WorkItem[];
  /** The intent dispatcher (default `postIntent`). */
  onIntent: IntentDispatch;
}

export function GateQueue({ items, onIntent }: GateQueueProps) {
  // Pending verdicts, keyed by the stable WorkItem id (present on every node; gateId is review-only).
  const [pending, setPending] = useState<Record<string, Pending>>({});
  // Which card is expanded (the "look") and which is being noted for a send-back.
  const [openId, setOpenId] = useState<string | null>(null);
  const [noteFor, setNoteFor] = useState<string | null>(null);
  const [noteText, setNoteText] = useState("");
  // A coarse clock for the grace countdown label; ticks only while a grace window is open.
  const [now, setNow] = useState(() => Date.now());
  const timers = useRef<Record<string, ReturnType<typeof setTimeout>>>({});

  const clearTimer = useCallback((id: string) => {
    const t = timers.current[id];
    if (t !== undefined) {
      clearTimeout(t);
      delete timers.current[id];
    }
  }, []);

  // Send the chosen intent exactly once (called by the grace timer, or immediately for redispatch). The
  // idempotency key is STABLE per decision so a resend can never double-apply (client.ts §idempotencyKey).
  const send = useCallback(
    (id: string, label: string, intent: Intent, chosenAt: number) => {
      setPending((cur) => (cur[id] ? { ...cur, [id]: { ...cur[id], phase: "sending" } } : cur));
      onIntent(intent, { idempotencyKey: `${id}:${label}:${chosenAt}` })
        .then(() =>
          setPending((cur) => (cur[id] ? { ...cur, [id]: { ...cur[id], phase: "sent" } } : cur)),
        )
        .catch((e: unknown) =>
          setPending((cur) =>
            cur[id] ? { ...cur, [id]: { ...cur[id], phase: "failed", error: errText(e) } } : cur,
          ),
        );
    },
    [onIntent],
  );

  const choose = useCallback(
    (id: string, label: string, intent: Intent, graced: boolean) => {
      const chosenAt = Date.now();
      // Sync the countdown clock to the click so the first render shows a full 5s, not 6 (the interval
      // clock is otherwise stale from mount, making `now - chosenAt` negative for one render).
      setNow(chosenAt);
      setPending((cur) => ({ ...cur, [id]: { label, intent, phase: "grace", chosenAt, graced } }));
      setNoteFor(null);
      if (graced) {
        clearTimer(id);
        timers.current[id] = setTimeout(() => {
          delete timers.current[id];
          send(id, label, intent, chosenAt);
        }, GATE_GRACE_WINDOW_MS);
      } else {
        send(id, label, intent, chosenAt); // redispatch: no grace, fire now
      }
    },
    [send, clearTimer],
  );

  const undo = useCallback(
    (id: string) => {
      clearTimer(id);
      setPending((cur) => {
        if (!cur[id]) return cur;
        const { [id]: _dropped, ...rest } = cur;
        return rest;
      });
    },
    [clearTimer],
  );

  // The countdown clock — runs only while at least one verdict is inside its grace window.
  const graceOpen = Object.values(pending).some((p) => p.phase === "grace" && p.graced);
  useEffect(() => {
    if (!graceOpen) return;
    const t = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(t);
  }, [graceOpen]);

  // Cleanup: when a card leaves the queue (server processed the verdict → the node left review/blocked)
  // drop its pending entry + timer, so a later reappearance of the same UUID doesn't show a stale verb.
  const liveIds = items.map((i) => i.id).join(",");
  useEffect(() => {
    const ids = new Set(liveIds ? liveIds.split(",") : []);
    setPending((cur) => {
      let changed = false;
      const next: Record<string, Pending> = {};
      for (const [id, p] of Object.entries(cur)) {
        if (ids.has(id)) next[id] = p;
        else {
          changed = true;
          clearTimer(id);
        }
      }
      return changed ? next : cur;
    });
  }, [liveIds, clearTimer]);

  // Clear every timer on unmount (the pending sends are abandoned; the human left the surface).
  useEffect(() => {
    const t = timers.current;
    return () => {
      for (const id of Object.keys(t)) clearTimeout(t[id]);
    };
  }, []);

  if (items.length === 0) {
    return (
      <div className="mcc-allclear" data-testid="gate-allclear">
        <Check size={14} strokeWidth={2} />
        Nothing at your gate. The loop holds everything.
      </div>
    );
  }

  return (
    <div className="mcc-gateq" data-testid="gate-queue" data-screen-label="Gate queue">
      {items.map((item) => {
        const p = pending[item.id];
        const v = workStatusView(item.state, item.kind);
        const isReview = item.state === "review";
        return (
          <div
            key={item.id}
            className="mcc-gateq-card"
            data-testid="gate-card"
            data-state={item.state}
          >
            {/* The row IS the disclosure toggle (an accessible button — no nested interactive: the verbs
                are siblings below, not descendants). Disabled once a verdict is pending. */}
            <button
              type="button"
              className="mcc-gateq-row"
              aria-expanded={openId === item.id}
              disabled={Boolean(p)}
              onClick={() => setOpenId((cur) => (cur === item.id ? null : item.id))}
            >
              <span className="mc-chip-dot" style={{ background: STATUS_DOT_VAR[v.tone] }} />
              <span className="mcc-gateq-title">{item.title}</span>
              <span className="mcc-loops-t">
                {relativeTime(item.lastEventAt ?? item.updatedAt, now)}
              </span>
            </button>
            {item.look?.ran ? <span className="mcc-gateq-meta">{item.look.ran}</span> : null}

            {p ? (
              <GateDone
                pending={p}
                secondsLeft={p.graced ? secondsLeft(p.chosenAt, now) : 0}
                onUndo={() => undo(item.id)}
              />
            ) : openId === item.id ? (
              <>
                <GateLook item={item} />
                {noteFor === item.id && isReview ? (
                  <SendBackNote
                    value={noteText}
                    onChange={setNoteText}
                    onCancel={() => setNoteFor(null)}
                    onSend={() => {
                      const feedback = noteText.trim();
                      if (!feedback || !item.gateId) return;
                      choose(
                        item.id,
                        "Sent back",
                        { type: "revise", gateId: item.gateId, feedback },
                        true,
                      );
                      setNoteText("");
                    }}
                  />
                ) : (
                  <GateActions
                    item={item}
                    onApprove={() =>
                      item.gateId &&
                      choose(item.id, "Approved", { type: "approve", gateId: item.gateId }, true)
                    }
                    onSendBack={() => {
                      setNoteFor(item.id);
                      setNoteText("");
                    }}
                    onRedispatch={() =>
                      choose(item.id, "Redispatching", { type: "dispatch", nodeId: item.id }, false)
                    }
                  />
                )}
              </>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

/** The "look" — what changed · what it decided · what it asks. Evidence on the card so approve is never
 *  blind (concepts.css §.mcc-gateq-look). Falls back to the run receipt when no gate compression exists. */
function GateLook({ item }: { item: WorkItem }) {
  const rows: [string, string][] = [];
  if (item.run) rows.push(["changed", item.run]);
  if (item.look?.decided?.length) rows.push(["decided", item.look.decided.join(" · ")]);
  if (item.look?.ask) rows.push(["asks", item.look.ask]);
  else if (item.reason) rows.push(["blocked", item.reason]);
  if (rows.length === 0) return null;
  return (
    <div className="mcc-gateq-look">
      {rows.map(([key, val]) => (
        <div key={key} className="mcc-gateq-look-row">
          <span className="mcc-gateq-look-key">{key}</span>
          <span className="mcc-gateq-look-val">{val}</span>
        </div>
      ))}
    </div>
  );
}

/** The verb row (rung-2 control). Review → Approve (primary) + Send back (secondary); blocked (Stuck,
 *  no gate) → Redispatch. Siblings of the disclosure toggle, so no propagation dance is needed. */
function GateActions({
  item,
  onApprove,
  onSendBack,
  onRedispatch,
}: {
  item: WorkItem;
  onApprove: () => void;
  onSendBack: () => void;
  onRedispatch: () => void;
}) {
  return (
    <div className="mc-detail-actions">
      {item.state === "review" ? (
        <>
          <Button variant="primary" size="sm" onClick={onApprove}>
            <Check size={13} strokeWidth={2} />
            Approve
          </Button>
          <Button variant="secondary" size="sm" onClick={onSendBack}>
            Send back
          </Button>
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
 *  collects it inline (rung 2 stays verbs; the rich edit is the inspector, FID-5). Empty note can't send. */
function SendBackNote({
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
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={2}
      />
      <div className="mc-detail-actions">
        <Button variant="primary" size="sm" disabled={value.trim().length === 0} onClick={onSend}>
          Send back
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
function GateDone({
  pending,
  secondsLeft,
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
          Undo · {secondsLeft}s
        </Button>
      ) : null}
    </div>
  );
}

function errText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return typeof e === "string" ? e : "network error";
}
