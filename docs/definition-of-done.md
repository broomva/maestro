# Definition of done

The standing rules **every ticket and every phase** holds before it is done. Write once, referenced
everywhere: each phase-exit ticket links here rather than re-listing the bar. This is the floor, not
the ceiling — a ticket's own `done.check` adds the specific proof for that unit of work.

Sources this consolidates (each still authoritative for its detail):
[`CLAUDE.md`](../CLAUDE.md) (hard rules) ·
[`build-docs/BUILD-PLAN.md` §Definition of done](../handoff/design_handoff_maestro/build-docs/BUILD-PLAN.md) ·
[`build-docs/ROADMAP.md` §Standing rules](../handoff/design_handoff_maestro/build-docs/ROADMAP.md) ·
[`docs/porting-notes.md` §Production hardening](../handoff/design_handoff_maestro/docs/porting-notes.md).

---

## 1. Design canon (every UI ticket)

- **Light AND dark, both correct.** No ticket ships one theme. Dark is a deep blue-purple canvas,
  never pure black; light is white with barely-blue ink, never pure `#000`.
- **Reduced-motion safe.** Under `prefers-reduced-motion` all animation stops while every state stays
  legible — the Undertow, dot-comet, enter/exit, and morph transitions all gate on it. Motion encodes
  presence, not urgency; calm is load-bearing.
- **Tokens-only.** No raw color, spacing, radius, shadow, or type values in product code — consume the
  `@maestro/tokens` package (`styles.css` + the `@theme` map). Tokens-only is a hard rule, not a
  preference: a hand-typed `oklch(...)`, `#hex`, or `13px` in an app surface fails review. New values
  change the token source, never the consumer.
- **Holds every hard rule in [`CLAUDE.md`](../CLAUDE.md)**: monochrome by default (every neutral on the
  cool axis, no warm grays); color earns its place in exactly the five sanctioned situations; glass is
  earned in exactly three places (overlays, popovers, composer) and never on cards/panels/sidebars/
  chrome; OKLCH only, no invented hex; plain voice, second person, lead with the verb; sentence case
  everywhere (no Title Case, UPPERCASE eyebrows, or wide letterspacing); no emoji in chrome and no
  em dashes in user-facing copy; **no progress percentages** — show receipts; "Needs you" and every
  gate render in accent-blue, **never red**; Lucide icons only; the radii ladder unchanged (pill radius
  for buttons/avatars only, never cards).
- **Matches the prototype where a canon export exists.** Port per
  [`porting-notes.md`](../handoff/design_handoff_maestro/docs/porting-notes.md), do not transplant:
  no `window` globals, state lives in its taxonomy home (not `useState` piles or prop drilling), the
  domain vocabulary/protocol/component boundaries survive the port. `concepts.css` selectors are ported
  only when referenced by a canon export (see `canon-map.md`).

## 2. Correctness — the branch is the receipt

- `bun run typecheck` and `bun run lint` (Biome) are clean.
- The ticket's `done.check` runs green — it is the specific, re-runnable proof for that unit.
- **Empirical validation (P11): reasoning is not validation, interaction is.** Before "done," interact
  with the running/built version — drive the flow, tail logs, capture screenshots/output — and attach
  the evidence. A UI ticket without a light + dark artifact is not verified.
- Show **receipts** (branch, diffstat, checks, judge verdict, timeline), never fake progress.

## 3. Production hardening & accessibility bar

- **Error boundaries** at the surface level: one around each routed view, one around the chat pane, one
  around the inspector. A crashed inspector must not take down the loop.
- **Accessibility minimum (M1–M6):** keyboard reachability for every verb (gate buttons, view toggles,
  palette); `aria-live="polite"` on streaming chat + the wake log; focus trap in the palette and
  drawers; visible focus rings (the tokens define the ai-blue ring).

## 4. Governance (bstack) is part of done

Governance is present from the first commit and **grows with the repo** (Crystallize · P16) — it is not
a one-time setup, and it is never bypassed to ship faster.

- `bstack doctor` is green (primitive-contract compliance).
- The control gates in [`.control/policy.yaml`](../.control/policy.yaml) (G1–G4) are present and enforced;
  the Claude Code hooks are wired (`Stop` → conversation-bridge · P1, `PreToolUse` → control-gate · P2,
  `SessionStart` → skill-freshness · P7).
- The primitive reflexes hold: a ticket for every work unit (P3), PR → CI green → merge (P4), never merge
  red, the cross-model adversarial gate (P20) before substantive merges, a clean tree between work units
  (P10).
- The merge gate is **human on every merge** during the build — no PR auto-completes; clean runs park at
  the gate for approval.

## 5. Standing rules across all phases

- Nothing runs unattended before its guardrails exist (P2's list — budget-in-path, iteration cap,
  no-progress halt, kill switch — is the floor).
- Every phase ships light + dark, reduced-motion safe, holding every hard rule in `CLAUDE.md`.
- When a task conflicts with a pattern in `PATTERNS.md`, the pattern wins or the pattern gets amended —
  no silent exceptions.

---

*Referenced by every phase-exit ticket. When the bar changes, change it here and the phase tickets
inherit it.*
