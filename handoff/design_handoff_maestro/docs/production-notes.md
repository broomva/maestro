# Production notes — decisions and known gaps

Things the prototype punts on or that need a decision before/while implementing.

## 1. Build environment (the big one)

The prototype runs React 18 **development** builds + `@babel/standalone` transpiling ~28 JSX files in the browser, sharing scope via `window` globals (`Object.assign(window, {...})` at the end of each file). This is a design rig. Production needs:

- A bundler (Vite or Next). Convert globals → ES module imports; the export lists at the bottom of each file are effectively the module manifests.
- Production React, error boundaries, and real routing (the prototype's `view` state in `MaestroApp.jsx` maps 1:1 to routes: `/`, `/knowledge`, `/history`, `/settings`, `/account`).
- The `?v=N` query strings on script/css tags are manual cache-busting — drop them.

## 2. Service worker / offline

`index.html` currently ships a **kill switch**: it unregisters any service worker and clears `bv-mc-*` caches on every load, because a stale offline shell wedged iteration. Decision recorded: **no service worker until the app is stable.** For production PWA:

- Network-first for HTML, stale-while-revalidate for hashed assets (workbox or equivalent).
- Keep the manifest + icons (`manifest.webmanifest`, `pwa/`); regenerate icons from the vector logo when available.
- Remove the kill switch once a versioned worker ships.

## 3. Icons

- The app does **not** load Lucide from a CDN — `McIcon` (`apps/maestro/WorkData.jsx`) renders inline Lucide path data (stroke 2, round caps, `currentColor`). In production use the `lucide-react` package, **pinned to an exact version** (path data drifts between releases; pin and update deliberately).
- Sizes: 20px standard, 16px inline, 24px empty-state; 1.5px stroke in chips. No other icon libraries, no emoji-as-icons.

## 4. CSS layering — source of truth

Layer order, each composing the one below, never redefining it:

1. `styles.css` → `tokens/*.css` — **the single source of truth** (158 tokens, OKLCH, full dark theme, glass utilities, the Undertow keyframes).
2. `ui_kits/desktop/styles.css` — app-shell base classes (`.bv-app`, `.bv-sidebar`, `.bv-card`, …). Maestro requires it.
3. `apps/maestro/*.css` — surface styles (`styles.css`, `concepts.css`, `mobile.css`, `settings.css`, `feedback.css`, `command.css`).

`concepts.css` (1.7k lines) carries some canvas-exhibit styles alongside shipped ones — audit selectors against the canon map when porting. If any raw color/size value appears in app CSS, replace it with the token.

## 5. Components — compose, don't re-implement

`components/` (core, forms, navigation, overlays, work) are the standard components; each has a `.d.ts` spec and a `.prompt.md` usage doc. The app consumes them via `ds-adapter.jsx` — in production, make them a real package/workspace lib and import directly. Composition rules: RunCard wraps in Undertow while running; WorkState renders only plain voice; Receipt replaces every progress indicator; **glass only on Dialog, Menu, Tooltip, Toast, CommandPalette, and the composer** — everything else matte.

## 6. Asset gaps (open asks for Broomva)

- **Logo is raster** (`assets/broomva-blackhole-logo.png`). Request the SVG — needed for small sizes, favicons, and clean PWA icon regeneration. Don't recolor it; it sits on dark/frosted surfaces and carries its own glow.
- **CalSans ships SemiBold only** (`fonts/CalSans-SemiBold.ttf`; OFL-licensed). The `@font-face` declares 400–700 so it renders at any weight, always semibold-drawn. Marketing/hero only — never app chrome. Add WOFF2 conversion in the build.

## 7. Accessibility

Prototype-level only; production must add:

- Focus management for overlays (dialogs, ⌘K, menus): trap, restore, `Esc`. Focus ring is always ai-blue (`--ring`), never black — already tokenized.
- `prefers-reduced-motion` stops the Undertow/tidepool/typing-bounce — keyframes already guard this; keep the guard when porting.
- Live regions for streaming chat and gate arrivals; the gate queue is the priority surface for screen readers.
- Color is never the only carrier: state dots always pair with the plain-voice label (`WorkState` does this — keep it).
- Hit targets ≥ 44px on mobile surfaces.

## 8. Not designed yet (scope honestly)

- Real auth/session, multi-user presence, workspace sharing (Settings shows a Members section as a surface sketch).
- Error states beyond blocked-worker cards (network failure, transport disconnect, judge failure UI).
- Empty states exist for main planes; audit lesser surfaces (History filters with no results, empty knowledge scopes).
- Notifications (Settings sketches preferences; no delivery design).
