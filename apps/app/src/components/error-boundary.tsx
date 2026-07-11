// Surface-level error boundaries (BRO-1824, porting-notes §Production hardening). One around each routed
// view, one around the chat pane, one around the inspector: a crashed pane renders a calm plain-voice
// fallback instead of blanking the shell (the done.check: "a thrown render error in one pane never blanks
// the shell"). CLAUDE.md §Voice — plain, second person, calm; NEVER a raw stack in the chrome.

import { Component, type ErrorInfo, type ReactNode } from "react";

/** The calm fallback a crashed pane shows — plain voice, no stack, no red alarm tone. Reused by the
 *  ErrorBoundary class (component subtrees) AND as a TanStack Router route `errorComponent`. */
export function PaneErrorFallback({ label }: { label?: string }) {
  return (
    <div
      data-testid="pane-error"
      role="alert"
      className="flex h-full min-h-[140px] flex-col items-center justify-center gap-1 p-6 text-center"
    >
      <p className="font-medium text-foreground text-sm">
        {label ? `${label} hit a snag.` : "This pane hit a snag."}
      </p>
      <p className="max-w-[320px] text-muted-foreground text-sm">
        The rest of the app is fine. Reload to bring it back.
      </p>
    </div>
  );
}

interface Props {
  children: ReactNode;
  /** Plain-voice name of what crashed (e.g. "The inspector") — leads the fallback line. */
  label?: string;
}
interface State {
  errored: boolean;
}

/**
 * Wrap a component subtree so a render throw inside it becomes the calm fallback rather than propagating
 * up and blanking the shell. Used for the panes that are NOT their own route (the inspector; the chat
 * pane when it lands) — routed views use TanStack Router's `errorComponent` instead (same fallback UI).
 */
export class ErrorBoundary extends Component<Props, State> {
  override state: State = { errored: false };

  static getDerivedStateFromError(): State {
    return { errored: true };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    // Log for diagnosis; the user never sees the stack (CLAUDE.md §Voice).
    console.error("[maestro] pane error boundary caught", error, info.componentStack);
  }

  override render(): ReactNode {
    if (this.state.errored) return <PaneErrorFallback label={this.props.label} />;
    return this.props.children;
  }
}
