# P1 exit — evidence (BRO-1823)

The ROADMAP §P1 exit gate, run end to end against a real runtime over a temp fixture workspace:

> **Exit:** edit a `_work.md` by hand; the board updates over the stream without reload. Kill the
> index file; it rebuilds identical.

Both halves are one scripted E2E — `apps/app/tests/p1-exit.pw.ts` — backstopped by the runtime-side
identity unit test. Reproduce with:

```bash
# The Playwright webServer serves the built SPA (vite preview over the gitignored dist/), so build first:
bun run --filter @maestro/app build

# ① the read seam + ② the rebuild seam, end to end (boots a real runtime, drives a browser)
bunx playwright test p1-exit          # (the ticket done.check writes `playwright test p1-exit.spec`,
                                      #  whose `.spec` never follows `p1-exit.` in the shipped filename
                                      #  `p1-exit.pw.ts` — it matches zero tests and exits 1. The repo
                                      #  names E2E specs *.pw.ts so `bun test` skips them; the runnable
                                      #  form is `playwright test p1-exit`.)
# the index-rebuild identity invariant, unit level (in-process backstop)
bun test apps/runtime/src/db/rebuild.test.ts
```

## Result — both halves pass

```
Running 2 tests using 1 worker
maestro runtime · protocol 1 · http://localhost:4319 · workspace …/maestro-p1-exit-YpK6yT · index 4 nodes
  ✓  P1 exit ①: a hand-edit to a _work.md propagates to the board live, with no reload (630ms)
maestro runtime · index rebuilt · 4 nodes · …/maestro-p1-exit-YpK6yT/.maestro/index.db
P1 exit ②: --rebuild produced an identical index — 4 nodes
  ✓  P1 exit ②: killing the index rebuilds it identical (from --rebuild's own output) (510ms)
  2 passed (2.6s)

# rebuild.test.ts: 5 pass / 0 fail (identity modulo clock, delete-not-reopen, fresh-ws mkdir, rejects :memory:)
```

## ① Hand-edit → board, no reload

The fixture `later/_work.md` is flipped `proposed → review` on disk mid-session. The card moves into
the **Needs you** (review) group within the SSE window, and a `window.__boardAlive` sentinel planted
before the edit **survives** — proving the update arrived over the stream, not via a reload (a reload
preserves the URL, so a URL check can't prove it; the sentinel can).

## ② Kill the index → rebuilds identical

The runtime is killed, its on-disk index is dumped (the pre-kill truth), `<ws>/.maestro/index.db`
(+ WAL/SHM) is deleted, `--rebuild` rescans the FS (the source of truth) and exits 0, and the rebuilt
index is dumped again. Both dumps are read **directly from the db file** (canonical: every node column
except the volatile `updatedAt` scan clock, id-sorted) — deliberately, not via a restarted runtime,
whose unconditional boot rescan would repopulate the index from the FS and mask whatever `--rebuild`
actually wrote. So the identity below is **load-bearing on `--rebuild`'s own output** — a broken
rebuild (0 rows / wrong states) fails it:

```
nodes before kill:   4
nodes after rebuild: 4
identical (updatedAt-stripped canonical dump): true
```

Then the runtime restarts over the rebuilt index and `/app` renders — the app works end to end over
the rebuilt cache. `rebuild.test.ts` is the in-process backstop for the same identity invariant
(dump-equal with the clock moved, proving the strip is load-bearing not vacuous).

## Artifacts

- Screen recordings (one per half): `apps/app/test-results/p1-exit.pw.ts-*/video.webm` (Playwright
  `video: "on"`; gitignored local artifact).
- Rebuild diff (node counts + verdict + the compared trees): `apps/app/test-results/p1-exit-rebuild-diff.txt`.

P1 exits; P2 opens (BRO-1756 runner port, BRO-1746 sandbox port).
