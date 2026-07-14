// The overlay host (FID-7 · BRO-1894) — mounts once in the shell and owns the transient overlay layer:
// the ⌘K command palette and the feedback drawer. The shell already dispatches the events (⌘K keydown +
// top-bar command field → `bv:command-open`; sidebar footer → `bv:feedback-open`); this listens and
// renders. Keeping the open-state here (not in the shell) keeps the shell chrome pure and the overlays
// decoupled — any surface can open one by firing the event.
//
// Both children return null when closed, so mounting this in the shell never affects the shell's SSR
// render (shell.test.tsx). Opening one closes the other (you never want the palette stacked on the
// drawer).

import { useEffect, useState } from "react";
import { CommandPalette } from "./command-palette";
import { FeedbackDrawer } from "./feedback-drawer";

export function OverlayHost() {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [feedbackOpen, setFeedbackOpen] = useState(false);

  useEffect(() => {
    // ⌘K toggles the palette (and closes the drawer); the sidebar's Feedback opens the drawer.
    const onCommand = () => {
      setFeedbackOpen(false);
      setPaletteOpen((v) => !v);
    };
    const onFeedback = () => {
      setPaletteOpen(false);
      setFeedbackOpen(true);
    };
    window.addEventListener("bv:command-open", onCommand);
    window.addEventListener("bv:feedback-open", onFeedback);
    return () => {
      window.removeEventListener("bv:command-open", onCommand);
      window.removeEventListener("bv:feedback-open", onFeedback);
    };
  }, []);

  return (
    <>
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        onOpenFeedback={() => {
          setPaletteOpen(false);
          setFeedbackOpen(true);
        }}
      />
      <FeedbackDrawer open={feedbackOpen} onClose={() => setFeedbackOpen(false)} />
    </>
  );
}
