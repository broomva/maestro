# Canon amendments ledger

The vendored handoff under `handoff/design_handoff_maestro/` is **canon** (START-HERE §2:
where docs disagree, the owner wins). When a build decision supersedes the handoff, we
**repair the canon in place** rather than override it silently — and record the repair here.

Each entry cites the decision code, what changed, why, and where. Applied as a single
governance PR per **BRO-1769** (`p0-canon-repairs`). Date: **2026-07-09**.

> Provenance: the decision codes (D-*) are the user-approved decisions captured during the
> Maestro build-planning arc (2026-07-09). They live in the Linear project description and the
> workspace planning artifacts; this ledger is the in-repo record so a reader who only has the
> repo can trace every canon change to its decision.

---

## Applied 2026-07-09 (BRO-1769)

| Decision | What changed | Files |
|---|---|---|
| **D-ENUM** | 8-state OrchState is canon (`proposed · reviewing · triggered · running · blocked · review · done · canceled`); `queued` removed from the enum; six plain-voice UI states with the fixed mapping table (Queued = proposed\|reviewing\|triggered; `canceled` renders under the Done group). Legacy `Todo/InProgress/Blocked/InReview` vocabulary deleted. | `docs/data-contract.md` (enum, mapping table, TS union), `build-docs/DATA-MODEL.md` §B.2 mapping table, `build-docs/CLAUDE.md` §Work states, `build-docs/design-system/SKILL.md` (Quick reference), `README.md` (lifecycle rail). repo-root `CLAUDE.md` §Work states — **deferred** (see Deferred below). |
| **D-GATE** | Frontmatter `gate: you\|none` → `gate: human\|auto` (the removed `none` value also fixed in the routine standing-loop example). Verdict enum `approve\|revise\|block\|escalate` recorded. `block` → `canceled` (terminal; "parked" wording removed). Send-back (`revise`) → `triggered` (redispatch, F2). `grant`/`point` do not decide a gate. | `docs/data-contract.md` (frontmatter, verdict rules, routine example), `build-docs/FLOWS.md` F5 (block → canceled) |
| **D-AUTODONE** | "No transition auto-enters `done`" qualified: holds **only when `gate: human`**. Under `gate: auto`, a verifier pass merges and enters `done` (F4 sanctioned). | `docs/data-contract.md`, `build-docs/design-system/SKILL.md` ("The gate is yours"). repo-root `CLAUDE.md` — **deferred** (see Deferred below). |
| **D-DURABILITY** | `budget.*` and `gate.decided` events journal to FS (`session.jsonl` / workspace journal); the `event` table is a pure projection; the index-rebuild guarantee holds **unqualified**. `event.session_id` is nullable for synthetics (persisted). Node creation (F1 `new_mission`) surfaces as `node.updated` — the API §1 synthetic list is **closed** (no `node.created`). | `build-docs/ARCHITECTURE.md` §3(b), `build-docs/DATA-MODEL.md` §B.3, `build-docs/API.md` §1, `build-docs/FLOWS.md` F1 (`node.updated`, not `node.created`) |
| **D-ORDER** | Board/gate attention comparator is **review-first**: `review, blocked, running, triggered, reviewing, proposed, done, canceled`. | `docs/data-contract.md`, `build-docs/DATA-MODEL.md` §B.5 |
| **D-EVENTNAMES** | Child terminal event is `run.exiting {code, reason}` (HARNESS owns the seam); the supervisor derives `run.finished` after reap; F4/VERIFIER spawn on the supervisor-derived event. Bare `verdict` event → `check.verdict` everywhere. Exit-`10` `reason` enum pinned: `budget \| iteration_cap \| no_progress \| user_stop \| fresh_context`. | `build-docs/FLOWS.md` F4, `build-docs/specs/VERIFIER.md` §2/§4/§7, `build-docs/specs/HARNESS.md` §4, `build-docs/DATA-MODEL.md` §B.4 |
| **D-COLOR** | StatusBadge "Needs you" dot uses `--bv-blue-accent` (accent-blue 235), not `--bv-blue`. | `build-docs/COMPONENTS.md` §StatusBadge |
| **D-NAME** | Protocol header `x-broomva-protocol` → `x-maestro-protocol`. Composer placeholder default → "Message Maestro" (kept as "Message <agent>"). | `build-docs/API.md`, `build-docs/COMPONENTS.md` §Composer |

### Notes

- **Frozen prototype artifacts.** The `build-docs/design-system/components/**/*.txt` and
  `*.html` files are frozen copies of the running prototype and are **not** edited here — the
  port rebuilds components against the canon spec (`COMPONENTS.md` + the `.d.ts` contract),
  which now carries the "Message Maestro" placeholder. `porting-notes.md` already directs the
  port to the canon, not the prototype mechanics.

### Round 2 — P20 remediation (same day)

The first application of these amendments was **incomplete**: each decision was applied to its
*primary* file but sibling occurrences of the same vocabulary were left stale, so the canon
self-contradicted on the exact mappings the PR was meant to fix. A P20 cross-model adversarial
review (3 lenses — amendment-correctness, completeness/done.check-honesty, collateral-drift)
scored 5/10 and flagged the residue. All confirmed findings were reconciled in a second commit:

- **D-ENUM** — `build-docs/DATA-MODEL.md` §B.2 plain-voice table mapped `triggered → Running`
  and invented a 7th plain-voice state `Canceled`; corrected to `triggered → Queued` and
  `canceled → Done` (matching `data-contract.md`). `build-docs/design-system/SKILL.md` and
  `README.md` still carried legacy `queued`/`Todo·InProgress·Blocked·InReview` vocabulary; both
  reconciled to the 8-state OrchState.
- **D-DURABILITY** — `build-docs/FLOWS.md` F1 step 4 still emitted the abolished `node.created`;
  changed to `node.updated`, matching the closed synthetic list in `API.md` §1.
- **D-GATE** — `build-docs/FLOWS.md` F5 block verdict still offered `canceled or parked`;
  dropped "or parked" (terminal `canceled`). The removed `gate: none` value survived in the
  routine standing-loop examples of `data-contract.md` and `SKILL.md`; both → `gate: auto`.
- **D-AUTODONE** — qualifier added to `SKILL.md` ("The gate is yours").
- **D-EVENTNAMES** — `VERIFIER.md` §7 Events list still named the event `verdict`; → `check.verdict`.
- **Ledger honesty** — this ledger previously listed repo-root `CLAUDE.md` (deferred) and
  `build-docs/CLAUDE.md` (never got a D-AUTODONE edit) as amended; the Files columns above now
  reflect what was actually touched.

The lesson: a canon-repair PR needs a repo-wide `grep` for each removed/renamed token, not a
per-decision primary-file edit. The done.check greps (which matched only the migrated frontmatter
strings) went green while sibling residue survived — an asymmetric check. Widen `done.check` to
assert *absence* of the removed tokens (`gate: none`, `node.created`, bare ` verdict ` event,
`queued` system-state) in a future hardening pass.

### Deferred (this PR's scope)

- **repo-root `CLAUDE.md` §Work states** carries the same legacy `Todo/InProgress/Blocked/InReview`
  vocabulary. Its one-line sync was **deferred**: the L3 governance-churn rate gate (RCS stability
  budget, one governance-file mutation per 24h) had already been spent today, and the edit is out
  of this PR's stated scope (the canon lives under `handoff/`; repo-root `CLAUDE.md` is workspace
  governance). Fold into the next governance PR / next L3 window.

## Recorded, doc-body edits deferred

| Decision | Status | Owner |
|---|---|---|
| **D-AUTH** | **Recorded.** The relay's client↔relay auth is **Better Auth** (the Broomva convention — never NextAuth/Clerk), superseding the handoff's Clerk/WorkOS references (`build-docs/STACK.md`, `build-docs/ARCHITECTURE.md` §6, `build-docs/API.md` §3). The relay does not exist until P6, so the handoff-doc *body* edits are deferred to the relay ticket rather than made speculatively now. | **BRO-1748** (`p6-relay-better-auth`) applies the doc-body edits when the relay lands. |

> This entry retires the traceability gap noted in the BRO-1758 P20 review: `apps/relay/README.md`
> states the Better Auth direction and points here for the recorded decision; the canon docs
> (Clerk/WorkOS) are formally superseded by this ledger, with the in-doc edits owned by BRO-1748.
