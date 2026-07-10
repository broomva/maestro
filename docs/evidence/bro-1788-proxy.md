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
bound: http://127.0.0.1:49341
authorized: 200 | upstream saw key: sk-ant-RUNTIME | body: {"ok":true,"model":"claude-opus-4-8"}
no-bearer:  401 | upstream saw key: null
exhausted:  402 | code: budget_exhausted
refused events: 1 | metered events: 2
```

- **authorized** → 200; the upstream forwarder saw `sk-ant-RUNTIME` — the key attached at forward
  time, from the supervisor, never in the child; the agent role resolved the pinned `claude-opus-4-8`.
- **no bearer** → 401; the request never reached the upstream (`upstream saw key: null`).
- **exhausted** → 402 `budget_exhausted`; `budget.refused` journaled.

## Design notes

- **Authoritative state:** `run_budget` (per-session `spent_usd` + `iterations`) via atomic
  single-statement SQL — a conditional UPDATE reserves an iteration (`rowsAffected 0` ⇒ a cap
  blocked it), `spent += usd` meters. libSQL serializes these as the single writer, so no lost
  updates and no over-reservation under concurrency. (No `db.transaction()` — it is broken on
  `:memory:`; single-statement atomics are both correct and simpler.)
- **Day total:** an in-memory accumulator (the runtime is single-writer), seeded at startup from
  `budget.metered` events (D5 derive-and-max — `deriveDayTotal` / `deriveSpentBySession` ship here;
  BRO-1814 wires them at F9.2).
- **Key confinement:** `apiKey` is a getter read only at forward time — never stored on a
  long-lived object graph a child could reach. The child (BRO-1756 `buildChildEnv`) gets only
  `BROOMVA_MODEL_PROXY` + `BROOMVA_MODEL_TOKEN`.
- **Tokens:** per-session bearer minted at spawn, revoked on kill; a re-mint (fresh-context restart,
  same session id) drops the prior token so it can't outlive its process.
- **Model pinning** lives supervisor-side (role → pinned model; `MAESTRO_MODEL_<ROLE>` override) so
  a version bump is one config change and the D8 canary (BRO-1806) can route without the child knowing.

## Reproduce

```
cd apps/maestro && bun test apps/runtime --filter proxy
```
