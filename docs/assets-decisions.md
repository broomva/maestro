# Asset intake — decisions (BRO-1797)

The pinned decisions for brand assets, icons, and PWA icons. This file is the record; the
`check:icons` audit is the machine enforcement of the icon rules, and as of BRO-1766 it **is wired
into the required CI `quality` job** (`bun run check:icons`, scope=all) — so a PR that adds a second
icon library or a non-canon glyph fails the gate on every PR. (BRO-1834 is a separate, later
`--strict` governance gate; it is not what enforces the icon rules.)

## Blackhole logo (brand mark)

- **No true-SVG asset yet — inline-SVG stand-in today.** The prototype's blackhole mark is a
  raster tile, and an opaque `#000` raster on a light surface reads as a canon violation (see the
  M2 shell fix, BRO-1771), so the app currently renders the brand mark as an **inline SVG on a
  cool-axis `--bv-ink` chip** rather than the raster.
- **Requested from Broomva:** the blackhole logo as a **true SVG** (single-path, `currentColor`
  or a defined OKLCH token, transparent background). This is an **external dependency and must
  not block any work** (loop note on BRO-1797) — the inline-SVG chip stands in until it arrives.
- **Fallback plan (if only PNG is ever delivered):** PNG at fixed sizes only (16 / 32 / 180 /
  192 / 512), **no runtime recolor**, and only on a **dark or frosted surface** (never on the
  near-white sidebar) — a raster mark cannot adopt `currentColor`, so it must never sit where the
  theme would demand a tint it can't provide.

## Icon strategy (production-notes §3, CLAUDE.md §Icons)

- **One library: `lucide-react`, pinned EXACT `0.469.0`** — no caret, no float, so the whole
  workspace draws from one icon set (icon drift across minor bumps is a silent visual regression).
  - `apps/app/package.json` pins the plain exact spec `"lucide-react": "0.469.0"` (no `^`), the
    same idiomatic form `packages/ui` uses — not the `npm:lucide-react@…` self-alias (that alias
    was only ever a way to satisfy the ticket's `grep 'lucide-react@'` acceptance proxy, which is
    imprecise: it does not match a plain exact pin. The real invariant is "exact pin, no range,"
    and the effective check is `grep -q '"lucide-react": "0.469.0"'` — satisfied by both packages).
  - `packages/ui/package.json` pins `0.469.0` in both `peerDependencies` and `devDependencies`.
- **Conventions** (design canon): sizes **20 (standard) / 16 (inline) / 24 (empty-state)**,
  **stroke 2**, **round caps**, **`currentColor`**. No fill icons, no mixed libraries, no
  emoji-as-icons.
- **Custom-drawn UI glyphs** (anything not in Lucide) live as **local SVG components in
  `packages/ui/src/icons/`** — never a second icon package — and must follow the stroke-icon canon
  (currentColor, stroke-width 2, round caps, no hard-coded fill). BRO-1766 did the port: the app
  source was already Lucide-only, so the only inline custom glyph was the blackhole **brand mark**.
- **Brand marks are NOT UI glyphs.** The blackhole has a filled singularity in a fixed brand color
  on a fixed dark chip, so it cannot follow the stroke-icon canon. BRO-1766 extracted it to
  `packages/ui/src/brand.tsx` as `BlackholeMark` (drawn in `currentColor` so the surface sets the
  color) — **outside** the audited `packages/ui/src/icons/` dir on purpose. `icons/` is reserved
  for canon-conforming UI glyphs; logos live in `brand.tsx`.
- **Enforcement: `check:icons`** — one shared audit at **`scripts/check-icons.ts`** (moved to the
  repo root in BRO-1766; scope-aware, so no per-package duplication). Two hard checks: (1) no import
  from any icon library other than lucide-react; (2) every glyph under `packages/ui/src/icons/`
  draws with `currentColor` + `stroke-width="2"` + round caps and no hard-coded fill/stroke color
  (dormant until a custom UI glyph lands there). Entry points:
  - **`bun run check:icons`** (repo root) → `--scope all`, scans app + ui + the glyph dir. **This
    is the form wired into CI.**
  - **`bun run --filter @maestro/app check:icons`** → `--scope app` (apps/app/src only).
  - **`bun run --filter @maestro/ui check:icons`** → `--scope ui` (packages/ui/src + the glyph dir).

  bun's `--filter` matches the package NAME (`@maestro/app` / `@maestro/ui`), so the bare
  `--filter app` / `--filter ui` in the tickets' done.checks does not match — it errors
  `No packages matched the filter` and exits **non-zero** (the BRO-1782 gotcha). Use the qualified
  names above, or just the root `bun run check:icons`.

## PWA icons

- **Keep** `manifest.webmanifest` + `pwa/` icons in `apps/app` as the app grows — but the icon
  files themselves depend on the real logo asset (above), so they land **with** that asset, not
  before (placeholder raster icons would re-introduce the opaque-`#000`-on-light hazard).
- **Service worker is explicitly DEFERRED** to the P6 PWA ticket — the install/offline kill
  switch stays off until then. This ticket does not add or enable a service worker.
