import { Check } from "lucide-react";
import { ThemeToggle } from "../components/theme-toggle";

/** The invariants this M0 foundation is built to hold, shown in plain voice. */
const FOUNDATION = [
  "Barely-blue ink on white; deep blue-purple in dark.",
  "Monochrome by default, every neutral on the cool axis.",
  "Focus rings are ai-blue; borders are whispers.",
  "Glass is earned; nothing on this page wears it.",
] as const;

/**
 * M0 landing — an empty app that already feels Broomva (BUILD-PLAN §M0). Matte
 * surfaces, the closed type scale, a focusable input that shows the ai-blue ring,
 * and the theme toggle. No sidebar/board yet (M2/M3); this proves the foundation.
 */
export function Landing() {
  return (
    <main className="flex min-h-dvh flex-col bg-background text-foreground">
      <header className="flex h-[52px] shrink-0 items-center justify-between border-b border-border px-5">
        <span className="text-sm font-medium">maestro</span>
        <ThemeToggle />
      </header>

      <div className="grid flex-1 place-items-center px-6 py-16">
        <div className="flex w-full max-w-[560px] flex-col gap-8">
          <div className="flex flex-col gap-3">
            <h1 className="text-h1">Maestro</h1>
            <p className="text-base text-muted-foreground">
              The scarce resource is unsupervised hours. The foundation is wired: tokens, theme, and
              glass, all on the Arcan blue axis.
            </p>
          </div>

          <section className="flex flex-col gap-4 rounded-card border border-border bg-card p-5">
            <span className="bv-section-header">Foundation</span>
            <ul className="flex flex-col gap-2.5">
              {FOUNDATION.map((line) => (
                <li key={line} className="flex items-start gap-2.5 text-sm text-muted-foreground">
                  <Check size={16} strokeWidth={2} className="mt-0.5 shrink-0" />
                  <span>{line}</span>
                </li>
              ))}
            </ul>

            <label className="flex flex-col gap-1.5">
              <span className="text-sm text-muted-foreground">
                Focus this field for the ai-blue ring
              </span>
              <input
                type="text"
                placeholder="type here"
                className="h-9 rounded-input border border-input bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground"
              />
            </label>

            <div className="flex gap-2">
              <button
                type="button"
                className="h-9 rounded-row bg-primary px-3.5 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90"
              >
                New mission
              </button>
              <button
                type="button"
                className="h-9 rounded-row border border-border px-3.5 text-sm font-medium text-foreground transition-colors hover:bg-accent"
              >
                Open a session
              </button>
            </div>
          </section>
        </div>
      </div>
    </main>
  );
}
