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
| **D-ENUM** | 8-state OrchState is canon (`proposed · reviewing · triggered · running · blocked · review · done · canceled`); `queued` removed from the enum; six plain-voice UI states with the fixed mapping table (Queued = proposed\|reviewing\|triggered). Legacy `Todo/InProgress/Blocked/InReview` vocabulary deleted. | `docs/data-contract.md` (enum, mapping table, TS union), `build-docs/CLAUDE.md` §Work states, repo-root `CLAUDE.md` §Work states |
| **D-GATE** | Frontmatter `gate: you\|none` → `gate: human\|auto`. Verdict enum `approve\|revise\|block\|escalate` recorded. `block` → `canceled` (terminal; "parked" wording removed). Send-back (`revise`) → `triggered` (redispatch, F2). `grant`/`point` do not decide a gate. | `docs/data-contract.md` |
| **D-AUTODONE** | "No transition auto-enters `done`" qualified: holds **only when `gate: human`**. Under `gate: auto`, a verifier pass merges and enters `done` (F4 sanctioned). | `docs/data-contract.md`, `build-docs/CLAUDE.md` §Work states, repo-root `CLAUDE.md` |
| **D-DURABILITY** | `budget.*` and `gate.decided` events journal to FS (`session.jsonl` / workspace journal); the `event` table is a pure projection; the index-rebuild guarantee holds **unqualified**. `event.session_id` is nullable for synthetics (persisted). Node creation (F1 `new_mission`) surfaces as `node.updated` — the API §1 synthetic list is **closed** (no `node.created`). | `build-docs/ARCHITECTURE.md` §3(b), `build-docs/DATA-MODEL.md` §B.3, `build-docs/API.md` §1 |
| **D-ORDER** | Board/gate attention comparator is **review-first**: `review, blocked, running, triggered, reviewing, proposed, done, canceled`. | `docs/data-contract.md`, `build-docs/DATA-MODEL.md` §B.5 |
| **D-EVENTNAMES** | Child terminal event is `run.exiting {code, reason}` (HARNESS owns the seam); the supervisor derives `run.finished` after reap; F4/VERIFIER spawn on the supervisor-derived event. Bare `verdict` event → `check.verdict` everywhere. Exit-`10` `reason` enum pinned: `budget \| iteration_cap \| no_progress \| user_stop \| fresh_context`. | `build-docs/FLOWS.md` F4, `build-docs/specs/VERIFIER.md` §2/§4, `build-docs/specs/HARNESS.md` §4, `build-docs/DATA-MODEL.md` §B.4 |
| **D-COLOR** | StatusBadge "Needs you" dot uses `--bv-blue-accent` (accent-blue 235), not `--bv-blue`. | `build-docs/COMPONENTS.md` §StatusBadge |
| **D-NAME** | Protocol header `x-broomva-protocol` → `x-maestro-protocol`. Composer placeholder default → "Message Maestro" (kept as "Message <agent>"). | `build-docs/API.md`, `build-docs/COMPONENTS.md` §Composer |

### Notes

- **Frozen prototype artifacts.** The `build-docs/design-system/components/**/*.txt` and
  `*.html` files are frozen copies of the running prototype and are **not** edited here — the
  port rebuilds components against the canon spec (`COMPONENTS.md` + the `.d.ts` contract),
  which now carries the "Message Maestro" placeholder. `porting-notes.md` already directs the
  port to the canon, not the prototype mechanics.

## Recorded, doc-body edits deferred

| Decision | Status | Owner |
|---|---|---|
| **D-AUTH** | **Recorded.** The relay's client↔relay auth is **Better Auth** (the Broomva convention — never NextAuth/Clerk), superseding the handoff's Clerk/WorkOS references (`build-docs/STACK.md`, `build-docs/ARCHITECTURE.md` §6, `build-docs/API.md` §3). The relay does not exist until P6, so the handoff-doc *body* edits are deferred to the relay ticket rather than made speculatively now. | **BRO-1748** (`p6-relay-better-auth`) applies the doc-body edits when the relay lands. |

> This entry retires the traceability gap noted in the BRO-1758 P20 review: `apps/relay/README.md`
> states the Better Auth direction and points here for the recorded decision; the canon docs
> (Clerk/WorkOS) are formally superseded by this ledger, with the in-doc edits owned by BRO-1748.
