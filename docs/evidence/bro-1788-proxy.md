# BRO-1788 — model proxy, budget-in-path — evidence

The GUARDRAIL: the model proxy that keeps the Anthropic key on the supervisor side and puts the
budget check in the request path (HARNESS §3, F3.1, AUTONOMY §4).

## done.check

```
$ bun test apps/runtime --filter proxy
 138 pass  0 fail  455 expect() calls   (13 proxy/budget tests included)
```

The three done.check guarantees, each a test:

| Guarantee | Test |
|---|---|
| child env has no key | `proxy: the child env is key-free, and the runtime key attaches only at forward time` — `buildChildEnv` (BRO-1756) over a host env holding `ANTHROPIC_API_KEY` yields no key; the proxy forwards WITH that runtime key (the upstream mock captures it) |
| race, no overspend | `RACE: the iteration cap is EXACT under concurrency` — 40 callers race 8 slots → exactly 8 allowed, `iterations` never exceeds the cap; `RACE: concurrent meters never lose an update` — 50 concurrent `meter($0.1)` → `spent_usd == $5.00` exactly |
| refusal → budget.refused + parks blocked | `budget preflight REFUSES per_run and journals budget.refused (parks blocked)` + `proxy: an exhausted budget answers 402 budget_exhausted and never forwards` |

## Live dogfood (P11) — over a real loopback socket

`serveProxy` bound on `127.0.0.1` (loopback only — the proxy holds the key, so never off-host),
driven with real `fetch`:

```
off-host bind refused: serveProxy refuses a non-loopback hostna ...
bound: http://127.0.0.1:50915
authorized: 200 | upstream key: sk-ant-RUNTIME | model: claude-opus-4-8
no-bearer:  401 | upstream key: null
exhausted:  402 | code: budget_exhausted
upstream-throw: 502 | type: upstream_unavailable
```

- **off-host bind** (`hostname: 0.0.0.0`) → **refused** — key confinement is code, not a comment.
- **authorized** → 200; the upstream forwarder saw `sk-ant-RUNTIME` — the key attached at forward
  time, from the supervisor, never in the child; the agent role resolved the pinned `claude-opus-4-8`.
- **no bearer** → 401; the request never reached the upstream (`upstream key: null`).
- **exhausted** → 402 `budget_exhausted`; `budget.refused` journaled.
- **upstream throw** → 502 `upstream_unavailable` (retryable), reservation released — never a bare 500.

## Design notes

- **Reserve-then-reconcile (the no-overspend guarantee):** `preflight` RESERVES a conservative
  per-call cost (`DEFAULT_RESERVE_USD`, configurable) against every cap **atomically** — per_run +
  iterations in one conditional SQL UPDATE (`rowsAffected 0` ⇒ a cap blocked it), per_day in a
  synchronous in-memory check+increment the single-threaded event loop makes atomic. `meter`
  reconciles the reservation to the actual cost; a failed/non-billable call `release`s it (refund
  dollars, keep the consumed iteration → a flaky upstream drains iterations and parks, fail-closed).
  So spend-including-in-flight-reservations never exceeds a cap. **This replaced a
  check-then-meter design that a P20 adversarial probe empirically overspent 4× ($4.00 against a
  $1 cap) — metering only after the response let N concurrent callers all pass on stale spend.**
- **Concurrency:** the reservation UPDATE is a single statement libSQL serializes as the sole writer;
  the day reservation is synchronous (no await between check and increment). Regression-locked by
  RACE tests: 40 callers race a $1 per_run cap at $0.25 reserve → exactly 4 pass; same for per_day.
  (No `db.transaction()` — broken on `:memory:`; single-statement atomics are correct and simpler.)
- **Day total** is workspace-scope: EVERY session's reservation counts toward it (a session with no
  per_day cap still contributes to the day other sessions' caps check against). It **rolls over at
  the UTC day boundary** so per_day is a daily cap, not a lifetime one on the 24/7 runtime. Seeded at
  startup from `budget.metered` (D5 derive-and-max — `deriveDayTotal`/`deriveSpentBySession` ship
  here; BRO-1814 wires them at F9.2).
- **Key confinement:** `apiKey` is a getter read only at forward time — never stored on a
  long-lived object graph a child could reach. `serveProxy` hard-rejects any non-loopback hostname.
  The child (BRO-1756 `buildChildEnv`) gets only `BROOMVA_MODEL_PROXY` + `BROOMVA_MODEL_TOKEN`.
- **Tokens:** per-session bearer minted at spawn, revoked on kill; a re-mint (fresh-context restart,
  same session id) drops the prior token. Revocation is re-checked AFTER preflight, so a kill
  mid-preflight can't let one more call land.
- **Model pinning** lives supervisor-side (role → pinned model; `MAESTRO_MODEL_<ROLE>` override) so
  a version bump is one config change and the D8 canary (BRO-1806) can route without the child knowing.

## P20 round-1 fixes (block 3/10 → applied)

The cross-model gate BLOCKED the first cut and was right to. Applied:

1. **Dollar overspend under concurrency** (disqualifying) → reserve-then-reconcile (above).
2. **serveProxy loopback** was a comment → now a hard error on any non-loopback hostname.
3. **per_day never reset** → UTC day-boundary rollover.
4. **RACE tests pinned per_run at 1000** so money caps never fired → added per_run + per_day dollar-cap
   concurrency tests that fail against the old non-reserving code.
5. **Upstream throw → bare 500** → caught, releases the reservation, returns a retryable 502.
   Plus: revocation re-checked after preflight; no-usage response releases; `PARK_STATE` const.

## Reproduce

```
cd apps/maestro && bun test apps/runtime --filter proxy
```
