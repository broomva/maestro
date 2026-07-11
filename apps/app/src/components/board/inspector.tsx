// Inspector (BRO-1825, M3 stub of the M5 panel) — the right panel, rung 3 of the disclosure ladder:
// for VERIFYING, not operating (CLAUDE.md §disclosure ladder). Selection drives it — a selected work
// item shows its RECEIPTS (run branch, verdict, reason, the gate "look", worker, age), never worktrees /
// index.db / the engine room, and NEVER a progress percentage (CLAUDE.md §Work states: receipts, not
// progress). This is the persistent ~45% panel M5 fills in with look/chat/activity tabs + the human
// verbs (approve, send back, grant, point); M3 wires selection → a read-only receipts view.

import type { WorkItem } from "@maestro/protocol";
import { DotComet, StatusBadge, workStatusView } from "@maestro/ui";
import { relativeTime } from "./board-view";

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

export function Inspector({ item }: { item: WorkItem | null }) {
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

      <p className="mt-1 text-muted-foreground text-xs">
        Full look, chat, and activity land with the inspector.
      </p>
    </aside>
  );
}
