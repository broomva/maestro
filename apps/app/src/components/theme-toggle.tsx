import { Moon, Sun } from "lucide-react";
import { useThemeState } from "../theme";

/**
 * The theme toggle (TOKENS-INTEGRATION §2). Hover lightens via the frosted-blue
 * accent — never scale (CLAUDE.md motion rule). In M2 this presence moves to the
 * top bar next to the orchestrator chip; here it proves theme switching works.
 *
 * Uses the shared reactive theme state so it stays in sync with the Settings >
 * Appearance segmented (either control reflects a change made by the other).
 */
export function ThemeToggle() {
  const [theme, apply] = useThemeState();

  return (
    <button
      type="button"
      aria-label={theme === "dark" ? "Switch to light" : "Switch to dark"}
      onClick={() => apply(theme === "dark" ? "light" : "dark")}
      className="grid h-9 w-9 place-items-center rounded-row text-muted-foreground transition-colors motion-reduce:transition-none hover:bg-accent hover:text-foreground"
    >
      {theme === "dark" ? <Sun size={20} strokeWidth={2} /> : <Moon size={20} strokeWidth={2} />}
    </button>
  );
}
