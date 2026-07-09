# Porting notes — prototype → production components

How `apps/maestro/` becomes `packages/ui` + `packages/protocol` + `apps/app` code. The prototype's *domain design* (vocabulary, protocol, component boundaries) is canon and must survive the port; its *code mechanics* (window globals, prop drilling, useState piles) are Babel-standalone scaffolding and must not. `canon-map.md` says **which** exports to port; this file says **how**.

## The mechanical translation

Apply to every canon export, no judgment calls needed:

| Prototype | Production |
|---|---|
| `Object.assign(window, {…})` exports | ES modules, named exports; one component per file where a file currently packs several |
| `Ds*` globals (via `ds-adapter.jsx`) | direct imports from `packages/ui` |
| Inline `Ic*`/`Icx*`/`McIcon` SVG paths | pinned `lucide-react`; where a glyph was drawn custom, keep it as a local SVG component in `packages/ui/icons` |
| Implicit shapes (work items, messages, gates) | types in `packages/protocol` — extract from `WorkData.jsx` + `AiProtocol.jsx`, don't invent |
| `WK_STATES` / `WK_TONE_COLOR` / `WK_GROUP_ORDER` / `WK_GROUP_HINTS` / `WK_ATTENTION` / `WK_PHASE` / `MCC_TICK_WAKES` | typed constants in `packages/protocol` — these ARE the state machine; port values verbatim |
| Seed data (`WK_INITIATIVES`, `WK_ITEMS`, `WK_REPLY`, `BV_SEED_*`, `KG_SCOPES`) | delete; the store feeds real data. Keep as fixtures for Storybook/tests only |
| Mock transports (`BvAnthropicTransport` etc.) | one real transport speaking the runtime's SSE, same `ChatTransport` interface. The interface is the contract; the mocks become test doubles |
| `localStorage` persistence sprinkled in components (`mc4-view`, `bv-nav-open`, `bv-ml-cols`) | a single persisted UI-prefs slice in the store; components never touch storage |
| CSS class names | keep verbatim (they compose tokens); translate to Tailwind `@theme` utilities opportunistically, never as a big-bang rewrite |
| `index.html` load order, SW kill switch, `tweaks-panel.jsx` | delete — Vite entry, real SW strategy later, density/theme move to Settings → Appearance |

## State taxonomy — the one real refactor

The prototype has no store; every component owns its state. Production has three homes, and every `useState` in a canon export lands in exactly one:

1. **Server truth** — lives in the Zustand store, fed by the event-log subscription; components read selectors, mutate only via intents. *Never* `useState`.
2. **Persisted UI prefs** — the store's persisted slice (replaces ad-hoc `localStorage`).
3. **Ephemeral UI state** — stays as local `useState`. Hover, drag, open/closed, cursor position.

### `MccMaestroLoopV2` (ConceptMaestroLoop.jsx) — decomposition map

The home surface currently owns ~19 pieces of state. Port it as a thin layout shell over the store, not as a transplant:

**→ Server truth (store):**
- `chatTabs`, `chatAct` — open sessions and the focused one; sessions come from the runtime, tab state syncs to it
- `fileTabs` — open workspace files (FS pane)
- gate items + the grace window (`MccGateQueue`'s `done` map — pending intents with their undo timer; the grace window is *intent-not-yet-sent*, which is store/transport logic, not component state)
- tick history (`ticks`, `buckets` in the rail — projections of `tick` events)

**→ Persisted UI prefs (store slice):**
- `cols` (column widths), `navOpen`, `view` kind, mission-plane `view` (feed/board/list, currently `mc4-view`)

**→ Ephemeral (stays local):**
- `mode` (mission ⇄ workspace plane), `scope`, `shut`, `fsOpen` (responsive collapse), `split`, `dragging`, `overDrop`, rail `hover`/`active`/`pointerY`/`railH`, gate-queue `open` index

Same triage applies to `MccKnowledge` (KnowledgeApp.jsx: `pinned`/`recent` → persisted; `scopeId`/`sel`/`q`/`view`/`filter`/`drawer`/`ask` → ephemeral) and `MccSettings` (all ephemeral except the settings values themselves, which are server truth).

### Rule of thumb
If losing it on refresh would lose *work or context* → store. If losing it would lose a *layout preference* → persisted slice. If nobody would notice → local.

## Production hardening (absent from the prototype by design)

- **Error boundaries** at the surface level: one around each routed view, one around the chat pane, one around the inspector. A crashed inspector must not take down the loop.
- **Memoization**: stateless projections (WorkFeed, WorkPlanes, WorkPanel cards) render per event-log tick — wrap list items in `React.memo` with stable selectors. Don't memoize speculatively elsewhere.
- **A11y**: the prototype was not audited. Minimum bar for M1–M6: keyboard reachability for every verb (gate buttons, view toggles, palette), `aria-live="polite"` on streaming chat + wake log, focus trap in the palette and drawers, visible focus rings (tokens define them).
- **Tests**: vocabulary/reducer level first — `bvApplyChunk` against recorded chunk sequences, state-machine transitions against `WK_STATES`, feed grouping against fixtures. Component tests only for the gate queue's grace window (the one timing-sensitive behavior).
- **`concepts.css` audit**: port only selectors the shipped components reference (grep classnames from the canon exports first); the exhibit styles stay behind.

## Sequencing

Extract `packages/protocol` from `WorkData.jsx` + `AiProtocol.jsx` **before** porting any surface (BUILD-PLAN M0/M1 already order it this way). Every surface port then imports types instead of assuming shapes, and drift is impossible by construction.
