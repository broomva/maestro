// The feedback drawer (FID-7 · BRO-1894) — ConceptFeedback.jsx `MccFeedback`, ported honest. A right-
// docked matte drawer over a dimmed app (a drawer is a panel, not one of the three glass places, so it
// stays matte — CLAUDE.md §Glass). Opens from the sidebar footer and from the ⌘K palette.
//
// HONEST DATA: the client store has no feedback read/write path yet, so (1) the composer is a real
// local form but delivery is labelled a PREVIEW (submitting does not fabricate a "the team has it"
// receipt), and (2) the thread history is a clearly-labelled SAMPLE, not live threads. The real wiring
// lands with the runtime feedback endpoint (a later ticket).
//
// Renders in place with `position: fixed` (no portal), so it is unit-testable directly at open=true;
// it returns null when closed, so the shell's SSR render stays clean.

import { Bug, Check, Heart, Lightbulb, MessageSquare, Send, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { handleTrapTab } from "./focus-trap";

type FeedbackType = "idea" | "issue" | "praise";

const FB_TYPES: { id: FeedbackType; label: string; icon: typeof Lightbulb; placeholder: string }[] =
  [
    {
      id: "idea",
      label: "Idea",
      icon: Lightbulb,
      placeholder: "What would make the loop work better for you?",
    },
    {
      id: "issue",
      label: "Issue",
      icon: Bug,
      placeholder: "What went wrong, and what did you expect instead?",
    },
    {
      id: "praise",
      label: "Praise",
      icon: Heart,
      placeholder: "What's landing well? maestro likes to know too.",
    },
  ];

type SampleStatus = "triage" | "ship" | "log";

/** Sample feedback history — illustrative examples, NOT live threads (no read path yet). */
const FB_SAMPLE_THREADS: {
  id: string;
  title: string;
  time: string;
  status: SampleStatus;
  statusLabel: string;
  detail: string;
}[] = [
  {
    id: "s1",
    title: "Let the gate queue group by folder, not just by time",
    time: "3d",
    status: "triage",
    statusLabel: "With the team",
    detail: "an example of a thread in triage",
  },
  {
    id: "s2",
    title: "Dark mode washed out the run timeline ticks",
    time: "1w",
    status: "ship",
    statusLabel: "Shipped",
    detail: "an example of a shipped thread",
  },
  {
    id: "s3",
    title: "A shortcut to jump straight to Needs you",
    time: "2w",
    status: "log",
    statusLabel: "Logged",
    detail: "an example of a logged thread",
  },
];

/** The honest send receipt (no backend yet). Exported so §Voice guards can machine-check the copy. */
export const FEEDBACK_PREVIEW_NOTE = "Preview only. Feedback delivery isn't wired yet.";

function statusColor(status: SampleStatus): string {
  if (status === "ship") return "var(--bv-success)";
  if (status === "triage") return "var(--bv-blue)";
  return "var(--bv-gray-400)";
}

interface FeedbackDrawerProps {
  open: boolean;
  onClose: () => void;
}

export function FeedbackDrawer({ open, onClose }: FeedbackDrawerProps) {
  const [type, setType] = useState<FeedbackType>("idea");
  const [text, setText] = useState("");
  const [attach, setAttach] = useState(true);
  const [previewed, setPreviewed] = useState(false);
  const asideRef = useRef<HTMLElement>(null);
  const textRef = useRef<HTMLTextAreaElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  // Read the latest onClose via a ref so the open effect can key on [open] ALONE. Shell subscribes to
  // the live server slice, so it (and this drawer's OverlayHost parent) re-renders on every SSE event;
  // keying the effect on onClose — a fresh inline identity each render — would re-run it on ordinary
  // background traffic and WIPE the user's in-progress text + yank focus. The palette is immune the
  // same way (its effect keys on [open, place]). P20 BRO-1894.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (!open) return;
    // Capture the trigger so close returns focus to it (WAI-ARIA dialog), reset the form, focus in.
    restoreFocusRef.current = document.activeElement as HTMLElement | null;
    setType("idea");
    setText("");
    setAttach(true);
    setPreviewed(false);
    const t = setTimeout(() => textRef.current?.focus(), 40);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCloseRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      clearTimeout(t);
      window.removeEventListener("keydown", onKey);
      restoreFocusRef.current?.focus?.();
    };
  }, [open]);

  if (!open) return null;

  const active = FB_TYPES.find((t) => t.id === type) ?? FB_TYPES[0];
  const canSend = text.trim().length > 0;

  // Honest send: no backend to deliver to yet, so this surfaces a PREVIEW receipt rather than
  // fabricating a "routing to the team" thread. The real endpoint lands in a later ticket.
  const send = () => {
    if (!canSend) {
      textRef.current?.focus();
      return;
    }
    setPreviewed(true);
  };

  return (
    <>
      {/* Decorative dismiss backdrop (aria-hidden): keyboard users close with Esc; the Close button is reachable. */}
      <div className="fb-scrim" aria-hidden="true" onMouseDown={onClose} />
      <aside
        ref={asideRef}
        className="fb-drawer"
        data-screen-label="Feedback drawer"
        data-testid="feedback-drawer"
        role="dialog"
        aria-modal="true"
        aria-label="Feedback"
        onKeyDown={(e) => handleTrapTab(asideRef.current, e)}
      >
        <header className="fb-head">
          <span className="fb-head-glyph">
            <MessageSquare size={18} />
          </span>
          <div className="fb-head-main">
            <div className="fb-title-row">
              <span className="fb-title">Feedback</span>
              <span className="set-preview">preview</span>
            </div>
            <div className="fb-sub">Tell the loop what to build or fix.</div>
          </div>
          <button type="button" className="fb-x" aria-label="Close" onClick={onClose}>
            <X size={17} />
          </button>
        </header>

        <div className="fb-body">
          <div className="fb-compose">
            <div className="fb-compose-field">
              <textarea
                ref={textRef}
                className="fb-text"
                aria-label="Your feedback"
                value={text}
                placeholder={active?.placeholder}
                onChange={(e) => {
                  setText(e.target.value);
                  setPreviewed(false);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    send();
                  }
                }}
              />
              <div className="fb-tray">
                <div className="fb-types">
                  {FB_TYPES.map((t) => {
                    const Icon = t.icon;
                    return (
                      <button
                        key={t.id}
                        type="button"
                        aria-pressed={type === t.id}
                        className={`fb-type${type === t.id ? " is-active" : ""}`}
                        onClick={() => setType(t.id)}
                      >
                        <Icon size={14} />
                        {t.label}
                      </button>
                    );
                  })}
                </div>
                <button
                  type="button"
                  className="fb-send"
                  aria-label="Send feedback"
                  disabled={!canSend}
                  onClick={send}
                >
                  <Send size={16} />
                </button>
              </div>
            </div>

            {/* A real checkbox (semantic + keyboard-native), visually replaced by the styled box. */}
            <label className="fb-ctx">
              <input
                type="checkbox"
                className="fb-ctx-input"
                checked={attach}
                onChange={(e) => setAttach(e.target.checked)}
              />
              <span className={`fb-ctx-check${attach ? " is-on" : ""}`} aria-hidden="true">
                <Check size={12} />
              </span>
              <span className="fb-ctx-label">Attach this screen and its context</span>
            </label>

            {previewed ? (
              <div className="fb-receipt" role="status">
                {FEEDBACK_PREVIEW_NOTE}
              </div>
            ) : null}
          </div>

          <div className="fb-threads">
            <div className="fb-threads-head">
              <span className="fb-threads-label">Recent feedback</span>
              <span className="set-preview">sample</span>
            </div>
            {FB_SAMPLE_THREADS.map((t) => (
              <div key={t.id} className="fb-thread">
                <span
                  className="mc-chip-dot fb-thread-dot"
                  style={{ width: 9, height: 9, background: statusColor(t.status) }}
                />
                <span className="fb-thread-body">
                  <span className="fb-thread-top">
                    <span className="fb-thread-title">{t.title}</span>
                    <span className="fb-thread-time">{t.time}</span>
                  </span>
                  <span className="fb-thread-meta">
                    <span className={`fb-thread-status fb-thread-status--${t.status}`}>
                      {t.statusLabel}
                    </span>
                    <span className="fb-thread-detail">{t.detail}</span>
                  </span>
                </span>
              </div>
            ))}
          </div>
        </div>
      </aside>
    </>
  );
}
