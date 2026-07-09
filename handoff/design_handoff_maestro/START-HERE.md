# START HERE — Broomva Maestro handoff

You're building **Maestro**, Broomva's work-orchestration agent. This folder plus the
project around it is the complete handoff: a running design prototype, the full design
system, the build plan, and the contracts that join them.

There is a lot of documentation. This page is the map. Read it first, then follow the
route for the job you're doing. **Do not read everything front to back** — each doc owns
one concern; the rest reference it.

---

## 1. The one-paragraph brief

The scarce resource is **unsupervised hours**. Work is the noun (folders with frontmatter
contracts), sessions are the verb, chat is a projection. A 24/7 runtime schedules agent
sessions against a filesystem workspace; the filesystem is truth and a derived index makes
it queryable. The human's one verb is **the gate** — approvals on work that lands at
"Needs you." Calm, light-first, barely-blue monochrome; color earns its place; glass is
earned; show receipts, never percentages.

---

## 2. Canon — who owns what

Documentation drifts when two files describe the same thing. Each concern below has **one
owner**. If another doc touches it, that doc is wrong and defers to the owner. When you
change something, change it in the owner and nowhere else.

| Concern | Single source of truth |
|---|---|
| **Visual language** (color, type, glass, motion, voice) | `../readme.md` + `build-docs/CLAUDE.md` (the always-on ruleset) |
| **Design tokens** (the 158 values) | `../styles.css` + `../tokens/*` — consume as a package, never copy values |
| **Pixel & interaction target** | the running prototype `../apps/maestro/` (`index.html`) |
| **Which prototype exports are real** | `docs/canon-map.md` |
| **Data shapes, state machine, wire protocol** | `docs/data-contract.md` |
| **System topology & stack** | `build-docs/ARCHITECTURE.md` + `build-docs/STACK.md` |
| **Build sequence & exit tests** | `build-docs/ROADMAP.md` (backend/infra) + `build-docs/BUILD-PLAN.md` (UI) |

---

## 3. The two READMEs — which lens you're in

There are two entry docs below this one. They are **two lenses on one product**, not two
products. Pick by what you're doing:

- **`README.md`** — the *design* lens. The prototype and design system as the pixel/behavior
  reference to recreate. Read this to understand the screens, interactions, and what fidelity
  means. Start here if you are building UI.
- **`build-docs/README.md`** — the *build* lens. The greenfield build bundle for a Claude Code
  session: architecture, autonomy loops, data model, flows, API, phased roadmap. Its
  `CLAUDE.md` is meant to sit at the new repo's root. Start here if you are standing up the
  system.

They agree on canon (§2). Where they seem to differ, §2 wins.

---

## 4. The build spine — do it in this order

Sequenced so each phase is verifiable before the next depends on it. Don't parallelize past
a broken foundation.

1. **Foundations** — publish tokens as a real dependency (`build-docs/TOKENS-INTEGRATION.md`),
   then the eight core primitives (`build-docs/COMPONENTS.md`, contracts in
   `../components/core/*.d.ts`). Exit: primitives match the specimen pages in `../guidelines/`.
2. **App shell** — the non-scrolling chrome (200px sidebar · 52px top bar · flex main), light
   and dark. Exit: shell matches `../apps/maestro/` empty state; only inner panels scroll.
3. **One vertical slice** — the gate flow end to end: work item → running (with the Undertow,
   `build-docs/LIVE-SIGNALS.md`) → gate card → Approve. This exercises the store, the
   `ChatTransport` seam, and live signals at once, so it de-risks everything after it.
   Contracts in `docs/data-contract.md`; flow spec in `build-docs/FLOWS.md`. Exit: a seeded
   run reaches "Needs you" and Approve resolves it.
4. **Remaining screens** off that proven spine — Work detail, Knowledge, History, Settings,
   Account, palette, mobile shell. Each mapped in `README.md` §Screens and `docs/canon-map.md`.

Backend/autonomy phases (runtime, harness, verifier, orchestrator) run in parallel per
`build-docs/ROADMAP.md` (P0–P6) with deep specs in `build-docs/specs/`.

---

## 5. The seams — agree these before coding

The risky joints. Get the contract right here and the rest is mechanical:

- **`ChatTransport`** — the prototype ships mock transports (`apps/maestro/AiProtocol.jsx`) a
  real backend replaces **1:1**. Wire protocol in `docs/data-contract.md` + `build-docs/API.md`.
- **The canonical work-item store** — one shape, everything derives from it. `docs/data-contract.md`.
- **The gate queue** — derived from work in `review`/`blocked`, ordered attention-first. Not a
  separate store.
- **FS-as-truth + derived index** — the filesystem is canon; the index is rebuildable.
  `build-docs/DATA-MODEL.md`.

---

## 6. What to drop — don't port the scaffolding

The prototype is a design-iteration rig. These are **not** product and should not be recreated:

- The in-browser Babel/React-dev rig — re-author as real modules + production React.
- The `components/core/*.jsx` files as-is — read them for markup, rebuild against the `.d.ts`.
- The **Tweaks panel** knobs (`MaestroApp.jsx`) — design-review controls. Exceptions: **density**
  and **theme** map to Settings → Appearance.
- **Chat-length** selectors (short/stress/extreme) — seed-transcript stress-testing only.
- All **seed data** — shapes are canon (see data contract), values are demo content.
- The old `reference/desktop` kit — already removed; it drifted. The prototype is the target.

---

## 7. Fidelity acceptance

"Matches the prototype" has teeth:

- **Tokens** are the objective check — colors/spacing/radii/shadows come from the package or
  they're wrong. No raw values.
- **Components** pass when they match the specimen in `../guidelines/` and the API in the `.d.ts`.
- **Screens** pass against `../apps/maestro/` per the `docs/canon-map.md` triage.
- Motion, voice, and the glass/matte split are canon (`build-docs/CLAUDE.md`), not polish.

---

## 8. Open items to resolve with Broomva

- **Blackhole logo is raster only** (`../assets/broomva-blackhole-logo.png`) — request the SVG.
- **CalSans ships SemiBold only**, display/marketing use only — never in app chrome.
- Icons use **inline path data** (`McIcon` in `apps/maestro/WorkData.jsx`), no CDN — decide the
  production icon strategy (`docs/production-notes.md`).
- Remaining decisions (git-on-approve, notifications, threat model, testing) in
  `build-docs/specs/DECISIONS.md`.

---

## 9. Kicking off the build

`KICKOFF-PROMPT.md` is the ready-to-paste Claude Code prompt: it has Claude read this handoff,
write a brief + open questions, then plan the whole roadmap into a **Linear project** —
one milestone per phase (P0–P6), tickets sized as single-loop work contracts (`done.check` +
`gate: human`), dependency links enforcing the spine and guardrails-before-features, and the
four seams gated as their own tickets. The autonomy loops then execute the backlog via
`/goal` (seed the objective) and `/loop` (run ready tickets to the gate).
