# Tokens → Tailwind v4 integration

The design tokens are **already production-shaped**. `design-system/tokens/colors.css` defines the raw `--bv-*` primitives *and* maps them onto the shadcn semantic layer (`--primary`, `--ring`, `--muted-foreground`, `--sidebar`, …) for light and a fully-specified dark theme. The expensive part of the handoff is done. This file is the packaging step.

## 1. Drop the tokens in

Copy `design-system/tokens/` and `design-system/styles.css` into the app (e.g. `src/styles/broomva/`). `styles.css` is a single `@import` chain that pulls in `colors`, `typography`, `spacing`, `glass`, `motion`, `base`. Import it once, before Tailwind's own layers, from your root CSS:

```css
/* app/globals.css */
@import "../styles/broomva/styles.css";   /* tokens + base element styles */
@import "tailwindcss";
```

`base.css` already sets body type/color, heading scale, the **ai-blue `:focus-visible` ring**, link color, code styling, and cool scrollbars. Don't re-declare these in Tailwind's preflight — let the tokens own them.

> Two things to decide on import: (a) `typography.css` declares `@font-face` for CalSans with a relative `url("../fonts/…")` — keep that path correct in the Vite build (or inline the face; see §4). (b) `base.css` styles bare `html, body, h1, a, code` — if Tailwind preflight resets fight it, load the tokens **after** `@import "tailwindcss"` instead, and verify headings/links still resolve.

## 2. Theme switching

Light is the `:root` default. Dark is `[data-theme="dark"]` (see `colors.css`). Set `data-theme` on `<html>`:

```tsx
// keep it server-consistent to avoid a flash; a tiny inline script in <head>
// reading localStorage('bv-theme') and setting documentElement.dataset.theme works.
<html data-theme={theme}>
```

Do **not** use Tailwind's default `.dark` class strategy — the tokens key on the `data-theme` attribute. If you prefer `next-themes`, configure it with `attribute="data-theme"`.

## 3. Expose tokens to Tailwind utilities (`@theme inline`)

The semantic vars already exist; this block just lets you write `bg-background`, `text-muted-foreground`, `border-border`, `bg-sidebar`, etc. Add to `globals.css` **after** the imports:

```css
@theme inline {
  /* surfaces */
  --color-background: var(--background);
  --color-foreground: var(--foreground);
  --color-card: var(--card);
  --color-card-foreground: var(--card-foreground);
  --color-primary: var(--primary);
  --color-primary-foreground: var(--primary-foreground);
  --color-secondary: var(--secondary);
  --color-secondary-foreground: var(--secondary-foreground);
  --color-muted: var(--muted);
  --color-muted-foreground: var(--muted-foreground);
  --color-accent: var(--accent);
  --color-accent-foreground: var(--accent-foreground);
  --color-border: var(--border);
  --color-input: var(--input);
  --color-ring: var(--ring);
  --color-sidebar: var(--sidebar);

  /* the one accent — for status/glow/links, used sparingly */
  --color-blue: var(--bv-blue);

  /* radii ladder (Houston, unchanged) */
  --radius-chip: var(--bv-radius-chip);     /* 0.25rem */
  --radius-input: var(--bv-radius-input);   /* 0.375rem */
  --radius-row: var(--bv-radius-row);       /* 0.5rem */
  --radius-card: var(--bv-radius-xl);       /* 0.75rem */
  --radius-dialog: var(--bv-radius-2xl);    /* 1rem */
  --radius-composer: var(--bv-radius-composer); /* 28px */

  /* type scale */
  --text-xs: var(--bv-text-xs);   /* 12 */
  --text-sm: var(--bv-text-sm);   /* 14 */
  --text-base: var(--bv-text-base); /* 16 */
  --text-lg: var(--bv-text-lg);   /* 18 */
  --text-xl: var(--bv-text-xl);   /* 22 */
  --text-2xl: var(--bv-text-2xl); /* 24 */
  --text-h1: var(--bv-text-h1);   /* 28 */

  --font-sans: var(--bv-font-sans);
  --font-mono: var(--bv-font-mono);
  --font-display: var(--bv-font-display);
}
```

> Confirm the exact `--bv-radius-*` and `--bv-text-*` names against `design-system/tokens/spacing.css` and `typography.css` before pasting — they're the source of truth. The type scale is **closed**: 12/14/16/18/22/24/28. Don't add Tailwind's default `text-3xl`+ or a `text-[13px]` one-off.

## 4. Fonts

- **System stack is the default and owns app chrome.** It's already in `--bv-font-sans`. Nothing to install.
- **CalSans is display-only** (hero/marketing headings, opt in with `data-display-font="calsans"` on a surface root). Ship `design-system/fonts/CalSans-SemiBold.ttf` (add a WOFF2 in the build) through the Vite asset pipeline and keep the `@font-face` in `typography.css` pointing at it. CalSans ships SemiBold only — the face declares a 400–700 range so it renders at any weight, but it's always semibold-drawn.

## 5. Glass utilities

`glass.css` ships `.bv-glass`, `.bv-glass-heavy`, `.bv-glass-composer`, `.bv-scrim`. Keep them as plain classes — don't reimplement as Tailwind utilities, because each one bundles the **inner light line** (`inset 0 1px 0 var(--glass-light-line)`) that defines "glass" here. Apply them to overlays, popovers, and the composer **only**.

## 6. Sanity checks after wiring

- `bg-background text-foreground` renders white/barely-blue-ink in light, deep blue-purple in dark.
- A focused input shows the **ai-blue** ring (not black, not Tailwind's default).
- `border-border` is a whisper, not a solid line.
- No element outside overlays/popovers/composer has `backdrop-filter`.
- Headings are regular-weight (400) except h2 specimens; nothing is Title Case or uppercase.
