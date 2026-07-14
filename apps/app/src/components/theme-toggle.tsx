import { Moon, Sun } from "lucide-react";
import { useState } from "react";
import { getTheme, type Theme, toggleTheme } from "../theme";

/**
 * The theme toggle (TOKENS-INTEGRATION §2). Hover lightens via the frosted-blue
 * accent — never scale (CLAUDE.md motion rule). In M2 this presence moves to the
 * top bar next to the orchestrator chip; here it proves theme switching works.
 */
export function ThemeToggle() {
  const [theme, setThemeState] = useState<Theme>(() =>
    typeof document === "undefined" ? "light" : getTheme(),
  );

  return (
    <button
      type="button"
      aria-label={theme === "dark" ? "Switch to light" : "Switch to dark"}
      onClick={() => setThemeState(toggleTheme())}
      className="grid h-9 w-9 place-items-center rounded-row text-muted-foreground transition-colors motion-reduce:transition-none hover:bg-accent hover:text-foreground"
    >
      {theme === "dark" ? <Sun size={20} strokeWidth={2} /> : <Moon size={20} strokeWidth={2} />}
    </button>
  );
}
