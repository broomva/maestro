# Decisions

The decision record: git semantics on approve (the hard one), the four `DATA-MODEL.md` open questions, notification delivery, the threat model, and the testing strategy. Each entry is a decision, its rule, and the reasoning — argue with the reasoning, then change the rule.

---

## D1 — Git semantics on approve

**Decision: squash-merge, verdict-fresh, branch preserved.**

- **Approve = squash-merge `run/<id>` onto the workspace branch.** One commit per approved run, message from the node title, trailers `Run-Id: <id>`, `Node-Id: <id>`, `Verdict: pass@<attempt>`. Rationale: the workspace history reads as a ledger of *approved work*, one look = one commit; the fine-grained history stays on the run branch.
- **The branch is never deleted.** On merge, the worktree directory is removed; the branch is renamed `archive/run-<id>`. The branch is the receipt (`DATA-MODEL.md` A.1); receipts don't get shredded.
- **Verdict freshness rule:** a verdict is valid only for the base commit it was judged against (`verdict.md` frontmatter `base`). At approve time the runtime attempts the merge:
  - **Clean merge, base unmoved** → merge, done.
  - **Clean merge, base moved but no file overlap** → merge, done (the check ran on the same files it judged).
  - **File overlap or conflict** → the gate stays open; event `merge.stale`; the client shows one extra button: **"Rebase & re-verify."** That intent redispatches (F2, skip folder setup) with a single fix_plan item — "rebase onto <sha>, resolve, do not change scope" — and the verifier runs again. Approve is only ever clicked against a fresh verdict. No human ever resolves a merge conflict inside Broomva v1; the agent does, and re-earns the verdict.
- **Parallel runs touching the same files:** allowed (leases are per-node, not per-file). The freshness rule makes the *second* approve pay the rebase cost. If this bites often, the fix is a Loop-4 observation ("these two routines fight over `nav.tsx`"), not file locking.
- **Human edits mid-run:** same rule — humans commit to the workspace branch freely; the run pays the rebase at approve time.

## D2 — Workspace = one repo (open question 1)

**Decision: yes, one workspace is one git repo, for v1.** Multi-repo work = multiple workspaces, each with its own runtime index; cross-repo initiatives are a later "meta-workspace" whose nodes *reference* other workspaces rather than containing them. No `repo` column on `node` now. Revisit only when a real cross-repo user exists — the cost of adding the dimension later is a migration; the cost of carrying it unused from day one is every query and every code path.

## D3 — Event-log rotation (open question 2)

**Decision: bound the file, keep the index.**

- Routines produce **one run per fire** (F7 already implies this), so task/run logs are naturally bounded.
- Within a run: when `session.jsonl` exceeds **5 MB or 5,000 lines**, the supervisor rotates it to `session.jsonl.1` (…`.2`) and writes `summary.md` — the "summarize every 10–20 steps" rule applied at the file layer. The index `event` table keeps everything (seq is the archive); the FS keeps the tail + summaries. Rebuild-from-FS then reproduces recent truth exactly and older truth summarized — acceptable, because the receipts that matter (verdicts, gates, merges) are their own files.

## D4 — One runtime per workspace (open question 3)

**Decision: exactly one, enforced.** The runtime writes `.broomva/runtime.lock` (runtime id + heartbeat timestamp) at startup; a second runtime seeing a fresh lock refuses to start against the workspace. Team tier = more *clients* on one runtime's stream, plus gate routing by `owner` — never more runtimes. Lease arbitration across runtimes is a distributed-systems bill that nothing in the roadmap orders.

## D5 — Budget reconciliation on crash (open question 4)

**Decision: derive-and-max.** On startup (F9.2), re-derive per-session spend from `budget.metered` events; set `run_budget.spent_usd = max(stored, derived)`. Overcounting a crash-window call is a cent lost; undercounting is the guard leaking. Day totals recompute from the same events. `budget.refused` events are excluded (nothing was spent).

## D6 — Notification delivery (pulled forward from P6)

**Decision: design at P4, ship with P5 dogfood** — "Needs you while the lid is closed" *is* the product promise; unsupervised hours nobody notices are wasted hours.

- The runtime emits `attention.raised { nodeId, kind, headline }` whenever a node enters `blocked | review` (and `attention.cleared` on exit — deliveries can be revoked).
- Delivery adapters, in order: **Tauri native notification** (free, local, P5) → **Web Push** for the PWA (P5) → **email digest** (P6). 
- The relay grows one narrow `POST /notify` endpoint for push/email to devices it can reach: payload is `{ runtimeId, headline, deepLink }` — **no work content beyond the headline**, keeping the relay's zero-knowledge posture. Self-hosted runtimes may instead call a user-configured webhook (ntfy, Slack) directly, bypassing the relay entirely — consistent with the direct/LAN path.

## D7 — Threat model (sketch to hold until a real doc)

The boundary story in one table — phase-1 honest, phase-2 aimed:

| Surface | Threat | Defense (phase 1) |
|---|---|---|
| Runtime host | agent shells out of the worktree | Honest limit: worktrees share the host (`ARCHITECTURE.md` §5). Mitigate: env allowlist + no-secrets-in-child (`HARNESS.md` §1); self-host = user's own trust boundary. Phase 2: containers. |
| Secrets | key exfiltration by agent | Anthropic key only in the model proxy (`HARNESS.md` §3); per-session revocable tokens; `protect` globs stop checker tampering. Workspace files must never hold secrets — bootstrap warns on `.env`-like files. |
| Prompt injection | hostile content in workspace files / hook payloads steers the agent | The gate: irreversible actions and completions route through a human regardless of what the model was talked into. Verifier Stage 0 catches contract/test tampering. Hook payloads render as data in the briefing, never as instructions. |
| Relay | relay compromise reads work | Relay moves opaque bytes, holds one grant table, no work state (`API.md` §3); notifications carry headlines only (D6). |
| Client | stolen client session | Client holds no truth; verbs require the relay identity; the runtime credential lives in the OS keychain (Tauri) — a web session leak exposes read + intents until revoked, never the key. |

## D8 — Testing strategy (to grow into TESTING.md)

Three layers, cheapest first:

1. **Deterministic loop tests** — a **mock model server** behind the model proxy (`HARNESS.md` §3 makes this trivial: point the proxy upstream at a fixture server). Scripted responses drive full F2→F4→F5 flows in CI with zero tokens: budget refusal mid-run, no-progress exit, fresh-context restart resuming from `progress.md`, kill mid-tool.
2. **Adversarial verifier evals** — a fixture workspace + a set of *hostile runs* the verifier must fail: deletes a failing test (Stage 0), edits `package.json` to stub the test script (Stage 0), passes checks with an out-of-scope 400-file diff (diff guard), judge-only contract trying `gate: auto` (contract rejection). P3's exit test is this suite green.
3. **Model-pin canaries** — pin models in the proxy (already required); on any pin bump, run layers 1–2 plus a small live-token smoke suite before rollout. Version drift is a listed failure mode; this is its regression net.

Loop-level correctness lives at the harness seam, so the seams already specified are the test points — no test-only hooks needed.
