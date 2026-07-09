# Broomva Design System

**Houston's calm, monochrome philosophy on an Arcan-blue axis.** Broomva takes the Houston design system as base and scaffold — its restraint, its tight scales, its plain-language voice — and recasts the color foundation: every "black" becomes a barely-perceptible dark blue (the Arcan ink), and depth cues (shadows, hovers, the composer halo, focus rings) carry subtle tones of frosted liquid-glass blue.

The result reads as Houston until you look closely. Then you notice everything is faintly, coolly blue.

---

## Sources

This system was synthesized from two parent systems. They are the ground truth for anything underspecified here:

- **Houston Design System** — the base and scaffold. Tokens, type scale, radii ladder, spacing, layout rules, voice, component shapes all inherit from it. ([gethouston.ai](https://gethouston.ai), [`broomva/houston`](https://github.com/broomva/houston))
- **Arcan Glass** — the flavor. The dark-blue ink, the OKLCH blue axis (surfaces hue ~265–275, ai-blue hue 260, accent-blue hue 235), the glass treatment, the inner light line, the glow shadows. (Distilled from [`broomva/broomva.tech`](https://github.com/broomva/broomva.tech), `apps/chat/app/globals.css`)

## What Broomva is (product context)

Same shape as Houston: a **chat-first AI agent product**. Workspaces and agents; each board card is a conversation; chat is the primary interface and everything else is a side-panel. The shell never scrolls; inner panels scroll.

---

## The system in one paragraph

Light-first. White canvas, cool-gray sidebar, dark-blue ink `oklch(0.175 0.022 265)` that reads as black until you look. Monochrome by default — color only carries signal. The one accent is **ai-blue** `oklch(0.60 0.12 260)`: it owns focus rings, hover frost, the composer's halo, and the running-card glow. Glass is earned, not ambient: only overlays, popovers, and the composer get `backdrop-filter`; everything else is matte. System fonts, sentence case, no emoji, no em dashes. Dark mode is fully specified and Arcan-deep: blue-purple canvas `oklch(0.135 0.02 272)`, never pure black.

## Decision log (what came from where)

| Decision | Verdict | Source |
|---|---|---|
| Default theme | Light, dark fully specified | Houston light-first; Arcan supplies the dark palette |
| "Black" | `--bv-ink` oklch(0.175 0.022 265) — barely-blue | User: "reads as black until you look" |
| Grays | Houston's ladder, cooled to hue 265 at whisper chroma | Hybrid |
| Primary buttons | Ink fill (dark blue, not black), pill shape | Houston shape, Arcan ink |
| Hover/selected | Frosted blue tint `--bv-frost-8`, not gray | Arcan |
| Focus ring | ai-blue, not black | Arcan |
| Shadows | Houston's two-tier system, every shadow tinted blue | Hybrid |
| Glass | Overlays + composer + popovers ONLY; cards/panels/chrome stay matte | User: "subtle" |
| Inner light line | `inset 0 1px 0` on every glass surface | Arcan signature |
| Type scale | 12/14/16/18/22/24/28, system fonts | Houston |
| Display face | CalSans, opt-in via `[data-display-font="calsans"]` — hero/marketing only | Arcan, demoted to optional |
| Casing | Sentence case everywhere; no uppercase eyebrows | Houston rule wins (user confirmed) |
| Radii ladder | chips 4 → inputs 6 → rows 8 → cards 12 → dialogs 16 → composer 28 → pills full | Houston verbatim |
| Running-card glow | Conic sweep, recolored blue → indigo → cyan → ice (no orange/yellow) | Houston motion, Arcan palette |
| Voice | Plain-language, second person, lead with the verb | Houston verbatim |
| Icons | Lucide only, stroke, currentColor | Houston |

---

## Content fundamentals

Inherited from Houston wholesale. The voice is the **Mom-test, plain-language, capable assistant**:

- **Plain over technical.** "Needs you" not "In Review". "New mission" not "Create task".
- **Second person, never first.** The product speaks to *you*.
- **Short, declarative sentences. Lead with the verb.** "Connect both apps, then come back."
- **No filler.** No "Welcome!", no "Let's get started!", no celebration.
- **Sentence case everywhere.** Never Title Case Buttons. Never UPPERCASE LABELS. Never wide letterspacing — Arcan's uppercase eyebrow labels are explicitly **dropped** in Broomva.
- **No em dashes** in user-facing copy. No marketing superlatives.
- **Emoji are banned** from UI chrome. Avatars may render Unicode a user typed, as data, never as decoration.
- Bold real-world nouns ("Reply **Google** or **Microsoft**"), not UI elements.

Vibe: quiet, capable, calm. The agent has done a thousand of these.

## Visual foundations

### Color
- **Monochrome by default**, on a cool axis. Whites, cool grays (hue 265, chroma ≤ 0.01), dark-blue ink. Never pure `#000`; never pure-white text in dark mode.
- **Color earns its place.** It appears in exactly five situations: (1) the running-card conic glow, (2) status pills, (3) agent avatar accents, (4) inline assistant link pills, (5) the frosted-blue interaction layer (hover, selected, focus, glow shadows).
- **One hue family.** ai-blue (260) everywhere; accent-blue (235) only when two accents must coexist. No decorative color, no gradient washes, no colored left-border strips.
- **All colors are OKLCH.** Tint with `color-mix(in oklab, …)` or alpha; never invent hexes.

### Backgrounds
- Flat white canvas; cool-gray `--bv-canvas-soft` sidebar. No imagery in chrome, no textures, no grain, no gradient washes.
- Dark mode: deep blue-purple `oklch(0.135 0.02 272)` canvas, sidebar one step deeper. Atmospheric orbs (Arcan's huge blurred circles) are allowed **only** on marketing/hero surfaces, never in app chrome.

### Borders
- Whisper-soft, blue-black: `--bv-border-5` for dividers, `--bv-border-15` for edges that must be seen, `--bv-border-25` for emphasis only. In dark mode borders are translucent cool-white. A solid opaque border looks wrong in this system.

### Shadows — two tiers, all blue-tinted
1. **Edge** — `--bv-shadow-edge` (1px, 6% blue-black) for cards and rows at rest. The default depth cue.
2. **The composer halo** — `--bv-shadow-composer`, the single dramatic depth cue: a wide 80px frosted-blue bloom + tight 1px ring. Used on the composer only.
- Hover cards lift to `--bv-shadow-card-hover` (16px diffuse, faintly blue). Active surfaces may add `--bv-shadow-glow`.
- No inner shadows except the glass light line. No dense single-layer shadows.

### Glass (the Broomva rule)
Glass appears in **exactly three places**: overlays (dialogs, command palette), popovers (menus, dropdowns, tooltips), and the composer. Utilities: `.bv-glass`, `.bv-glass-heavy`, `.bv-glass-composer`, scrim `.bv-scrim`. Every glass surface carries the **inner light line** `inset 0 1px 0 var(--glass-light-line)` — that's what makes it glass and not a gray panel. Cards, boards, sidebars, headers: matte, always.

### Corner radii
Houston's deliberate ladder, unchanged: chips `0.25rem` → inputs `0.375rem` → icon buttons/rows `0.5rem` → cards `0.75rem` → dialogs `1rem` → composer `28px` → pills full. Pill radius is reserved for buttons and avatars — never cards.

### Typography
- System stack default; regular 400 default weight; medium 500 for buttons/section headers; semibold 600 reserved for empty-state titles.
- Tight scale: 12 / 14 / 16 / 18 / 22 / 24 / 28. Nothing else.
- CalSans (`--bv-font-display`) is opt-in for hero/marketing headings only — set `data-display-font="calsans"` on the surface root. App chrome never uses it.

### Motion
- Interaction feedback under 300ms; 200ms common, `--bv-ease-standard`. Enter `opacity:0, y:8 → 1, 0`; exit `y:-8`.
- Morph transitions (panel resize, theme change) 500ms `--bv-ease-morph`.
- Three signature animations: `card-running-glow` (2.5s conic sweep, blue→indigo→cyan→ice), `typing-bounce` (3 staggered dots), `tool-pulse`.
- No bouncy entrances. Spring is for layout reorder, never for flourish.

### Hover / pressed states
- **Hover = frosted blue fill** (`--bv-frost-8`) on ghost/soft elements; one-step lighten (`--bv-ink-hover`) on primary buttons; shadow lift on cards. No scale, no rotation.
- **Pressed = one step deeper frost** (`--bv-frost-12`). Buttons stay still.
- No hover-only affordances.

### Cards
- Matte white (light) / `--card` (dark), `--bv-radius-xl`, whisper border, edge shadow at rest, diffuse blue-tinted shadow on hover.
- Running state swaps the border for the blue conic `card-running-glow`.

### Layout
- 200px fixed sidebar + 52px header + flex main. Chat column max 768px. Right panel ~45% viewport, 380px min. 4px spacing base. The shell never scrolls.

## Iconography

- **Lucide only** (CDN: `https://unpkg.com/lucide@latest`). 20px standard, 16px inline, 24px empty-state. Stroke 2px (1.5px in chips), round caps, `currentColor`. No fill icons, no mixed libraries, no emoji-as-icons.
- **Brand mark:** the Broomva blackhole glyph — `assets/broomva-blackhole-logo.png`. Use on a dark or frosted surface; it carries its own glow. Don't recolor it.

---

## Index

| Path | Purpose |
|---|---|
| `styles.css` | Root entry — `@import`s every token file. Link this one file. |
| `tokens/colors.css` | Ink, grays, accents, frost, borders, status, shadows, semantic tokens, dark theme. |
| `tokens/typography.css` | Font stacks (+ CalSans `@font-face`), scale, weights. |
| `tokens/spacing.css` | Radii ladder, 4px spacing, heights, motion. |
| `tokens/glass.css` | Glass tokens + `.bv-glass*` utilities + scrim. |
| `tokens/base.css` | Element defaults: body, headings, links, code, focus ring, scrollbars. |
| `assets/` | Broomva blackhole logo. |
| `fonts/` | CalSans SemiBold. |
| `components/core/` | Reusable primitives: Button, IconButton, Input, Card, StatusBadge, Avatar, Composer. |
| `guidelines/` | Foundation specimen cards (populate the Design System tab). |
| `ui_kits/desktop/` | Full desktop shell demo: sidebar + mission board + chat + composer, light/dark, tweakable. |
| `SKILL.md` | Agent-facing manifest. |

## Caveats

- CalSans ships in SemiBold only; the `@font-face` declares a 400–700 range so it renders at any weight, but it is always semibold-drawn.
- The blackhole logo is a raster PNG (from the Arcan project). A vector version would help for small sizes — ask Broomva for the SVG.
- Status colors are OKLCH re-derivations of Houston's hexes, harmonized to the cool axis.
