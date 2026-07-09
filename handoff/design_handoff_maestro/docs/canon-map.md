# Canon map — what each file is

The prototype grew out of a concepts canvas; several files keep `Concept` names but export **shipped** pieces. Nothing in `apps/maestro/` is dead code — every file exports something the running app uses — but many files *also* contain superseded exploration frames. This map says, per file, what to implement and what to ignore.

Status legend: **canon** = implement it · **mixed** = implement the listed exports, ignore the rest · **infra** = prototype rig, replace with your stack's equivalent.

This map says **what** to implement; `porting-notes.md` (same folder) says **how** — the mechanical translation table, where each piece of component state lands (store / persisted prefs / local), and the production hardening bar.

## Entry + shell

- `index.html` — **infra.** Load order, PWA meta, and the service-worker kill switch (see production notes). Replace with your app entry.
- `MaestroApp.jsx` — **canon.** Root: view routing (app/knowledge/history/settings/user), theme, ⌘K wiring, viewport switch, Tweaks panel (design-review only).
- `ds-adapter.jsx` — **infra.** Bridges the compiled design-system bundle to `Ds*` globals. In production, import the design-system components directly.
- `tweaks-panel.jsx` — **infra.** Design-review rig; drop it. Density + theme move to Settings → Appearance.

## The main surface

- `ConceptMaestroLoop.jsx` — **mixed.** `MccMaestroLoopV2` (with `app={true}, initialMode="mission"`) IS the home screen: mission plane ⇄ collapsed dock, tab strip, gate queue, chat pane. Ignore `MccMaestroLoop` / `MccMaestroLoopFolder` / `MccMaestroLoopMission` / `MccLoopStory` (earlier canvas frames).
- `PromptPlate.jsx` — **canon.** The composer (`MccPromptPlate`) + dispatch-rail bits + `Icx*` icons.
- `WorkPanel.jsx` — **canon.** Top bar (`McvTopBar` — bench + tick timer), live right panel (`McvLivePanel`), `MCC_TICK_WAKES` (wake-cause vocabulary).
- `WorkPlanes.jsx` — **canon.** Feed / board / list planes, view toggle, the Undertow-wearing live card.
- `WorkFeed.jsx` — **canon.** Feed grouping (attention-first), work cards, state labels.
- `WorkDetail.jsx` — **canon.** The inspector: lifecycle rail, receipt timeline, chat projection, gate buttons.
- `WorkShell.jsx` — **mixed.** `McSidebar` + `McAvatar` + folder icons are canon; `McTopBar` is the legacy top bar superseded by `McvTopBar`.
- `ConceptMcDock.jsx` — **mixed.** `MccDockFeedBody` (the dock's feed vocabulary, wired as variant M1) is canon; the other `MccDock*` frames are the exploration that led to it.
- `ConceptTreeClick.jsx` — **mixed.** `MccTcSidebar` is the canonical workspace tree (used by every surface); `MccTcPlane`/`MccTcPanel` document root/initiative/project inspector behavior. `MccTreeSchema`/`MccTree*` frames are canvas exhibits.
- `ConceptFsTabs.jsx` — **mixed.** `MccFilePane`, `MccFsDoc` (the FS pane + file view, placement B: chrome-level tabs) are canon and used by the loop + mobile. `MccFsTabsPanel`/`MccFsTabsChrome` are the A/B exploration exhibit.
- `ConceptAttention.jsx` — **mixed (mostly historical).** Only `MccLoopDot` + `MCC_AT_LOOPS` (loop presence dot + seed loops) are consumed. The three attention frames are the exploration that produced the maestro loop — read for rationale, don't build.
- `ConceptNavIA.jsx` — **mixed.** `BvNav`/`NavTreeRows`/`NavBench`/`NavAutonomy` + `Ic*` icons are the canonical sidebar architecture (History/Knowledge/Settings/User all sit on it). The `MccNav*` inventory frames are the exploration exhibit.

## Full-page views

- `ConceptHistory.jsx` — **canon** (`MccHistory`; `MccHistAnatomy` is an exhibit).
- `ConceptKnowledge.jsx` + `KgGraph.jsx` + `KnowledgeApp.jsx` — **canon.** `MccKnowledge` is the page; `KgGraph`/`KgMiniGraph` the force-directed scope graph; `KG_SCOPES` the demo graph data.
- `ConceptSettings.jsx` — **canon** (`MccSettings`). Ship the two-pane section nav; the editorial-scroll layout is the alternative kept for comparison.
- `ConceptUser.jsx` — **canon** (`MccUser`).
- `ConceptFeedback.jsx` — **canon** (`MccFeedback` drawer).
- `LiveCommand.jsx` — **canon** (`MccCommandPalette`).
- `MobileShell.jsx` — **canon.** `useBvViewport` + `MccMobileShell`. Ship the **sheet** nav model; page/menu/edge are explorations behind the Tweaks knob.

## Data + protocol

- `WorkData.jsx` — **canon shapes, demo values.** `McIcon` + `Ic*` (inline Lucide paths), `WK_STATES`/`WK_TONE_COLOR`/`WK_GROUP_ORDER`/`WK_GROUP_HINTS`/`WK_ATTENTION`/`WK_PHASE` (the state vocabulary — canon), `WK_INITIATIVES`/`WK_ITEMS`/`WK_REPLY` (seed data — replace with the real store).
- `AiProtocol.jsx` — **canon contract, mock transports.** The UIMessage shapes, stream-chunk reducer (`bvApplyChunk`), `useBvChat`, message/tool-part renderers, dispatch rail, and the model/harness/effort catalog are canon. `BvAnthropicTransport`/`BvOpenAITransport`/`BvHarnessTransport` are mocks a real backend replaces (same `ChatTransport` interface). Seed transcripts (`BV_SEED_*`) are demo.

## CSS

- `styles.css` — **canon.** The app-shell layout (planes, cards, panel).
- `concepts.css` — **mixed.** Large (1.7k lines); carries both shipped surface styles and canvas-exhibit styles. Port what the shipped components reference; audit selectors against usage.
- `mobile.css`, `settings.css`, `feedback.css`, `command.css` — **canon** for their surfaces.
- Depends on `ui_kits/desktop/styles.css` (`.bv-app`, `.bv-sidebar`, `.bv-card`, …) and the token entry `styles.css`. **Tokens are the single source of truth**; app CSS should only compose them.

## PWA

- `manifest.webmanifest` + `pwa/*` icons — **canon** (raster icons generated from the raster logo; regenerate from the SVG when available).
