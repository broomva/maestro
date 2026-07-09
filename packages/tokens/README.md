# @maestro/tokens

The Broomva design tokens, **consumed as a package — never copied** (CLAUDE.md
canon rule). The token canon lives in the vendored handoff
(`handoff/design_handoff_maestro/build-docs/design-system/`); this package
re-publishes it into `dist/` and guards it against drift.

## Build

```bash
bun run --filter @maestro/tokens build       # handoff → dist/ (css + WOFF2 + theme.css + manifest.json)
bun run --filter @maestro/tokens check:sync   # verify handoff matches tokens.lock.json (drift tripwire)
bun run --filter @maestro/tokens sync:lock    # re-pin after an *intentional* handoff token change
```

`build` produces (all gitignored):

```
dist/styles.css + dist/tokens/*.css       the token entry (@import chain: colors, typography, spacing, glass, motion, base)
dist/fonts/CalSans-SemiBold.{ttf,woff2}   display font (TTF + built WOFF2)
dist/theme.css                            the `@theme inline` block, from the manifest
dist/manifest.json                        the machine-readable token-name manifest
```

## Wiring into an app (TOKENS-INTEGRATION §1/§3)

```css
@import "@maestro/tokens/styles.css";   /* tokens + base — before Tailwind */
@import "tailwindcss";
@import "@maestro/tokens/theme.css";    /* @theme inline map — after Tailwind */
```

Theme switching keys on `data-theme="dark"` on `<html>` (not Tailwind's `.dark`).
CalSans is display-only — opt in per surface with `[data-display-font="calsans"]`.

## Drift check

The handoff is the single source of truth. `tokens.lock.json` pins the SHA-256 of
every handoff token file; `check:sync` fails if the canon changed without a re-pin,
so a token edit is a reviewed event, not a silent fork. The `@theme` name map lives
in `src/manifest.ts` (`THEME_TOKENS`) — verified against the source var names
(`--bv-radius-md`/`-lg`, not the TOKENS-INTEGRATION §3 example's
`--bv-radius-input`/`-row`; §3 line 81 says the source wins).
