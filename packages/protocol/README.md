# @maestro/protocol

The single language: **events, intents, work items** — the wire contract as code,
imported by both `apps/runtime` and `apps/app` so it is the same source on both
sides, never a codegen seam that drifts (PATTERNS §10).

## Modules

| File | Contract | Canon |
|---|---|---|
| `version.ts` | `x-maestro-protocol: 1` header + version constant | API §Versioning, D-NAME |
| `state.ts` | 8-state `OrchState`, the enumerated transition machine (`transition`, illegal edges throw), gate verdict resolver | DATA-MODEL §B.2, PATTERNS §7, FLOWS F1–F8, D-ENUM/GATE/AUTODONE |
| `plain-voice.ts` | `OrchState → Queued/Running/Stuck/Needs you/Done/Standing`, tones, dots, attention order (`WK_GROUP_ORDER`) | DATA-MODEL §B.2/§B.5, D-ENUM/COLOR/ORDER |
| `intents.ts` | the `Intent` discriminated union (the only writes), `Kind`, `Trigger`, `TickCause` | API §1, FLOWS F5, PATTERNS §3 |
| `events.ts` | `EventEnvelope`, namespaced event types + the closed synthetic list, error codes/shape | DATA-MODEL §A.3/§B.3, API §stream/§4, D-DURABILITY/EVENTNAMES |
| `work.ts` | `WorkContract` frontmatter + the full `done:` schema, session/verdict shapes | DATA-MODEL §A.2, VERIFIER §1/§4 |

## The one rule (PATTERNS §7)

`OrchState` transitions are enumerated in `state.ts`; illegal transitions throw.
Two edges are guarded: `review → done` requires an `approve` gate verdict, and
`running → done` is legal only under `gate: auto` (D-AUTODONE) — under
`gate: human` a clean run parks at `review` ("Needs you"), never auto-done.

## Verify

```bash
bun test packages/protocol   # transition tests (PATTERNS §7) + envelope/type round-trips
bun run typecheck            # proves apps/runtime + apps/app both import this package
```

> The envelope pins **six** namespaces — `run.* | tool.* | check.* | gate.* | budget.* | agent.*`
> (`agent.*` added in BRO-1756 to admit `agent.said` per HARNESS §6; a deliberate widening logged
> in `docs/canon-amendments.md`).
>
> Canon discrepancy still tracked for a later ticket: VERIFIER §7 names `verify.started`,
> `judge.result`, `verify.error`, which fall outside those namespaces. `EventType` does not admit
> them; the verifier-implementation ticket owns reconciling them (fold into `check.*` or widen the
> namespace set as a deliberate protocol edit — the same move BRO-1756 made for `agent.*`).
