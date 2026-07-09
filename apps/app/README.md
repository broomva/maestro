# @maestro/app

The client SPA — one Vite + React + TypeScript build, three targets: browser, PWA,
Tauri 2 shell (STACK.md §app client). Chat is a projection; the shell never scrolls,
inner panels do.

## M0 (BRO-1782) — scaffold & foundation

Vite + React + TS + Tailwind v4 (`@tailwindcss/vite`) + shadcn base + TanStack Router
(code-based) + lucide-react. Design tokens are consumed as `@maestro/tokens` — never
copied (CLAUDE.md canon rule). Theme switches on `data-theme` (light default, dark via
attribute) with a no-flash head script keyed on `localStorage('bv-theme')`.

```bash
bun run --filter @maestro/app dev        # dev server
bun run --filter @maestro/app build      # builds tokens → dist, then vite build
bun run --filter @maestro/app typecheck  # DOM/JSX tsconfig (own lib, not the root no-DOM one)
bun run --filter @maestro/app test:m0    # bun static token checks + playwright light/dark smoke
```

`test:m0` needs a Chromium once: `bunx playwright install chromium`.

## Layout

```
index.html            #root + the no-flash theme head script
src/main.tsx          React entry → RouterProvider
src/router.tsx        TanStack Router (code-based, one index route)
src/routes/landing.tsx the M0 foundation page (matte; proves tokens/theme/type/focus)
src/components/        theme-toggle (M2 moves it to the top bar)
src/styles/globals.css the token wiring: styles.css → tailwindcss → theme.css (§1 order)
src/theme.ts          resolveInitialTheme / get / set / toggle (data-theme + localStorage)
src/lib/utils.ts       shadcn `cn` base
src/m0.test.ts        static §6 invariants (bun:test — runs in CI quality)
tests/m0.pw.ts        rendered §6 checks + light/dark screenshots (playwright)
```

Milestones M1+ (primitives, shell, board, chat, inspector) follow `BUILD-PLAN.md`.
