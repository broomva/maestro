// Inspector (BRO-1825 M3 stub → BRO-1809 M5) — the right panel, rung 3 of the disclosure ladder: for
// VERIFYING, not operating (CLAUDE.md §disclosure ladder). Selection drives it — a selected work item
// shows its RECEIPTS (run branch, verdict, reason, the gate "look", worker, age), never worktrees /
// index.db / the engine room, and NEVER a progress percentage (CLAUDE.md §Work states: receipts, not
// progress). M5 adds the human VERBS at the gate: a `review` item leads with the look, then approve /
// send back (primary) + block / escalate (secondary) — the SAME grace-windowed machine the gate queue
// uses (gate-verbs.ts). "The gate is the human's": the inspector is where you verify AND decide. The
// per-event activity timeline + full diffstat (the other M5 receipts) ride the session-events read path
// (BRO-1895) and land in a follow-up; this slice ships the verbs + the receipts the WorkItem carries.

import type { WorkItem } from "@maestro/protocol";
import { DotComet, StatusBadge, workStatusView } from "@maestro/ui";
import { GateVerbs, type IntentDispatch } from "../gate/gate-verbs";
import { relativeTime } from "./board-view";
import { LifecycleRail } from "./lifecycle-rail";

/** One receipt row — a labelled fact from the work item. `mono` for identifiers (the run branch). */
function Receipt({ label, mono, children }: { label: string; mono?: boolean; children: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-muted-foreground text-xs">{label}</dt>
      <dd className={mono ? "font-mono text-foreground text-xs" : "text-foreground text-sm"}>
        {children}
      </dd>
    </div>
  );
}

export function Inspector({
  item,
  onIntent,
}: {
  item: WorkItem | null;
  /** The intent dispatcher for the gate verbs (`postIntent` in production). */
  onIntent: IntentDispatch;
}) {
  if (item === null) {
    return (
      <aside
        data-testid="inspector-empty"
        aria-label="Inspector"
        className="flex h-full items-center justify-center px-6 text-center text-muted-foreground text-sm"
      >
        Select work to see its receipts.
      </aside>
    );
  }

  const v = workStatusView(item.state, item.kind);
  const crumb = [item.initiative, item.project].filter(Boolean).join(" › ");
  const age = relativeTime(item.lastEventAt ?? item.updatedAt);

  return (
    <aside data-testid="inspector" aria-label="Inspector" className="flex flex-col gap-4">
      <header className="flex flex-col gap-2">
        <span className="truncate text-muted-foreground text-xs">{crumb || item.path}</span>
        {/* Title is a populated section heading → weight 500 (font-medium). 600 is reserved for
            empty-state titles only (CLAUDE.md §Type). */}
        <h2 className="font-medium text-foreground text-lg">{item.title}</h2>
        <StatusBadge
          status={v.tone}
          pulse={v.pulse}
          // The running dot is the same live signal the board card uses — DotComet, not a static ink
          // dot (bg-current is none of the five canon dot colors; CLAUDE.md §Work-states).
          dot={v.running ? <DotComet size={8} /> : undefined}
        >
          {v.label}
        </StatusBadge>
      </header>

      {/* The lifecycle rail — where this item sits on its progression (rung-3, read-only; derived
          purely from state, never a percentage; CLAUDE.md §Work states). */}
      <section data-testid="inspector-rail" className="border-border border-t pt-3">
        <LifecycleRail state={item.state} />
      </section>

      {/* The gate "look" — what ran · what it decided · what it asks. The one card a "Needs you" item
          leads with (FLOWS §F5); rendered as receipts, never as a control (verbs are M5's). */}
      {item.look ? (
        // The Receipt rows emit <dt>/<dd>, so the look receipts live in a <dl> (dt/dd must be within a
        // dl; dl > div(dt,dd) is valid HTML5) — else screen readers do not pair them.
        <section data-testid="inspector-look" className="border-border border-t pt-3">
          <dl className="flex flex-col gap-2">
            <Receipt label="Ran">{item.look.ran}</Receipt>
            {item.look.decided.length > 0 ? (
              <div className="flex flex-col gap-0.5">
                <dt className="text-muted-foreground text-xs">Decided</dt>
                <dd>
                  <ul className="flex flex-col gap-0.5 text-foreground text-sm">
                    {item.look.decided.map((d, i) => (
                      // `decided` is a free-form string[] (duplicates possible, so the string itself is
                      // not a safe key), and it is a render-only list of stateless text with no reordering
                      // semantics — the index is the correct key here (React docs).
                      // biome-ignore lint/suspicious/noArrayIndexKey: render-only list, no reorder/state
                      <li key={i}>{d}</li>
                    ))}
                  </ul>
                </dd>
              </div>
            ) : null}
            {item.look.ask ? <Receipt label="Asks">{item.look.ask}</Receipt> : null}
          </dl>
        </section>
      ) : null}

      {/* The verbs — rung-3 control (M5, gate-verbs.ts). A `review` item leads with the look above,
          then acts: approve / send back (primary) + block / escalate (secondary); a `blocked` item
          redispatches. The grace window (reversible for a beat) is the shared machine the queue uses. */}
      {(item.state === "review" && item.gateId) || item.state === "blocked" ? (
        <section data-testid="inspector-verbs" className="border-border border-t pt-3">
          <GateVerbs item={item} onIntent={onIntent} />
        </section>
      ) : null}

      <dl className="flex flex-col gap-3 border-border border-t pt-3">
        {item.run ? (
          <Receipt label="Branch" mono>
            {item.run}
          </Receipt>
        ) : null}
        {item.verdict ? <Receipt label="Verdict">{item.verdict}</Receipt> : null}
        {item.reason ? <Receipt label="Reason">{item.reason}</Receipt> : null}
        {item.worker ? <Receipt label="Worker">{item.worker.name}</Receipt> : null}
        {age ? <Receipt label="Last event">{`${age} ago`}</Receipt> : null}
      </dl>

      {/* Honest scope line: the RECEIPTS shown here (look, run, verdict, state, the rail) are the real,
          projected work noun. The per-event activity timeline + full diffstat live behind the
          session-event read path (the P1 deliverable) — WorkItem deliberately excludes the event stream
          (no chat / events / budget / percent), so they open once that read path lands. Copy is plain
          voice: no em dashes, no internal build-phase names (CLAUDE.md §Voice). */}
      <p className="mt-1 text-muted-foreground text-xs">
        {item.sessionId
          ? "The full activity timeline and diffstat open once this run's events are recorded."
          : "No run yet · the activity timeline appears once a session dispatches."}
      </p>
    </aside>
  );
}
