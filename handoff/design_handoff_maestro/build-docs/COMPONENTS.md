# Component specs

Eight primitives carry the whole system. Each one already ships a **typed contract** (`design-system/components/core/<Name>.d.ts`) and a **usage note with voice rules** (`<Name>.prompt.md`). The `.jsx` next to them is a working prototype (Babel-runtime) — read it for exact markup and class structure, but **re-author each as a real app primitive** (shadcn base + CVA variants), don't ship the prototype.

Build order: Button → IconButton → Input → Avatar → StatusBadge → DotComet → Card → Composer. (DotComet and Card depend on `LIVE-SIGNALS.md`.)

The token names below resolve in `design-system/tokens/`. Everything maps through the semantic layer where possible (`--primary`, `--muted`, `--ring`) so light/dark come free.

---

## Button — `Button.d.ts` · `Button.prompt.md`
Pill-shaped action button. The one primary action per view gets `primary`; everything else is secondary/soft/ghost.

- **Props:** `variant: "primary" | "secondary" | "soft" | "ghost"` (default primary) · `size: "sm" | "md" | "lg"` → **28 / 36 / 44px** tall (default md) · standard button attrs.
- **Shape:** `border-radius: full` (pill — allowed here and on avatars only). Horizontal padding scales with size; label is `--bv-text-sm` (14), weight 500.
- **Variants:**
  - `primary` — fill `--primary` (the ink, dark-blue not black), text `--primary-foreground`. **Hover lightens one step** (`--bv-ink-hover`); no frost, no lift.
  - `secondary` — `--card` bg + 16% border (`--bv-border-15`), text `--foreground`. Hover → frosted blue `--bv-frost-8`.
  - `soft` — `--bv-frost-8` bg, text `--foreground`. Hover → `--bv-frost-12`.
  - `ghost` — transparent, text `--foreground`. Hover → `--bv-frost-8`.
- **Pressed:** one step deeper frost (`--bv-frost-12`). Button stays still — no translate, no scale.
- **Focus:** inherits the global ai-blue `:focus-visible` ring — don't add one.
- **Copy:** sentence case, lead with the verb ("New mission", "Connect"). Never Title Case, never emoji.

## IconButton — `IconButton.d.ts` · `IconButton.prompt.md`
36px square ghost button holding one 20px Lucide icon. Toolbars, card row actions, composer attachments.

- **Props:** `label: string` **(required** — becomes `aria-label` + `title`) · `children` = the icon.
- **Style:** 36×36, radius `--bv-radius-row` (0.5rem), transparent, icon `currentColor` stroke 2. Hover → frosted blue `--bv-frost-8`. No scale.

## Input — `Input.d.ts` · `Input.prompt.md`
Single-line text input for forms and settings.

- **Style:** 36px tall, radius `--bv-radius-input` (0.375rem), 1px `--bv-gray-200` edge (`--input`), bg `--card`/`--background`.
- **Focus:** ai-blue ring is handled globally by `:focus-visible` — **do not add your own.**
- **Placeholders:** sentence-case nouns ("Prompt"), not instructions.

## Avatar — `Avatar.d.ts` · `Avatar.prompt.md`
Circular avatar — initials over an accent, or an image.

- **Props:** `name?` (initials derived) · `color?` (accent fill, default `--bv-blue`) · `size?` px (default 22) · `src?` (image replaces initials).
- **Style:** full radius (allowed here). Initials in `--primary-foreground` over the accent; agent accents come from the user's pick, default ai-blue. May render a Unicode char a user typed **as data**, never as decoration.

## StatusBadge — `StatusBadge.d.ts` · `StatusBadge.prompt.md`
Status pill for work state. **The dot carries the color; the capsule stays gray.**

- **Props:** `status: "success" | "info" | "warning" | "danger" | "neutral"` (default info) · `pulse?` (pulses the dot) · `children` = label.
- **Style:** soft gray capsule (`--muted`), `--bv-text-xs`, colored dot left of a sentence-case label. Dot color from the status tokens (`--bv-success` / `--bv-info` / `--bv-warning` / `--bv-blue-accent` for "Needs you", accent-blue 235 / neutral gray) (**D-COLOR**). Status colors are the **only** non-blue hues allowed in chrome.
- **Copy:** plain language — "Needs you", not "In Review"; "Running", "Stuck", "Queued", "Done", "Standing".
- For *running* work, use `DotComet` as the dot (see below) rather than a static pulse.

## DotComet — `DotComet.d.ts` · `DotComet.prompt.md`
The **tidepool dot** — the Undertow miniaturized: blue→ice weather drifting inside a 15px circle. The running signal at dot scale (list rows, status lines, the orchestrator's presence chip).

- **Props:** `size?` px (default 15) · `color?` core color (default `--bv-info`).
- **Implementation:** renders `.bv-dot-live` from `tokens/motion.css` — see `LIVE-SIGNALS.md`. Blue → ice only; stops under reduced motion. **Use only for running/live state** — never as a generic dot.

## Card — `Card.d.ts` · `Card.prompt.md`
Matte content card — work items, board cards, settings groups, integration rows. **Never glass, never pill-radius.**

- **Props:** `interactive?` (blue-tinted hover lift) · `running?` (wraps the card in the **Undertow** halo).
- **Style:** matte `--card`, radius `--bv-radius-xl` (0.75rem), whisper border, edge shadow at rest. `interactive` → diffuse blue-tinted shadow on hover (`--bv-shadow-card-hover`), no scale.
- **Running:** wrap in the Undertow (`.bv-undertow` + `.bv-undertow-orbit`) — the card itself stays matte; the halo is the signal. The old border comet is **retired**. Pair with a `DotComet` on the status row. See `LIVE-SIGNALS.md`.

## Composer — `Composer.d.ts` · `Composer.prompt.md`
The chat composer — **the one place glass and dramatic depth are allowed.** Bottom of any chat surface.

- **Props:** `placeholder?` (default "Message Maestro" — keep as "Message <agent>") (**D-NAME**) · controlled `value?`/`onChange?` · `onSend?(text)` fired on Enter or send click (trimmed) · `leading?` (e.g. an attach IconButton).
- **Style:** `.bv-glass-composer` — `rounded-[28px]` glass capsule + the signature frosted-blue **halo** (`--bv-shadow-composer`) + inner light line. This is the single dramatic depth cue in the product; nothing else gets this shadow.

---

### Cross-cutting
- Re-create with **CVA** so variants are typed and exhaustive; keep prop names identical to the `.d.ts` so the contracts stay the source of truth.
- All interactive states: hover = frosted blue or one-step lighten; pressed = deeper frost; focus = global ai-blue ring. **No scale, rotate, or glow on hover.**
- Verify every primitive against the Maestro prototype (`apps/maestro/`, served from the design-project root) — canonical usage per `design_handoff_maestro/docs/canon-map.md`.
