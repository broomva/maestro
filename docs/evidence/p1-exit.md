# P1 exit — evidence (BRO-1823)

The ROADMAP §P1 exit gate, run end to end against a real runtime over a temp fixture workspace:

> **Exit:** edit a `_work.md` by hand; the board updates over the stream without reload. Kill the
> index file; it rebuilds identical.

Both halves are one scripted E2E — `apps/app/tests/p1-exit.pw.ts` — plus the runtime-side identity
unit test. Reproduce with the two done.check commands:

```bash
# ① the read seam + ② the rebuild seam, end to end (boots a real runtime, drives a browser)
bunx playwright test p1-exit          # (done.check writes `playwright test p1-exit.spec`; the repo
                                      #  names E2E specs *.pw.ts so `bun test` skips them, so the
                                      #  runnable form is `playwright test p1-exit`)
# ② the index-rebuild identity invariant, unit level
bun test apps/runtime/src/db/rebuild.test.ts
```

## Result — both halves pass

```
Running 2 tests using 1 worker
maestro runtime · protocol 1 · http://localhost:4319 · workspace …/maestro-p1-exit-BgnBw9 · index 4 nodes
  ✓  P1 exit ①: a hand-edit to a _work.md propagates to the board live, with no reload (645ms)
maestro runtime · index rebuilt · 4 nodes · …/maestro-p1-exit-BgnBw9/.maestro/index.db
maestro runtime · protocol 1 · http://localhost:4319 · workspace …/maestro-p1-exit-BgnBw9 · index 4 nodes
P1 exit ②: rebuilt index identical — 4 nodes, before === after
  ✓  P1 exit ②: killing the index rebuilds it identical and the app serves the same board (427ms)
  2 passed (2.5s)

# rebuild.test.ts: 5 pass / 0 fail (identity modulo clock, delete-not-reopen, fresh-ws mkdir, rejects :memory:)
```

## ① Hand-edit → board, no reload

The fixture `later/_work.md` is flipped `proposed → review` on disk mid-session. The card moves into
the **Needs you** (review) group within the SSE window, and a `window.__boardAlive` sentinel planted
before the edit **survives** — proving the update arrived over the stream, not via a reload (a reload
preserves the URL, so a URL check can't prove it; the sentinel can).

## ② Kill the index → rebuilds identical

The runtime is killed, `<ws>/.maestro/index.db` (+ WAL/SHM) is deleted, `--rebuild` rescans the FS
(the source of truth) and exits 0, and the runtime restarts over the rebuilt index. The work tree the
app serves — `/api/tree`, minus the one volatile `updatedAt` scan clock (exactly what `dumpIndex`
strips), id-sorted — is **identical before the kill and after the rebuild**:

```
nodes before kill:   4
nodes after rebuild: 4
identical (updatedAt-stripped): true
```

## Artifacts

- Screen recordings (one per half): `apps/app/test-results/p1-exit.pw.ts-*/video.webm` (Playwright
  `video: "on"`; gitignored local artifact).
- Rebuild diff (node counts + verdict + the compared trees): `apps/app/test-results/p1-exit-rebuild-diff.txt`.

P1 exits; P2 opens (BRO-1756 runner port, BRO-1746 sandbox port).
