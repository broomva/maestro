# BRO-1788 — model proxy, budget-in-path — evidence

The GUARDRAIL: the model proxy that keeps the Anthropic key on the supervisor side and puts the
budget check in the request path (HARNESS §3, F3.1, AUTONOMY §4).

## done.check

```
$ bun test apps/runtime --filter proxy
 157 pass  0 fail  500 expect() calls   (32 proxy/budget tests included)
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

## P20 round-5 fixes (block 3/10 → applied)

Round 5 confirmed key confinement + the per_run/iteration race closed, but proved round-4's
bucket-tagging was the WRONG fix — the overspend was still open (a SECOND new-day call overspends),
plus a MAJOR availability defect. Both closed; the day-accounting model was replaced with a simpler,
correct one.

1. **Cross-midnight straddle STILL overspent per_day** (disqualifier, reproduced 5/5): round-4 tagged
   reservations with their day bucket, but `#rolloverIfNeeded` still ZEROED `#dayReservedUsd`. So a
   call A that reserved on day D and was in flight across midnight had its commitment erased at
   rollover; on D+1, B AND C both reserved (the day looked empty) before A metered — both admitted —
   then A's actual booked on top settled **$1.5 on a $1 cap**. Round-4's test only let ONE new-day
   call reserve before A metered (landing exactly at cap), so it missed it. **Fixed by the correct
   model: outstanding reservations CARRY across the rollover** — only settled spend resets. A
   straddler stays visible, so C is refused. This also drops the round-4 bucket field entirely
   (simpler) and aligns the live day total with the D5 derivation (both attribute a call to its
   settlement/meter timestamp). Live guard dogfood of the reviewer's exact repro:

   ```
   B reserve (D+1):        admitted
   C reserve (D+1):        refused (per_day)   ← pre-fix (zeroing rollover) ADMITS C → $1.5 on a $1 cap
   A meter (settles D+1):  booked
   day total:              $1.0000 (cap $1 — never exceeded)
   ```

   Anti-vacuity: making `#rolloverIfNeeded` also zero `#dayReservedUsd` (drop the carry) fails exactly
   the new `MULTI-CALL cross-midnight straddle` test (`vC.ok` becomes true) — mutation verified.

2. **Image base64 priced as text tokens → false 402 on screenshots** (MAJOR availability): the whole
   payload (including a screenshot's ~410KB base64) went through `JSON.stringify` → byte length, so a
   routine 300KB PNG yielded a ~$7 Opus ceiling and was refused under any small `per_run` cap —
   breaking a core modality (P11 / agent-browser screenshots). **Fixed by stripping image/document
   `source.data` before the byte-length bound** and pricing each block by its per-block token bound
   (images ≤ ~1600 tok, sound; documents bounded to Anthropic's 100-page max, `MAX_DOCUMENT_TOKENS`
   raised 20k→160k — the round-5 finding #3). Live dogfood: the screenshot ceiling drops to **$0.13**
   (admitted under a $5 cap). Anti-vacuity: NOT stripping fails exactly the new `STRIPS … big
   screenshot` test (ceiling > $7) — mutation verified. Plus the modality-magnitude test now pins the
   floor (asserts ≥ text + full `MAX_DOCUMENT_TOKENS`-worth) so a floor mutation is caught (finding #6),
   and the straddle test's discriminator is `vC.ok===false`, not the under-reporting `dayTotalUsd`
   (finding #7).

## P20 round-4 fixes (block 5/10 → superseded by round-5)

Round-4 introduced day-bucket-tagged reservations to close the straddle overspend. Round-5 proved this
incomplete (a second new-day call still overspent) and REPLACED it with the carry-across-rollover model
above; the bucket field is gone. The round-4 modality floors survive (corrected: base64 stripped, doc
floor raised). Retained here for provenance:

Round 4 confirmed key-confinement closed but reopened the overspend (Finding 2, active) plus a latent
modality under-price (Finding 1). Both closed, each with a **mutation-proven** discriminating test.

1. **per_day rollover erased in-flight reservations → overspend** (the disqualifier): `#dayReservedUsd`
   was a single scalar, so a call that RESERVED before UTC midnight and METERED after decremented the
   *new* day's live reservations — freeing a slot still in flight and admitting an over-cap call. The
   single-rollover round-2 test missed it (masked by `Math.max(0,…)`). Fixed by **tagging every
   reservation with its day bucket** (`Reservation {reserveUsd, bucket}`): `#releaseDayReservation`
   only decrements when the reservation's bucket is still the current day; a rolled-over reservation
   was already dropped by `#rolloverIfNeeded`. Live guard dogfood of the multi-call straddle
   (`$1/day` cap, `$0.5` each, A reserves day D and meters D+1 while B/C run on D+1):

   ```
   B reserve (D+1):  admitted
   A meter (D→D+1):  booked full actual to D+1
   C reserve (D+1):  refused (per_day)        ← pre-fix scalar code ADMITS C, settling $1.5 on a $1 cap
   day total:        $1.0000 (cap $1 — never exceeded)
   ```

   Anti-vacuity: dropping the `reservation.bucket === this.#dayBucket` guard fails exactly the new
   `MULTI-CALL cross-midnight straddle` test (mutation verified: 155 pass / 1 fail), green when restored.

2. **byte-length ceiling under-prices image/PDF input** (latent): images/documents are billed by
   DIMENSIONS, not their (compressible) base64 bytes, so a large-dimension highly-compressible blob can
   under-run the byte bound. Fixed with a per-block token **FLOOR** on top of the bytes
   (`MAX_IMAGE_TOKENS = 2000`, `MAX_DOCUMENT_TOKENS = 20000`, counted recursively so nested
   `tool_result` content is caught). The docstring no longer claims `tokens <= bytes` for ANY input —
   it holds for byte-level-BPE TEXT; modalities add explicit token terms. Live dogfood:

   ```
   text-only ceiling:  $0.0023
   +image ceiling:     $0.0383  (+$0.0359, ~2000-token floor)
   +document ceiling:  $0.3484  (20000-token floor >> image)
   ```

   Anti-vacuity: dropping the modality term fails exactly the new `per-image / per-document token
   FLOOR` test (mutation verified), green when restored.

## P20 round-3 fixes (block 4/10 → applied)

Round 3 confirmed key-confinement closed but found the overspend **still open**: the cost ceiling
could UNDER-estimate, and — critically — the fix was **vacuously tested** (reverting to a flat
reserve left all tests green). Both closed:

1. **Ceiling under-count → overspend** (the disqualifier, reopened): `chars / 3.5` is an upper bound
   only for Latin text; dense CJK / base64 packs more tokens per char, so `ceiling < actual`. Fixed
   by bounding input tokens with the payload's **UTF-8 byte length** — for a byte-level BPE tokenizer
   `tokens <= bytes` for ANY input, so `ceiling >= actual` is now guaranteed. Live dogfood of the
   round-3 repro (150K CJK chars, Opus, `per_run $2`, real cost $2.9):

   ```
   byteLength ceiling for 150K-CJK Opus call: $8.12 (round-3 char/3.5 gave $1.45 < actual $2.9)
   dense CJK call on $2 cap: 402 (refused, ceiling > cap) | spent: 0 (round-3 settled $2.9)
   ```

2. **VACUOUS TEST** (the [[self-hosting-vacuous-pass]] failure again): no test discriminated the
   ceiling from a flat reserve. Added two mutation-proven discriminating tests — reverting `proxy.ts`
   to `reserve = 0.5` fails `refused when its CEILING exceeds the budget`; reverting `models.ts` to
   `chars/3.5` fails `uses BYTE length … dense input`. Both mutations verified to fail exactly their
   test, green when restored.
3. **meter()/release() DB throw stranded the day reservation** (availability DoS): both now do the
   in-memory reconcile FIRST (can't strand) with the SQL cache update in try/catch; the durable
   `budget.metered` event is the source of truth (D5 rebuilds the cache). preflight fails CLOSED on a
   DB throw (proxy → retryable 503); step-4 reconcile is best-effort (a 200 never turns into a 500).
   Plus price-override validation edge cases (NaN/negative/missing-slash → table fallback) tested.

## P20 round-2 fixes (block 4/10 → applied)

Round 2 confirmed D2 (key confinement) closed but found D1 (overspend) still open **at shipped
defaults**, plus two new fail-open seams the round-1 fix introduced. All closed:

1. **Reserve too small → overspend** (round-1 disqualifier reproduced): a flat `$0.50` reserve is
   below a real Opus call, so the reservation didn't bound spend. Fixed with a **per-call cost
   ceiling** (`models.ts estimateCallCeilingUsd`) = model price × request `max_tokens` (+ input
   over-estimate + safety margin) ≥ actual. A call whose ceiling exceeds the remaining budget is
   **refused up-front**. The reviewer's exact repro is now closed — live socket dogfood:

   ```
   ceiling for a 64k-out Opus call: $5.52 (> the $1 cap → must refuse)
   two concurrent $2 calls on a $1 cap: 402 402 (both refused upfront)
   spent_usd after the round-1 repro: 0 (0 = NO overspend; was $4.00)
   right-sized Haiku call: 200 | metered spent: 0.020
   ```

2. **per_day fail-open across UTC midnight**: meter applied a relative delta to a bucket rollover
   had zeroed. Fixed — the day accounting is **split into reserved + spent**; meter books the **full
   actual** to the current day, so a call reserved before midnight and metered after is counted on
   the new day (regression test: `books FULL actual to the new day`).
3. **Refusal-path day rollback drove the accumulator negative** → centralized `#releaseDayReservation`
   (rollover-aware + `Math.max(0,…)`), the one mutation helper so no site drifts.
4. Plus: preflight rolls back the day reservation if the SQL UPDATE **throws** (not just
   rowsAffected 0); dropped the dead `token === null ||` disjunct in the revocation re-check.

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
